//! Everything the desk UI knows, and everything it can ask for.
//!
//! The views in [`crate::views`] are pure functions of [`AppState`]: they read
//! it and push [`Action`]s, and they never mutate anything. That split is what
//! makes the UI testable without a GPU, a relay or a peer — a test builds a
//! state, runs a view, and asserts on the actions that came out.
//!
//! The price of purity is that typing into a text box cannot write straight
//! into the state, so an edit is reported as [`Action::Edit`] and applied by
//! the runtime through [`AppState::apply_edit`]. The widget still shows the
//! character in the frame it was typed, because the view edits its own clone
//! of the field before reporting it.

use std::time::Instant;

use remu_proto::{
    ConnectionRecord, DisplayInfo, HostPermissions, InputEvent, PeerId, SessionId, SessionStats,
    Settings,
};
use uuid::Uuid;

use crate::theme::{Palette, StatusTone};

/// Which screen is showing.
///
/// The predecessor also had a "Discovered" screen that only ever said LAN
/// discovery was not implemented. It is not reproduced: a navigation entry
/// that leads to an apology is worse than no entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Route {
    #[default]
    NewConnection,
    Recent,
    AddressBook,
    Settings,
    About,
    Session,
}

impl Route {
    /// The sidebar entries, in order. [`Route::Session`] is absent because a
    /// session is entered by connecting, not by navigating.
    pub const NAV: &'static [Route] = &[
        Route::NewConnection,
        Route::Recent,
        Route::AddressBook,
        Route::Settings,
        Route::About,
    ];

    /// The sidebar label, which is also the top-bar title.
    pub fn label(self) -> &'static str {
        match self {
            Route::NewConnection => "New Connection",
            Route::Recent => "Recent Sessions",
            Route::AddressBook => "Address Book",
            Route::Settings => "Settings",
            Route::About => "About",
            Route::Session => "Remote Session",
        }
    }

    /// A glyph from the subset egui's bundled fonts are documented to carry,
    /// so a missing icon can never render as tofu.
    pub fn icon(self) -> &'static str {
        match self {
            Route::NewConnection => "+",
            Route::Recent => "🕓",
            Route::AddressBook => "🗐",
            Route::Settings => "🔘",
            Route::About => "❓",
            Route::Session => "🖵",
        }
    }
}

/// How the desk is doing on the relay, and in a session.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum LinkStatus {
    #[default]
    Offline,
    Connecting,
    Ready,
    Calling,
    Incoming,
    Negotiating,
    Connected,
    Failed(String),
}

impl LinkStatus {
    pub fn label(&self) -> &str {
        match self {
            LinkStatus::Offline => "Disconnected",
            LinkStatus::Connecting => "Connecting…",
            LinkStatus::Ready => "Ready",
            LinkStatus::Calling => "Calling…",
            LinkStatus::Incoming => "Incoming…",
            LinkStatus::Negotiating => "Negotiating…",
            LinkStatus::Connected => "Connected",
            // A failure with nothing to say still needs a word in the pill.
            LinkStatus::Failed(reason) if reason.trim().is_empty() => "Error",
            LinkStatus::Failed(reason) => reason,
        }
    }

