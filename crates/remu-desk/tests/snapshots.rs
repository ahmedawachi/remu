//! Rendered snapshots of every screen.
//!
//! These render the real views through wgpu, headless, at a fixed size, and
//! compare the result against a committed PNG. They exist for two reasons:
//!
//! - A layout regression is invisible to a unit test. The bug that stretched
//!   the desk cards to the bottom of the window, and the one that clipped the
//!   ID chip against the window edge, both passed every assertion we had.
//! - Reviewing the UI should not require a human to build and click through
//!   five screens.
//!
//! Update the committed images deliberately, after looking at the diff:
//!
//! ```text
//! UPDATE_SNAPSHOTS=1 cargo test -p remu-desk --test snapshots
//! ```
//!
//! Rendering needs a GPU adapter, so the whole file is skipped where none is
//! available rather than failing a CI box that has no display.

use egui_kittest::Harness;
use remu_desk::state::{Action, AppState, Route};
use remu_desk::theme::Palette;
use remu_desk::{app, theme, views};

/// Matches the default window, so the snapshots show what a user opening the
/// app actually sees rather than some convenient smaller frame.
const SIZE: egui::Vec2 = egui::vec2(1180.0, 760.0);

/// Renders one route inside the real application chrome.
fn shoot(name: &str, state: AppState) {
    let mut harness = Harness::builder().with_size(SIZE).build_ui(move |ui| {
        // Snapshots must show the fonts the app really uses, or they
        // review a layout nobody will ever see.
        remu_desk::fonts::install(ui.ctx());
        theme::apply(ui.ctx(), state.palette);
        let p = state.palette;
        let mut out: Vec<Action> = Vec::new();

        egui::Panel::left("sidebar")
            .exact_size(app::SIDEBAR_WIDTH)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(p.bg_surface)
                    .stroke(egui::Stroke::new(1.0, p.border))
                    .inner_margin(egui::Margin {
                        left: theme::space::MD as i8,
                        right: theme::space::MD as i8,
                        top: (theme::space::MD + app::TITLEBAR_INSET) as i8,
                        bottom: theme::space::MD as i8,
                    }),
            )
            .show(ui, |ui| views::sidebar::show(ui, &state, &mut out));

        egui::Panel::top("topbar")
            .exact_size(app::TOPBAR_HEIGHT)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(p.bg_surface)
                    .inner_margin(egui::Margin::symmetric(
                        theme::space::LG as i8,
                        theme::space::SM as i8,
                    )),
            )
            .show(ui, |ui| views::topbar::show(ui, &state, &mut out));

        egui::Panel::bottom("statusbar")
            .exact_size(app::STATUSBAR_HEIGHT)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(p.bg_surface)
                    .inner_margin(egui::Margin::symmetric(
                        theme::space::LG as i8,
                        theme::space::XS as i8,
                    )),
            )
            .show(ui, |ui| views::statusbar::show(ui, &state, &mut out));

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(p.bg_base).inner_margin(
                if state.route == Route::Session {
                    egui::Margin::same(theme::space::SM as i8)
                } else {
                    egui::Margin::symmetric(theme::space::XL as i8, theme::space::LG as i8)
                },
            ))
            .show(ui, |ui| views::show_route(ui, &state, &mut out));

        if state.incoming.is_some() {
            views::incoming_dialog::show(ui, &state, &mut out);
        }
        views::toasts::show(ui, &state, &mut out);
    });

    // Two passes: egui is immediate-mode and several widgets size themselves
    // from the previous frame, so a single pass captures a half-settled layout.
    harness.run();
    harness.run();
    harness.snapshot(name);
}

fn base(route: Route) -> AppState {
    AppState {
        route,
        palette: Palette::DARK,
        version: "0.1.0".to_owned(),
        platform: "macos".to_owned(),
        ..AppState::default()
    }
}

/// A desk that has been used: an ID issued, contacts saved, relay connected.
fn lived_in(route: Route) -> AppState {
    let mut state = base(route);
    state.my_id = Some(remu_proto::PeerId::new(482_100_337).expect("valid id"));
    state.link = remu_desk::state::LinkStatus::Ready;
    state.settings.alias = "Ahmed — MacBook Pro".to_owned();
    // Ages, not instants. The Recent list renders this column through
    // `format_ago`, which subtracts from the clock — so a fixed timestamp makes
    // the committed image drift by a day every day, and the snapshot was
    // failing on nothing but the calendar.
    //
    // One age each for the minutes, hours and days branches of `format_ago`
    // ("never" and "just now" are its unit tests' business, not a lived-in
    // list's). `format_ago` floors, so each age sits half a unit above a whole
    // one: a render seconds slower than this line still floors to the same
    // label, and could only reach the next one after half a unit.
    state.history = [
        (
            987_654_321u32,
            "Reception PC",
            true,
            12 * MINUTE + 30 * SECOND,
        ),
        (
            903_771_204,
            "Warehouse terminal",
            true,
            3 * HOUR + 30 * MINUTE,
        ),
        (115_640_982, "Site office", false, 5 * DAY + 12 * HOUR),
    ]
    .into_iter()
    .map(|(id, alias, favorite, age)| remu_proto::ConnectionRecord {
        peer_id: remu_proto::PeerId::new(id).expect("valid id"),
        alias: Some(alias.to_owned()),
        last_connected_at: now_ms() - age,
        favorite,
    })
    .collect();
    state
}

const SECOND: u64 = 1_000;
const MINUTE: u64 = 60 * SECOND;
const HOUR: u64 = 60 * MINUTE;
const DAY: u64 = 24 * HOUR;

/// The same clock the views read, so an age computed here and rendered there
/// differ by the microseconds between the two calls rather than by a timezone.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .expect("a clock at or after the epoch")
}

#[test]
fn new_connection_empty() {
    shoot("new_connection_empty", base(Route::NewConnection));
}

#[test]
fn new_connection_lived_in() {
    shoot("new_connection", lived_in(Route::NewConnection));
}

#[test]
fn recent_sessions() {
    shoot("recent", lived_in(Route::Recent));
}

#[test]
fn address_book() {
    shoot("address_book", lived_in(Route::AddressBook));
}

#[test]
fn settings() {
    shoot("settings", lived_in(Route::Settings));
}

#[test]
fn about() {
    shoot("about", lived_in(Route::About));
}

#[test]
fn light_theme() {
    let mut state = lived_in(Route::NewConnection);
    state.palette = Palette::LIGHT;
    state.settings.theme = remu_proto::Theme::Light;
    shoot("new_connection_light", state);
}

#[test]
fn incoming_request_prompt() {
    let mut state = lived_in(Route::NewConnection);
    state.incoming = Some(remu_desk::state::IncomingRequest {
        from: remu_proto::PeerId::new(555_666_777).expect("valid id"),
        from_alias: Some("Warehouse terminal".to_owned()),
        session_id: remu_proto::SessionId::random(),
        needs_password: false,
    });
    shoot("incoming_request", state);
}

/// The narrowest supported window. Catches the negative-width panics and the
/// squeezed two-column layouts that only appear when the user drags the frame in.
#[test]
fn narrow_window_does_not_panic_or_overflow() {
    let state = lived_in(Route::NewConnection);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(880.0, 560.0))
        .build_ui(move |ui| {
            remu_desk::fonts::install(ui.ctx());
            theme::apply(ui.ctx(), state.palette);
            let mut out: Vec<Action> = Vec::new();
            egui::CentralPanel::default().show(ui, |ui| views::show_route(ui, &state, &mut out));
        });
    harness.run();
    harness.run();
    harness.snapshot("narrow_new_connection");
}
