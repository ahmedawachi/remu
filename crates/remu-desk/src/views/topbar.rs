//! The window's title strip: where you are, how the link is, and your ID.

use egui::{Align, Layout, Stroke};

use crate::state::{Action, AppState, Route};
use crate::theme::{space, text};
use crate::widgets;

pub fn show(ui: &mut egui::Ui, state: &AppState, out: &mut Vec<Action>) {
    let palette = &state.palette;

    ui.horizontal(|ui| {
        if state.route == Route::Session
            && widgets::ghost_button(ui, palette, "← Back", false, true).clicked()
        {
            out.push(Action::Navigate(Route::NewConnection));
        }
        ui.label(
            egui::RichText::new(state.route.label())
                .size(13.0)
                .color(palette.fg_secondary),
        );

        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            // Right-to-left places the first item hard against the edge, and a
            // bordered control whose stroke touches the window frame reads as
            // clipped. Hold it off the boundary.
            ui.add_space(space::XS);
            my_id(ui, state, out);
            ui.add_space(space::MD);
            widgets::pill(ui, palette, state.link.tone(), state.link.label());
        });
    });

    let line = ui.available_rect_before_wrap();
    ui.painter()
        .hline(line.x_range(), line.min.y, Stroke::new(1.0, palette.border));
}

/// The desk's own ID, click-to-copy. Disabled until the relay has issued one:
/// a "Copy ID" that copies nothing is worse than a greyed-out button.
fn my_id(ui: &mut egui::Ui, state: &AppState, out: &mut Vec<Action>) {
    let palette = &state.palette;
    let label = state
        .my_id
        .map_or_else(|| "— — —".to_owned(), remu_proto::PeerId::grouped);

    let response = widgets::ghost_button(
        ui,
        palette,
        &format!("{label}  🗐"),
        false,
        state.my_id.is_some(),
    );
    if response.on_hover_text("Copy your Remu ID").clicked() {
        out.push(Action::CopyMyId);
    }
    ui.label(
        egui::RichText::new("YOUR ID")
            .size(text::SECTION)
            .color(palette.fg_muted),
    );
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
    fn shows_a_placeholder_id_until_the_relay_issues_one() {
        let state = AppState::default();
        assert!(state.my_id.is_none());
        let mut out = Vec::new();
        egui::__run_test_ui(|ui| show(ui, &state, &mut out));
        // Nothing can be copied before there is an ID, so nothing is emitted.
        assert!(out.is_empty());
    }
}
