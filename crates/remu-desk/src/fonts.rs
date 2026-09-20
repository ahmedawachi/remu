//! Native font loading.
//!
//! egui ships a bundled sans that looks the same everywhere, which is exactly
//! the problem: on macOS it reads as "not a Mac app", and text weight and
//! spacing sit slightly wrong next to every other window on the desktop. Remu
//! loads the platform's own UI font at runtime instead, so it renders in SF Pro
//! on macOS, Segoe UI on Windows, and whatever the distribution ships on Linux.
//!
//! These files are *read from the OS*, never vendored, so no proprietary font
//! is redistributed with the binary. When nothing on the list is present the
//! bundled fallback is kept and [`Report::using_fallback`] says so.

use std::sync::Arc;

use egui::{FontData, FontDefinitions, FontFamily};

/// Candidate UI fonts, best first.
#[cfg(target_os = "macos")]
const UI_CANDIDATES: &[&str] = &[
    "/System/Library/Fonts/SFNS.ttf",
    "/System/Library/Fonts/Helvetica.ttc",
];

#[cfg(target_os = "windows")]
const UI_CANDIDATES: &[&str] = &[
    r"C:\Windows\Fonts\segoeuivf.ttf",
    r"C:\Windows\Fonts\segoeui.ttf",
    r"C:\Windows\Fonts\arial.ttf",
];

#[cfg(all(unix, not(target_os = "macos")))]
const UI_CANDIDATES: &[&str] = &[
    "/usr/share/fonts/truetype/inter/Inter-Regular.ttf",
    "/usr/share/fonts/TTF/Inter-Regular.ttf",
    "/usr/share/fonts/cantarell/Cantarell-VF.otf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
    "/usr/share/fonts/noto/NotoSans-Regular.ttf",
];

/// Candidate monospace fonts, best first. Used for desk IDs, which people read
/// digit by digit and out loud — a tabular, unambiguous face matters more here
/// than anywhere else in the UI.
#[cfg(target_os = "macos")]
const MONO_CANDIDATES: &[&str] = &["/System/Library/Fonts/SFNSMono.ttf"];

#[cfg(target_os = "windows")]
const MONO_CANDIDATES: &[&str] = &[
    r"C:\Windows\Fonts\CascadiaMono.ttf",
    r"C:\Windows\Fonts\consola.ttf",
];

#[cfg(all(unix, not(target_os = "macos")))]
const MONO_CANDIDATES: &[&str] = &[
    "/usr/share/fonts/truetype/jetbrains-mono/JetBrainsMono-Regular.ttf",
    "/usr/share/fonts/TTF/JetBrainsMono-Regular.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationMono-Regular.ttf",
];

/// Which fonts were actually installed, for the About screen and for tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub ui: Option<String>,
    pub mono: Option<String>,
}

impl Report {
    /// True when neither native font could be loaded and egui's bundled face is
    /// still in use.
    pub fn using_fallback(&self) -> bool {
        self.ui.is_none() && self.mono.is_none()
    }
}

/// Installs the best available native fonts into `ctx`.
///
/// Native faces are *prepended* to each family rather than replacing it, so
/// egui's bundled font stays as the last resort for glyphs the system face has
/// no coverage for — emoji in a chat message, say, or Arabic in a filename.
pub fn install(ctx: &egui::Context) -> Report {
    let mut defs = FontDefinitions::default();
    let mut report = Report::default();

    if let Some((path, bytes)) = load_first(UI_CANDIDATES) {
        defs.font_data
            .insert("remu-ui".to_owned(), Arc::new(FontData::from_owned(bytes)));
        prepend(&mut defs, FontFamily::Proportional, "remu-ui");
        report.ui = Some(path);
    }

    if let Some((path, bytes)) = load_first(MONO_CANDIDATES) {
        defs.font_data.insert(
            "remu-mono".to_owned(),
            Arc::new(FontData::from_owned(bytes)),
        );
        prepend(&mut defs, FontFamily::Monospace, "remu-mono");
        report.mono = Some(path);
    }

    if report.using_fallback() {
        tracing::info!("no native UI font found; using the bundled fallback");
    } else {
        tracing::debug!(ui = ?report.ui, mono = ?report.mono, "loaded native fonts");
    }

    ctx.set_fonts(defs);
    report
}

fn prepend(defs: &mut FontDefinitions, family: FontFamily, key: &str) {
    defs.families
        .entry(family)
        .or_default()
        .insert(0, key.to_owned());
}

/// Reads the first candidate that exists. A font present but unreadable (a
/// permissions quirk, a file being replaced by an OS update) is skipped rather
/// than fatal — the next candidate, or the bundled fallback, still renders.
fn load_first(candidates: &[&str]) -> Option<(String, Vec<u8>)> {
    for path in candidates {
        match std::fs::read(path) {
            Ok(bytes) if !bytes.is_empty() => return Some(((*path).to_owned(), bytes)),
            Ok(_) => tracing::debug!(path, "font file is empty; skipping"),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => tracing::debug!(path, %err, "could not read font; skipping"),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_candidate_list_yields_no_font_rather_than_panicking() {
        assert!(load_first(&["/definitely/not/a/font.ttf"]).is_none());
        assert!(load_first(&[]).is_none());
    }

    #[test]
    fn skips_an_unreadable_candidate_and_takes_the_next() {
        let real = UI_CANDIDATES
            .iter()
            .find(|p| std::path::Path::new(p).exists());
        let Some(real) = real else {
            // A machine with none of the listed fonts is a legitimate
            // configuration; there is nothing to assert about fallback order.
            return;
        };
        let (picked, bytes) = load_first(&["/nope/missing.ttf", real]).expect("second candidate");
        assert_eq!(&picked, *real);
        assert!(!bytes.is_empty());
    }

    #[test]
    fn report_only_claims_fallback_when_nothing_loaded() {
        assert!(Report::default().using_fallback());
        assert!(!Report {
            ui: Some("x".into()),
            mono: None
        }
        .using_fallback());
    }

    #[test]
    fn prepending_keeps_the_bundled_font_as_a_coverage_fallback() {
        let mut defs = FontDefinitions::default();
        let bundled = defs.families[&FontFamily::Proportional].clone();
        defs.font_data
            .insert("remu-ui".to_owned(), Arc::new(FontData::from_static(&[])));
        prepend(&mut defs, FontFamily::Proportional, "remu-ui");

        let after = &defs.families[&FontFamily::Proportional];
        assert_eq!(after[0], "remu-ui", "native font must be tried first");
        assert!(
            after.ends_with(&bundled),
            "bundled fonts must remain after it for glyph coverage"
        );
    }

    #[test]
    fn installs_into_a_context_without_panicking() {
        // Exercises the real platform paths on whatever machine runs the suite.
        let ctx = egui::Context::default();
        let report = install(&ctx);
        if let Some(path) = &report.ui {
            assert!(std::path::Path::new(path).exists());
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_finds_its_system_faces() {
        // If Apple moves these, the app silently regresses to the bundled font
        // and looks foreign again; better to fail here.
        let ctx = egui::Context::default();
        let report = install(&ctx);
        assert!(report.ui.is_some(), "SF Pro not found at the expected path");
        assert!(
            report.mono.is_some(),
            "SF Mono not found at the expected path"
        );
    }
}
