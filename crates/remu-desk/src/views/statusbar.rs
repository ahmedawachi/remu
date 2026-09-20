//! The one-line footer: build, relay, status, platform.

use egui::{Align, Layout, Stroke};

use crate::state::{Action, AppState};
use crate::theme::{space, text};

pub fn show(ui: &mut egui::Ui, state: &AppState, _out: &mut Vec<Action>) {
    let palette = &state.palette;
    let line = ui.available_rect_before_wrap();
    ui.painter()
        .hline(line.x_range(), line.min.y, Stroke::new(1.0, palette.border));

    ui.horizontal(|ui| {
        ui.add_space(space::XS);
        cell(ui, state, &format!("Remu · v{}", state.version));
        cell(ui, state, &format!("Relay: {}", state.settings.relay_url));
        cell(ui, state, &format!("Status: {}", state.link.label()));
        if !state.secret_backend_is_keychain {
            // Said out loud rather than downgrading silently: the password is
            // sitting in a file, not in the OS keychain.
            ui.label(
                egui::RichText::new("Secrets: file (no OS keychain)")
                    .size(text::SECTION)
                    .color(palette.warning),
            )
            .on_hover_text(
                "No OS keychain was reachable, so secrets are kept in a 0600 file \
                 beside the settings.",
            );
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            cell(ui, state, &state.platform);
        });
    });
}

fn cell(ui: &mut egui::Ui, state: &AppState, body: &str) {
    ui.label(
        egui::RichText::new(body)
            .size(text::SECTION)
            .color(state.palette.fg_muted),
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
            assert!(out.is_empty(), "the status bar is read-only");
        }
    }
}
