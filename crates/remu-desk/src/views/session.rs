//! The live session: the remote picture, the toolbar, chat and files.
//!
//! The stage is painted rather than built from widgets. That is deliberate: the
//! frame, the letterbox bars and the placeholder all have to occupy exactly the
//! rect the pointer is normalized against, and a painter cannot accidentally
//! advance the layout cursor out from under that rect.

use egui::{
    pos2, vec2, Align, Align2, Color32, CornerRadius, FontId, Layout, Pos2, Rect, Sense, Stroke,
    StrokeKind, Vec2,
};
use remu_proto::InputEvent;

use crate::state::{Action, AppState, Edit, SessionUi, SidePanel};
use crate::theme::{radius, space, StatusTone};
use crate::widgets;

const TOOLBAR_HEIGHT: f32 = 34.0;
const PANEL_WIDTH: f32 = 320.0;

pub fn show(ui: &mut egui::Ui, state: &AppState, out: &mut Vec<Action>) {
    let palette = &state.palette;
    let Some(session) = state.session.as_ref() else {
        widgets::card(palette).show(ui, |ui| {
            widgets::hint(ui, palette, "No session is running.");
        });
        return;
    };

    let full = ui.available_rect_before_wrap();
    let panel_gutter = if session.panel == SidePanel::None {
        0.0
    } else {
        PANEL_WIDTH + space::MD
    };
    let stage_width = (full.width() - panel_gutter).max(160.0);
    let stage_height = (full.height() - TOOLBAR_HEIGHT - space::SM).max(120.0);

    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            vec2(stage_width, full.height()),
            Layout::top_down(Align::Min),
            |ui| {
                let (stage, response) = ui
                    .allocate_exact_size(vec2(stage_width, stage_height), Sense::click_and_drag());
                let picture = paint_stage(ui, state, session, stage);
                if session.role_is_controller {
                    let response = response.on_hover_cursor(egui::CursorIcon::Crosshair);
                    forward_input(ui, session, stage, picture, &response, out);
                }
                ui.add_space(space::SM);
                toolbar(ui, state, session, out);
            },
        );

        if session.panel != SidePanel::None {
            ui.add_space(space::MD);
            ui.allocate_ui_with_layout(
                vec2(PANEL_WIDTH, full.height()),
                Layout::top_down(Align::Min),
                |ui| match session.panel {
                    SidePanel::Chat => chat_panel(ui, state, session, out),
                    SidePanel::Files => files_panel(ui, state, session, out),
                    SidePanel::None => {}
                },
            );
        }
    });
}

/// Paints the stage and returns where the picture ended up, which is the rect
/// every pointer coordinate is measured against.
fn paint_stage(ui: &mut egui::Ui, state: &AppState, session: &SessionUi, stage: Rect) -> Rect {
    let palette = &state.palette;
    ui.painter().rect(
        stage,
        CornerRadius::same(radius::MD),
        palette.stage,
        Stroke::new(1.0, palette.border),
        StrokeKind::Inside,
    );

    if !session.role_is_controller {
        host_card(ui, state, session, stage);
        return stage;
    }

    let Some((texture, size)) = session.frame.as_ref() else {
        placeholder(ui, state, session, stage);
        return stage;
    };

    let picture = widgets::picture_rect(stage, *size);
    ui.painter().image(
        texture.id(),
        picture,
        Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
        Color32::WHITE,
    );
    picture
}

/// What the controller sees before the first frame arrives.
fn placeholder(ui: &egui::Ui, state: &AppState, session: &SessionUi, stage: Rect) {
    let palette = &state.palette;
    let centre = stage.center();
    spinner(ui, palette.accent, centre - vec2(0.0, 24.0), 13.0);
    widgets::centered_note(
        ui,
        palette,
        Rect::from_center_size(centre + vec2(0.0, 14.0), vec2(stage.width(), 20.0)),
        &format!("Negotiating session with {}…", session.remote.grouped()),
    );
    // Without this the arc freezes: egui only repaints on input.
    ui.ctx().request_repaint();
}

