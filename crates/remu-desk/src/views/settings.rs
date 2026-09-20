//! Identity, connection, security and OS permissions.
//!
//! Every control edits a clone of [`AppState::settings`] and the whole draft is
//! reported once, at the end, if anything moved. That keeps the view pure while
//! still letting a checkbox feel instant: the widget already drew the new value
//! this frame.

use egui::FontId;
use remu_input::permissions::PrivacyPane;
use remu_proto::{AcceptPolicy, Quality, Theme};

use crate::state::{Action, AppState, Edit};
use crate::theme::{space, StatusTone};
use crate::widgets;

pub fn show(ui: &mut egui::Ui, state: &AppState, out: &mut Vec<Action>) {
    let palette = &state.palette;
    let mut draft = state.settings.clone();

    // The shell owns the scroll container; nesting a second one here made the
    // inner area claim the full viewport height and stretched every card.
    {
        // Clamped to what is actually available. A bare `set_max_width(980)`
        // forces the Ui wider than the panel on a default-sized window, which
        // pushed the right-hand column past the window edge and clipped every
        // hint and permission pill in it.
        ui.set_max_width(ui.available_width().min(980.0));
        ui.columns(2, |cols| {
            identity(&mut cols[0], state, &mut draft);
            connection(&mut cols[0], state, &mut draft, out);
            security(&mut cols[1], state, &mut draft);
            permissions(&mut cols[1], state, out);
        });

        ui.add_space(space::MD);
        ui.horizontal(|ui| {
            if widgets::primary_button(ui, palette, "Save settings", true).clicked() {
                out.push(Action::SaveSettings);
            }
            widgets::hint(
                ui,
                palette,
                "Quality and permission changes apply to new sessions.",
            );
        });
    }

    if draft != state.settings {
        out.push(Action::Edit(Edit::Settings(Box::new(draft))));
    }
}

fn identity(ui: &mut egui::Ui, state: &AppState, draft: &mut remu_proto::Settings) {
    let palette = &state.palette;
    widgets::card(palette).show(ui, |ui| {
        widgets::section_title(ui, palette, "Identity");
        field_label(ui, state, "Alias");
        ui.add(
            egui::TextEdit::singleline(&mut draft.alias)
                .id_salt("settings-alias")
                .hint_text("Shown to remote peers")
                .desired_width(f32::INFINITY),
        );

        ui.add_space(space::SM);
        field_label(ui, state, "Theme");
        ui.horizontal(|ui| {
            for (theme, label) in [
                (Theme::Dark, "Dark"),
                (Theme::Light, "Light"),
                (Theme::System, "System"),
            ] {
                ui.radio_value(&mut draft.theme, theme, label);
            }
        });

        ui.add_space(space::SM);
        ui.checkbox(&mut draft.start_minimized, "Start minimized to the tray");
    });
    ui.add_space(space::MD);
}

fn connection(
    ui: &mut egui::Ui,
    state: &AppState,
    draft: &mut remu_proto::Settings,
    out: &mut Vec<Action>,
) {
    let palette = &state.palette;
    widgets::card(palette).show(ui, |ui| {
        widgets::section_title(ui, palette, "Connection");

        field_label(ui, state, "Relay server URL");
        ui.add(
            egui::TextEdit::singleline(&mut draft.relay_url)
                .id_salt("settings-relay-url")
                .hint_text(remu_proto::settings::DEFAULT_RELAY_URL)
                .desired_width(f32::INFINITY),
        );
        widgets::hint(
            ui,
            palette,
            "A small WebSocket server brokers IDs and SDP/ICE exchange. \
             Media and data flow peer-to-peer.",
        );

        field_label(ui, state, "Relay token (optional)");
        ui.add(
            egui::TextEdit::singleline(&mut draft.relay_token)
                .id_salt("settings-relay-token")
                .password(true)
                .desired_width(f32::INFINITY),
        );

        ui.add_space(space::SM);
        if widgets::secondary_button(ui, palette, "Reconnect relay", true).clicked() {
            out.push(Action::ReconnectRelay);
        }

        ui.add_space(space::MD);
        field_label(ui, state, "TURN relay URL (optional)");
        ui.add(
            egui::TextEdit::singleline(&mut draft.turn.url)
                .id_salt("settings-turn-url")
                .hint_text("turn:turn.example.com:3478")
                .desired_width(f32::INFINITY),
        );
        field_label(ui, state, "TURN username");
        ui.add(
            egui::TextEdit::singleline(&mut draft.turn.username)
                .id_salt("settings-turn-user")
                .desired_width(f32::INFINITY),
        );
        field_label(ui, state, "TURN credential");
        ui.add(
            egui::TextEdit::singleline(&mut draft.turn.credential)
                .id_salt("settings-turn-credential")
                .password(true)
                .desired_width(f32::INFINITY),
        );
        widgets::hint(
            ui,
            palette,
            "Configure a TURN server to connect across strict corporate NATs \
             and firewalls. Not needed on a local network.",
        );

        ui.add_space(space::MD);
        field_label(ui, state, "Quality");
        ui.horizontal(|ui| {
            for (quality, label) in [
                (Quality::Low, "Low"),
                (Quality::Balanced, "Balanced"),
                (Quality::Sharp, "Sharp"),
            ] {
                ui.radio_value(&mut draft.quality, quality, label);
            }
        });
        let (bitrate, fps) = draft.quality.targets();
        widgets::hint(
            ui,
            palette,
            &format!("Targets {bitrate} kbps at {fps} fps."),
        );
    });
    ui.add_space(space::MD);
}

