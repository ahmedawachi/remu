//! The Remu desk application.

#![forbid(unsafe_code)]
// A release build should not also pop a console window on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use remu_desk::app::DeskApp;

fn main() -> eframe::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("REMU_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_inner_size([1180.0, 760.0])
        .with_min_inner_size([880.0, 560.0])
        .with_app_id("dev.remu.desk")
        .with_title("Remu");

    // On macOS the content runs under the title bar and the traffic lights
    // float over the sidebar, so the window reads as one dark surface instead
    // of a dark app wearing a light hat. The sidebar reserves
    // `app::TITLEBAR_INSET` at the top to keep clear of the buttons.
    #[cfg(target_os = "macos")]
    {
        viewport = viewport
            .with_fullsize_content_view(true)
            .with_titlebar_shown(false)
            .with_title_shown(false);
    }

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "Remu",
        options,
        Box::new(|cc| Ok(Box::new(DeskApp::new(cc)))),
    )
}
