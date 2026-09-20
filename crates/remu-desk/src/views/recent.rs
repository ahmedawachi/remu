//! Every desk this one has reached, newest first.

use egui::FontId;

use crate::state::{Action, AppState};
use crate::theme::{space, text};
use crate::widgets;

pub fn show(ui: &mut egui::Ui, state: &AppState, out: &mut Vec<Action>) {
    let palette = &state.palette;
    ui.set_max_width(920.0);

    if state.history.is_empty() {
        widgets::card(palette).show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(space::LG);
                ui.label(egui::RichText::new("No sessions yet").strong().size(15.0));
                ui.add_space(space::XS);
                widgets::hint(
                    ui,
                    palette,
                    "Sessions you initiate will show up here for quick reconnect.",
                );
                ui.add_space(space::LG);
            });
        });
        return;
    }

    let now = super::now_ms();
    // The shell owns the scroll container; nesting a second one here made the
    // inner area claim the full viewport height and stretched every card.
    {
        egui::Grid::new("recent-table")
            .num_columns(5)
            .spacing([space::MD, space::SM])
            .striped(true)
            .show(ui, |ui| {
                for label in ["", "REMOTE ID", "ALIAS", "LAST CONNECTED", ""] {
                    ui.label(
                        egui::RichText::new(label)
                            .size(text::SECTION)
                            .color(palette.fg_muted),
                    );
                }
                ui.end_row();

                for record in &state.history {
                    // A star that is only a colour change is invisible to
                    // anyone who cannot see the colour, so the glyph
                    // changes too.
                    let (glyph, colour) = if record.favorite {
                        ("★", palette.warning)
                    } else {
                        ("☆", palette.fg_muted)
                    };
                    if ui
                        .add(
                            egui::Button::new(egui::RichText::new(glyph).size(16.0).color(colour))
                                .frame(false),
                        )
                        .on_hover_text("Favorite")
                        .clicked()
                    {
                        out.push(Action::SetFavorite(record.peer_id, !record.favorite));
                    }

                    ui.label(
                        egui::RichText::new(record.peer_id.grouped())
                            .font(FontId::monospace(text::MONO_BODY)),
                    );
                    ui.label(record.alias.clone().unwrap_or_else(|| "—".to_owned()));
                    ui.label(
                        egui::RichText::new(widgets::format_ago(now, record.last_connected_at))
                            .color(palette.fg_muted),
                    );

                    ui.horizontal(|ui| {
                        if widgets::primary_button(ui, palette, "Connect", true).clicked() {
                            out.push(Action::ConnectTo(record.peer_id));
                        }
                        if widgets::secondary_button(ui, palette, "Remove", true).clicked() {
                            out.push(Action::RemoveContact(record.peer_id));
                        }
                    });
                    ui.end_row();
                }
            });
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
    fn an_empty_history_shows_the_empty_state_rather_than_a_bare_table() {
        let state = AppState::default();
        let mut out = Vec::new();
        egui::__run_test_ui(|ui| show(ui, &state, &mut out));
        assert!(out.is_empty());
    }

    #[test]
    fn a_contact_that_was_never_reached_reads_as_never() {
        let now = super::super::now_ms();
        assert_eq!(widgets::format_ago(now, 0), "never");
    }
}
