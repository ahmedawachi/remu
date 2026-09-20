//! The application shell: panels, the action loop, and persistence.
//!
//! Views are pure — they read [`AppState`] and push [`Action`]s. This module is
//! the only place that mutates state, writes to disk or talks to the OS, which
//! is what keeps the views testable without a GPU or a network.
//!
//! One frame is: draw the chrome, draw the route, drain the actions, apply
//! them. Actions are applied *after* drawing so a view never observes a
//! half-applied frame.

use std::time::Instant;

use eframe::egui;
use egui::{Align, Layout, Margin};

use crate::runtime::{now_ms, Command, Runtime, Update};
use crate::state::{Action, AppState, Edit, LinkStatus, Route, SessionUi, ToastKind};
use crate::store::Store;
use crate::theme::{self, space, Palette};
use crate::views;

/// Vertical room reserved at the top of the sidebar for the macOS traffic
/// lights, which float over the content because the window uses a full-size
/// content view. Zero elsewhere, where the OS draws its own title bar.
#[cfg(target_os = "macos")]
pub const TITLEBAR_INSET: f32 = 26.0;
#[cfg(not(target_os = "macos"))]
pub const TITLEBAR_INSET: f32 = 0.0;

pub const SIDEBAR_WIDTH: f32 = 236.0;
pub const TOPBAR_HEIGHT: f32 = 52.0;
pub const STATUSBAR_HEIGHT: f32 = 28.0;

pub struct DeskApp {
    state: AppState,
    store: Store,
    /// Monotonic source for toast ids.
    next_toast: u64,
    /// The palette the context was last styled with, so the (expensive) style
    /// rebuild only happens when the theme actually changes rather than on
    /// every frame.
    styled_with: Palette,
    /// Relay, WebRTC and the media loops.
    engine: Runtime,
}

