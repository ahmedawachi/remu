//! The prompt that decides whether a stranger gets this desk.

use egui::{vec2, Align2, CornerRadius, FontId, Margin, Sense, Stroke};

use crate::state::{Action, AppState};
use crate::theme::{radius, space};
use crate::widgets;

pub fn show(ui: &mut egui::Ui, state: &AppState, out: &mut Vec<Action>) {
    let Some(incoming) = state.incoming.as_ref() else {
        return;
    };
    let palette = &state.palette;

    let modal = egui::Modal::new(egui::Id::new("remu-incoming"))
        .backdrop_color(egui::Color32::from_black_alpha(115))
        .frame(
            egui::Frame::new()
                .fill(palette.bg_surface)
                .stroke(Stroke::new(1.0, palette.border))
                .corner_radius(CornerRadius::same(radius::MD))
                .inner_margin(Margin::same(space::LG as i8))
                .shadow(ui.style().visuals.window_shadow),
        )
        .show(ui.ctx(), |ui| {
            ui.set_width(420.0);

            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(vec2(52.0, 52.0), Sense::hover());
                ui.painter()
                    .circle_filled(rect.center(), 26.0, palette.accent_soft);
                ui.painter().text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    "🖵",
                    FontId::proportional(22.0),
                    palette.accent,
                );
                ui.add_space(space::SM);
                ui.vertical(|ui| {
                    ui.label(
                        egui::RichText::new("Incoming session request")
                            .strong()
                            .size(14.0),
                    );
                    ui.label(
                        egui::RichText::new(incoming.from.grouped())
                            .font(FontId::monospace(13.0))
                            .color(palette.fg_secondary),
                    );
                    if let Some(alias) = incoming
                        .from_alias
                        .as_deref()
                        .filter(|a| !a.trim().is_empty())
                    {
                        // The alias is peer-supplied, so it is shown as a claim
                        // beside the ID, never instead of it.
                        ui.label(
                            egui::RichText::new(format!("calls itself “{alias}”"))
                                .size(11.0)
                                .color(palette.fg_muted),
                        );
                    }
                });
            });

            ui.add_space(space::MD);
            ui.label(
                egui::RichText::new(
                    "This peer wants to view and control your screen. \
                     Make sure you trust them before accepting.",
                )
                .size(13.0)
                .color(palette.fg_secondary),
            );
            if incoming.needs_password {
                ui.add_space(space::XS);
                ui.label(
                    egui::RichText::new("They did not present the unattended access password.")
                        .size(12.0)
                        .color(palette.warning),
                );
            }

            ui.add_space(space::LG);
            // Explicit height: an unbounded right-to-left layout would take the
            // rest of the modal and float the buttons in its middle.
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), 32.0),
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| {
                    if widgets::primary_button(ui, palette, "Accept", true).clicked() {
                        out.push(Action::AcceptIncoming);
                    }
                    if widgets::secondary_button(ui, palette, "Decline", true).clicked() {
                        out.push(Action::DeclineIncoming);
                    }
                },
            );
        });

    // Escape or a click on the backdrop is a decline, not a dismissal: leaving
    // the peer hanging is worse than telling them no.
    if modal.should_close() {
        out.push(Action::DeclineIncoming);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::test_support::populated;

    #[test]
    fn draws_against_a_default_and_a_populated_state() {
        for state in [AppState::default(), populated()] {
            let mut out = Vec::new();
            egui::__run_test_ui(|ui| show(ui, &state, &mut out));
        }
    }

    #[test]
    fn nothing_is_drawn_and_nothing_is_decided_when_no_one_is_asking() {
        let state = AppState::default();
        assert!(state.incoming.is_none());
        let mut out = Vec::new();
        egui::__run_test_ui(|ui| show(ui, &state, &mut out));
        assert!(out.is_empty(), "an absent request must not be declined");
    }
}
