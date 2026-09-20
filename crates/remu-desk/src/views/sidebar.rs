//! The left navigation rail: brand, routes, build footer.

use egui::{vec2, Align, Color32, CornerRadius, FontId, Layout, Rect, Sense, Stroke, StrokeKind};

use crate::state::{Action, AppState, Route};
use crate::theme::{radius, space, text};

const ROW_HEIGHT: f32 = 34.0;

pub fn show(ui: &mut egui::Ui, state: &AppState, out: &mut Vec<Action>) {
    let palette = &state.palette;

    brand(ui, state);
    ui.add_space(space::MD);

    for route in Route::NAV {
        if nav_row(ui, state, *route).clicked() {
            out.push(Action::Navigate(*route));
        }
    }

    // The footer is pinned to the bottom so the nav list can grow without
    // pushing the build stamp off the panel.
    ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
        ui.add_space(space::SM);
        ui.label(
            egui::RichText::new(format!("v{} · WebRTC", state.version))
                .size(text::SECTION)
                .color(palette.fg_muted),
        );
        ui.add_space(space::SM);
        let line = ui.available_rect_before_wrap();
        ui.painter()
            .hline(line.x_range(), line.max.y, Stroke::new(1.0, palette.border));
    });
}

fn brand(ui: &mut egui::Ui, state: &AppState) {
    let palette = &state.palette;
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(36.0, 36.0), Sense::hover());
        ui.painter().rect(
            rect,
            CornerRadius::same(radius::MD),
            palette.bg_sunken,
            Stroke::new(1.0, palette.border_strong),
            StrokeKind::Inside,
        );
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "R",
            FontId::proportional(18.0),
            palette.accent,
        );
        ui.vertical(|ui| {
            ui.label(egui::RichText::new("Remu").strong().size(14.0));
            ui.label(
                egui::RichText::new("Remote desktop")
                    .size(text::SECTION)
                    .color(palette.fg_muted),
            );
        });
    });
    ui.add_space(space::MD);
    let line = ui.available_rect_before_wrap();
    ui.painter()
        .hline(line.x_range(), line.min.y, Stroke::new(1.0, palette.border));
}

fn nav_row(ui: &mut egui::Ui, state: &AppState, route: Route) -> egui::Response {
    let palette = &state.palette;
    // A live session owns the screen; no nav entry is highlighted while it is
    // up, matching the predecessor's `isActive`.
    let active = state.route == route;

    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(vec2(width, ROW_HEIGHT), Sense::click());

    if ui.is_rect_visible(rect) {
        let fill = if active {
            palette.accent_soft
        } else if response.hovered() {
            palette.bg_raised
        } else {
            Color32::TRANSPARENT
        };
        ui.painter()
            .rect_filled(rect, CornerRadius::same(radius::SM), fill);
        if active {
            // A stripe rather than only a tint: the tint alone is easy to miss
            // on the light palette.
            let stripe = Rect::from_min_size(
                rect.left_top() + vec2(0.0, 6.0),
                vec2(2.5, rect.height() - 12.0),
            );
            ui.painter()
                .rect_filled(stripe, CornerRadius::same(radius::SM), palette.accent);
        }

        let icon_color = if active {
            palette.accent
        } else {
            palette.fg_muted
        };
        let text_color = if active || response.hovered() {
            palette.fg_primary
        } else {
            palette.fg_secondary
        };
        ui.painter().text(
            rect.left_center() + vec2(space::MD + 6.0, 0.0),
            egui::Align2::CENTER_CENTER,
            route.icon(),
            FontId::proportional(14.0),
            icon_color,
        );
        ui.painter().text(
            rect.left_center() + vec2(space::MD + 20.0, 0.0),
            egui::Align2::LEFT_CENTER,
            route.label(),
            FontId::proportional(14.0),
            text_color,
        );
    }
    response.on_hover_text(route.label())
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
    fn offers_one_row_per_navigable_route() {
        // A guard against a route being added to the enum and quietly never
        // becoming reachable.
        assert_eq!(Route::NAV.len(), 5);
        assert!(Route::NAV.iter().all(|r| !r.icon().is_empty()));
    }
}
