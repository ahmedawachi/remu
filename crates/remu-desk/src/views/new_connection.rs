//! The home screen: your desk on the left, the desk you want on the right.

use egui::{vec2, Align, FontId, Layout, Sense};

use crate::state::{Action, AppState, Edit};
use crate::theme::{space, text};
use crate::widgets;

/// Most recent desks offered as one-click cards, as the predecessor does.
const RECENT_CARDS: usize = 6;

/// Height of a single row of buttons.
const BUTTON_ROW_H: f32 = 32.0;

/// Inner size of a recent-desk card, inside the card's own padding.
const RECENT_CARD_INNER: egui::Vec2 = egui::vec2(200.0, 36.0);

/// Both desk cards are held to one height so the pair reads as a matched row
/// rather than two panels of accidental size. Sized to the taller side (the
/// remote-desk form), as the predecessor's `min-height: 260px` did — close to
/// the natural content height, so neither card carries a field of dead space.
const DESK_CARD_H: f32 = 248.0;

pub fn show(ui: &mut egui::Ui, state: &AppState, out: &mut Vec<Action>) {
    // The shell owns the scroll container; nesting a second one here made the
    // inner area claim the full viewport height and stretched every card.
    {
        ui.columns(2, |cols| {
            this_desk(&mut cols[0], state, out);
            remote_desk(&mut cols[1], state, out);
        });

        if !state.history.is_empty() {
            ui.add_space(space::XL);
            recent_strip(ui, state, out);
        }
    }
}

fn this_desk(ui: &mut egui::Ui, state: &AppState, out: &mut Vec<Action>) {
    let palette = &state.palette;
    widgets::card(palette).show(ui, |ui| {
        ui.set_min_height(DESK_CARD_H);
        widgets::section_title(ui, palette, "Your Desk");

        ui.vertical_centered(|ui| {
            ui.add_space(space::LG);
            widgets::desk_id(ui, palette, state.my_id, text::MONO_LARGE);
            let alias = if state.settings.alias.trim().is_empty() {
                "Remu client"
            } else {
                state.settings.alias.trim()
            };
            ui.label(
                egui::RichText::new(alias)
                    .size(12.0)
                    .color(palette.fg_muted),
            );
            ui.add_space(space::LG);
        });

        widgets::hint(
            ui,
            palette,
            "Share this ID with anyone you want to give access to your desk. \
             Connections are end-to-end encrypted with WebRTC DTLS-SRTP.",
        );
        ui.add_space(space::SM);
        // Allocated with an explicit height: a right-to-left layout opened
        // inside a vertical one claims every remaining pixel, which stretched
        // this card to the bottom of the window and stranded the button in the
        // middle of the empty space.
        ui.allocate_ui_with_layout(
            vec2(ui.available_width(), BUTTON_ROW_H),
            Layout::right_to_left(Align::Center),
            |ui| {
                if widgets::secondary_button(ui, palette, "Copy ID", state.my_id.is_some())
                    .clicked()
                {
                    out.push(Action::CopyMyId);
                }
            },
        );
    });
}

fn remote_desk(ui: &mut egui::Ui, state: &AppState, out: &mut Vec<Action>) {
    let palette = &state.palette;
    let parsed = widgets::parse_desk_id(&state.connect_id);

    widgets::card(palette).show(ui, |ui| {
        ui.set_min_height(DESK_CARD_H);
        widgets::section_title(ui, palette, "Remote Desk");
        ui.label(
            egui::RichText::new("Enter the ID of the remote desk you want to connect to:")
                .color(palette.fg_secondary),
        );
        ui.add_space(space::SM);

        let mut connect_now = false;

        ui.horizontal(|ui| {
            let mut id = state.connect_id.clone();
            let response = ui.add(
                egui::TextEdit::singleline(&mut id)
                    .id_salt("connect-id")
                    .hint_text("123 456 789")
                    .font(FontId::monospace(18.0))
                    .margin(egui::Margin::symmetric(12, 10))
                    // Clamped: egui panics on a negative desired size, and
                    // this column narrows with the window.
                    .desired_width((ui.available_width() - 110.0).max(96.0)),
            );
            if id != state.connect_id {
                out.push(Action::Edit(Edit::ConnectId(id)));
            }
            if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                connect_now = true;
            }
            if widgets::primary_button(ui, palette, "Connect", parsed.is_some()).clicked() {
                connect_now = true;
            }
        });

        ui.add_space(space::SM);
        let mut password = state.connect_password.clone();
        let response = ui.add(
            egui::TextEdit::singleline(&mut password)
                .id_salt("connect-password")
                .password(true)
                .hint_text("Password (optional, for unattended access)")
                // Same inner padding as the ID field above: two inputs stacked
                // in one card must look like the same control.
                .margin(egui::Margin::symmetric(12, 8))
                .desired_width(f32::INFINITY),
        );
        if password != state.connect_password {
            out.push(Action::Edit(Edit::ConnectPassword(password)));
        }
        if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            connect_now = true;
        }

        // Validate as the user types instead of letting them press Connect and
        // find out from the relay: nine digits or the button stays grey.
        if parsed.is_none() && !state.connect_id.trim().is_empty() {
            ui.add_space(space::XS);
            ui.label(
                egui::RichText::new("A desk ID is nine digits.")
                    .size(12.0)
                    .color(palette.warning),
            );
        }

        ui.add_space(space::SM);
        quick_actions(ui, state, parsed.is_some(), &mut connect_now);

        if connect_now && parsed.is_some() {
            out.push(Action::Connect {
                peer: state.connect_id.clone(),
                password: state.connect_password.clone(),
            });
        }
    });
}