impl DeskApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        crate::fonts::install(&cc.egui_ctx);

        let store = Store::load();
        let system_dark = cc
            .egui_ctx
            .system_theme()
            .is_none_or(|t| t == egui::Theme::Dark);
        let palette = Palette::for_theme(store.settings().theme, system_dark);
        theme::apply(&cc.egui_ctx, palette);

        let state = AppState {
            palette,
            settings: store.settings().clone(),
            history: store.history().to_vec(),
            secret_backend_is_keychain: matches!(
                store.secret_backend(),
                crate::store::SecretBackend::Keychain
            ),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            platform: std::env::consts::OS.to_owned(),
            link: LinkStatus::Offline,
            ..AppState::default()
        };

        // The engine runs off the UI thread and wakes it when something
        // arrives, so the app repaints on events rather than spinning.
        let ctx = cc.egui_ctx.clone();
        let engine = Runtime::start(store.settings().clone(), move || ctx.request_repaint());

        Self {
            state,
            store,
            next_toast: 1,
            styled_with: palette,
            engine,
        }
    }

    /// Applies everything the engine has reported since the last frame.
    fn drain_engine(&mut self, ctx: &egui::Context) {
        let updates: Vec<Update> = self.engine.drain().collect();
        for update in updates {
            match update {
                Update::Link(link) => self.state.link = link,
                Update::MyId(id) => self.state.my_id = Some(id),
                Update::Incoming {
                    from,
                    alias,
                    session_id,
                    authenticated,
                } => {
                    self.state.incoming = Some(crate::state::IncomingRequest {
                        from,
                        from_alias: alias,
                        session_id,
                        // The dialog warns when the caller did not prove the
                        // password, so invert: "needs" means "did not present".
                        needs_password: !authenticated,
                    });
                }
                Update::IncomingWithdrawn => {
                    self.state.incoming = None;
                    self.toast(ToastKind::Info, "The caller hung up");
                }
                Update::SessionStarted {
                    remote,
                    alias,
                    controller,
                } => {
                    self.state.incoming = None;
                    let mut session = SessionUi::new(remote, controller);
                    session.remote_alias = alias;
                    self.state.session = Some(session);
                    self.state.route = Route::Session;
                    // Only a connection that actually happened is worth
                    // remembering, which is why this is here and not at Connect.
                    let _ = self.store.touch_contact(remote, now_ms());
                    self.state.history = self.store.history().to_vec();
                }
                Update::SessionEnded(reason) => {
                    self.state.session = None;
                    if self.state.route == Route::Session {
                        self.state.route = Route::NewConnection;
                    }
                    self.toast(ToastKind::Info, format!("Session ended — {reason}"));
                }
                Update::Frame {
                    width,
                    height,
                    rgba,
                } => self.present_frame(ctx, width, height, rgba),
                Update::Displays { displays, active } => {
                    if let Some(session) = self.state.session.as_mut() {
                        session.displays = displays;
                        session.active_display = active;
                    }
                }
                Update::Chat { text, at } => {
                    if let Some(session) = self.state.session.as_mut() {
                        session.chat.push(crate::state::ChatLine {
                            mine: false,
                            text,
                            at,
                        });
                    }
                }
                Update::Stats(stats) => {
                    if let Some(session) = self.state.session.as_mut() {
                        session.stats = Some(stats);
                    }
                }
                Update::Toast { error, text } => self.toast(
                    if error {
                        ToastKind::Error
                    } else {
                        ToastKind::Ok
                    },
                    text,
                ),
            }
        }
    }

    /// Uploads a decoded frame as the session's texture.
    ///
    /// The texture is replaced in place rather than allocated per frame: at 60
    /// fps a fresh handle each time churns GPU memory and makes egui's texture
    /// table grow without bound.
    fn present_frame(&mut self, ctx: &egui::Context, width: u32, height: u32, rgba: Vec<u8>) {
        let Some(session) = self.state.session.as_mut() else {
            return;
        };
        let expected = width as usize * height as usize * 4;
        if rgba.len() != expected {
            tracing::debug!(got = rgba.len(), expected, "malformed frame; dropping");
            return;
        }
        let image =
            egui::ColorImage::from_rgba_unmultiplied([width as usize, height as usize], &rgba);
        match session.frame.as_mut() {
            Some((handle, dims)) if *dims == [width, height] => {
                handle.set(image, egui::TextureOptions::LINEAR);
            }
            _ => {
                let handle = ctx.load_texture("remote-screen", image, egui::TextureOptions::LINEAR);
                session.frame = Some((handle, [width, height]));
            }
        }
    }

    fn toast(&mut self, kind: ToastKind, text: impl Into<String>) {
        let id = self.next_toast;
        self.next_toast += 1;
        self.state.toasts.push(crate::state::Toast {
            id,
            kind,
            text: text.into(),
            expires_at: Instant::now() + std::time::Duration::from_millis(3500),
        });
    }

    /// Recomputes the palette from the current settings and restyles only if it
    /// changed.
    ///
    /// Called after every settings edit, not only on save: picking "Light" in
    /// Settings has to repaint immediately, or the control looks broken. An
    /// earlier build set the setting but never refreshed the palette, so the
    /// radio moved and nothing else did.
    fn sync_palette(&mut self, ctx: &egui::Context) {
        let system_dark = ctx.system_theme().is_none_or(|t| t == egui::Theme::Dark);
        let wanted = Palette::for_theme(self.state.settings.theme, system_dark);
        self.state.palette = wanted;
        if wanted != self.styled_with {
            theme::apply(ctx, wanted);
            self.styled_with = wanted;
        }
    }

    fn apply(&mut self, ctx: &egui::Context, action: Action) {
        match action {
            Action::Navigate(route) => {
                // Leaving a live session by navigating would strand it; the
                // session route is only left via End.
                if self.state.route != Route::Session || self.state.session.is_none() {
                    self.state.route = route;
                }
            }
            Action::Edit(edit) => {
                let touches_theme = matches!(&edit, Edit::Settings(_));
                self.state.apply_edit(edit);
                if touches_theme {
                    self.sync_palette(ctx);
                }
            }
            Action::SaveSettings => {
                let next = self.state.settings.clone();
                match self.store.update_settings(|s| *s = next) {
                    Ok(()) => self.toast(ToastKind::Ok, "Settings saved"),
                    Err(err) => self.toast(ToastKind::Error, format!("Could not save: {err}")),
                }
                self.sync_palette(ctx);
                self.engine
                    .send(Command::ConnectRelay(Box::new(self.state.settings.clone())));
            }
            Action::CopyMyId => match self.state.my_id {
                Some(id) => {
                    ctx.copy_text(id.to_string());
                    self.toast(ToastKind::Ok, "Desk ID copied");
                }
                None => self.toast(ToastKind::Info, "No desk ID yet — not connected to a relay"),
            },
            Action::AddContact => self.add_contact(),
            Action::RemoveContact(peer) => {
                if let Err(err) = self.store.remove_contact(peer) {
                    self.toast(ToastKind::Error, format!("Could not remove: {err}"));
                }
                self.state.history = self.store.history().to_vec();
            }
            Action::SetFavorite(peer, favorite) => {
                if let Err(err) = self.store.set_favorite(peer, favorite) {
                    self.toast(ToastKind::Error, format!("Could not save: {err}"));
                }
                self.state.history = self.store.history().to_vec();
            }
            Action::RequestAccessibility => {
                remu_input::permissions::request_accessibility();
                self.refresh_permissions();
            }
            Action::OpenPrivacySettings(pane) => {
                remu_input::permissions::open_privacy_settings(pane);
            }
            Action::ToggleFullscreen => {
                let now = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!now));
            }
            Action::SetPanel(panel) => {
                if let Some(session) = self.state.session.as_mut() {
                    session.panel = panel;
                }
            }
            Action::Connect { peer, password } => match remu_proto::PeerId::parse_lenient(&peer) {
                Ok(peer) => self.engine.send(Command::Connect { peer, password }),
                Err(_) => self.toast(ToastKind::Error, "That is not a nine-digit desk ID"),
            },
            Action::ConnectTo(peer) => self.engine.send(Command::Connect {
                peer,
                password: String::new(),
            }),
            Action::AcceptIncoming => self.engine.send(Command::AcceptIncoming),
            Action::DeclineIncoming => {
                self.state.incoming = None;
                self.engine.send(Command::DeclineIncoming);
            }
            Action::EndSession => self.engine.send(Command::EndSession),
            Action::SendChat(text) => {
                if let Some(session) = self.state.session.as_mut() {
                    session.chat.push(crate::state::ChatLine {
                        mine: true,
                        text: text.clone(),
                        at: now_ms(),
                    });
                    session.chat_draft.clear();
                }
                self.engine.send(Command::SendChat(text));
            }
            Action::SwitchDisplay(id) => self.engine.send(Command::SwitchDisplay(id)),
            Action::RemoteInput(event) => self.engine.send(Command::RemoteInput(event)),
            Action::ReconnectRelay => {
                self.engine
                    .send(Command::ConnectRelay(Box::new(self.state.settings.clone())));
            }
            Action::PickFiles => {
                let picked = rfd::FileDialog::new().pick_files().unwrap_or_default();
                if !picked.is_empty() {
                    self.engine.send(Command::SendFiles(picked));
                }
            }
            Action::ToggleRecording => {
                // Recording is not implemented; saying so beats a button that
                // toggles a label and records nothing.
                self.toast(ToastKind::Info, "Session recording is not implemented yet");
            }
        }
    }

    fn add_contact(&mut self) {
        let Ok(peer) = remu_proto::PeerId::parse_lenient(&self.state.new_contact_id) else {
            self.toast(ToastKind::Error, "That is not a nine-digit desk ID");
            return;
        };
        let alias = self.state.new_contact_alias.trim();
        // A contact saved by hand has never been connected to, so
        // last_connected_at stays at the existing value (zero for a new one)
        // rather than pretending the user just used it.
        let existing = self.store.contact(peer).cloned();
        let record = remu_proto::ConnectionRecord {
            peer_id: peer,
            alias: if alias.is_empty() {
                existing.as_ref().and_then(|r| r.alias.clone())
            } else {
                Some(alias.to_owned())
            },
            favorite: true,
            last_connected_at: existing.map(|r| r.last_connected_at).unwrap_or(0),
        };
        match self.store.upsert_contact(record) {
            Ok(()) => {
                self.state.history = self.store.history().to_vec();
                self.state.new_contact_id.clear();
                self.state.new_contact_alias.clear();
                self.toast(ToastKind::Ok, "Added to the address book");
            }
            Err(err) => self.toast(ToastKind::Error, format!("Could not save: {err}")),
        }
    }

    fn refresh_permissions(&mut self) {
        use remu_input::permissions;
        self.state.permissions = crate::state::PermissionsUi {
            screen_recording: format!("{:?}", permissions::screen_recording()).to_lowercase(),
            accessibility: format!("{:?}", permissions::accessibility()).to_lowercase(),
        };
    }
}

