//! The reusable pieces of the Remu visual language, and the pointer maths the
//! session view depends on.
//!
//! The buttons are custom-painted rather than styled `egui::Button`s because
//! the predecessor's CSS gives each variant three distinct fills (rest, hover,
//! press) and `Button::fill` only accepts one. Painting them here also keeps
//! every colour in one place: nothing below reaches for a literal `Color32`,
//! it all comes from the [`Palette`].

use egui::{
    vec2, Align2, Color32, CornerRadius, FontId, Margin, Pos2, Rect, Response, Sense, Stroke,
    StrokeKind, Ui, Vec2,
};
use remu_proto::{InputEvent, KeyCode, KeyModifiers, MouseButton, PeerId};

use crate::theme::{radius, space, status_color, text, Palette, StatusTone};

/// A wheel event this large in one frame is a runaway trackpad, not a user.
/// Matches the predecessor's cap so the host never jumps a whole document.
const MAX_WHEEL_NOTCHES: f32 = 5.0;

/// Points of scroll egui reports per notch of a physical wheel.
const POINTS_PER_NOTCH: f32 = 30.0;

/// The fills one button variant uses at rest, hovered and pressed.
#[derive(Debug, Clone, Copy)]
struct ButtonSkin {
    fill: Color32,
    hover: Color32,
    fg: Color32,
    border: Color32,
}

/// Paints one of the predecessor's `.btn` variants.
///
/// Disabled buttons are dimmed rather than removed, because the predecessor
/// uses them to advertise features that exist but are not reachable yet (file
/// transfer before a session is up), and hiding them would lose that.
fn button(ui: &mut Ui, label: &str, skin: ButtonSkin, enabled: bool) -> Response {
    let font = egui::TextStyle::Button.resolve(ui.style());
    // PLACEHOLDER defers the colour to paint time, so the galley can be laid
    // out (and thus measured) before we know whether the button is hovered.
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font, Color32::PLACEHOLDER);

    let padding = ui.spacing().button_padding;
    let desired = Vec2::new(
        galley.size().x + 2.0 * padding.x,
        (galley.size().y + 2.0 * padding.y).max(ui.spacing().interact_size.y),
    );
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(desired, sense);

    if ui.is_rect_visible(rect) {
        let (fill, fg, border) = if !enabled {
            // The predecessor's `opacity: 0.5`, expressed as alpha so the
            // button still sits on whatever surface is behind it.
            (
                skin.fill.gamma_multiply(0.45),
                skin.fg.gamma_multiply(0.45),
                skin.border.gamma_multiply(0.45),
            )
        } else if response.is_pointer_button_down_on() || response.hovered() {
            (skin.hover, skin.fg, skin.border)
        } else {
            (skin.fill, skin.fg, skin.border)
        };

        ui.painter().rect(
            rect,
            CornerRadius::same(radius::SM),
            fill,
            Stroke::new(1.0, border),
            StrokeKind::Inside,
        );
        let pos = rect.center() - galley.size() / 2.0;
        ui.painter().galley(pos, galley, fg);
    }
    response
}

/// The affirmative action on a screen. At most one per screen.
pub fn primary_button(ui: &mut Ui, palette: &Palette, label: &str, enabled: bool) -> Response {
    button(
        ui,
        label,
        ButtonSkin {
            fill: palette.accent,
            hover: palette.accent_hover,
            fg: palette.on_accent,
            border: palette.accent,
        },
        enabled,
    )
}

/// Ending a session, removing a contact — anything the user cannot undo.
pub fn danger_button(ui: &mut Ui, palette: &Palette, label: &str, enabled: bool) -> Response {
    button(
        ui,
        label,
        ButtonSkin {
            fill: palette.danger,
            hover: palette.danger.gamma_multiply(1.15),
            fg: Color32::WHITE,
            border: palette.danger,
        },
        enabled,
    )
}

/// The default button: visible, but not competing with the primary one.
pub fn secondary_button(ui: &mut Ui, palette: &Palette, label: &str, enabled: bool) -> Response {
    button(
        ui,
        label,
        ButtonSkin {
            fill: palette.bg_sunken,
            hover: palette.bg_hover,
            fg: palette.fg_primary,
            border: palette.border,
        },
        enabled,
    )
}

/// Frameless until hovered. Used in dense toolbars where a row of outlined
/// buttons would read as a fence.
pub fn ghost_button(
    ui: &mut Ui,
    palette: &Palette,
    label: &str,
    active: bool,
    enabled: bool,
) -> Response {
    let skin = if active {
        ButtonSkin {
            fill: palette.accent_soft,
            hover: palette.accent_soft,
            fg: palette.accent,
            border: palette.accent_soft,
        }
    } else {
        ButtonSkin {
            fill: Color32::TRANSPARENT,
            hover: palette.bg_hover,
            fg: palette.fg_secondary,
            border: Color32::TRANSPARENT,
        }
    };
    button(ui, label, skin, enabled)
}

/// The predecessor's `.card`: a bordered surface with generous padding.
pub fn card(palette: &Palette) -> egui::Frame {
    egui::Frame::new()
        .fill(palette.bg_surface)
        .stroke(Stroke::new(1.0, palette.border))
        .corner_radius(CornerRadius::same(radius::MD))
        .inner_margin(Margin::same(space::LG as i8))
}

/// The panel variant: the same surface, tighter, for side panels and rows.
pub fn subtle_card(palette: &Palette) -> egui::Frame {
    egui::Frame::new()
        .fill(palette.bg_raised)
        .stroke(Stroke::new(1.0, palette.border))
        .corner_radius(CornerRadius::same(radius::SM))
        .inner_margin(Margin::same(space::SM as i8))
}

/// The predecessor's `.section-title`: small, uppercase, muted.
pub fn section_title(ui: &mut Ui, palette: &Palette, label: &str) {
    ui.label(
        egui::RichText::new(label.to_uppercase())
            .size(text::SECTION)
            .strong()
            .color(palette.fg_muted),
    );
    ui.add_space(space::XS);
}

pub fn hint(ui: &mut Ui, palette: &Palette, body: &str) {
    ui.label(egui::RichText::new(body).size(12.0).color(palette.fg_muted));
}

/// A status pill: a coloured dot and a word, in a rounded outline.
pub fn pill(ui: &mut Ui, palette: &Palette, tone: StatusTone, label: &str) -> Response {
    egui::Frame::new()
        .fill(palette.bg_raised)
        .stroke(Stroke::new(1.0, palette.border))
        .corner_radius(CornerRadius::same(radius::PILL))
        .inner_margin(Margin::symmetric(10, 3))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                dot(ui, status_color(palette, tone));
                ui.label(
                    egui::RichText::new(label)
                        .size(12.0)
                        .color(palette.fg_secondary),
                );
            });
        })
        .response
}