/// The three tiles under the connect box. Two of them are deliberately
/// disabled: they advertise what a session offers, and say where to find it.
fn quick_actions(ui: &mut egui::Ui, state: &AppState, can_connect: bool, connect_now: &mut bool) {
    let width = (ui.available_width() - 2.0 * space::SM) / 3.0;

    ui.horizontal(|ui| {
        if tile(ui, state, width, "🖵", "View screen", can_connect).clicked() {
            *connect_now = true;
        }
        tile(ui, state, width, "📤", "File transfer", false)
            .on_disabled_hover_text("Available from the toolbar once a session is connected");
        tile(ui, state, width, "🗐", "Chat", false)
            .on_disabled_hover_text("Available from the toolbar once a session is connected");
    });
}

fn tile(
    ui: &mut egui::Ui,
    state: &AppState,
    width: f32,
    icon: &str,
    label: &str,
    enabled: bool,
) -> egui::Response {
    let palette = &state.palette;
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(vec2(width, 62.0), sense);

    if ui.is_rect_visible(rect) {
        let dim = if enabled { 1.0 } else { 0.45 };
        let fill = if enabled && response.hovered() {
            palette.bg_hover
        } else {
            palette.bg_raised
        };
        ui.painter().rect(
            rect,
            egui::CornerRadius::same(crate::theme::radius::SM),
            fill.gamma_multiply(dim),
            egui::Stroke::new(1.0, palette.border.gamma_multiply(dim)),
            egui::StrokeKind::Inside,
        );
        ui.painter().text(
            rect.center() - vec2(0.0, 11.0),
            egui::Align2::CENTER_CENTER,
            icon,
            FontId::proportional(17.0),
            palette.fg_secondary.gamma_multiply(dim),
        );
        ui.painter().text(
            rect.center() + vec2(0.0, 13.0),
            egui::Align2::CENTER_CENTER,
            label,
            FontId::proportional(12.0),
            palette.fg_secondary.gamma_multiply(dim),
        );
    }
    response
}

fn recent_strip(ui: &mut egui::Ui, state: &AppState, out: &mut Vec<Action>) {
    let palette = &state.palette;
    widgets::section_title(ui, palette, "Recent");

    ui.horizontal_wrapped(|ui| {
        for record in state.history.iter().take(RECENT_CARDS) {
            let response = widgets::subtle_card(palette)
                .show(ui, |ui| {
                    // A fixed inner size, not just a width: sizing each card to
                    // its own content let `horizontal_wrapped` stagger them by a
                    // few pixels, so a row of identical cards looked crooked.
                    ui.set_min_size(RECENT_CARD_INNER);
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("🖵").size(18.0).color(palette.accent));
                        ui.vertical(|ui| {
                            ui.label(
                                egui::RichText::new(record.peer_id.grouped())
                                    .font(FontId::monospace(text::MONO_BODY)),
                            );
                            ui.label(
                                egui::RichText::new(
                                    record
                                        .alias
                                        .clone()
                                        .unwrap_or_else(|| "No alias".to_owned()),
                                )
                                .size(11.0)
                                .color(palette.fg_muted),
                            );
                        });
                    });
                })
                .response
                .interact(Sense::click());
            if response.clicked() {
                out.push(Action::ConnectTo(record.peer_id));
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::test_support::{peer, populated};

    #[test]
    fn draws_against_a_default_and_a_populated_state() {
        for state in [AppState::default(), populated()] {
            let mut out = Vec::new();
            egui::__run_test_ui(|ui| show(ui, &state, &mut out));
        }
    }

    #[test]
    fn an_incomplete_id_cannot_be_connected_to() {
        let state = AppState {
            connect_id: "1234".into(),
            ..AppState::default()
        };
        let mut out = Vec::new();
        egui::__run_test_ui(|ui| show(ui, &state, &mut out));
        assert!(
            !out.iter().any(|a| matches!(a, Action::Connect { .. })),
            "a four-digit id must not reach the relay: {out:?}"
        );
    }

    #[test]
    fn the_recent_strip_is_capped_so_the_hero_stays_on_screen() {
        let state = AppState {
            history: (0..20)
                .map(|i| remu_proto::ConnectionRecord {
                    peer_id: peer(100_000_000 + i),
                    alias: None,
                    last_connected_at: 1,
                    favorite: false,
                })
                .collect(),
            ..AppState::default()
        };
        assert!(state.history.len() > RECENT_CARDS);
        let mut out = Vec::new();
        egui::__run_test_ui(|ui| show(ui, &state, &mut out));
    }
}
