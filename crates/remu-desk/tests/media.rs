//! Regenerates the images the README uses.
//!
//! The banner and the mark are drawn with the application's own palette and
//! type rather than in a design tool, so the project's front page cannot drift
//! away from the product it is advertising: change an accent colour in
//! `theme.rs` and the banner follows.
//!
//! Ignored by default — it writes into the repository. Run it deliberately:
//!
//! ```text
//! cargo test -p remu-desk --test media -- --ignored --nocapture
//! ```

use std::path::{Path, PathBuf};

use egui::{pos2, vec2, Align2, Color32, CornerRadius, FontId, Rect, Stroke, StrokeKind};
use egui_kittest::Harness;
use remu_desk::theme::Palette;

/// A directory in the repository, created if a checkout has not got it yet.
///
/// `CARGO_MANIFEST_DIR` is `<repo>/crates/remu-desk`, so every caller passes a
/// path relative to that.
fn repo_dir(relative: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
    std::fs::create_dir_all(&dir).unwrap_or_else(|err| panic!("create {relative}: {err}"));
    dir.canonicalize().unwrap_or(dir)
}

fn media_dir() -> PathBuf {
    repo_dir("../../docs/media")
}

/// Renders `draw` at `size` and writes it as a PNG under `docs/media`.
fn render(name: &str, size: egui::Vec2, draw: impl Fn(&egui::Painter, Rect) + Send + 'static) {
    render_into(media_dir(), name, size, draw);
}

/// Renders `draw` at `size` and writes it as a PNG into `dir`.
fn render_into(
    dir: PathBuf,
    name: &str,
    size: egui::Vec2,
    draw: impl Fn(&egui::Painter, Rect) + Send + 'static,
) {
    let mut harness = Harness::builder().with_size(size).build_ui(move |ui| {
        remu_desk::fonts::install(ui.ctx());
        let rect = ui.max_rect();
        draw(ui.painter(), rect);
    });
    harness.run();
    harness.run();

    let image = harness.render().expect("render the frame");
    let path = dir.join(format!("{name}.png"));
    image.save(&path).expect("write png");
    println!("wrote {}", path.display());
}

/// The rounded-square mark: a desk seen from another desk.
fn draw_mark(painter: &egui::Painter, rect: Rect, corner: u8) {
    let p = Palette::DARK;
    painter.rect_filled(rect, CornerRadius::same(corner), p.accent);

    // Two overlapping screens: the near one solid, the far one outlined, which
    // is the whole idea of the product in one glyph.
    let unit = rect.width() / 10.0;
    let far = Rect::from_min_size(
        rect.min + vec2(unit * 2.2, unit * 2.4),
        vec2(unit * 4.4, unit * 3.2),
    );
    let near = Rect::from_min_size(
        rect.min + vec2(unit * 3.6, unit * 4.0),
        vec2(unit * 4.4, unit * 3.2),
    );
    painter.rect_stroke(
        far,
        CornerRadius::same(2),
        Stroke::new(unit * 0.42, Color32::from_white_alpha(150)),
        StrokeKind::Inside,
    );
    painter.rect_filled(near, CornerRadius::same(2), Color32::WHITE);
}

#[test]
#[ignore = "writes docs/media; run with --ignored to regenerate"]
fn mark() {
    render("mark", vec2(256.0, 256.0), |painter, rect| {
        draw_mark(painter, rect.shrink(8.0), 56);
    });
}

/// The master the platform icons are cut from.
///
/// 1024 is the largest slot macOS asks for, and every other size an `.icns` or
/// `.ico` needs divides into it, so the packaging scripts only ever downsample.
/// Rendering it rather than upscaling `mark.png` is the point: the mark is
/// drawn, not a raster, so there is no reason for an app icon to be soft.
#[test]
#[ignore = "writes packaging/icons; run with --ignored to regenerate"]
fn app_icon() {
    render_into(
        repo_dir("../../packaging/icons"),
        "icon-1024",
        vec2(1024.0, 1024.0),
        |painter, rect| {
            // Proportional to `mark`: the same inset and corner at four times
            // the scale, so the two images stay the same drawing.
            draw_mark(painter, rect.shrink(32.0), 224);
        },
    );
}

