//! What this program is, and what it is not yet.

use crate::state::{Action, AppState};
use crate::theme::space;
use crate::widgets;

pub fn show(ui: &mut egui::Ui, state: &AppState, _out: &mut Vec<Action>) {
    let palette = &state.palette;
    ui.set_max_width(640.0);

    widgets::card(palette).show(ui, |ui| {
        ui.heading("Remu");
        ui.label(egui::RichText::new("An open remote-desktop client.").color(palette.fg_muted));
        ui.add_space(space::MD);

        for point in [
            "End-to-end encrypted screen sharing over WebRTC (DTLS-SRTP).",
            "Remote keyboard, mouse and scroll wheel via native input injection.",
            "Drag-drop file transfer over a dedicated data channel.",
            "In-session chat.",
            "Cross-platform: macOS, Windows, Linux.",
        ] {
            ui.horizontal_top(|ui| {
                ui.label(egui::RichText::new("·").color(palette.fg_muted));
                ui.label(egui::RichText::new(point).color(palette.fg_secondary));
            });
        }

        ui.add_space(space::MD);
        widgets::hint(
            ui,
            palette,
            &format!("Version {} on {}.", state.version, state.platform),
        );
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
            assert!(out.is_empty(), "About offers nothing to act on");
        }
    }
}
