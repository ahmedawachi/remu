//! The Remu visual language, as egui styling.
//!
//! These tokens are a direct port of the Svelte predecessor's `app.css`, kept
//! deliberately faithful so the two products look like the same product. The
//! palette is defined once here and referenced by name everywhere else: a view
//! that hardcodes a `Color32` is a bug, because it will not follow the theme.
//!
//! egui is an immediate-mode toolkit, which by default looks like a debug
//! panel. Most of what follows is the deliberate work of making it not: tighter
//! rounding, a real spacing scale, flatter widget fills, and no visible frame
//! on anything that is not actually interactive.

use egui::{Color32, CornerRadius, FontFamily, FontId, Stroke, TextStyle, Visuals};

/// One palette, resolved for the active theme.
///
/// Both themes define every token, so a view never needs to branch on which is
/// active — it asks the palette for `surface` and gets the right surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    /// Window background, behind everything.
    pub bg_base: Color32,
    /// Cards, panels, the sidebar.
    pub bg_surface: Color32,
    /// Inputs and inactive widget fills.
    pub bg_raised: Color32,
    /// Pressed and selected fills.
    pub bg_sunken: Color32,
    pub bg_hover: Color32,

    pub border: Color32,
    pub border_strong: Color32,

    /// Primary text.
    pub fg_primary: Color32,
    /// Secondary text: labels, descriptions.
    pub fg_secondary: Color32,
    /// Tertiary text: hints, timestamps, disabled.
    pub fg_muted: Color32,

    pub accent: Color32,
    pub accent_hover: Color32,
    /// Accent at low opacity, for selected-row and badge backgrounds.
    pub accent_soft: Color32,
    /// Text drawn on top of `accent`.
    pub on_accent: Color32,

    pub success: Color32,
    pub warning: Color32,
    pub danger: Color32,

    /// The video stage behind a remote frame — near-black so letterboxing
    /// reads as absence of picture rather than as a grey panel.
    pub stage: Color32,
}

impl Palette {
    pub const DARK: Self = Self {
        bg_base: Color32::from_rgb(0x0c, 0x0e, 0x12),
        bg_surface: Color32::from_rgb(0x11, 0x14, 0x1a),
        bg_raised: Color32::from_rgb(0x16, 0x1a, 0x22),
        bg_sunken: Color32::from_rgb(0x1d, 0x22, 0x2c),
        bg_hover: Color32::from_rgb(0x23, 0x2a, 0x36),
        border: Color32::from_rgb(0x23, 0x29, 0x36),
        border_strong: Color32::from_rgb(0x2e, 0x36, 0x45),
        fg_primary: Color32::from_rgb(0xe7, 0xec, 0xf3),
        fg_secondary: Color32::from_rgb(0xaa, 0xb2, 0xc1),
        fg_muted: Color32::from_rgb(0x6e, 0x77, 0x87),
        accent: Color32::from_rgb(0x2f, 0x7b, 0xff),
        accent_hover: Color32::from_rgb(0x3f, 0x88, 0xff),
        accent_soft: Color32::from_rgba_premultiplied(0x0a, 0x1c, 0x3a, 0xff),
        on_accent: Color32::WHITE,
        success: Color32::from_rgb(0x34, 0xc8, 0x78),
        warning: Color32::from_rgb(0xf4, 0xb7, 0x40),
        danger: Color32::from_rgb(0xef, 0x4f, 0x4f),
        stage: Color32::from_rgb(0x04, 0x07, 0x0b),
    };

    /// The light theme is not in the predecessor, which was dark-only. It is
    /// derived from the same hues so the two read as one family rather than as
    /// an inverted copy.
    pub const LIGHT: Self = Self {
        bg_base: Color32::from_rgb(0xf6, 0xf7, 0xf9),
        bg_surface: Color32::from_rgb(0xff, 0xff, 0xff),
        bg_raised: Color32::from_rgb(0xf0, 0xf2, 0xf5),
        bg_sunken: Color32::from_rgb(0xe4, 0xe8, 0xee),
        bg_hover: Color32::from_rgb(0xe9, 0xed, 0xf3),
        border: Color32::from_rgb(0xdd, 0xe2, 0xea),
        border_strong: Color32::from_rgb(0xc4, 0xcc, 0xd8),
        fg_primary: Color32::from_rgb(0x11, 0x17, 0x20),
        fg_secondary: Color32::from_rgb(0x48, 0x52, 0x63),
        fg_muted: Color32::from_rgb(0x7b, 0x85, 0x96),
        accent: Color32::from_rgb(0x1a, 0x64, 0xe6),
        accent_hover: Color32::from_rgb(0x15, 0x57, 0xcc),
        accent_soft: Color32::from_rgb(0xe4, 0xed, 0xfd),
        on_accent: Color32::WHITE,
        success: Color32::from_rgb(0x15, 0x9b, 0x55),
        warning: Color32::from_rgb(0xb5, 0x7d, 0x0b),
        danger: Color32::from_rgb(0xd0, 0x30, 0x30),
        // Still near-black: a remote screen is being letterboxed here whatever
        // the surrounding UI is doing.
        stage: Color32::from_rgb(0x0b, 0x0e, 0x13),
    };

