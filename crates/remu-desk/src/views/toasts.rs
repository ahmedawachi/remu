//! Transient notices, stacked above the status bar.
//!
//! Drawn in their own [`egui::Area`] so they float over whatever screen is
//! showing, including the incoming-session modal — an error about the session
//! you are being asked to accept has to be visible while you decide.

use std::time::Instant;

use egui::{vec2, Align2, CornerRadius, Margin, Order, Stroke};

use crate::state::{Action, AppState};
use crate::theme::{radius, space, status_color};
use crate::widgets;

pub fn show(ui: &mut egui::Ui, state: &AppState, _out: &mut Vec<Action>) {
    let palette = &state.palette;
    let now = Instant::now();
    let live: Vec<_> = state.live_toasts(now).collect();
    if live.is_empty() {
        return;
    }

    egui::Area::new(egui::Id::new("remu-toasts"))
        .order(Order::Foreground)
        .anchor(Align2::RIGHT_BOTTOM, vec2(-space::LG, -40.0))
        .interactable(false)
        .show(ui.ctx(), |ui| {
            // Newest at the bottom, nearest the user's attention.
            for toast in live {
                egui::Frame::new()
                    .fill(palette.bg_raised)
                    .stroke(Stroke::new(1.0, palette.border_strong))
                    .corner_radius(CornerRadius::same(radius::MD))
                    .inner_margin(Margin::symmetric(14, 10))
                    .shadow(ui.style().visuals.window_shadow)
                    .show(ui, |ui| {
                        ui.set_max_width(360.0);
                        ui.horizontal(|ui| {
                            widgets::dot(ui, status_color(palette, toast.kind.tone()));
                            ui.label(
                                egui::RichText::new(&toast.text)
                                    .size(13.0)
                                    .color(palette.fg_primary),
                            );
                        });
                    });
                ui.add_space(space::SM);
            }
        });
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
            assert!(out.is_empty(), "toasts are not interactive");
        }
    }

    #[test]
    fn an_expired_toast_is_not_drawn() {
        // The populated fixture deliberately carries one already-expired toast.
        let state = populated();
        assert_eq!(state.toasts.len(), 3);
        assert_eq!(state.live_toasts(Instant::now()).count(), 2);
    }
}
