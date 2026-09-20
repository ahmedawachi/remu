//! The screens, one file each.
//!
//! Every view has the same shape: `show(ui, state, out)`. It reads the state,
//! draws, and pushes [`crate::state::Action`]s onto `out`. None of them own
//! state, touch the network or block, which is why they can be unit-tested
//! against a hand-built [`AppState`].

pub mod about;
pub mod address_book;
pub mod incoming_dialog;
pub mod new_connection;
pub mod recent;
pub mod session;
pub mod settings;
pub mod sidebar;
pub mod statusbar;
pub mod toasts;
pub mod topbar;

use crate::state::{Action, AppState, Route};

/// Draws whichever screen the current route names.
pub fn show_route(ui: &mut egui::Ui, state: &AppState, out: &mut Vec<Action>) {
    match state.route {
        Route::NewConnection => new_connection::show(ui, state, out),
        Route::Recent => recent::show(ui, state, out),
        Route::AddressBook => address_book::show(ui, state, out),
        Route::Settings => settings::show(ui, state, out),
        Route::About => about::show(ui, state, out),
        Route::Session => session::show(ui, state, out),
    }
}

/// Unix milliseconds, for the "last connected" column.
///
/// A clock before the epoch is impossible on any machine that can run this, so
/// the error case collapses to zero, which reads as "never".
pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::time::{Duration, Instant};

    use remu_proto::{
        ConnectionRecord, DisplayInfo, HostPermissions, PeerId, SessionId, SessionStats,
    };
    use uuid::Uuid;

    use crate::state::{
        AppState, ChatLine, IncomingRequest, LinkStatus, PermissionsUi, SessionUi, SidePanel,
        Toast, ToastKind, TransferRow,
    };
    use crate::theme::Palette;

    pub fn peer(n: u32) -> PeerId {
        PeerId::new(n).expect("test peer id is in range")
    }

    /// A state with something in every field, so a view that dereferences an
    /// optional or indexes a list is exercised rather than skipped.
    pub fn populated() -> AppState {
        let mut session = SessionUi::new(peer(987_654_321), true);
        session.remote_alias = Some("Reception PC".into());
        session.chat = vec![
            ChatLine {
                mine: false,
                text: "are you seeing this?".into(),
                at: 1_700_000_000_000,
            },
            ChatLine {
                mine: true,
                text: "yes".into(),
                at: 1_700_000_001_000,
            },
        ];
        session.chat_draft = "typing…".into();
        session.transfers = vec![TransferRow {
            transfer_id: Uuid::nil(),
            name: "report.pdf".into(),
            size: 91_233,
            transferred: 40_000,
            outgoing: true,
            state: "sending".into(),
        }];
        session.displays = vec![
            DisplayInfo {
                id: "screen:0".into(),
                name: "Built-in Retina".into(),
                width: 3456,
                height: 2234,
                primary: true,
            },
            DisplayInfo {
                id: "screen:1".into(),
                name: "Dell U2720Q".into(),
                width: 3840,
                height: 2160,
                primary: false,
            },
        ];
        session.active_display = "screen:0".into();
        session.panel = SidePanel::Chat;
        session.stats = Some(SessionStats {
            fps: 58.5,
            bitrate_kbps: 4200,
            rtt_ms: 18,
            dropped_frames: 3,
        });
        session.permissions = HostPermissions::default();
        session.recording = true;

        AppState {
            palette: Palette::LIGHT,
            my_id: Some(peer(123_456_789)),
            link: LinkStatus::Connected,
            history: vec![
                ConnectionRecord {
                    peer_id: peer(987_654_321),
                    alias: Some("Reception PC".into()),
                    last_connected_at: 1_700_000_000_000,
                    favorite: true,
                },
                ConnectionRecord {
                    peer_id: peer(111_222_333),
                    alias: None,
                    last_connected_at: 0,
                    favorite: false,
                },
            ],
            toasts: vec![
                Toast {
                    id: 1,
                    kind: ToastKind::Ok,
                    text: "Saved".into(),
                    expires_at: Instant::now() + Duration::from_secs(30),
                },
                Toast {
                    id: 2,
                    kind: ToastKind::Error,
                    text: "Connection rejected: busy".into(),
                    expires_at: Instant::now() + Duration::from_secs(30),
                },
                Toast {
                    id: 3,
                    kind: ToastKind::Info,
                    text: "expired, must not be drawn".into(),
                    expires_at: Instant::now() - Duration::from_secs(1),
                },
            ],
            incoming: Some(IncomingRequest {
                from: peer(555_666_777),
                from_alias: Some("Ahmed's Mac".into()),
                session_id: SessionId::random(),
                needs_password: true,
            }),
            session: Some(session),
            connect_id: "987 654 321".into(),
            connect_password: "hunter2".into(),
            new_contact_id: "111222333".into(),
            new_contact_alias: "Warehouse".into(),
            permissions: PermissionsUi {
                screen_recording: "granted".into(),
                accessibility: "denied".into(),
            },
            secret_backend_is_keychain: false,
            ..AppState::default()
        }
    }
}