#[test]
#[ignore = "writes docs/media; run with --ignored to regenerate"]
fn banner() {
    render("banner", vec2(1200.0, 340.0), |painter, rect| {
        let p = Palette::DARK;
        painter.rect_filled(rect, CornerRadius::ZERO, p.bg_base);

        // A wide, very low-alpha accent wash behind the wordmark keeps the
        // banner from reading as a flat black bar.
        for i in 0..28 {
            let t = i as f32 / 28.0;
            let band = Rect::from_min_size(
                pos2(rect.left(), rect.top() + rect.height() * t),
                vec2(rect.width(), rect.height() / 28.0 + 1.0),
            );
            let alpha = (18.0 * (1.0 - t) * (1.0 - t)) as u8;
            painter.rect_filled(
                band,
                CornerRadius::ZERO,
                Color32::from_rgba_unmultiplied(p.accent.r(), p.accent.g(), p.accent.b(), alpha),
            );
        }

        // The lockup is centred as a unit: mark, then the widest line of text.
        // Measured rather than eyeballed so a wording change stays balanced.
        let widest = painter
            .layout_no_wrap(
                "Remote desktop that owns its own pixels".to_owned(),
                FontId::proportional(25.0),
                p.fg_secondary,
            )
            .rect
            .width()
            .max(
                painter
                    .layout_no_wrap(
                        "Rust  ·  WebRTC  ·  self-hosted relay  ·  macOS · Windows · Linux"
                            .to_owned(),
                        FontId::proportional(16.0),
                        p.fg_muted,
                    )
                    .rect
                    .width(),
            );
        let lockup = 116.0 + 40.0 + widest;
        let left = rect.left() + ((rect.width() - lockup) / 2.0).max(48.0);
        let mark_rect = Rect::from_min_size(pos2(left, rect.top() + 112.0), vec2(116.0, 116.0));
        draw_mark(painter, mark_rect, 28);

        let text_x = mark_rect.right() + 40.0;
        painter.text(
            pos2(text_x, rect.center().y - 34.0),
            Align2::LEFT_CENTER,
            "Remu",
            FontId::proportional(78.0),
            p.fg_primary,
        );
        painter.text(
            pos2(text_x + 4.0, rect.center().y + 26.0),
            Align2::LEFT_CENTER,
            "Remote desktop that owns its own pixels",
            FontId::proportional(25.0),
            p.fg_secondary,
        );
        painter.text(
            pos2(text_x + 4.0, rect.center().y + 66.0),
            Align2::LEFT_CENTER,
            "Rust  ·  WebRTC  ·  self-hosted relay  ·  macOS · Windows · Linux",
            FontId::proportional(16.0),
            p.fg_muted,
        );

        // A hairline of accent along the bottom edge, the same device the
        // sidebar uses to mark the active row.
        painter.rect_filled(
            Rect::from_min_size(
                pos2(rect.left(), rect.bottom() - 4.0),
                vec2(rect.width(), 4.0),
            ),
            CornerRadius::ZERO,
            p.accent,
        );
    });
}

/// Copies the committed UI snapshots into `docs/media` so the README links to
/// stable paths while the images stay generated from the real views.
#[test]
#[ignore = "writes docs/media; run with --ignored to regenerate"]
fn screenshots() {
    let snapshots = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots");
    let dir = media_dir();
    std::fs::create_dir_all(&dir).expect("create docs/media");
    for name in [
        "new_connection",
        "new_connection_light",
        "settings",
        "recent",
        "address_book",
        "incoming_request",
    ] {
        let from = snapshots.join(format!("{name}.png"));
        let to = dir.join(format!("screenshot-{name}.png"));
        assert!(from.exists(), "missing snapshot {}", from.display());
        std::fs::copy(&from, &to).expect("copy snapshot");
        println!("wrote {}", to.display());
    }
}