/// A rotating arc, drawn as line segments so it needs no widget and no texture.
fn spinner(ui: &egui::Ui, color: Color32, centre: Pos2, radius: f32) {
    const SEGMENTS: usize = 18;
    let time = ui.input(|i| i.time) as f32;
    let start = time * 3.0;
    let painter = ui.painter();
    for i in 0..SEGMENTS {
        let t0 = start + i as f32 * 0.09;
        let t1 = t0 + 0.07;
        let fade = i as f32 / SEGMENTS as f32;
        painter.line_segment(
            [
                centre + Vec2::angled(t0) * radius,
                centre + Vec2::angled(t1) * radius,
            ],
            Stroke::new(2.5, color.gamma_multiply(fade)),
        );
    }
}

/// The host's side of a session: no picture, just what the other end can do.
fn host_card(ui: &egui::Ui, state: &AppState, session: &SessionUi, stage: Rect) {
    let palette = &state.palette;
    let painter = ui.painter();
    let mut y = stage.center().y - 48.0;

    painter.text(
        pos2(stage.center().x, y),
        Align2::CENTER_CENTER,
        "Sharing your screen",
        FontId::proportional(16.0),
        palette.fg_primary,
    );
    y += 28.0;
    painter.text(
        pos2(stage.center().x, y),
        Align2::CENTER_CENTER,
        format!("Connected to {}.", session.remote.grouped()),
        FontId::proportional(13.0),
        palette.fg_secondary,
    );
    y += 28.0;
    for line in [
        format!(
            "Keyboard & mouse: {}",
            yes_no(session.permissions.input, "forwarded", "blocked")
        ),
        format!(
            "File transfer: {}",
            yes_no(session.permissions.files, "allowed", "blocked")
        ),
        format!(
            "Clipboard: {}",
            yes_no(session.permissions.clipboard, "shared", "blocked")
        ),
    ] {
        painter.text(
            pos2(stage.center().x, y),
            Align2::CENTER_CENTER,
            line,
            FontId::proportional(13.0),
            palette.fg_muted,
        );
        y += 20.0;
    }
}

fn yes_no(flag: bool, yes: &'static str, no: &'static str) -> &'static str {
    if flag {
        yes
    } else {
        no
    }
}

/// Turns this frame's egui input into remote input events.
///
/// Three gates, all of which have to hold: only a controller sends anything,
/// only a host that permits input receives it, and the keyboard is only
/// forwarded when no local text field has focus — otherwise typing a chat
/// message would also type it on the remote machine.
fn forward_input(
    ui: &egui::Ui,
    session: &SessionUi,
    stage: Rect,
    picture: Rect,
    response: &egui::Response,
    out: &mut Vec<Action>,
) {
    let keyboard = keyboard_is_ours(session.panel, ui.memory(|m| m.focused().is_some()));

    let (events, pointer) = ui.input(|i| (i.events.clone(), i.pointer.hover_pos()));
    // Whether the *stage* owns the pointer, not merely whether the pointer is
    // inside its rectangle: `contains_pointer` is false when another layer — a
    // popup, a tooltip, anything drawn over the video — covers it, and a click
    // there is handled locally. Gating on the rectangle alone sent that click
    // to the remote desktop as well. `is_pointer_button_down_on` keeps a drag
    // that started on the stage alive after it wanders off.
    let owns_pointer = response.contains_pointer() || response.is_pointer_button_down_on();

    let frame = widgets::StageInput {
        stage,
        picture,
        pointer,
        owns_pointer,
        keyboard,
    };
    // The forwarder has to remember what it pressed on the host between frames,
    // and the view is handed nothing mutable, so it rides in egui's per-widget
    // memory next to the rest of this widget's state.
    let id = response.id.with("remu-input-forwarder");
    let mut forwarder: widgets::InputForwarder =
        ui.ctx().data_mut(|d| d.get_temp(id).unwrap_or_default());
    let translated = forwarder.translate(frame, &events);
    ui.ctx().data_mut(|d| d.insert_temp(id, forwarder));

    for event in translated {
        // A host with input switched off still needs ReleaseAll: it is what
        // clears a key that was already held when the toggle flipped.
        if !session.permissions.input && !matches!(event, InputEvent::ReleaseAll) {
            continue;
        }
        out.push(Action::RemoteInput(event));
    }
}