impl eframe::App for DeskApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.drain_engine(&ctx);
        let p = self.state.palette;
        self.state.prune_toasts(Instant::now());

        let mut out: Vec<Action> = Vec::new();

        // Sidebar spans the full height so the macOS traffic lights sit over it
        // rather than over a light strip of their own, matching the predecessor.
        egui::Panel::left("sidebar")
            .exact_size(SIDEBAR_WIDTH)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(p.bg_surface)
                    .stroke(egui::Stroke::new(1.0, p.border))
                    .inner_margin(Margin {
                        left: space::MD as i8,
                        right: space::MD as i8,
                        top: (space::MD + TITLEBAR_INSET) as i8,
                        bottom: space::MD as i8,
                    }),
            )
            .show(ui, |ui| views::sidebar::show(ui, &self.state, &mut out));

        egui::Panel::top("topbar")
            .exact_size(TOPBAR_HEIGHT)
            .resizable(false)
            .frame(egui::Frame::new().fill(p.bg_surface).inner_margin(Margin {
                left: space::LG as i8,
                right: space::LG as i8,
                top: space::SM as i8,
                bottom: space::SM as i8,
            }))
            .show(ui, |ui| views::topbar::show(ui, &self.state, &mut out));

        egui::Panel::bottom("statusbar")
            .exact_size(STATUSBAR_HEIGHT)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(p.bg_surface)
                    .inner_margin(Margin::symmetric(space::LG as i8, space::XS as i8)),
            )
            .show(ui, |ui| views::statusbar::show(ui, &self.state, &mut out));

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(p.bg_base)
                    // The session view fills its area edge to edge; every other
                    // screen is a document and wants breathing room.
                    .inner_margin(if self.state.route == Route::Session {
                        Margin::same(space::SM as i8)
                    } else {
                        Margin::symmetric(space::XL as i8, space::LG as i8)
                    }),
            )
            .show(ui, |ui| {
                if self.state.route == Route::Session {
                    // The session fills its area exactly and must not scroll.
                    views::show_route(ui, &self.state, &mut out);
                } else {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.with_layout(Layout::top_down(Align::Min), |ui| {
                                views::show_route(ui, &self.state, &mut out);
                            });
                        });
                }
            });

        if self.state.incoming.is_some() {
            views::incoming_dialog::show(ui, &self.state, &mut out);
        }
        views::toasts::show(ui, &self.state, &mut out);

        for action in out {
            self.apply(&ctx, action);
        }

        // A live toast has to tick away on its own; without this the last one
        // sits on screen until some other input happens to repaint.
        if !self.state.toasts.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chrome_heights_leave_room_for_content_at_the_minimum_window_size() {
        // The window's minimum inner height, from main.rs.
        const MIN_WINDOW_H: f32 = 560.0;
        let chrome = TOPBAR_HEIGHT + STATUSBAR_HEIGHT + TITLEBAR_INSET;
        assert!(
            MIN_WINDOW_H - chrome > 380.0,
            "chrome leaves only {}px for content",
            MIN_WINDOW_H - chrome
        );
    }
}