    pub fn tone(&self) -> StatusTone {
        match self {
            LinkStatus::Offline => StatusTone::Idle,
            LinkStatus::Connecting | LinkStatus::Calling | LinkStatus::Negotiating => {
                StatusTone::Working
            }
            LinkStatus::Ready | LinkStatus::Connected => StatusTone::Good,
            LinkStatus::Incoming => StatusTone::Warn,
            LinkStatus::Failed(_) => StatusTone::Bad,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Ok,
    Info,
    Error,
}

impl ToastKind {
    pub fn tone(self) -> StatusTone {
        match self {
            ToastKind::Ok => StatusTone::Good,
            ToastKind::Info => StatusTone::Working,
            ToastKind::Error => StatusTone::Bad,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Toast {
    pub id: u64,
    pub kind: ToastKind,
    pub text: String,
    pub expires_at: Instant,
}

impl Toast {
    pub fn is_expired(&self, now: Instant) -> bool {
        self.expires_at <= now
    }
}

/// A peer asking to control this desk, awaiting the user's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingRequest {
    pub from: PeerId,
    pub from_alias: Option<String>,
    pub session_id: SessionId,
    /// True when the host's policy would have let a correct unattended
    /// password in without asking, so the prompt can say the peer did not
    /// present one.
    pub needs_password: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatLine {
    pub mine: bool,
    pub text: String,
    /// Unix milliseconds.
    pub at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferRow {
    pub transfer_id: Uuid,
    pub name: String,
    pub size: u64,
    pub transferred: u64,
    pub outgoing: bool,
    /// Human-readable progress word from the runtime ("sending", "done",
    /// "declined"…). The UI shows it verbatim rather than inventing wording
    /// for states only the transfer code knows about.
    pub state: String,
}

impl TransferRow {
    /// Progress in `0.0..=1.0`, with a zero-byte file counted as complete
    /// rather than dividing by zero.
    pub fn fraction(&self) -> f32 {
        if self.size == 0 {
            return 1.0;
        }
        (self.transferred as f64 / self.size as f64).clamp(0.0, 1.0) as f32
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SidePanel {
    #[default]
    None,
    Chat,
    Files,
}

/// Everything on screen while a session is up.
pub struct SessionUi {
    pub role_is_controller: bool,
    pub remote: PeerId,
    pub remote_alias: Option<String>,
    pub chat: Vec<ChatLine>,
    pub chat_draft: String,
    pub transfers: Vec<TransferRow>,
    pub displays: Vec<DisplayInfo>,
    pub active_display: String,
    pub panel: SidePanel,
    pub stats: Option<SessionStats>,
    pub permissions: HostPermissions,
    pub recording: bool,
    /// Latest decoded frame, already uploaded by the runtime. None until the
    /// first frame.
    pub frame: Option<(egui::TextureHandle, [u32; 2])>,
}

impl SessionUi {
    pub fn new(remote: PeerId, role_is_controller: bool) -> Self {
        Self {
            role_is_controller,
            remote,
            remote_alias: None,
            chat: Vec::new(),
            chat_draft: String::new(),
            transfers: Vec::new(),
            displays: Vec::new(),
            active_display: String::new(),
            panel: SidePanel::None,
            stats: None,
            permissions: HostPermissions::default(),
            recording: false,
            frame: None,
        }
    }

    /// What to call the peer: their alias if they gave one, else their ID.
    pub fn remote_label(&self) -> String {
        match self.remote_alias.as_deref().map(str::trim) {
            Some(alias) if !alias.is_empty() => alias.to_owned(),
            _ => self.remote.grouped(),
        }
    }
}

// `egui::TextureHandle` is not `Debug`, and a frame buffer would be noise in a
// log anyway, so the handle is summarized by its dimensions.
impl std::fmt::Debug for SessionUi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionUi")
            .field("role_is_controller", &self.role_is_controller)
            .field("remote", &self.remote)
            .field("remote_alias", &self.remote_alias)
            .field("chat", &self.chat.len())
            .field("transfers", &self.transfers.len())
            .field("displays", &self.displays)
            .field("active_display", &self.active_display)
            .field("panel", &self.panel)
            .field("stats", &self.stats)
            .field("permissions", &self.permissions)
            .field("recording", &self.recording)
            .field("frame", &self.frame.as_ref().map(|(_, size)| *size))
            .finish()
    }
}

/// OS permission states, already rendered to words by the runtime so the views
/// never have to know which platform they are on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionsUi {
    pub screen_recording: String,
    pub accessibility: String,
}

impl Default for PermissionsUi {
    fn default() -> Self {
        Self {
            screen_recording: "unknown".to_owned(),
            accessibility: "unknown".to_owned(),
        }
    }
}

/// The whole UI state. One of these exists; views borrow it.
#[derive(Debug)]
pub struct AppState {
    pub route: Route,
    pub palette: Palette,
    pub my_id: Option<PeerId>,
    pub link: LinkStatus,
    /// The live draft the Settings view edits. Persisted only on
    /// [`Action::SaveSettings`].
    pub settings: Settings,
    pub history: Vec<ConnectionRecord>,
    pub toasts: Vec<Toast>,
    pub incoming: Option<IncomingRequest>,
    pub session: Option<SessionUi>,
    pub connect_id: String,
    pub connect_password: String,
    pub new_contact_id: String,
    pub new_contact_alias: String,
    pub permissions: PermissionsUi,
    /// False means secrets fell back to a `0600` file, which the status bar
    /// says out loud rather than downgrading silently.
    pub secret_backend_is_keychain: bool,
    pub version: String,
    pub platform: String,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            route: Route::default(),
            palette: Palette::DARK,
            my_id: None,
            link: LinkStatus::default(),
            settings: Settings::default(),
            history: Vec::new(),
            toasts: Vec::new(),
            incoming: None,
            session: None,
            connect_id: String::new(),
            connect_password: String::new(),
            new_contact_id: String::new(),
            new_contact_alias: String::new(),
            permissions: PermissionsUi::default(),
            secret_backend_is_keychain: false,
            version: env!("CARGO_PKG_VERSION").to_owned(),
            platform: std::env::consts::OS.to_owned(),
        }
    }
}

impl AppState {
    /// Toasts that have not yet timed out, oldest first.
    pub fn live_toasts(&self, now: Instant) -> impl Iterator<Item = &Toast> {
        self.toasts.iter().filter(move |t| !t.is_expired(now))
    }

    /// Drops timed-out toasts. The runtime calls this once a frame; the view
    /// filters as well, so a missed call delays cleanup rather than showing a
    /// stale toast.
    pub fn prune_toasts(&mut self, now: Instant) {
        self.toasts.retain(|t| !t.is_expired(now));
    }

    pub fn favorites(&self) -> impl Iterator<Item = &ConnectionRecord> {
        self.history.iter().filter(|r| r.favorite)
    }

    /// Applies a field edit a view reported.
    pub fn apply_edit(&mut self, edit: Edit) {
        match edit {
            Edit::ConnectId(value) => self.connect_id = value,
            Edit::ConnectPassword(value) => self.connect_password = value,
            Edit::NewContactId(value) => self.new_contact_id = value,
            Edit::NewContactAlias(value) => self.new_contact_alias = value,
            Edit::ChatDraft(value) => {
                if let Some(session) = self.session.as_mut() {
                    session.chat_draft = value;
                }
            }
            Edit::Settings(settings) => self.settings = *settings,
        }
    }
}

/// A field the user typed into.
///
/// Boxed settings because the struct dwarfs every other variant and clippy is
/// right to object to a one-variant-shaped enum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    ConnectId(String),
    ConnectPassword(String),
    NewContactId(String),
    NewContactAlias(String),
    ChatDraft(String),
    Settings(Box<Settings>),
}

/// Something a view is asking the runtime to do.
///
/// Views never act; they only ask. The runtime drains this list once per frame
/// and is the only code that touches the network, the disk or the OS.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Navigate(Route),
    Connect {
        peer: String,
        password: String,
    },
    AcceptIncoming,
    DeclineIncoming,
    EndSession,
    SendChat(String),
    PickFiles,
    SwitchDisplay(String),
    ToggleRecording,
    ToggleFullscreen,
    SetPanel(SidePanel),
    SaveSettings,
    ReconnectRelay,
    AddContact,
    RemoveContact(PeerId),
    SetFavorite(PeerId, bool),
    ConnectTo(PeerId),
    CopyMyId,
    RequestAccessibility,
    RequestScreenRecording,
    OpenPrivacySettings(remu_input::permissions::PrivacyPane),
    RemoteInput(InputEvent),
    /// A text field changed. See the module docs for why this is an action.
    Edit(Edit),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn peer(n: u32) -> PeerId {
        PeerId::new(n).unwrap()
    }

    #[test]
    fn every_link_status_has_a_label_and_a_tone() {
        let cases = [
            (LinkStatus::Offline, "Disconnected", StatusTone::Idle),
            (LinkStatus::Connecting, "Connecting…", StatusTone::Working),
            (LinkStatus::Ready, "Ready", StatusTone::Good),
            (LinkStatus::Calling, "Calling…", StatusTone::Working),
            (LinkStatus::Incoming, "Incoming…", StatusTone::Warn),
            (LinkStatus::Negotiating, "Negotiating…", StatusTone::Working),
            (LinkStatus::Connected, "Connected", StatusTone::Good),
            (
                LinkStatus::Failed("relay refused the token".into()),
                "relay refused the token",
                StatusTone::Bad,
            ),
        ];
        for (status, label, tone) in cases {
            assert_eq!(status.label(), label, "label for {status:?}");
            assert_eq!(status.tone(), tone, "tone for {status:?}");
        }
    }

    #[test]
    fn a_failure_with_no_message_still_reads_as_an_error() {
        assert_eq!(LinkStatus::Failed(String::new()).label(), "Error");
        assert_eq!(LinkStatus::Failed("   ".into()).label(), "Error");
    }

    #[test]
    fn live_toasts_drops_the_ones_that_have_timed_out() {
        let now = Instant::now();
        let mut state = AppState {
            toasts: vec![
                Toast {
                    id: 1,
                    kind: ToastKind::Ok,
                    text: "gone".into(),
                    expires_at: now - Duration::from_millis(1),
                },
                Toast {
                    id: 2,
                    kind: ToastKind::Error,
                    text: "still here".into(),
                    expires_at: now + Duration::from_secs(3),
                },
            ],
            ..AppState::default()
        };

        let live: Vec<_> = state.live_toasts(now).map(|t| t.id).collect();
        assert_eq!(live, vec![2], "an expired toast must not be drawn");

        state.prune_toasts(now);
        assert_eq!(state.toasts.len(), 1);
        assert_eq!(state.toasts[0].id, 2);
    }

    #[test]
    fn a_toast_expiring_exactly_now_is_already_gone() {
        // The boundary matters: a toast whose deadline is this instant must not
        // survive to the next frame and flicker.
        let now = Instant::now();
        let toast = Toast {
            id: 1,
            kind: ToastKind::Info,
            text: "x".into(),
            expires_at: now,
        };
        assert!(toast.is_expired(now));
    }

    #[test]
    fn applying_an_edit_writes_the_field_the_view_reported() {
        let mut state = AppState::default();
        state.apply_edit(Edit::ConnectId("123 456 789".into()));
        state.apply_edit(Edit::ConnectPassword("hunter2".into()));
        assert_eq!(state.connect_id, "123 456 789");
        assert_eq!(state.connect_password, "hunter2");

        let settings = remu_proto::Settings {
            alias: "Reception PC".into(),
            ..remu_proto::Settings::default()
        };
        state.apply_edit(Edit::Settings(Box::new(settings)));
        assert_eq!(state.settings.alias, "Reception PC");
    }

    #[test]
    fn a_chat_edit_with_no_session_is_dropped_rather_than_panicking() {
        let mut state = AppState::default();
        state.apply_edit(Edit::ChatDraft("typed after the session ended".into()));
        assert!(state.session.is_none());

        state.session = Some(SessionUi::new(peer(123_456_789), true));
        state.apply_edit(Edit::ChatDraft("hello".into()));
        assert_eq!(state.session.as_ref().unwrap().chat_draft, "hello");
    }

    #[test]
    fn a_zero_byte_transfer_reports_complete_instead_of_dividing_by_zero() {
        let row = TransferRow {
            transfer_id: Uuid::nil(),
            name: "empty.txt".into(),
            size: 0,
            transferred: 0,
            outgoing: true,
            state: "done".into(),
        };
        assert_eq!(row.fraction(), 1.0);
    }

    #[test]
    fn transfer_progress_is_clamped_even_if_the_peer_overruns_the_size() {
        let row = TransferRow {
            transfer_id: Uuid::nil(),
            name: "lying.bin".into(),
            size: 100,
            transferred: 10_000,
            outgoing: false,
            state: "receiving".into(),
        };
        assert_eq!(row.fraction(), 1.0);
    }

    #[test]
    fn a_peer_without_an_alias_is_labelled_by_its_grouped_id() {
        let mut session = SessionUi::new(peer(123_456_789), true);
        assert_eq!(session.remote_label(), "123 456 789");
        session.remote_alias = Some("   ".into());
        assert_eq!(
            session.remote_label(),
            "123 456 789",
            "a blank alias must not leave the label empty"
        );
        session.remote_alias = Some("Reception".into());
        assert_eq!(session.remote_label(), "Reception");
    }

    #[test]
    fn the_sidebar_never_offers_to_navigate_into_a_session() {
        assert!(!Route::NAV.contains(&Route::Session));
        for route in Route::NAV {
            assert!(!route.label().is_empty());
        }
    }
}
