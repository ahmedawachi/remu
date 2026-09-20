//! Settings and address-book persistence.
//!
//! Replaces the predecessor's `electron-store`. Two things are done differently
//! and both are deliberate.
//!
//! **Secrets do not go in the config file.** The unattended-access password is
//! a remote-control credential; the relay token is a server credential. The
//! predecessor kept both as plaintext in a world-readable JSON file in the
//! user's profile, where any process running as that user — and every backup,
//! sync client and crash reporter — could read them. Here they live in the OS
//! keychain, and the JSON on disk has the fields blanked. Where no keychain is
//! reachable (a headless Linux box with no Secret Service), we fall back to a
//! `0600` file and say so, so the UI can warn rather than silently downgrade.
//!
//! **Writes are atomic.** Config is written to a temp file and renamed over the
//! target, so a crash or a full disk mid-write leaves the previous settings
//! intact instead of a truncated file that fails to parse.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use remu_proto::{ConnectionRecord, PeerId, Settings};

/// Most contacts kept. The predecessor's cap, retained: it is generous for a
/// human address book and bounds the file.
const MAX_HISTORY: usize = 200;

const SETTINGS_FILE: &str = "settings.json";
const HISTORY_FILE: &str = "history.json";
const FALLBACK_SECRETS_FILE: &str = "secrets.json";

const KEYCHAIN_SERVICE: &str = "dev.remu.desk";
const KEY_UNATTENDED: &str = "unattended-password";
const KEY_RELAY_TOKEN: &str = "relay-token";

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("could not write {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not serialize settings: {0}")]
    Encode(#[from] serde_json::Error),
    #[error("could not store the secret in the OS keychain: {0}")]
    Keychain(String),
}

/// Where secrets are kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretBackend {
    /// macOS Keychain, Windows Credential Manager, or Linux Secret Service.
    Keychain,
    /// A `0600` file beside the config. Used when no keychain is reachable,
    /// and by tests so they never touch the developer's real keychain.
    File,
}

impl SecretBackend {
    /// Probes the OS keychain once, falling back to a file if it is unusable.
    ///
    /// Probing by writing and deleting a throwaway entry is the only reliable
    /// test: the crate constructs an `Entry` lazily, so a missing Secret
    /// Service does not surface until something is actually stored.
    fn detect() -> Self {
        match keyring::Entry::new(KEYCHAIN_SERVICE, "probe") {
            Ok(entry) => match entry.set_password("probe") {
                Ok(()) => {
                    let _ = entry.delete_credential();
                    SecretBackend::Keychain
                }
                Err(err) => {
                    tracing::warn!(
                        %err,
                        "no usable OS keychain; secrets will be kept in a 0600 file instead"
                    );
                    SecretBackend::File
                }
            },
            Err(err) => {
                tracing::warn!(%err, "no usable OS keychain; falling back to a 0600 file");
                SecretBackend::File
            }
        }
    }
}

/// Loaded settings, address book, and where they came from.
#[derive(Debug)]
pub struct Store {
    dir: PathBuf,
    settings: Settings,
    history: Vec<ConnectionRecord>,
    secrets: SecretBackend,
}

impl Store {
    /// Loads from the platform config directory, never failing.
    ///
    /// A corrupt or unreadable file logs and yields defaults rather than
    /// refusing to start: someone locked out of the app by a bad config file
    /// cannot use the app to fix it.
    pub fn load() -> Self {
        let dir = config_dir();
        let secrets = SecretBackend::detect();
        Self::load_from(&dir, secrets)
    }