    pub fn for_theme(theme: remu_proto::Theme, system_is_dark: bool) -> Self {
        match theme {
            remu_proto::Theme::Dark => Self::DARK,
            remu_proto::Theme::Light => Self::LIGHT,
            remu_proto::Theme::System => {
                if system_is_dark {
                    Self::DARK
                } else {
                    Self::LIGHT
                }
            }
        }
    }

    pub fn is_dark(&self) -> bool {
        // Comparing against the known dark base is exact and cheap; there are
        // only ever two palettes.
        self.bg_base == Self::DARK.bg_base
    }
}

/// Spacing scale, in points. Layout code uses these rather than literals so
/// density can be retuned in one place.
pub mod space {
    pub const XS: f32 = 4.0;
    pub const SM: f32 = 8.0;
    pub const MD: f32 = 12.0;
    pub const LG: f32 = 18.0;
    pub const XL: f32 = 26.0;
}

/// Corner radii, matching the predecessor's `--radius-*` tokens.
pub mod radius {
    pub const SM: u8 = 4;
    pub const MD: u8 = 8;
    pub const LG: u8 = 14;
    pub const PILL: u8 = 255;
}

/// Named text styles beyond egui's defaults.
pub mod text {
    /// Desk IDs and anything the user reads digit by digit.
    pub const MONO_LARGE: f32 = 34.0;
    pub const MONO_BODY: f32 = 13.0;
    /// Small uppercase section headings.
    pub const SECTION: f32 = 11.0;
}

/// Applies the palette to an egui context.
///
/// egui 0.35 keeps a separate `Style` per theme and picks between them, so the
/// style is registered against the matching theme *and* the preference is
/// pinned. We resolve "system" ourselves in [`Palette::for_theme`] rather than
/// letting egui do it, so that one concrete palette drives both egui and our
/// own custom-painted widgets — otherwise the two can disagree mid-frame.
pub fn apply(ctx: &egui::Context, palette: Palette) {
    let egui_theme = if palette.is_dark() {
        egui::Theme::Dark
    } else {
        egui::Theme::Light
    };
    ctx.set_theme(egui_theme);

    let mut style = (*ctx.style_of(egui_theme)).clone();

    style.text_styles = [
        (
            TextStyle::Heading,
            FontId::new(19.0, FontFamily::Proportional),
        ),
        (TextStyle::Body, FontId::new(14.0, FontFamily::Proportional)),
        (
            TextStyle::Button,
            FontId::new(14.0, FontFamily::Proportional),
        ),
        (
            TextStyle::Small,
            FontId::new(11.5, FontFamily::Proportional),
        ),
        (
            TextStyle::Monospace,
            FontId::new(13.0, FontFamily::Monospace),
        ),
    ]
    .into();

    let mut visuals = if palette.is_dark() {
        Visuals::dark()
    } else {
        Visuals::light()
    };

    visuals.override_text_color = Some(palette.fg_primary);
    visuals.panel_fill = palette.bg_base;
    visuals.window_fill = palette.bg_surface;
    visuals.extreme_bg_color = palette.bg_raised;
    visuals.faint_bg_color = palette.bg_raised;
    visuals.hyperlink_color = palette.accent;
    visuals.warn_fg_color = palette.warning;
    visuals.error_fg_color = palette.danger;
    visuals.selection.bg_fill = palette.accent_soft;
    visuals.selection.stroke = Stroke::new(1.0, palette.accent);

    let r = CornerRadius::same(radius::SM);

    // Non-interactive: labels and panel bodies. No visible frame at all —
    // egui's default outlines every widget, which is what makes an unstyled
    // egui app read as a debug tool.
    visuals.widgets.noninteractive.bg_fill = palette.bg_surface;
    visuals.widgets.noninteractive.weak_bg_fill = palette.bg_surface;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, palette.border);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, palette.fg_secondary);
    visuals.widgets.noninteractive.corner_radius = r;

    visuals.widgets.inactive.bg_fill = palette.bg_sunken;
    visuals.widgets.inactive.weak_bg_fill = palette.bg_sunken;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, palette.border);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, palette.fg_primary);
    visuals.widgets.inactive.corner_radius = r;

    visuals.widgets.hovered.bg_fill = palette.bg_hover;
    visuals.widgets.hovered.weak_bg_fill = palette.bg_hover;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, palette.border_strong);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, palette.fg_primary);
    visuals.widgets.hovered.corner_radius = r;

    visuals.widgets.active.bg_fill = palette.accent;
    visuals.widgets.active.weak_bg_fill = palette.accent;
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, palette.accent);
    visuals.widgets.active.fg_stroke = Stroke::new(1.0, palette.on_accent);
    visuals.widgets.active.corner_radius = r;

    visuals.widgets.open.bg_fill = palette.bg_raised;
    visuals.widgets.open.weak_bg_fill = palette.bg_raised;
    visuals.widgets.open.bg_stroke = Stroke::new(1.0, palette.border_strong);
    visuals.widgets.open.fg_stroke = Stroke::new(1.0, palette.fg_primary);
    visuals.widgets.open.corner_radius = r;

    visuals.window_corner_radius = CornerRadius::same(radius::MD);
    visuals.menu_corner_radius = CornerRadius::same(radius::MD);
    visuals.window_stroke = Stroke::new(1.0, palette.border);
    visuals.popup_shadow.color = Color32::from_black_alpha(140);
    visuals.window_shadow.color = Color32::from_black_alpha(140);

    style.visuals = visuals;
    // Wrap by default. egui's default is to let a long label grow its parent,
    // which in a two-column settings page pushed the right-hand card straight
    // off the edge of the window and clipped every hint in it.
    style.wrap_mode = Some(egui::TextWrapMode::Wrap);
    style.spacing.item_spacing = egui::vec2(space::SM, space::SM);
    style.spacing.button_padding = egui::vec2(12.0, 6.0);
    style.spacing.window_margin = egui::Margin::same(space::LG as i8);
    style.spacing.menu_margin = egui::Margin::same(space::SM as i8);
    style.spacing.indent = space::LG;
    style.spacing.scroll.bar_width = 9.0;
    style.spacing.interact_size.y = 28.0;

    ctx.set_style_of(egui_theme, style);
}