fn security(ui: &mut egui::Ui, state: &AppState, draft: &mut remu_proto::Settings) {
    let palette = &state.palette;
    widgets::card(palette).show(ui, |ui| {
        widgets::section_title(ui, palette, "Security");

        field_label(ui, state, "When someone asks to connect");
        for (policy, label) in [
            (AcceptPolicy::Prompt, "Always ask me"),
            (
                AcceptPolicy::PasswordOrPrompt,
                "Let the unattended password in, otherwise ask",
            ),
            (AcceptPolicy::Always, "Accept everything"),
        ] {
            ui.radio_value(&mut draft.accept_policy, policy, label);
        }
        if draft.accept_policy == AcceptPolicy::Always {
            ui.label(
                egui::RichText::new(
                    "Anyone who knows this desk's ID can take it over without being asked.",
                )
                .size(12.0)
                .color(palette.danger),
            );
        }

        ui.add_space(space::SM);
        ui.checkbox(&mut draft.allow_input, "Allow remote keyboard & mouse");
        ui.checkbox(&mut draft.allow_files, "Allow remote file transfer");
        ui.checkbox(&mut draft.allow_clipboard, "Sync clipboard during sessions");

        ui.add_space(space::SM);
        field_label(ui, state, "Unattended access password");
        ui.add(
            egui::TextEdit::singleline(&mut draft.unattended_password)
                .id_salt("settings-unattended")
                .password(true)
                .hint_text("Empty = ask on every connection")
                .desired_width(f32::INFINITY),
        );
        widgets::hint(
            ui,
            palette,
            "Peers who prove this password connect without a prompt. \
             Leave it empty to always ask.",
        );
        if !draft.unattended_password.is_empty() && !draft.unattended_enabled() {
            // Otherwise a user sets a password, sees it saved, and never learns
            // that the policy above is still refusing to honour it.
            ui.label(
                egui::RichText::new(
                    "This password is ignored while the policy is \"Always ask me\".",
                )
                .size(12.0)
                .color(palette.warning),
            );
        }

        ui.add_space(space::SM);
        field_label(ui, state, "Save received files to");
        ui.add(
            egui::TextEdit::singleline(&mut draft.download_dir)
                .id_salt("settings-download-dir")
                .hint_text("Empty = the system download folder")
                .desired_width(f32::INFINITY),
        );
    });
    ui.add_space(space::MD);
}

fn permissions(ui: &mut egui::Ui, state: &AppState, out: &mut Vec<Action>) {
    let palette = &state.palette;
    widgets::card(palette).show(ui, |ui| {
        widgets::section_title(ui, palette, "System permissions");

        permission_row(
            ui,
            state,
            "Screen recording",
            &state.permissions.screen_recording,
        );
        permission_row(
            ui,
            state,
            "Accessibility (remote input)",
            &state.permissions.accessibility,
        );

        ui.add_space(space::SM);
        ui.horizontal(|ui| {
            if widgets::secondary_button(ui, palette, "Request accessibility", true).clicked() {
                out.push(Action::RequestAccessibility);
            }
            if widgets::secondary_button(ui, palette, "Open settings", true).clicked() {
                out.push(Action::OpenPrivacySettings(PrivacyPane::Accessibility));
            }
        });
        ui.add_space(space::XS);
        if widgets::secondary_button(ui, palette, "Open screen-recording settings", true).clicked()
        {
            out.push(Action::OpenPrivacySettings(PrivacyPane::ScreenRecording));
        }
        widgets::hint(
            ui,
            palette,
            "On macOS, grant Remu permission in System Settings → Privacy & Security → \
             Accessibility and Screen Recording, then relaunch.",
        );
    });
}

fn permission_row(ui: &mut egui::Ui, state: &AppState, label: &str, value: &str) {
    let palette = &state.palette;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).color(palette.fg_secondary));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            widgets::pill(ui, palette, permission_tone(value), value);
        });
    });
}

/// Maps the runtime's permission wording onto a pill colour. An unrecognized
/// word is shown as-is in the neutral tone rather than being claimed as good.
fn permission_tone(value: &str) -> StatusTone {
    match value {
        "granted" | "not required" => StatusTone::Good,
        "denied" => StatusTone::Bad,
        "unknown" => StatusTone::Warn,
        _ => StatusTone::Idle,
    }
}

fn field_label(ui: &mut egui::Ui, state: &AppState, label: &str) {
    ui.add_space(space::XS);
    ui.label(
        egui::RichText::new(label)
            .font(FontId::proportional(12.0))
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
        }
    }

    #[test]
    fn an_untouched_settings_screen_reports_no_edit() {
        // The draft-and-compare pattern must not report a change every frame,
        // or the runtime would save on every repaint.
        let state = AppState::default();
        let mut out = Vec::new();
        egui::__run_test_ui(|ui| show(ui, &state, &mut out));
        assert!(
            !out.iter().any(|a| matches!(a, Action::Edit(_))),
            "drawing alone must not look like an edit: {out:?}"
        );
    }

    #[test]
    fn permission_words_map_to_the_colour_they_deserve() {
        assert_eq!(permission_tone("granted"), StatusTone::Good);
        assert_eq!(permission_tone("not required"), StatusTone::Good);
        assert_eq!(permission_tone("denied"), StatusTone::Bad);
        assert_eq!(permission_tone("unknown"), StatusTone::Warn);
        assert_eq!(
            permission_tone("something a future platform said"),
            StatusTone::Idle,
            "an unknown word must not be claimed as granted"
        );
    }
}