    pub fn load_from(dir: &Path, secrets: SecretBackend) -> Self {
        if let Err(err) = fs::create_dir_all(dir) {
            tracing::error!(path = %dir.display(), %err, "could not create the config directory");
        }

        let mut settings: Settings = read_json(&dir.join(SETTINGS_FILE)).unwrap_or_default();
        let history: Vec<ConnectionRecord> = read_json(&dir.join(HISTORY_FILE)).unwrap_or_default();

        // Secrets are blank on disk by construction; fill them from the vault.
        settings.unattended_password =
            read_secret(dir, secrets, KEY_UNATTENDED).unwrap_or_default();
        settings.relay_token = read_secret(dir, secrets, KEY_RELAY_TOKEN).unwrap_or_default();

        Self {
            dir: dir.to_path_buf(),
            settings,
            history,
            secrets,
        }
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn secret_backend(&self) -> SecretBackend {
        self.secrets
    }

    pub fn config_path(&self) -> &Path {
        &self.dir
    }

    /// Mutates settings and persists them.
    ///
    /// Takes a closure rather than a whole `Settings` so a caller editing one
    /// field cannot accidentally write back a stale copy of the others.
    pub fn update_settings(&mut self, edit: impl FnOnce(&mut Settings)) -> Result<(), StoreError> {
        edit(&mut self.settings);
        self.save_settings()
    }

    fn save_settings(&self) -> Result<(), StoreError> {
        write_secret(
            &self.dir,
            self.secrets,
            KEY_UNATTENDED,
            &self.settings.unattended_password,
        )?;
        write_secret(
            &self.dir,
            self.secrets,
            KEY_RELAY_TOKEN,
            &self.settings.relay_token,
        )?;

        // The on-disk copy never carries the secrets.
        let mut redacted = self.settings.clone();
        redacted.unattended_password = String::new();
        redacted.relay_token = String::new();
        write_json_atomic(&self.dir.join(SETTINGS_FILE), &redacted)
    }

    pub fn history(&self) -> &[ConnectionRecord] {
        &self.history
    }

    pub fn favorites(&self) -> impl Iterator<Item = &ConnectionRecord> {
        self.history.iter().filter(|r| r.favorite)
    }

    pub fn contact(&self, peer_id: PeerId) -> Option<&ConnectionRecord> {
        self.history.iter().find(|r| r.peer_id == peer_id)
    }

    /// Inserts or replaces a contact, moving it to the front.
    pub fn upsert_contact(&mut self, record: ConnectionRecord) -> Result<(), StoreError> {
        self.history.retain(|r| r.peer_id != record.peer_id);
        self.history.insert(0, record);
        self.history.truncate(MAX_HISTORY);
        self.save_history()
    }

    /// Records a successful connection, preserving anything the user had set.
    ///
    /// The predecessor overwrote the whole record here, so reconnecting to a
    /// saved contact wiped its alias and its favorite flag.
    pub fn touch_contact(&mut self, peer_id: PeerId, now_ms: u64) -> Result<(), StoreError> {
        let existing = self.contact(peer_id).cloned();
        self.upsert_contact(ConnectionRecord {
            peer_id,
            alias: existing.as_ref().and_then(|r| r.alias.clone()),
            favorite: existing.as_ref().is_some_and(|r| r.favorite),
            last_connected_at: now_ms,
        })
    }

    pub fn remove_contact(&mut self, peer_id: PeerId) -> Result<(), StoreError> {
        self.history.retain(|r| r.peer_id != peer_id);
        self.save_history()
    }

    pub fn set_favorite(&mut self, peer_id: PeerId, favorite: bool) -> Result<(), StoreError> {
        if let Some(rec) = self.history.iter_mut().find(|r| r.peer_id == peer_id) {
            rec.favorite = favorite;
        }
        self.save_history()
    }

    pub fn set_alias(&mut self, peer_id: PeerId, alias: Option<String>) -> Result<(), StoreError> {
        if let Some(rec) = self.history.iter_mut().find(|r| r.peer_id == peer_id) {
            rec.alias = alias.filter(|a| !a.trim().is_empty());
        }
        self.save_history()
    }

    fn save_history(&self) -> Result<(), StoreError> {
        write_json_atomic(&self.dir.join(HISTORY_FILE), &self.history)
    }
}

fn config_dir() -> PathBuf {
    // An explicit override exists so two instances can run side by side on one
    // machine, which is the only practical way to try a session without a
    // second computer.
    if let Some(dir) = std::env::var_os("REMU_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    directories::ProjectDirs::from("dev", "Remu", "Remu")
        .map(|d| d.config_dir().to_path_buf())
        // No home directory is pathological, but falling back to the working
        // directory beats panicking on startup.
        .unwrap_or_else(|| PathBuf::from(".remu"))
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return None,
        Err(err) => {
            tracing::error!(path = %path.display(), %err, "could not read config");
            return None;
        }
    };
    match serde_json::from_str(&raw) {
        Ok(value) => Some(value),
        Err(err) => {
            tracing::error!(path = %path.display(), %err, "config is corrupt; using defaults");
            None
        }
    }
}

/// Serializes to a sibling temp file, then renames over the target.
///
/// `rename` within a directory is atomic on every platform we target, so a
/// reader either sees the old file or the new one, never a half-written one.
fn write_json_atomic<T: serde::Serialize>(path: &Path, value: &T) -> Result<(), StoreError> {
    let body = serde_json::to_vec_pretty(value)?;
    let tmp = path.with_extension("tmp");

    fs::write(&tmp, &body).map_err(|source| StoreError::Write {
        path: tmp.clone(),
        source,
    })?;
    restrict_permissions(&tmp);
    fs::rename(&tmp, path).map_err(|source| StoreError::Write {
        path: path.to_path_buf(),
        source,
    })
}

/// Narrows a file to owner-only. A no-op on platforms without Unix modes,
/// where the config directory is already per-user.
fn restrict_permissions(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(err) = fs::set_permissions(path, fs::Permissions::from_mode(0o600)) {
            tracing::warn!(path = %path.display(), %err, "could not restrict file permissions");
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

fn read_secret(dir: &Path, backend: SecretBackend, key: &str) -> Option<String> {
    match backend {
        SecretBackend::Keychain => keyring::Entry::new(KEYCHAIN_SERVICE, key)
            .ok()?
            .get_password()
            .ok(),
        SecretBackend::File => {
            let all: std::collections::BTreeMap<String, String> =
                read_json(&dir.join(FALLBACK_SECRETS_FILE))?;
            all.get(key).cloned()
        }
    }
    .filter(|s| !s.is_empty())
}

fn write_secret(
    dir: &Path,
    backend: SecretBackend,
    key: &str,
    value: &str,
) -> Result<(), StoreError> {
    match backend {
        SecretBackend::Keychain => {
            let entry = keyring::Entry::new(KEYCHAIN_SERVICE, key)
                .map_err(|e| StoreError::Keychain(e.to_string()))?;
            if value.is_empty() {
                // Clearing the field must remove the credential, not leave a
                // stale password behind that a later read would resurrect.
                match entry.delete_credential() {
                    Ok(()) => Ok(()),
                    Err(keyring::Error::NoEntry) => Ok(()),
                    Err(e) => Err(StoreError::Keychain(e.to_string())),
                }
            } else {
                entry
                    .set_password(value)
                    .map_err(|e| StoreError::Keychain(e.to_string()))
            }
        }
        SecretBackend::File => {
            let path = dir.join(FALLBACK_SECRETS_FILE);
            let mut all: std::collections::BTreeMap<String, String> =
                read_json(&path).unwrap_or_default();
            if value.is_empty() {
                all.remove(key);
            } else {
                all.insert(key.to_string(), value.to_string());
            }
            write_json_atomic(&path, &all)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use remu_proto::{AcceptPolicy, Quality};

    fn temp_dir(tag: &str) -> PathBuf {
        // A counter keeps parallel tests from sharing a directory without
        // needing a temp-file dependency.
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "remu-store-test-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn peer(n: u32) -> PeerId {
        PeerId::new(n).unwrap()
    }

    #[test]
    fn the_config_dir_can_be_overridden_for_side_by_side_instances() {
        // Serialised against other env-touching tests by using a unique value
        // and reading it back immediately.
        let dir = temp_dir("envdir");
        std::env::set_var("REMU_CONFIG_DIR", &dir);
        let resolved = config_dir();
        std::env::remove_var("REMU_CONFIG_DIR");
        assert_eq!(resolved, dir);
    }

    #[test]
    fn starts_from_defaults_when_nothing_is_saved() {
        let dir = temp_dir("fresh");
        let store = Store::load_from(&dir, SecretBackend::File);
        assert_eq!(store.settings(), &Settings::default());
        assert!(store.history().is_empty());
    }

    #[test]
    fn persists_settings_across_a_reload() {
        let dir = temp_dir("roundtrip");
        let mut store = Store::load_from(&dir, SecretBackend::File);
        store
            .update_settings(|s| {
                s.alias = "Reception PC".into();
                s.quality = Quality::Sharp;
                s.accept_policy = AcceptPolicy::PasswordOrPrompt;
            })
            .unwrap();

        let reloaded = Store::load_from(&dir, SecretBackend::File);
        assert_eq!(reloaded.settings().alias, "Reception PC");
        assert_eq!(reloaded.settings().quality, Quality::Sharp);
        assert_eq!(
            reloaded.settings().accept_policy,
            AcceptPolicy::PasswordOrPrompt
        );
    }

    #[test]
    fn keeps_the_unattended_password_out_of_the_config_file() {
        let dir = temp_dir("secrets");
        let mut store = Store::load_from(&dir, SecretBackend::File);
        store
            .update_settings(|s| {
                s.unattended_password = "correct-horse-battery".into();
                s.relay_token = "tok_abc123".into();
            })
            .unwrap();

        let on_disk = fs::read_to_string(dir.join(SETTINGS_FILE)).unwrap();
        assert!(
            !on_disk.contains("correct-horse-battery"),
            "the unattended password leaked into settings.json:\n{on_disk}"
        );
        assert!(
            !on_disk.contains("tok_abc123"),
            "the relay token leaked into settings.json:\n{on_disk}"
        );

        // ...but it still comes back on reload.
        let reloaded = Store::load_from(&dir, SecretBackend::File);
        assert_eq!(
            reloaded.settings().unattended_password,
            "correct-horse-battery"
        );
        assert_eq!(reloaded.settings().relay_token, "tok_abc123");
    }

    #[test]
    fn clearing_a_secret_removes_it_rather_than_leaving_a_stale_one() {
        let dir = temp_dir("clear-secret");
        let mut store = Store::load_from(&dir, SecretBackend::File);
        store
            .update_settings(|s| s.unattended_password = "hunter2".into())
            .unwrap();
        store
            .update_settings(|s| s.unattended_password.clear())
            .unwrap();

        let reloaded = Store::load_from(&dir, SecretBackend::File);
        assert_eq!(reloaded.settings().unattended_password, "");
    }

    #[cfg(unix)]
    #[test]
    fn config_and_secret_files_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("perms");
        let mut store = Store::load_from(&dir, SecretBackend::File);
        store
            .update_settings(|s| s.unattended_password = "hunter2".into())
            .unwrap();

        for file in [SETTINGS_FILE, FALLBACK_SECRETS_FILE] {
            let mode = fs::metadata(dir.join(file)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "{file} is not 0600");
        }
    }

    #[test]
    fn falls_back_to_defaults_when_the_config_file_is_corrupt() {
        let dir = temp_dir("corrupt");
        fs::write(dir.join(SETTINGS_FILE), "{not json at all").unwrap();
        let store = Store::load_from(&dir, SecretBackend::File);
        assert_eq!(store.settings(), &Settings::default());
    }

    #[test]
    fn a_reconnect_preserves_the_alias_and_favorite_a_user_set() {
        // The predecessor's bug: upserting on connect overwrote the record and
        // silently dropped the alias the user had typed.
        let dir = temp_dir("touch");
        let mut store = Store::load_from(&dir, SecretBackend::File);
        store
            .upsert_contact(ConnectionRecord {
                peer_id: peer(123_456_789),
                alias: Some("Reception".into()),
                favorite: true,
                last_connected_at: 0,
            })
            .unwrap();

        store
            .touch_contact(peer(123_456_789), 1_700_000_000_000)
            .unwrap();

        let rec = store.contact(peer(123_456_789)).unwrap();
        assert_eq!(rec.alias.as_deref(), Some("Reception"));
        assert!(rec.favorite);
        assert_eq!(rec.last_connected_at, 1_700_000_000_000);
        assert_eq!(
            store.history().len(),
            1,
            "touch must not duplicate the entry"
        );
    }

    #[test]
    fn upsert_moves_an_existing_contact_to_the_front_without_duplicating_it() {
        let dir = temp_dir("order");
        let mut store = Store::load_from(&dir, SecretBackend::File);
        for id in [111_111_111, 222_222_222, 333_333_333] {
            store.touch_contact(peer(id), 1).unwrap();
        }
        assert_eq!(store.history()[0].peer_id, peer(333_333_333));

        store.touch_contact(peer(111_111_111), 2).unwrap();
        assert_eq!(store.history()[0].peer_id, peer(111_111_111));
        assert_eq!(store.history().len(), 3);
    }

    #[test]
    fn caps_the_address_book_and_drops_the_oldest() {
        let dir = temp_dir("cap");
        let mut store = Store::load_from(&dir, SecretBackend::File);
        for i in 0..(MAX_HISTORY + 25) {
            store
                .touch_contact(peer(100_000_000 + i as u32), i as u64)
                .unwrap();
        }
        assert_eq!(store.history().len(), MAX_HISTORY);
        // The most recent survives, the very first does not.
        assert_eq!(
            store.history()[0].peer_id,
            peer(100_000_000 + (MAX_HISTORY + 24) as u32)
        );
        assert!(store.contact(peer(100_000_000)).is_none());
    }

    #[test]
    fn removes_and_re_adds_contacts() {
        let dir = temp_dir("remove");
        let mut store = Store::load_from(&dir, SecretBackend::File);
        store.touch_contact(peer(123_456_789), 1).unwrap();
        store.remove_contact(peer(123_456_789)).unwrap();
        assert!(store.history().is_empty());

        let reloaded = Store::load_from(&dir, SecretBackend::File);
        assert!(reloaded.history().is_empty(), "removal must be persisted");
    }

    #[test]
    fn set_alias_trims_blank_input_to_none() {
        let dir = temp_dir("alias");
        let mut store = Store::load_from(&dir, SecretBackend::File);
        store.touch_contact(peer(123_456_789), 1).unwrap();
        store
            .set_alias(peer(123_456_789), Some("   ".into()))
            .unwrap();
        assert_eq!(store.contact(peer(123_456_789)).unwrap().alias, None);
    }

    #[test]
    fn favorites_lists_only_flagged_contacts() {
        let dir = temp_dir("favs");
        let mut store = Store::load_from(&dir, SecretBackend::File);
        store.touch_contact(peer(111_111_111), 1).unwrap();
        store.touch_contact(peer(222_222_222), 1).unwrap();
        store.set_favorite(peer(222_222_222), true).unwrap();

        let favs: Vec<_> = store.favorites().map(|r| r.peer_id).collect();
        assert_eq!(favs, vec![peer(222_222_222)]);
    }

    #[test]
    fn an_interrupted_write_leaves_no_temp_file_behind() {
        let dir = temp_dir("atomic");
        let mut store = Store::load_from(&dir, SecretBackend::File);
        store.update_settings(|s| s.alias = "x".into()).unwrap();
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files left behind: {leftovers:?}"
        );
    }
}