/// Whether this frame's keystrokes belong to the remote machine or to us.
///
/// The chat panel and any focused text field are local typing surfaces:
/// forwarding while one of them is open would type the chat message on the
/// remote machine as well.
fn keyboard_is_ours(panel: SidePanel, local_focus: bool) -> bool {
    panel != SidePanel::Chat && !local_focus
}

/// Ends the session, releasing the host first.
///
/// Tearing the session down stops input forwarding for good, so any key or
/// button we pressed on the host has to come up now — after this there is no
/// channel left to lift it on, and the host would hold it until a human pressed
/// it locally.
fn end_session(session: &SessionUi, out: &mut Vec<Action>) {
    if session.role_is_controller {
        out.push(Action::RemoteInput(InputEvent::ReleaseAll));
    }
    out.push(Action::EndSession);
}

fn toolbar(ui: &mut egui::Ui, state: &AppState, session: &SessionUi, out: &mut Vec<Action>) {
    let palette = &state.palette;
    ui.horizontal(|ui| {
        let verb = if session.role_is_controller {
            "Viewing"
        } else {
            "Sharing"
        };
        widgets::pill(
            ui,
            palette,
            StatusTone::Good,
            &format!("{verb} · {}", session.remote_label()),
        );

        if let Some(stats) = session.stats {
            ui.label(
                egui::RichText::new(format!(
                    "{:.0} fps · {} kbps · {} ms{}",
                    stats.fps,
                    stats.bitrate_kbps,
                    stats.rtt_ms,
                    if stats.dropped_frames > 0 {
                        format!(" · {} dropped", stats.dropped_frames)
                    } else {
                        String::new()
                    }
                ))
                .size(11.5)
                .color(palette.fg_muted),
            );
        }
        if session.role_is_controller && !session.permissions.input {
            widgets::pill(ui, palette, StatusTone::Warn, "Input disabled by host");
        }

        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if widgets::danger_button(ui, palette, "End", true).clicked() {
                end_session(session, out);
            }
            panel_toggle(ui, state, session, SidePanel::Files, "Files", out);
            panel_toggle(ui, state, session, SidePanel::Chat, "Chat", out);

            if session.role_is_controller {
                let has_frame = session.frame.is_some();
                if widgets::ghost_button(ui, palette, "⛶", false, true)
                    .on_hover_text("Fullscreen")
                    .clicked()
                {
                    out.push(Action::ToggleFullscreen);
                }
                let (label, tip) = if session.recording {
                    ("⏹ Stop", "Stop recording")
                } else {
                    ("⏺ Rec", "Record this session to a video file")
                };
                if widgets::ghost_button(ui, palette, label, session.recording, has_frame)
                    .on_hover_text(tip)
                    .clicked()
                {
                    out.push(Action::ToggleRecording);
                }
                display_picker(ui, session, out);
            }
        });
    });
}

fn panel_toggle(
    ui: &mut egui::Ui,
    state: &AppState,
    session: &SessionUi,
    panel: SidePanel,
    label: &str,
    out: &mut Vec<Action>,
) {
    let active = session.panel == panel;
    if widgets::ghost_button(ui, &state.palette, label, active, true).clicked() {
        out.push(Action::SetPanel(if active {
            SidePanel::None
        } else {
            panel
        }));
    }
}

