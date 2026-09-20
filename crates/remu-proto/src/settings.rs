//! Persisted user settings and the address book.
//!
//! Every field carries a `#[serde(default)]` so a settings file written by an
//! older build still loads: a missing key takes the default rather than failing
//! the whole parse and silently resetting someone's address book.

use serde::{Deserialize, Serialize};

use crate::peer::PeerId;

pub const DEFAULT_RELAY_URL: &str = "ws://localhost:8765";

/// How the host answers an incoming session request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AcceptPolicy {
    /// Always show the prompt and wait for a human.
    #[default]
    Prompt,
    /// Accept without a prompt when the controller proves the unattended
    /// password; still prompt when it does not.
    PasswordOrPrompt,
    /// Accept anything. Only sensible on a machine meant to be openly reachable.
    Always,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Theme {
    #[default]
    Dark,
    Light,
    System,
}

/// Video quality preference, traded against bandwidth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Quality {
    /// Cap resolution and frame rate hard; for constrained links.
    Low,
    /// 1080p-class, 30fps.
    #[default]
    Balanced,
    /// Native resolution, 60fps where the link allows.
    Sharp,
}

impl Quality {
    /// Target bitrate in kbps, and the frame-rate ceiling.
    pub fn targets(self) -> (u32, u32) {
        match self {
            Quality::Low => (800, 15),
            Quality::Balanced => (4_000, 30),
            Quality::Sharp => (12_000, 60),
        }
    }

    /// Longest edge the encoder will scale down to, or `None` for native.
    pub fn max_edge(self) -> Option<u32> {
        match self {
            Quality::Low => Some(1280),
            Quality::Balanced => Some(1920),
            Quality::Sharp => None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnConfig {
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub credential: String,
}

impl TurnConfig {
    pub fn is_configured(&self) -> bool {
        !self.url.trim().is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub relay_url: String,
    /// Shared secret, when the relay requires one to register.
    pub relay_token: String,
    /// Name shown to peers alongside this desk's ID.
    pub alias: String,
    pub accept_policy: AcceptPolicy,
    /// Empty disables unattended access entirely, whatever the policy says.
    pub unattended_password: String,
    pub allow_input: bool,
    pub allow_files: bool,
    pub allow_clipboard: bool,
    pub turn: TurnConfig,
    pub quality: Quality,
    pub theme: Theme,
    pub start_minimized: bool,
    /// Where received files land. Empty means the OS download directory.
    pub download_dir: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            relay_url: DEFAULT_RELAY_URL.to_string(),
            relay_token: String::new(),
            alias: String::new(),
            accept_policy: AcceptPolicy::Prompt,
            unattended_password: String::new(),
            allow_input: true,
            allow_files: true,
            allow_clipboard: true,
            turn: TurnConfig::default(),
            quality: Quality::default(),
            theme: Theme::default(),
            start_minimized: false,
            download_dir: String::new(),
        }
    }
}

impl Settings {
    /// Whether a controller may skip the prompt by proving the password.
    pub fn unattended_enabled(&self) -> bool {
        !self.unattended_password.is_empty()
            && matches!(
                self.accept_policy,
                AcceptPolicy::PasswordOrPrompt | AcceptPolicy::Always
            )
    }

    /// What the host advertises to a connected controller.
    pub fn host_permissions(&self) -> crate::control::HostPermissions {
        crate::control::HostPermissions {
            input: self.allow_input,
            files: self.allow_files,
            clipboard: self.allow_clipboard,
        }
    }
}

/// One entry in the address book / recent list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionRecord {
    pub peer_id: PeerId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    /// Unix milliseconds; zero means "saved but never connected".
    #[serde(default)]
    pub last_connected_at: u64,
    #[serde(default)]
    pub favorite: bool,
}

impl ConnectionRecord {
    pub fn never_connected(&self) -> bool {
        self.last_connected_at == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_a_settings_file_that_predates_every_current_field() {
        // The whole point of `default`: an old file must not wipe the profile.
        let old = r#"{"relayUrl":"wss://relay.example.com","alias":"Reception"}"#;
        let s: Settings = serde_json::from_str(old).unwrap();
        assert_eq!(s.relay_url, "wss://relay.example.com");
        assert_eq!(s.alias, "Reception");
        assert_eq!(s.quality, Quality::Balanced);
        assert!(s.allow_input);
        assert_eq!(s.accept_policy, AcceptPolicy::Prompt);
    }

    #[test]
    fn loads_an_entirely_empty_settings_object() {
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(s, Settings::default());
    }

    #[test]
    fn round_trips_settings_through_json() {
        let s = Settings {
            alias: "Ahmed's Mac".into(),
            quality: Quality::Sharp,
            accept_policy: AcceptPolicy::PasswordOrPrompt,
            turn: TurnConfig {
                url: "turn:turn.example.com:3478".into(),
                username: "u".into(),
                credential: "c".into(),
            },
            ..Settings::default()
        };
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<Settings>(&json).unwrap(), s);
    }

    #[test]
    fn unattended_needs_both_a_password_and_a_permitting_policy() {
        let mut s = Settings::default();
        assert!(!s.unattended_enabled(), "no password, no unattended access");

        s.unattended_password = "hunter2".into();
        assert!(
            !s.unattended_enabled(),
            "a password alone must not bypass the prompt policy"
        );

        s.accept_policy = AcceptPolicy::PasswordOrPrompt;
        assert!(s.unattended_enabled());

        s.unattended_password.clear();
        assert!(
            !s.unattended_enabled(),
            "clearing the password must disable unattended access immediately"
        );
    }

    #[test]
    fn quality_targets_increase_monotonically() {
        let (low_bitrate, low_fps) = Quality::Low.targets();
        let (bal_bitrate, bal_fps) = Quality::Balanced.targets();
        let (sharp_bitrate, sharp_fps) = Quality::Sharp.targets();
        assert!(low_bitrate < bal_bitrate && bal_bitrate < sharp_bitrate);
        assert!(low_fps < bal_fps && bal_fps < sharp_fps);
        assert!(Quality::Sharp.max_edge().is_none());
        assert!(Quality::Low.max_edge() < Quality::Balanced.max_edge());
    }

    #[test]
    fn host_permissions_mirror_the_toggles() {
        let s = Settings {
            allow_input: false,
            ..Settings::default()
        };
        let p = s.host_permissions();
        assert!(!p.input);
        assert!(p.files);
        assert!(p.clipboard);
    }

    #[test]
    fn a_saved_contact_is_distinguishable_from_a_connected_one() {
        let saved = ConnectionRecord {
            peer_id: PeerId::new(123_456_789).unwrap(),
            alias: Some("Reception".into()),
            last_connected_at: 0,
            favorite: true,
        };
        assert!(saved.never_connected());
        let json = serde_json::to_string(&saved).unwrap();
        assert_eq!(
            serde_json::from_str::<ConnectionRecord>(&json).unwrap(),
            saved
        );
    }
}