/// The 8px coloured dot the pills and toasts share.
pub fn dot(ui: &mut Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(8.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
}

/// The hero desk ID: monospace, grouped in threes, read out over the phone.
pub fn desk_id(ui: &mut Ui, palette: &Palette, id: Option<PeerId>, size: f32) {
    let label = id.map_or_else(|| "— — —".to_owned(), PeerId::grouped);
    ui.label(
        egui::RichText::new(label)
            .font(FontId::monospace(size))
            .color(palette.fg_primary),
    );
}

/// Groups a run of digits in threes, the way the predecessor's `formatPeerId`
/// does, without requiring the string to be a valid ID yet.
pub fn grouped_digits(raw: &str) -> String {
    let digits: String = raw.chars().filter(char::is_ascii_digit).collect();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

/// Parses whatever the user typed into the connect box.
///
/// Separators are noise, so `123-456-789`, `123 456 789` and `123456789` are
/// the same ID. Anything that is not a whole nine-digit ID is `None`, which is
/// what greys the Connect button out before the user can fail.
pub fn parse_desk_id(raw: &str) -> Option<PeerId> {
    PeerId::parse_lenient(raw).ok()
}

pub fn format_bytes(n: u64) -> String {
    const KB: f64 = 1024.0;
    let n = n as f64;
    if n < KB {
        format!("{n:.0} B")
    } else if n < KB * KB {
        format!("{:.1} KB", n / KB)
    } else if n < KB * KB * KB {
        format!("{:.1} MB", n / (KB * KB))
    } else {
        format!("{:.2} GB", n / (KB * KB * KB))
    }
}

/// "just now" / "12m ago" / "3h ago" / "5d ago", as the Recent list shows it.
///
/// A timestamp in the future (a clock that moved backwards, or a file synced
/// from another machine) reads as "just now" rather than a negative age.
pub fn format_ago(now_ms: u64, then_ms: u64) -> String {
    if then_ms == 0 {
        return "never".to_owned();
    }
    let minutes = now_ms.saturating_sub(then_ms) / 60_000;
    if minutes < 1 {
        return "just now".to_owned();
    }
    if minutes < 60 {
        return format!("{minutes}m ago");
    }
    let hours = minutes / 60;
    if hours < 24 {
        return format!("{hours}h ago");
    }
    format!("{}d ago", hours / 24)
}

// --- the letterbox maths -------------------------------------------------

/// Where the remote picture actually lands inside the stage.
///
/// The frame is drawn `object-fit: contain`, so it is scaled to the largest
/// size that fits and centred, leaving bars on two sides. Every pointer
/// coordinate has to be normalized against *this* rect, not the stage: the
/// predecessor's `normalizeFromEvent` learned that the hard way, and against
/// the widget rect every remote click lands offset by half a bar.
///
/// A stage or picture with no area has no meaningful fit, so the stage is
/// returned unchanged rather than producing a NaN scale.
pub fn picture_rect(stage: Rect, picture: [u32; 2]) -> Rect {
    let (pw, ph) = (picture[0] as f32, picture[1] as f32);
    let (sw, sh) = (stage.width(), stage.height());
    if pw <= 0.0 || ph <= 0.0 || sw <= 0.0 || sh <= 0.0 {
        return stage;
    }
    let scale = (sw / pw).min(sh / ph);
    Rect::from_center_size(stage.center(), vec2(pw * scale, ph * scale))
}

/// Maps a screen position to `0.0..=1.0` of the displayed picture.
///
/// Points in the letterbox bars clamp to the nearest edge, so dragging off the
/// picture still drives the host's cursor to the edge instead of stopping.
pub fn normalize_in_picture(picture: Rect, pos: Pos2) -> (f32, f32) {
    let (w, h) = (picture.width(), picture.height());
    if w <= 0.0 || h <= 0.0 {
        return (0.0, 0.0);
    }
    (
        ((pos.x - picture.left()) / w).clamp(0.0, 1.0),
        ((pos.y - picture.top()) / h).clamp(0.0, 1.0),
    )
}

/// What one frame of the session stage knows about its own input.
///
/// Bundled into a struct rather than five positional arguments because the
/// pointer gating is easy to get subtly wrong: `owns_pointer` in particular is
/// not the same question as "is the position inside `stage`".
#[derive(Debug, Clone, Copy)]
pub struct StageInput {
    /// Where the stage widget sits this frame.
    pub stage: Rect,
    /// Where the remote picture landed inside the stage.
    pub picture: Rect,
    /// The pointer's hover position, if it has one.
    pub pointer: Option<Pos2>,
    /// True only when the stage widget itself owns the pointer this frame.
    ///
    /// A rectangle test is not enough: the toolbar's popups, tooltips and any
    /// other `Area` are drawn *over* the stage, and a click on one of those is
    /// handled locally. Forwarding it as well pressed the remote desktop
    /// underneath the thing the user actually clicked.
    pub owns_pointer: bool,
    /// False while the keystrokes belong to a local text field or panel.
    pub keyboard: bool,
}

/// The little bit of state that input forwarding cannot do without.
///
/// A press and its release arrive in different frames, so a stateless
/// translator could not tell a release it *must* send (the press went out, the
/// button is down on the host) from one it must drop (the press never went
/// out). That is how a drag that ended outside the picture used to leave the
/// button held down on the host, with no way to clear it from the controller.
///
/// The same reasoning applies to keys, and to the moment keyboard forwarding is
/// switched off: whatever we pressed on the host is our responsibility to lift.
#[derive(Debug, Clone, Default)]
pub struct InputForwarder {
    /// Buttons whose `MouseDown` we forwarded and whose `MouseUp` we still owe.
    held_buttons: Vec<MouseButton>,
    /// Keys whose `KeyDown` we forwarded and whose `KeyUp` we still owe.
    held_keys: Vec<KeyCode>,
    /// Whether the previous frame was forwarding the keyboard.
    keyboard_was_live: bool,
    /// The last normalized position we sent, so a pointer that vanishes can be
    /// released where it was last seen instead of yanking the host's cursor to
    /// the top-left corner.
    last_pos: (f32, f32),
}

impl InputForwarder {
    /// Translates one frame of egui input into remote input events.
    ///
    /// - Pointer events the stage does not own are dropped; inside the stage
    ///   they are normalized against `picture`.
    /// - Consecutive moves collapse to the last one, so a frame that delivered
    ///   twenty motion samples sends one message, but a move that preceded a
    ///   click still arrives before it.
    /// - Key repeats are dropped: the host's own OS repeats a held key, and
    ///   forwarding ours too would double every repeat.
    /// - Losing window focus always emits [`InputEvent::ReleaseAll`], even when
    ///   `keyboard` is false, because a modifier stuck down on the host is the
    ///   one failure the user cannot clear from here.
    pub fn translate(&mut self, frame: StageInput, events: &[egui::Event]) -> Vec<InputEvent> {
        let StageInput {
            stage,
            picture,
            pointer,
            owns_pointer,
            keyboard,
        } = frame;
        let mut out = Vec::new();

        // Suspending keyboard forwarding (opening the chat panel, focusing a
        // local text field) has to lift what the host is holding for us first:
        // the KeyUp for a modifier held across the transition is never
        // forwarded, so Ctrl stayed down on the host until the session ended.
        if self.keyboard_was_live && !keyboard {
            self.release_all(&mut out);
        }
        self.keyboard_was_live = keyboard;

        let mut pending_move: Option<Pos2> = None;
        // Index of the `Event::Text` that the key event before it already
        // represents. See the `Event::Key` arm.
        let mut text_spoken_for: Option<usize> = None;

        macro_rules! flush_move {
            () => {
                if let Some(pos) = pending_move.take() {
                    let (x, y) = normalize_in_picture(picture, pos);
                    self.last_pos = (x, y);
                    out.push(InputEvent::MouseMove { x, y });
                }
            };
        }

        for (index, event) in events.iter().enumerate() {
            match event {
                egui::Event::PointerMoved(pos) => {
                    // A drag that wandered off the picture still steers the
                    // host's cursor to the edge (normalization clamps), so a
                    // move counts while a button we forwarded is down.
                    if (owns_pointer && stage.contains(*pos)) || !self.held_buttons.is_empty() {
                        pending_move = Some(*pos);
                    }
                }
                egui::Event::PointerButton {
                    pos,
                    button,
                    pressed,
                    ..
                } => {
                    let Some(button) = mouse_button_from_egui(*button) else {
                        continue;
                    };
                    if *pressed {
                        // A press the stage does not own belongs to the
                        // toolbar, a side panel or a popup drawn over the video.
                        if !owns_pointer || !stage.contains(*pos) {
                            continue;
                        }
                        if !self.held_buttons.contains(&button) {
                            self.held_buttons.push(button);
                        }
                    } else if let Some(i) = self.held_buttons.iter().position(|b| *b == button) {
                        // A release is forwarded wherever it lands: the press
                        // went out, so the host is holding this button and only
                        // we can lift it.
                        self.held_buttons.swap_remove(i);
                    } else {
                        continue;
                    }
                    flush_move!();
                    let (x, y) = normalize_in_picture(picture, *pos);
                    self.last_pos = (x, y);
                    out.push(if *pressed {
                        InputEvent::MouseDown { button, x, y }
                    } else {
                        InputEvent::MouseUp { button, x, y }
                    });
                }
                egui::Event::PointerGone => {
                    // Once the pointer has left the window the matching release
                    // may be delivered to whatever is under it instead of to
                    // us, so anything still held comes up here.
                    flush_move!();
                    let (x, y) = self.last_pos;
                    for button in std::mem::take(&mut self.held_buttons) {
                        out.push(InputEvent::MouseUp { button, x, y });
                    }
                }
                egui::Event::MouseWheel { unit, delta, .. }
                    if owns_pointer && pointer.is_some_and(|p| stage.contains(p)) =>
                {
                    flush_move!();
                    let (dx, dy) = wheel_notches(*unit, *delta);
                    if dx != 0.0 || dy != 0.0 {
                        out.push(InputEvent::Wheel { dx, dy });
                    }
                }
                egui::Event::Key {
                    key,
                    physical_key,
                    pressed,
                    repeat,
                    modifiers,
                } if keyboard => {
                    // The physical key is what the host must press: the
                    // controller's keymap has already been applied to `key`, and
                    // applying the host's on top would translate twice on a
                    // non-US layout.
                    let Some(code) = keycode_from_egui(physical_key.unwrap_or(*key)) else {
                        continue;
                    };
                    if *repeat {
                        // The key is still down on the host, so the host's own
                        // OS auto-repeat is already producing characters. The
                        // repeat must therefore send nothing — but it still
                        // arrives with an `Event::Text`, and letting that
                        // through typed every held character twice. Claim it,
                        // but only when we actually sent the KeyDown: if the
                        // key travelled as text in the first place, the text is
                        // the only representation there is.
                        if self.held_keys.contains(&code) {
                            if let Some(egui::Event::Text(text)) = events.get(index + 1) {
                                if key_types_text(code, text) {
                                    text_spoken_for = Some(index + 1);
                                }
                            }
                        }
                        continue;
                    }
                    if *pressed {
                        // egui-winit pushes BOTH an `Event::Key` and an
                        // `Event::Text` for one keystroke
                        // (egui-winit-0.35.0/src/lib.rs:1016 and :1045).
                        // Forwarding both typed every character twice on the
                        // host, so exactly one of the two representations wins:
                        // the key position when it is the thing that produces
                        // the text, and the text when it is not (a dead-key
                        // composition or a layout whose legend this position
                        // cannot express).
                        match events.get(index + 1) {
                            Some(egui::Event::Text(text)) if key_types_text(code, text) => {
                                text_spoken_for = Some(index + 1);
                            }
                            Some(egui::Event::Text(_)) => continue,
                            _ => {}
                        }
                        if !self.held_keys.contains(&code) {
                            self.held_keys.push(code);
                        }
                    } else if let Some(i) = self.held_keys.iter().position(|k| *k == code) {
                        self.held_keys.swap_remove(i);
                    } else {
                        // We never sent this key down — it was typed as text, or
                        // it was already released by a ReleaseAll. Sending the
                        // release anyway would lift a key on the host that the
                        // user is still holding on another layer.
                        continue;
                    }
                    flush_move!();
                    let modifiers = modifiers_from_egui(*modifiers);
                    out.push(if *pressed {
                        InputEvent::KeyDown { code, modifiers }
                    } else {
                        InputEvent::KeyUp { code, modifiers }
                    });
                }
                egui::Event::Text(value)
                    if keyboard && !value.is_empty() && text_spoken_for != Some(index) =>
                {
                    flush_move!();
                    out.push(InputEvent::Text {
                        value: value.clone(),
                    });
                }
                // An IME commit has no key event at all — it is the only
                // representation the composed characters have.
                egui::Event::Ime(egui::ImeEvent::Commit(value))
                    if keyboard && !value.is_empty() =>
                {
                    flush_move!();
                    out.push(InputEvent::Text {
                        value: value.clone(),
                    });
                }
                egui::Event::Copy if keyboard => {
                    flush_move!();
                    self.clipboard_chord(KeyCode::C, &mut out);
                }
                egui::Event::Cut if keyboard => {
                    flush_move!();
                    self.clipboard_chord(KeyCode::X, &mut out);
                }
                // The payload is the *controller's* clipboard; pasting it into
                // the host would be clipboard sharing, which is a separate,
                // separately permissioned feature. All the host needs is the
                // chord, which pastes whatever the host itself has copied.
                egui::Event::Paste(_) if keyboard => {
                    flush_move!();
                    self.clipboard_chord(KeyCode::V, &mut out);
                }
                egui::Event::WindowFocused(false) => {
                    flush_move!();
                    self.release_all(&mut out);
                }
                _ => {}
            }
        }
        flush_move!();
        out
    }

    /// Tells the host to drop everything, and forgets what we were holding.
    ///
    /// Our bookkeeping has to follow the host's: after this the host holds no
    /// key and no button, so a later release of one of them is ours to drop.
    fn release_all(&mut self, out: &mut Vec<InputEvent>) {
        self.held_buttons.clear();
        self.held_keys.clear();
        out.push(InputEvent::ReleaseAll);
    }

    /// The modifier flags implied by the modifier keys we pressed on the host.
    fn held_modifiers(&self) -> KeyModifiers {
        let held =
            |a: KeyCode, b: KeyCode| self.held_keys.contains(&a) || self.held_keys.contains(&b);
        KeyModifiers {
            shift: held(KeyCode::ShiftLeft, KeyCode::ShiftRight),
            ctrl: held(KeyCode::ControlLeft, KeyCode::ControlRight),
            alt: held(KeyCode::AltLeft, KeyCode::AltRight),
            meta: held(KeyCode::MetaLeft, KeyCode::MetaRight),
        }
    }

    /// Rebuilds the copy / cut / paste chord that egui swallowed.
    ///
    /// egui-winit turns ⌘/Ctrl + C, X and V into `Event::Copy`, `Event::Cut`
    /// and `Event::Paste` and returns *before* pushing any `Event::Key`
    /// (egui-winit-0.35.0/src/lib.rs:999-1013). The desk therefore never saw
    /// the keystroke at all, and the most-used remote shortcut there is did
    /// nothing.
    ///
    /// The modifier the user is holding has already been forwarded as its own
    /// physical key, so when it is still down the faithful translation is to
    /// send only the letter and let that modifier do its job — ⌘+C on a macOS
    /// host, Ctrl+C on a Windows or Linux one, whichever the user actually
    /// pressed.
    ///
    /// When no command modifier is held on the host there is nothing for the
    /// letter to combine with. The session view is never told which OS the host
    /// runs, so the fallback is the platform-neutral Ctrl form: right on
    /// Windows and Linux, and no worse than nothing on macOS.
    fn clipboard_chord(&mut self, code: KeyCode, out: &mut Vec<InputEvent>) {
        let mut modifiers = self.held_modifiers();
        let synthesize_ctrl = !modifiers.ctrl && !modifiers.meta;
        if synthesize_ctrl {
            modifiers.ctrl = true;
            out.push(InputEvent::KeyDown {
                code: KeyCode::ControlLeft,
                modifiers,
            });
        }
        out.push(InputEvent::KeyDown { code, modifiers });
        out.push(InputEvent::KeyUp { code, modifiers });
        if synthesize_ctrl {
            modifiers.ctrl = false;
            out.push(InputEvent::KeyUp {
                code: KeyCode::ControlLeft,
                modifiers,
            });
        }
    }
}

/// True when pressing `code` on the host is expected to type exactly `text`.
///
/// This is what decides which of egui's two representations of a keystroke is
/// forwarded. It is deliberately conservative: anything it cannot vouch for
/// travels as text, which any host can reproduce, rather than as a key position
/// that would type the wrong character.
fn key_types_text(code: KeyCode, text: &str) -> bool {
    let mut chars = text.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return false;
    };
    us_legend(code).is_some_and(|(plain, shifted)| c == plain || c == shifted)
}

/// What a key's US-QWERTY legend produces, unshifted and shifted.
///
/// `None` for keys with no printable legend — egui filters their text out as
/// control characters, so they never reach [`key_types_text`] with anything to
/// compare against.
fn us_legend(code: KeyCode) -> Option<(char, char)> {
    use KeyCode as C;
    let pair = match code {
        C::A => ('a', 'A'),
        C::B => ('b', 'B'),
        C::C => ('c', 'C'),
        C::D => ('d', 'D'),
        C::E => ('e', 'E'),
        C::F => ('f', 'F'),
        C::G => ('g', 'G'),
        C::H => ('h', 'H'),
        C::I => ('i', 'I'),
        C::J => ('j', 'J'),
        C::K => ('k', 'K'),
        C::L => ('l', 'L'),
        C::M => ('m', 'M'),
        C::N => ('n', 'N'),
        C::O => ('o', 'O'),
        C::P => ('p', 'P'),
        C::Q => ('q', 'Q'),
        C::R => ('r', 'R'),
        C::S => ('s', 'S'),
        C::T => ('t', 'T'),
        C::U => ('u', 'U'),
        C::V => ('v', 'V'),
        C::W => ('w', 'W'),
        C::X => ('x', 'X'),
        C::Y => ('y', 'Y'),
        C::Z => ('z', 'Z'),

        C::Num0 => ('0', ')'),
        C::Num1 => ('1', '!'),
        C::Num2 => ('2', '@'),
        C::Num3 => ('3', '#'),
        C::Num4 => ('4', '$'),
        C::Num5 => ('5', '%'),
        C::Num6 => ('6', '^'),
        C::Num7 => ('7', '&'),
        C::Num8 => ('8', '*'),
        C::Num9 => ('9', '('),

        C::Minus => ('-', '_'),
        C::Equal => ('=', '+'),
        C::BracketLeft => ('[', '{'),
        C::BracketRight => (']', '}'),
        C::Backslash => ('\\', '|'),
        C::Semicolon => (';', ':'),
        C::Quote => ('\'', '"'),
        C::Backquote => ('`', '~'),
        C::Comma => (',', '<'),
        C::Period => ('.', '>'),
        C::Slash => ('/', '?'),
        C::Space => (' ', ' '),

        _ => return None,
    };
    Some(pair)
}

/// Converts egui's scroll delta into wheel notches.
///
/// egui reports the direction the *content* moves, which is the opposite sign
/// to a wheel notch: scrolling the wheel down moves content up. Getting this
/// backwards makes the remote screen scroll the wrong way, which is subtle
/// enough to survive a casual test.
fn wheel_notches(unit: egui::MouseWheelUnit, delta: Vec2) -> (f32, f32) {
    let scale = match unit {
        egui::MouseWheelUnit::Point => 1.0 / POINTS_PER_NOTCH,
        egui::MouseWheelUnit::Line => 1.0,
        egui::MouseWheelUnit::Page => 3.0,
    };
    let notch = |v: f32| {
        if v.is_nan() {
            0.0
        } else {
            (-v * scale).clamp(-MAX_WHEEL_NOTCHES, MAX_WHEEL_NOTCHES)
        }
    };
    (notch(delta.x), notch(delta.y))
}

fn mouse_button_from_egui(button: egui::PointerButton) -> Option<MouseButton> {
    match button {
        egui::PointerButton::Primary => Some(MouseButton::Left),
        egui::PointerButton::Secondary => Some(MouseButton::Right),
        egui::PointerButton::Middle => Some(MouseButton::Middle),
        // The protocol has three buttons; the extras have no remote meaning.
        egui::PointerButton::Extra1 | egui::PointerButton::Extra2 => None,
    }
}

pub fn modifiers_from_egui(m: egui::Modifiers) -> KeyModifiers {
    KeyModifiers {
        shift: m.shift,
        ctrl: m.ctrl,
        alt: m.alt,
        // egui reports ⌘ separately from ctrl and sets `command` for whichever
        // is the platform's shortcut key; only `mac_cmd` is really the Meta key.
        meta: m.mac_cmd,
    }
}

/// Translates an egui key to the protocol's physical keycode.
///
/// Returns `None` for keys the protocol has no position for, and for the ones
/// egui reports as already-shifted characters whose position is ambiguous.
/// Unmapped keys are dropped rather than guessed: a wrong keycode presses the
/// wrong key on someone else's machine.
pub fn keycode_from_egui(key: egui::Key) -> Option<KeyCode> {
    use egui::Key as K;
    Some(match key {
        K::A => KeyCode::A,
        K::B => KeyCode::B,
        K::C => KeyCode::C,
        K::D => KeyCode::D,
        K::E => KeyCode::E,
        K::F => KeyCode::F,
        K::G => KeyCode::G,
        K::H => KeyCode::H,
        K::I => KeyCode::I,
        K::J => KeyCode::J,
        K::K => KeyCode::K,
        K::L => KeyCode::L,
        K::M => KeyCode::M,
        K::N => KeyCode::N,
        K::O => KeyCode::O,
        K::P => KeyCode::P,
        K::Q => KeyCode::Q,
        K::R => KeyCode::R,
        K::S => KeyCode::S,
        K::T => KeyCode::T,
        K::U => KeyCode::U,
        K::V => KeyCode::V,
        K::W => KeyCode::W,
        K::X => KeyCode::X,
        K::Y => KeyCode::Y,
        K::Z => KeyCode::Z,

        K::Num0 => KeyCode::Num0,
        K::Num1 => KeyCode::Num1,
        K::Num2 => KeyCode::Num2,
        K::Num3 => KeyCode::Num3,
        K::Num4 => KeyCode::Num4,
        K::Num5 => KeyCode::Num5,
        K::Num6 => KeyCode::Num6,
        K::Num7 => KeyCode::Num7,
        K::Num8 => KeyCode::Num8,
        K::Num9 => KeyCode::Num9,

        K::F1 => KeyCode::F1,
        K::F2 => KeyCode::F2,
        K::F3 => KeyCode::F3,
        K::F4 => KeyCode::F4,
        K::F5 => KeyCode::F5,
        K::F6 => KeyCode::F6,
        K::F7 => KeyCode::F7,
        K::F8 => KeyCode::F8,
        K::F9 => KeyCode::F9,
        K::F10 => KeyCode::F10,
        K::F11 => KeyCode::F11,
        K::F12 => KeyCode::F12,
        K::F13 => KeyCode::F13,
        K::F14 => KeyCode::F14,
        K::F15 => KeyCode::F15,
        K::F16 => KeyCode::F16,
        K::F17 => KeyCode::F17,
        K::F18 => KeyCode::F18,
        K::F19 => KeyCode::F19,
        K::F20 => KeyCode::F20,
        K::F21 => KeyCode::F21,
        K::F22 => KeyCode::F22,
        K::F23 => KeyCode::F23,
        K::F24 => KeyCode::F24,

        K::ArrowUp => KeyCode::ArrowUp,
        K::ArrowDown => KeyCode::ArrowDown,
        K::ArrowLeft => KeyCode::ArrowLeft,
        K::ArrowRight => KeyCode::ArrowRight,
        K::Escape => KeyCode::Escape,
        K::Tab => KeyCode::Tab,
        K::Backspace => KeyCode::Backspace,
        K::Enter => KeyCode::Enter,
        K::Space => KeyCode::Space,
        K::Insert => KeyCode::Insert,
        K::Delete => KeyCode::Delete,
        K::Home => KeyCode::Home,
        K::End => KeyCode::End,
        K::PageUp => KeyCode::PageUp,
        K::PageDown => KeyCode::PageDown,

        // Punctuation, by the position that produces the character on a US
        // layout: the shifted forms sit on the same physical key.
        K::Minus => KeyCode::Minus,
        K::Equals | K::Plus => KeyCode::Equal,
        K::OpenBracket | K::OpenCurlyBracket => KeyCode::BracketLeft,
        K::CloseBracket | K::CloseCurlyBracket => KeyCode::BracketRight,
        K::Backslash | K::Pipe | K::IntlBackslash => KeyCode::Backslash,
        K::Semicolon | K::Colon => KeyCode::Semicolon,
        K::Quote => KeyCode::Quote,
        K::Backtick => KeyCode::Backquote,
        K::Comma => KeyCode::Comma,
        K::Period => KeyCode::Period,
        K::Slash | K::Questionmark => KeyCode::Slash,
        K::Exclamationmark => KeyCode::Num1,

        K::ShiftLeft => KeyCode::ShiftLeft,
        K::ShiftRight => KeyCode::ShiftRight,
        K::ControlLeft => KeyCode::ControlLeft,
        K::ControlRight => KeyCode::ControlRight,
        K::AltLeft => KeyCode::AltLeft,
        K::AltRight => KeyCode::AltRight,
        K::SuperLeft => KeyCode::MetaLeft,
        K::SuperRight => KeyCode::MetaRight,

        // Copy/Cut/Paste are synthesized commands with no physical position,
        // F25..F35 have no protocol equivalent, and BrowserBack is a media key.
        _ => return None,
    })
}

/// Centres a one-line message in `rect`. Used for empty and loading states.
pub fn centered_note(ui: &Ui, palette: &Palette, rect: Rect, body: &str) {
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        body,
        FontId::proportional(13.0),
        palette.fg_secondary,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{pos2, Event, Modifiers, MouseWheelUnit, PointerButton};

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect::from_min_size(pos2(x, y), vec2(w, h))
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    /// One frame of a stage that owns its pointer and does not forward keys.
    fn frame(stage: Rect, picture: Rect) -> StageInput {
        StageInput {
            stage,
            picture,
            pointer: None,
            owns_pointer: true,
            keyboard: false,
        }
    }

    /// Translates a single frame through a fresh forwarder.
    fn forward(frame: StageInput, events: &[Event]) -> Vec<InputEvent> {
        InputForwarder::default().translate(frame, events)
    }

    /// The `Event::Key` + `Event::Text` pair egui-winit pushes for one ordinary
    /// printable keystroke.
    fn keystroke(key: egui::Key, text: &str) -> [Event; 2] {
        [
            Event::Key {
                key,
                physical_key: Some(key),
                pressed: true,
                repeat: false,
                modifiers: Modifiers::default(),
            },
            Event::Text(text.to_owned()),
        ]
    }

    #[test]
    fn a_wide_picture_in_a_tall_stage_is_letterboxed_top_and_bottom() {
        // 16:9 into 4:3: full width, bars above and below.
        let stage = rect(0.0, 0.0, 400.0, 300.0);
        let picture = picture_rect(stage, [1920, 1080]);
        assert!(close(picture.width(), 400.0));
        assert!(close(picture.height(), 225.0));
        assert!(close(picture.center().x, stage.center().x));
        assert!(close(picture.center().y, stage.center().y));
    }

    #[test]
    fn a_tall_picture_in_a_wide_stage_is_pillarboxed_left_and_right() {
        // 4:3 into 16:9: full height, bars at the sides.
        let stage = rect(0.0, 0.0, 1600.0, 900.0);
        let picture = picture_rect(stage, [1024, 768]);
        assert!(close(picture.height(), 900.0));
        assert!(close(picture.width(), 1200.0));
        assert!(close(picture.center().x, stage.center().x));
    }

    #[test]
    fn the_picture_corners_map_to_the_corners_of_the_normalized_range() {
        let stage = rect(0.0, 0.0, 400.0, 300.0);
        let picture = picture_rect(stage, [1920, 1080]);

        let (x, y) = normalize_in_picture(picture, picture.min);
        assert!(close(x, 0.0) && close(y, 0.0), "top-left was ({x}, {y})");

        let (x, y) = normalize_in_picture(picture, picture.max);
        assert!(
            close(x, 1.0) && close(y, 1.0),
            "bottom-right was ({x}, {y})"
        );

        let (x, y) = normalize_in_picture(picture, picture.center());
        assert!(close(x, 0.5) && close(y, 0.5), "centre was ({x}, {y})");
    }

    #[test]
    fn normalizing_against_the_widget_rect_would_have_been_wrong() {
        // The bug this whole function exists to prevent: the stage's centre and
        // the picture's centre coincide, but the stage's top-left is inside the
        // letterbox bar and must not read as the picture's origin.
        let stage = rect(0.0, 0.0, 400.0, 300.0);
        let picture = picture_rect(stage, [1920, 1080]);
        let (_, y_from_picture) = normalize_in_picture(picture, stage.min);
        let (_, y_from_stage) = normalize_in_picture(stage, stage.min);
        assert!(close(y_from_stage, 0.0));
        assert!(
            close(y_from_picture, 0.0),
            "a point in the bar must clamp, not go negative"
        );
        // A point one third down the stage is *not* one third down the picture.
        let probe = pos2(0.0, 100.0);
        let (_, y_picture) = normalize_in_picture(picture, probe);
        let (_, y_stage) = normalize_in_picture(stage, probe);
        assert!(
            (y_picture - y_stage).abs() > 0.05,
            "the two mappings must differ: {y_picture} vs {y_stage}"
        );
    }

    #[test]
    fn points_in_the_letterbox_bars_clamp_into_range() {
        let stage = rect(0.0, 0.0, 400.0, 300.0);
        let picture = picture_rect(stage, [1920, 1080]);
        for probe in [
            pos2(200.0, 0.0),   // top bar
            pos2(200.0, 299.0), // bottom bar
            pos2(-50.0, -50.0), // outside entirely
            pos2(9999.0, 9999.0),
        ] {
            let (x, y) = normalize_in_picture(picture, probe);
            assert!(
                (0.0..=1.0).contains(&x),
                "x out of range for {probe:?}: {x}"
            );
            assert!(
                (0.0..=1.0).contains(&y),
                "y out of range for {probe:?}: {y}"
            );
        }
    }

    #[test]
    fn a_zero_sized_stage_does_not_divide_by_zero() {
        let stage = rect(10.0, 10.0, 0.0, 0.0);
        let picture = picture_rect(stage, [1920, 1080]);
        assert_eq!(picture, stage, "no area means no fit to compute");
        let (x, y) = normalize_in_picture(picture, pos2(10.0, 10.0));
        assert!(x.is_finite() && y.is_finite(), "got ({x}, {y})");
        assert_eq!((x, y), (0.0, 0.0));
    }

    #[test]
    fn a_frame_with_no_pixels_does_not_divide_by_zero() {
        let stage = rect(0.0, 0.0, 400.0, 300.0);
        assert_eq!(picture_rect(stage, [0, 0]), stage);
        assert_eq!(picture_rect(stage, [1920, 0]), stage);
    }

    #[test]
    fn twenty_pointer_samples_become_one_mouse_move() {
        let stage = rect(0.0, 0.0, 400.0, 300.0);
        let picture = picture_rect(stage, [1920, 1080]);
        let events: Vec<Event> = (0..20)
            .map(|i| Event::PointerMoved(pos2(i as f32 * 10.0, 150.0)))
            .collect();

        let out = forward(frame(stage, picture), &events);
        assert_eq!(out.len(), 1, "moves must coalesce: {out:?}");
        let InputEvent::MouseMove { x, .. } = out[0] else {
            panic!("expected a move, got {:?}", out[0]);
        };
        // The last sample, at x = 190 of 400, wins.
        assert!(close(x, 190.0 / 400.0), "kept the wrong sample: {x}");
    }

    #[test]
    fn a_click_still_carries_the_move_that_preceded_it() {
        let stage = rect(0.0, 0.0, 400.0, 300.0);
        let picture = picture_rect(stage, [1920, 1080]);
        let pos = picture.center();
        let events = vec![
            Event::PointerMoved(pos),
            Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::default(),
            },
            Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::default(),
            },
        ];
        let out = forward(
            StageInput {
                pointer: Some(pos),
                ..frame(stage, picture)
            },
            &events,
        );
        assert!(
            matches!(out[0], InputEvent::MouseMove { .. }),
            "the move must arrive before the press: {out:?}"
        );
        assert!(matches!(
            out[1],
            InputEvent::MouseDown {
                button: MouseButton::Left,
                ..
            }
        ));
        assert!(matches!(out[2], InputEvent::MouseUp { .. }));
    }

    #[test]
    fn pointer_events_outside_the_stage_are_not_forwarded() {
        let stage = rect(0.0, 0.0, 400.0, 300.0);
        let picture = picture_rect(stage, [1920, 1080]);
        let outside = pos2(500.0, 500.0);
        let events = vec![
            Event::PointerMoved(outside),
            Event::PointerButton {
                pos: outside,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::default(),
            },
        ];
        assert!(
            forward(
                StageInput {
                    pointer: Some(outside),
                    keyboard: true,
                    ..frame(stage, picture)
                },
                &events,
            )
            .is_empty(),
            "clicks on the toolbar must not reach the host"
        );
    }

    #[test]
    fn scrolling_down_sends_a_positive_wheel_delta() {
        // egui reports the content moving up (negative y) when the user
        // scrolls down; the protocol wants positive dy for scrolling down.
        let stage = rect(0.0, 0.0, 400.0, 300.0);
        let picture = picture_rect(stage, [1920, 1080]);
        let events = vec![Event::MouseWheel {
            unit: MouseWheelUnit::Line,
            delta: vec2(0.0, -3.0),
            phase: egui::TouchPhase::Move,
            modifiers: Modifiers::default(),
        }];
        let out = forward(
            StageInput {
                pointer: Some(picture.center()),
                ..frame(stage, picture)
            },
            &events,
        );
        assert_eq!(out, vec![InputEvent::Wheel { dx: 0.0, dy: 3.0 }]);
    }

    #[test]
    fn a_runaway_trackpad_cannot_scroll_the_host_forever() {
        let stage = rect(0.0, 0.0, 400.0, 300.0);
        let picture = picture_rect(stage, [1920, 1080]);
        let events = vec![Event::MouseWheel {
            unit: MouseWheelUnit::Point,
            delta: vec2(0.0, -100_000.0),
            phase: egui::TouchPhase::Move,
            modifiers: Modifiers::default(),
        }];
        let out = forward(
            StageInput {
                pointer: Some(picture.center()),
                ..frame(stage, picture)
            },
            &events,
        );
        assert_eq!(out, vec![InputEvent::Wheel { dx: 0.0, dy: 5.0 }]);
    }

    #[test]
    fn a_wheel_with_the_pointer_off_the_stage_is_ignored() {
        let stage = rect(0.0, 0.0, 400.0, 300.0);
        let events = vec![Event::MouseWheel {
            unit: MouseWheelUnit::Line,
            delta: vec2(0.0, -3.0),
            phase: egui::TouchPhase::Move,
            modifiers: Modifiers::default(),
        }];
        assert!(forward(frame(stage, stage), &events).is_empty());
    }

    #[test]
    fn keys_are_dropped_when_the_keyboard_is_not_ours_to_forward() {
        let stage = rect(0.0, 0.0, 400.0, 300.0);
        let events = vec![
            Event::Key {
                key: egui::Key::A,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::default(),
            },
            Event::Text("a".into()),
        ];
        assert!(forward(frame(stage, stage), &events).is_empty());
        // One keystroke, one event: the Key and the Text egui pushes for it are
        // two spellings of the same thing, not two characters.
        assert_eq!(
            forward(
                StageInput {
                    keyboard: true,
                    ..frame(stage, stage)
                },
                &events,
            ),
            vec![InputEvent::KeyDown {
                code: KeyCode::A,
                modifiers: KeyModifiers::NONE,
            }]
        );
    }

    #[test]
    fn key_repeats_are_dropped_so_the_host_does_not_repeat_twice() {
        let stage = rect(0.0, 0.0, 400.0, 300.0);
        let events = vec![
            Event::Key {
                key: egui::Key::A,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::default(),
            },
            Event::Key {
                key: egui::Key::A,
                physical_key: None,
                pressed: true,
                repeat: true,
                modifiers: Modifiers::default(),
            },
        ];
        let out = forward(
            StageInput {
                keyboard: true,
                ..frame(stage, stage)
            },
            &events,
        );
        assert_eq!(out.len(), 1, "{out:?}");
    }

    #[test]
    fn holding_a_printable_key_does_not_double_type_on_the_host() {
        // The realistic stream: egui-winit pushes an Event::Text alongside the
        // first press AND alongside every OS repeat. We send one KeyDown and
        // no KeyUp, so the host is already auto-repeating on its own; letting
        // the repeats' Text through typed every held character twice.
        let stage = rect(0.0, 0.0, 400.0, 300.0);
        let mut events = vec![
            Event::Key {
                key: egui::Key::A,
                physical_key: Some(egui::Key::A),
                pressed: true,
                repeat: false,
                modifiers: Modifiers::default(),
            },
            Event::Text("a".into()),
        ];
        for _ in 0..3 {
            events.push(Event::Key {
                key: egui::Key::A,
                physical_key: Some(egui::Key::A),
                pressed: true,
                repeat: true,
                modifiers: Modifiers::default(),
            });
            events.push(Event::Text("a".into()));
        }

        let out = forward(
            StageInput {
                keyboard: true,
                ..frame(stage, stage)
            },
            &events,
        );

        let downs = out
            .iter()
            .filter(|e| matches!(e, InputEvent::KeyDown { .. }))
            .count();
        let texts = out
            .iter()
            .filter(|e| matches!(e, InputEvent::Text { .. }))
            .count();
        assert_eq!(downs, 1, "one press, one KeyDown: {out:?}");
        assert_eq!(
            texts, 0,
            "the host auto-repeats by itself; no Text may ride along: {out:?}"
        );
    }

    #[test]
    fn the_physical_key_wins_over_the_keymapped_one() {
        // A Dvorak user pressing the physical Q gets key = Apostrophe; the host
        // must be told the position, not the character.
        let stage = rect(0.0, 0.0, 400.0, 300.0);
        let events = vec![Event::Key {
            key: egui::Key::Quote,
            physical_key: Some(egui::Key::Q),
            pressed: true,
            repeat: false,
            modifiers: Modifiers::default(),
        }];
        let out = forward(
            StageInput {
                keyboard: true,
                ..frame(stage, stage)
            },
            &events,
        );
        assert_eq!(
            out,
            vec![InputEvent::KeyDown {
                code: KeyCode::Q,
                modifiers: KeyModifiers::NONE
            }]
        );
    }

    #[test]
    fn losing_focus_releases_every_held_key_even_with_the_keyboard_off() {
        let stage = rect(0.0, 0.0, 400.0, 300.0);
        let events = vec![Event::WindowFocused(false)];
        assert_eq!(
            forward(frame(stage, stage), &events),
            vec![InputEvent::ReleaseAll]
        );
    }

    #[test]
    fn only_the_mac_command_key_maps_to_meta() {
        // egui sets `command` to ctrl on Windows and Linux; treating that as
        // Meta would send Win+C to the host for every Ctrl+C.
        let mods = modifiers_from_egui(egui::Modifiers {
            ctrl: true,
            command: true,
            ..Default::default()
        });
        assert!(mods.ctrl && !mods.meta);

        let mods = modifiers_from_egui(egui::Modifiers {
            mac_cmd: true,
            command: true,
            ..Default::default()
        });
        assert!(mods.meta && !mods.ctrl);
    }

    #[test]
    fn shifted_punctuation_maps_to_the_key_that_produces_it() {
        assert_eq!(keycode_from_egui(egui::Key::Pipe), Some(KeyCode::Backslash));
        assert_eq!(
            keycode_from_egui(egui::Key::Questionmark),
            Some(KeyCode::Slash)
        );
        assert_eq!(keycode_from_egui(egui::Key::Plus), Some(KeyCode::Equal));
        assert_eq!(
            keycode_from_egui(egui::Key::Colon),
            Some(KeyCode::Semicolon)
        );
    }

    #[test]
    fn keys_with_no_physical_position_are_dropped_rather_than_guessed() {
        for key in [
            egui::Key::Copy,
            egui::Key::Cut,
            egui::Key::Paste,
            egui::Key::BrowserBack,
            egui::Key::F35,
        ] {
            assert_eq!(keycode_from_egui(key), None, "{key:?} must not be guessed");
        }
    }

    // --- regressions: the controller's input-forwarding path ---------------

    #[test]
    fn typing_hello_arrives_on_the_host_exactly_once() {
        // egui-winit pushes both an Event::Key and an Event::Text for one
        // printable keystroke; forwarding both typed "hheelllloo".
        let stage = rect(0.0, 0.0, 400.0, 300.0);
        let mut events = Vec::new();
        for (key, text) in [
            (egui::Key::H, "h"),
            (egui::Key::E, "e"),
            (egui::Key::L, "l"),
            (egui::Key::L, "l"),
            (egui::Key::O, "o"),
        ] {
            events.extend(keystroke(key, text));
        }
        let out = forward(
            StageInput {
                keyboard: true,
                ..frame(stage, stage)
            },
            &events,
        );
        assert_eq!(
            out,
            [KeyCode::H, KeyCode::E, KeyCode::L, KeyCode::L, KeyCode::O]
                .map(|code| InputEvent::KeyDown {
                    code,
                    modifiers: KeyModifiers::NONE,
                })
                .to_vec(),
            "one keystroke must produce one event"
        );
    }

    #[test]
    fn a_shifted_character_still_travels_as_its_key_position() {
        // Shift is already down on the host, so KeyDown H types "H" there:
        // the Text event would be a second, duplicate capital.
        let stage = rect(0.0, 0.0, 400.0, 300.0);
        let shift = Modifiers {
            shift: true,
            ..Default::default()
        };
        let events = vec![
            Event::Key {
                key: egui::Key::H,
                physical_key: Some(egui::Key::H),
                pressed: true,
                repeat: false,
                modifiers: shift,
            },
            Event::Text("H".into()),
        ];
        let out = forward(
            StageInput {
                keyboard: true,
                ..frame(stage, stage)
            },
            &events,
        );
        assert_eq!(
            out,
            vec![InputEvent::KeyDown {
                code: KeyCode::H,
                modifiers: KeyModifiers {
                    shift: true,
                    ..KeyModifiers::NONE
                },
            }]
        );
    }

    #[test]
    fn an_accented_character_its_key_position_cannot_express_travels_as_text() {
        // A dead-key composition on the physical E key: pressing E on the host
        // would type a bare "e", so the text is the only faithful spelling and
        // the key event stands down rather than adding a stray letter.
        let stage = rect(0.0, 0.0, 400.0, 300.0);
        let events = keystroke(egui::Key::E, "é").to_vec();
        let out = forward(
            StageInput {
                keyboard: true,
                ..frame(stage, stage)
            },
            &events,
        );
        assert_eq!(
            out,
            vec![InputEvent::Text {
                value: "é".to_owned()
            }],
            "the accented character must arrive, and only once"
        );
    }

    #[test]
    fn an_ime_commit_reaches_the_host_as_text() {
        // IME composition produces no key event at all.
        let stage = rect(0.0, 0.0, 400.0, 300.0);
        let events = vec![Event::Ime(egui::ImeEvent::Commit("日本".into()))];
        let out = forward(
            StageInput {
                keyboard: true,
                ..frame(stage, stage)
            },
            &events,
        );
        assert_eq!(
            out,
            vec![InputEvent::Text {
                value: "日本".to_owned()
            }]
        );
        assert!(
            forward(frame(stage, stage), &events).is_empty(),
            "and never while the keyboard is not ours"
        );
    }

    #[test]
    fn a_release_outside_the_picture_still_lifts_the_button_on_the_host() {
        // Press inside, drag out of the window, release: dropping the release
        // left the button held down on the host with no way to clear it.
        let stage = rect(0.0, 0.0, 400.0, 300.0);
        let picture = picture_rect(stage, [1920, 1080]);
        let inside = picture.center();
        let outside = pos2(900.0, 900.0);
        let mut forwarder = InputForwarder::default();

        let down = forwarder.translate(
            StageInput {
                pointer: Some(inside),
                ..frame(stage, picture)
            },
            &[Event::PointerButton {
                pos: inside,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::default(),
            }],
        );
        assert!(matches!(down.as_slice(), [InputEvent::MouseDown { .. }]));

        let up = forwarder.translate(
            StageInput {
                pointer: Some(outside),
                owns_pointer: false,
                ..frame(stage, picture)
            },
            &[Event::PointerButton {
                pos: outside,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::default(),
            }],
        );
        assert!(
            matches!(
                up.as_slice(),
                [InputEvent::MouseUp {
                    button: MouseButton::Left,
                    ..
                }]
            ),
            "the release must reach the host or the button sticks: {up:?}"
        );
    }

    #[test]
    fn a_release_whose_press_never_went_out_is_not_invented() {
        // The press was swallowed by a panel, so the host is holding nothing:
        // a release here would lift a button it never pressed.
        let stage = rect(0.0, 0.0, 400.0, 300.0);
        let picture = picture_rect(stage, [1920, 1080]);
        let inside = picture.center();
        let out = forward(
            StageInput {
                pointer: Some(inside),
                ..frame(stage, picture)
            },
            &[Event::PointerButton {
                pos: inside,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::default(),
            }],
        );
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn a_drag_off_the_picture_keeps_steering_the_host_cursor() {
        let stage = rect(0.0, 0.0, 400.0, 300.0);
        let picture = picture_rect(stage, [1920, 1080]);
        let inside = picture.center();
        let mut forwarder = InputForwarder::default();
        forwarder.translate(
            StageInput {
                pointer: Some(inside),
                ..frame(stage, picture)
            },
            &[Event::PointerButton {
                pos: inside,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::default(),
            }],
        );
        let out = forwarder.translate(
            StageInput {
                pointer: Some(pos2(900.0, 150.0)),
                owns_pointer: false,
                ..frame(stage, picture)
            },
            &[Event::PointerMoved(pos2(900.0, 150.0))],
        );
        assert_eq!(
            out,
            vec![InputEvent::MouseMove { x: 1.0, y: 0.5 }],
            "a drag must clamp to the edge, not stop"
        );
    }

    #[test]
    fn a_pointer_that_leaves_the_window_does_not_strand_a_held_button() {
        let stage = rect(0.0, 0.0, 400.0, 300.0);
        let picture = picture_rect(stage, [1920, 1080]);
        let inside = picture.center();
        let mut forwarder = InputForwarder::default();
        forwarder.translate(
            StageInput {
                pointer: Some(inside),
                ..frame(stage, picture)
            },
            &[Event::PointerButton {
                pos: inside,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::default(),
            }],
        );
        let out = forwarder.translate(frame(stage, picture), &[Event::PointerGone]);
        assert_eq!(
            out,
            vec![InputEvent::MouseUp {
                button: MouseButton::Left,
                x: 0.5,
                y: 0.5,
            }],
            "released where it was last seen, not at the origin"
        );
        // And only once: the button is no longer ours to lift.
        assert!(forwarder
            .translate(frame(stage, picture), &[Event::PointerGone])
            .is_empty());
    }

    #[test]
    fn closing_the_keyboard_gate_releases_what_the_host_is_holding() {
        // Hold Ctrl, then open the chat panel. The KeyUp is never forwarded, so
        // without this the modifier stayed down on the remote machine.
        let stage = rect(0.0, 0.0, 400.0, 300.0);
        let mut forwarder = InputForwarder::default();
        let live = StageInput {
            keyboard: true,
            ..frame(stage, stage)
        };
        let down = forwarder.translate(
            live,
            &[Event::Key {
                key: egui::Key::ControlLeft,
                physical_key: Some(egui::Key::ControlLeft),
                pressed: true,
                repeat: false,
                modifiers: Modifiers {
                    ctrl: true,
                    command: true,
                    ..Default::default()
                },
            }],
        );
        assert!(matches!(
            down.as_slice(),
            [InputEvent::KeyDown {
                code: KeyCode::ControlLeft,
                ..
            }]
        ));

        let closed = forwarder.translate(frame(stage, stage), &[]);
        assert_eq!(
            closed,
            vec![InputEvent::ReleaseAll],
            "a gate that closes mid-hold must release first"
        );
        assert!(
            forwarder.translate(frame(stage, stage), &[]).is_empty(),
            "and say it once, not every frame the panel is open"
        );
    }

    #[test]
    fn copy_cut_and_paste_reach_the_remote_machine() {
        // egui turns the chords into Event::Copy/Cut/Paste and never pushes a
        // key event, so the most-used remote shortcut used to do nothing at all.
        let stage = rect(0.0, 0.0, 400.0, 300.0);
        let ctrl = KeyModifiers {
            ctrl: true,
            ..KeyModifiers::NONE
        };
        for (event, code) in [
            (Event::Copy, KeyCode::C),
            (Event::Cut, KeyCode::X),
            (Event::Paste("clipboard".into()), KeyCode::V),
        ] {
            let out = forward(
                StageInput {
                    keyboard: true,
                    ..frame(stage, stage)
                },
                std::slice::from_ref(&event),
            );
            assert_eq!(
                out,
                vec![
                    InputEvent::KeyDown {
                        code: KeyCode::ControlLeft,
                        modifiers: ctrl,
                    },
                    InputEvent::KeyDown {
                        code,
                        modifiers: ctrl
                    },
                    InputEvent::KeyUp {
                        code,
                        modifiers: ctrl
                    },
                    InputEvent::KeyUp {
                        code: KeyCode::ControlLeft,
                        modifiers: KeyModifiers::NONE,
                    },
                ],
                "{event:?} must arrive as a chord the host can act on"
            );
            assert!(
                forward(frame(stage, stage), std::slice::from_ref(&event)).is_empty(),
                "but never while the keyboard is not ours: {event:?}"
            );
        }
    }

    #[test]
    fn a_clipboard_chord_rides_the_modifier_the_user_is_already_holding() {
        // A macOS controller has already sent ⌘ down as its own physical key.
        // Adding Ctrl on top would send ⌘+Ctrl+C; the letter alone is ⌘+C,
        // which is what the user pressed.
        let stage = rect(0.0, 0.0, 400.0, 300.0);
        let mut forwarder = InputForwarder::default();
        let live = StageInput {
            keyboard: true,
            ..frame(stage, stage)
        };
        forwarder.translate(
            live,
            &[Event::Key {
                key: egui::Key::SuperLeft,
                physical_key: Some(egui::Key::SuperLeft),
                pressed: true,
                repeat: false,
                modifiers: Modifiers {
                    mac_cmd: true,
                    command: true,
                    ..Default::default()
                },
            }],
        );
        let meta = KeyModifiers {
            meta: true,
            ..KeyModifiers::NONE
        };
        assert_eq!(
            forwarder.translate(live, &[Event::Copy]),
            vec![
                InputEvent::KeyDown {
                    code: KeyCode::C,
                    modifiers: meta,
                },
                InputEvent::KeyUp {
                    code: KeyCode::C,
                    modifiers: meta,
                },
            ]
        );
    }

    #[test]
    fn a_click_on_a_popup_drawn_over_the_stage_is_not_forwarded() {
        // The rect test alone said "inside the video"; the layer says the click
        // belongs to the popup, and the remote machine must not get it too.
        let stage = rect(0.0, 0.0, 400.0, 300.0);
        let picture = picture_rect(stage, [1920, 1080]);
        let over = picture.center();
        let events = vec![
            Event::PointerMoved(over),
            Event::PointerButton {
                pos: over,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::default(),
            },
            Event::MouseWheel {
                unit: MouseWheelUnit::Line,
                delta: vec2(0.0, -3.0),
                phase: egui::TouchPhase::Move,
                modifiers: Modifiers::default(),
            },
        ];
        let out = forward(
            StageInput {
                pointer: Some(over),
                owns_pointer: false,
                keyboard: true,
                ..frame(stage, picture)
            },
            &events,
        );
        assert!(
            out.is_empty(),
            "input the stage does not own must stay local: {out:?}"
        );
    }

    #[test]
    fn groups_digits_as_the_user_types_them() {
        assert_eq!(grouped_digits(""), "");
        assert_eq!(grouped_digits("1"), "1");
        assert_eq!(grouped_digits("1234"), "123 4");
        assert_eq!(grouped_digits("123456789"), "123 456 789");
        assert_eq!(grouped_digits("123-456-789"), "123 456 789");
        assert_eq!(grouped_digits("no digits here"), "");
    }

    #[test]
    fn parses_however_the_user_typed_the_remote_id() {
        let want = PeerId::new(123_456_789).unwrap();
        for input in ["123456789", "123 456 789", "123-456-789", " 123.456.789 "] {
            assert_eq!(parse_desk_id(input), Some(want), "input {input:?}");
        }
    }

    #[test]
    fn refuses_an_id_that_is_not_yet_complete() {
        // This is what keeps the Connect button greyed out instead of letting
        // the user press it and watch the relay reject them.
        for input in ["", "12345678", "1234567890", "abcdefghi", "012345678"] {
            assert_eq!(parse_desk_id(input), None, "input {input:?}");
        }
    }

    #[test]
    fn formats_byte_counts_at_every_scale() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(2048), "2.0 KB");
        assert_eq!(format_bytes(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(format_bytes(3 * 1024 * 1024 * 1024), "3.00 GB");
    }

    #[test]
    fn formats_how_long_ago_a_contact_was_reached() {
        let now = 1_700_000_000_000u64;
        assert_eq!(format_ago(now, 0), "never");
        assert_eq!(format_ago(now, now), "just now");
        assert_eq!(format_ago(now, now - 5 * 60_000), "5m ago");
        assert_eq!(format_ago(now, now - 3 * 3_600_000), "3h ago");
        assert_eq!(format_ago(now, now - 2 * 86_400_000), "2d ago");
    }

    #[test]
    fn a_timestamp_from_the_future_reads_as_just_now() {
        // A clock that moved backwards must not print a wrapped u64 age.
        let now = 1_700_000_000_000u64;
        assert_eq!(format_ago(now, now + 86_400_000), "just now");
    }
}