/// A dot whose colour encodes connection health, as used in status pills.
pub fn status_color(palette: &Palette, status: StatusTone) -> Color32 {
    match status {
        StatusTone::Good => palette.success,
        StatusTone::Working => palette.accent,
        StatusTone::Warn => palette.warning,
        StatusTone::Bad => palette.danger,
        StatusTone::Idle => palette.fg_muted,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusTone {
    Good,
    Working,
    Warn,
    Bad,
    Idle,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_palettes_identify_their_own_mode() {
        assert!(Palette::DARK.is_dark());
        assert!(!Palette::LIGHT.is_dark());
    }

    #[test]
    fn system_theme_follows_the_os_preference() {
        assert_eq!(
            Palette::for_theme(remu_proto::Theme::System, true),
            Palette::DARK
        );
        assert_eq!(
            Palette::for_theme(remu_proto::Theme::System, false),
            Palette::LIGHT
        );
    }

    #[test]
    fn explicit_theme_ignores_the_os_preference() {
        assert_eq!(
            Palette::for_theme(remu_proto::Theme::Dark, false),
            Palette::DARK
        );
        assert_eq!(
            Palette::for_theme(remu_proto::Theme::Light, true),
            Palette::LIGHT
        );
    }

    #[test]
    fn dark_palette_matches_the_predecessor_css_tokens() {
        // These exact values come from the Svelte app's app.css. Drifting from
        // them silently is how two products stop looking like one product.
        assert_eq!(Palette::DARK.bg_base, Color32::from_rgb(0x0c, 0x0e, 0x12));
        assert_eq!(Palette::DARK.accent, Color32::from_rgb(0x2f, 0x7b, 0xff));
        assert_eq!(Palette::DARK.danger, Color32::from_rgb(0xef, 0x4f, 0x4f));
        assert_eq!(Palette::DARK.success, Color32::from_rgb(0x34, 0xc8, 0x78));
    }

    #[test]
    fn text_is_legible_against_its_background_in_both_themes() {
        // Guards against a future palette edit that quietly makes secondary
        // text unreadable. Uses WCAG relative luminance contrast.
        for (name, p) in [("dark", Palette::DARK), ("light", Palette::LIGHT)] {
            assert!(
                contrast(p.fg_primary, p.bg_base) >= 7.0,
                "{name}: primary text contrast too low"
            );
            assert!(
                contrast(p.fg_secondary, p.bg_surface) >= 4.5,
                "{name}: secondary text contrast too low"
            );
            assert!(
                contrast(p.on_accent, p.accent) >= 3.0,
                "{name}: text on accent is unreadable"
            );
        }
    }

    fn contrast(a: Color32, b: Color32) -> f32 {
        let (la, lb) = (luminance(a), luminance(b));
        let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
        (hi + 0.05) / (lo + 0.05)
    }

    fn luminance(c: Color32) -> f32 {
        fn channel(v: u8) -> f32 {
            let v = v as f32 / 255.0;
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        }
        0.2126 * channel(c.r()) + 0.7152 * channel(c.g()) + 0.0722 * channel(c.b())
    }
}
