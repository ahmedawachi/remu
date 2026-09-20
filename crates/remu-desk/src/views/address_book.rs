//! Saved contacts: add one on the left, reach one on the right.

use egui::{vec2, Align2, FontId, Sense};

use crate::state::{Action, AppState, Edit};
use crate::theme::{space, text};
use crate::widgets;

pub fn show(ui: &mut egui::Ui, state: &AppState, out: &mut Vec<Action>) {
    let palette = &state.palette;

    ui.horizontal_top(|ui| {
        ui.allocate_ui(vec2(320.0, ui.available_height()), |ui| {
            add_contact(ui, state, out);
        });
        ui.add_space(space::LG);
        ui.vertical(|ui| {
            widgets::section_title(ui, palette, "Saved contacts");
            saved_contacts(ui, state, out);
        });
    });
}

fn add_contact(ui: &mut egui::Ui, state: &AppState, out: &mut Vec<Action>) {
    let palette = &state.palette;
    let valid = widgets::parse_desk_id(&state.new_contact_id).is_some();

    widgets::card(palette).show(ui, |ui| {
        widgets::section_title(ui, palette, "Add contact");

        ui.label(
            egui::RichText::new("Remote ID")
                .size(12.0)
                .color(palette.fg_muted),
        );
        let mut id = state.new_contact_id.clone();
        ui.add(
            egui::TextEdit::singleline(&mut id)
                .id_salt("new-contact-id")
                .hint_text("123 456 789")
                .font(FontId::monospace(text::MONO_BODY))
                .desired_width(f32::INFINITY),
        );
        if id != state.new_contact_id {
            out.push(Action::Edit(Edit::NewContactId(id)));
        }

        ui.add_space(space::XS);
        ui.label(
            egui::RichText::new("Alias (optional)")
                .size(12.0)
                .color(palette.fg_muted),
        );
        let mut alias = state.new_contact_alias.clone();
        ui.add(
            egui::TextEdit::singleline(&mut alias)
                .id_salt("new-contact-alias")
                .hint_text("e.g. Reception PC")
                .desired_width(f32::INFINITY),
        );
        if alias != state.new_contact_alias {
            out.push(Action::Edit(Edit::NewContactAlias(alias)));
        }

        ui.add_space(space::SM);
        let button = widgets::primary_button(ui, palette, "Add to address book", valid);
        if button.clicked() {
            out.push(Action::AddContact);
        }
        if !valid && !state.new_contact_id.trim().is_empty() {
            ui.add_space(space::XS);
            ui.label(
                egui::RichText::new("Enter a valid ID")
                    .size(12.0)
                    .color(palette.warning),
            );
        }
    });
}

fn saved_contacts(ui: &mut egui::Ui, state: &AppState, out: &mut Vec<Action>) {
    let palette = &state.palette;
    let favorites: Vec<_> = state.favorites().collect();

    if favorites.is_empty() {
        widgets::card(palette).show(ui, |ui| {
            widgets::hint(ui, palette, "No saved contacts yet.");
        });
        return;
    }

    ui.horizontal_wrapped(|ui| {
        for record in favorites {
            let alias = record
                .alias
                .clone()
                .unwrap_or_else(|| "No alias".to_owned());
            let response = widgets::subtle_card(palette)
                .show(ui, |ui| {
                    ui.set_width(210.0);
                    ui.horizontal(|ui| {
                        avatar(ui, state, &alias);
                        ui.vertical(|ui| {
                            ui.label(egui::RichText::new(&alias).strong());
                            ui.label(
                                egui::RichText::new(record.peer_id.grouped())
                                    .font(FontId::monospace(11.0))
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
            response.context_menu(|ui| {
                if ui.button("Remove from address book").clicked() {
                    out.push(Action::RemoveContact(record.peer_id));
                    ui.close();
                }
                if ui.button("Unstar").clicked() {
                    out.push(Action::SetFavorite(record.peer_id, false));
                    ui.close();
                }
            });
        }
    });
}

fn avatar(ui: &mut egui::Ui, state: &AppState, alias: &str) {
    let palette = &state.palette;
    let initial = alias
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "?".to_owned());
    let (rect, _) = ui.allocate_exact_size(vec2(36.0, 36.0), Sense::hover());
    ui.painter()
        .circle_filled(rect.center(), 18.0, palette.accent_soft);
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        initial,
        FontId::proportional(15.0),
        palette.accent,
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
    fn only_favorites_appear_as_saved_contacts() {
        let state = populated();
        let favorites: Vec<_> = state.favorites().map(|r| r.peer_id).collect();
        assert_eq!(favorites.len(), 1, "the populated fixture has one favorite");
        assert!(state.history.len() > favorites.len());
    }

    #[test]
    fn an_alias_of_only_whitespace_does_not_panic_on_its_initial() {
        let state = AppState {
            history: vec![remu_proto::ConnectionRecord {
                peer_id: crate::views::test_support::peer(123_456_789),
                alias: Some(String::new()),
                last_connected_at: 0,
                favorite: true,
            }],
            ..AppState::default()
        };
        let mut out = Vec::new();
        egui::__run_test_ui(|ui| show(ui, &state, &mut out));
    }
}