/// Only shown when there is a choice to make: one monitor needs no picker.
fn display_picker(ui: &mut egui::Ui, session: &SessionUi, out: &mut Vec<Action>) {
    if session.displays.len() < 2 {
        return;
    }
    let selected = session
        .displays
        .iter()
        .find(|d| d.id == session.active_display)
        .map_or_else(
            || "Display".to_owned(),
            |d| display_name(&session.displays, d),
        );

    egui::ComboBox::from_id_salt("session-display")
        .width(170.0)
        .selected_text(selected)
        .show_ui(ui, |ui| {
            for display in &session.displays {
                let label = display_name(&session.displays, display);
                if ui
                    .selectable_label(display.id == session.active_display, label)
                    .clicked()
                    && display.id != session.active_display
                {
                    out.push(Action::SwitchDisplay(display.id.clone()));
                }
            }
        });
}

/// A host that reported no name for a monitor still needs one in the list.
fn display_name(all: &[remu_proto::DisplayInfo], display: &remu_proto::DisplayInfo) -> String {
    if !display.name.trim().is_empty() {
        return display.name.clone();
    }
    let index = all.iter().position(|d| d.id == display.id).unwrap_or(0);
    format!("Display {}", index + 1)
}

fn chat_panel(ui: &mut egui::Ui, state: &AppState, session: &SessionUi, out: &mut Vec<Action>) {
    let palette = &state.palette;
    widgets::card(palette)
        .inner_margin(egui::Margin::same(space::SM as i8))
        .show(ui, |ui| {
            ui.set_min_height(ui.available_height());
            widgets::section_title(ui, palette, "Chat");

            let input_height = 40.0;
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .stick_to_bottom(true)
                .max_height((ui.available_height() - input_height).max(60.0))
                .show(ui, |ui| {
                    if session.chat.is_empty() {
                        widgets::hint(ui, palette, "No messages yet.");
                    }
                    for line in &session.chat {
                        let layout = if line.mine {
                            Layout::right_to_left(Align::Min)
                        } else {
                            Layout::left_to_right(Align::Min)
                        };
                        ui.with_layout(layout, |ui| {
                            let (fill, fg) = if line.mine {
                                (palette.accent, palette.on_accent)
                            } else {
                                (palette.bg_sunken, palette.fg_primary)
                            };
                            egui::Frame::new()
                                .fill(fill)
                                .corner_radius(CornerRadius::same(radius::MD))
                                .inner_margin(egui::Margin::symmetric(10, 6))
                                .show(ui, |ui| {
                                    ui.set_max_width(PANEL_WIDTH * 0.75);
                                    ui.label(egui::RichText::new(&line.text).size(13.0).color(fg));
                                });
                        });
                    }
                });

            ui.add_space(space::XS);
            ui.horizontal(|ui| {
                let mut draft = session.chat_draft.clone();
                let response = ui.add(
                    egui::TextEdit::singleline(&mut draft)
                        .id_salt("chat-draft")
                        .hint_text("Message…")
                        .desired_width(ui.available_width() - 70.0),
                );
                if draft != session.chat_draft {
                    out.push(Action::Edit(Edit::ChatDraft(draft)));
                }

                let sendable = !session.chat_draft.trim().is_empty();
                let submitted =
                    response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let clicked = widgets::primary_button(ui, palette, "Send", sendable).clicked();
                if sendable && (submitted || clicked) {
                    out.push(Action::SendChat(session.chat_draft.trim().to_owned()));
                }
            });
        });
}

fn files_panel(ui: &mut egui::Ui, state: &AppState, session: &SessionUi, out: &mut Vec<Action>) {
    let palette = &state.palette;
    widgets::card(palette)
        .inner_margin(egui::Margin::same(space::SM as i8))
        .show(ui, |ui| {
            ui.set_min_height(ui.available_height());
            widgets::section_title(ui, palette, "File transfer");

            let allowed = session.permissions.files;
            if widgets::secondary_button(ui, palette, "Send files…", allowed).clicked() {
                out.push(Action::PickFiles);
            }
            if allowed {
                widgets::hint(ui, palette, "Drag and drop files into this window to send.");
            } else {
                ui.label(
                    egui::RichText::new("The other end has file transfer switched off.")
                        .size(12.0)
                        .color(palette.warning),
                );
            }

            ui.add_space(space::SM);
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if session.transfers.is_empty() {
                        widgets::hint(ui, palette, "No transfers.");
                    }
                    for transfer in &session.transfers {
                        widgets::subtle_card(palette).show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} {}",
                                    if transfer.outgoing { "📤" } else { "📥" },
                                    transfer.name
                                ))
                                .size(12.0),
                            );
                            ui.add(
                                egui::ProgressBar::new(transfer.fraction())
                                    .desired_height(4.0)
                                    .fill(palette.accent)
                                    .corner_radius(CornerRadius::same(radius::SM)),
                            );
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} / {} · {}",
                                    widgets::format_bytes(transfer.transferred),
                                    widgets::format_bytes(transfer.size),
                                    transfer.state
                                ))
                                .size(11.0)
                                .color(palette.fg_muted),
                            );
                        });
                        ui.add_space(space::XS);
                    }
                });
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::SessionUi;
    use crate::views::test_support::{peer, populated};

    fn controller_state() -> AppState {
        AppState {
            session: Some(SessionUi::new(peer(987_654_321), true)),
            ..AppState::default()
        }
    }

    #[test]
    fn draws_against_a_default_and_a_populated_state() {
        for state in [AppState::default(), populated(), controller_state()] {
            let mut out = Vec::new();
            egui::__run_test_ui(|ui| show(ui, &state, &mut out));
        }
    }

    #[test]
    fn draws_the_host_side_without_a_frame() {
        let state = AppState {
            session: Some(SessionUi::new(peer(987_654_321), false)),
            ..AppState::default()
        };
        let mut out = Vec::new();
        egui::__run_test_ui(|ui| show(ui, &state, &mut out));
        assert!(
            !out.iter().any(|a| matches!(a, Action::RemoteInput(_))),
            "a host must never forward input to its controller: {out:?}"
        );
    }

    #[test]
    fn a_missing_session_draws_a_notice_instead_of_panicking() {
        let state = AppState::default();
        assert!(state.session.is_none());
        let mut out = Vec::new();
        egui::__run_test_ui(|ui| show(ui, &state, &mut out));
        assert!(out.is_empty());
    }

    /// One real egui frame of the session view, so the pointer gating and the
    /// forwarder's memory are exercised the way the app exercises them.
    fn run_frame(ctx: &egui::Context, state: &AppState, events: Vec<egui::Event>) -> Vec<Action> {
        run_frame_under(ctx, state, events, false)
    }

    /// `overlay` puts an interactive `Area` over the middle of the video, the
    /// way a popup or a tooltip does.
    fn run_frame_under(
        ctx: &egui::Context,
        state: &AppState,
        events: Vec<egui::Event>,
        overlay: bool,
    ) -> Vec<Action> {
        let mut out = Vec::new();
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(900.0, 600.0))),
            focused: true,
            events,
            ..Default::default()
        };
        // `run_ui` may run several passes; only the last one's actions are the
        // frame's actions.
        let _ = ctx.run_ui(input, |ui| {
            out.clear();
            show(ui, state, &mut out);
            if overlay {
                egui::Area::new(egui::Id::new("test-overlay"))
                    .fixed_pos(pos2(300.0, 150.0))
                    .show(ui.ctx(), |ui| {
                        let (rect, _) = ui.allocate_exact_size(vec2(200.0, 120.0), Sense::click());
                        ui.painter().rect_filled(rect, 0.0, Color32::RED);
                    });
            }
        });
        out
    }

    fn press(pos: Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        }
    }

    #[test]
    fn a_release_outside_the_stage_still_lifts_the_button_on_the_host() {
        // The whole path, not just the translator: press on the video, drag off
        // it, release. Dropping the release left the button down on the host.
        let state = controller_state();
        let ctx = egui::Context::default();
        let inside = pos2(400.0, 200.0);
        let outside = pos2(2000.0, 2000.0);

        run_frame(&ctx, &state, Vec::new());
        let down = run_frame(
            &ctx,
            &state,
            vec![egui::Event::PointerMoved(inside), press(inside, true)],
        );
        assert!(
            down.iter()
                .any(|a| matches!(a, Action::RemoteInput(InputEvent::MouseDown { .. }))),
            "the press should reach the host: {down:?}"
        );

        let up = run_frame(
            &ctx,
            &state,
            vec![egui::Event::PointerMoved(outside), press(outside, false)],
        );
        assert!(
            up.iter()
                .any(|a| matches!(a, Action::RemoteInput(InputEvent::MouseUp { .. }))),
            "and so should the release, or the button sticks: {up:?}"
        );
    }

    #[test]
    fn a_click_on_a_layer_drawn_over_the_video_is_not_forwarded() {
        // Gating on the stage rectangle alone sent a click on a popup, a
        // tooltip or any other overlay to the remote desktop as well as to the
        // thing the user actually clicked.
        let state = controller_state();
        let ctx = egui::Context::default();
        let over = pos2(400.0, 200.0);

        run_frame_under(&ctx, &state, Vec::new(), true);
        run_frame_under(&ctx, &state, vec![egui::Event::PointerMoved(over)], true);
        let out = run_frame_under(
            &ctx,
            &state,
            vec![egui::Event::PointerMoved(over), press(over, true)],
            true,
        );
        assert!(
            !out.iter()
                .any(|a| matches!(a, Action::RemoteInput(InputEvent::MouseDown { .. }))),
            "the overlay owns this click: {out:?}"
        );
    }

    #[test]
    fn ending_the_session_releases_the_host_first() {
        // The teardown is the last chance to lift anything we pressed on the
        // host: after it there is no channel left to send a release on.
        let state = controller_state();
        let session = state.session.as_ref().expect("controller session");
        let mut out = Vec::new();
        end_session(session, &mut out);
        assert_eq!(
            out.first(),
            Some(&Action::RemoteInput(InputEvent::ReleaseAll)),
            "release must come before the session goes away: {out:?}"
        );
        assert!(matches!(out.last(), Some(Action::EndSession)), "{out:?}");
    }

    #[test]
    fn a_host_ending_a_session_forwards_nothing() {
        let session = SessionUi::new(peer(987_654_321), false);
        let mut out = Vec::new();
        end_session(&session, &mut out);
        assert_eq!(out, vec![Action::EndSession]);
    }

    #[test]
    fn the_keyboard_is_not_ours_while_a_local_surface_has_it() {
        assert!(keyboard_is_ours(SidePanel::None, false));
        assert!(keyboard_is_ours(SidePanel::Files, false));
        assert!(
            !keyboard_is_ours(SidePanel::Chat, false),
            "chat is a local typing surface"
        );
        assert!(!keyboard_is_ours(SidePanel::None, true));
    }

    #[test]
    fn an_unnamed_monitor_still_gets_a_label() {
        let displays = vec![
            remu_proto::DisplayInfo {
                id: "screen:0".into(),
                name: String::new(),
                width: 1920,
                height: 1080,
                primary: true,
            },
            remu_proto::DisplayInfo {
                id: "screen:1".into(),
                name: "  ".into(),
                width: 1920,
                height: 1080,
                primary: false,
            },
        ];
        assert_eq!(display_name(&displays, &displays[0]), "Display 1");
        assert_eq!(display_name(&displays, &displays[1]), "Display 2");
    }
}
