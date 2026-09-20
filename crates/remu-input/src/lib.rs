//! Injecting a remote peer's input into this machine's desktop.
//!
//! The controller sends [`remu_proto::InputEvent`]s with pointer positions
//! normalized to `0.0..=1.0`; this crate turns them into real key presses and
//! cursor movements through `enigo`, and keeps track of what is currently held
//! down so a session can end without leaving a modifier stuck.
//!
//! Two failures from the Electron original are fixed here by construction, and
//! both are worth knowing before changing anything:
//!
//! 1. **Positions are pixels, not points.** [`ScreenGeometry`] must come from
//!    the same backend that captures the frames, in physical pixels. Feeding it
//!    a DPI-scaled logical size puts every click at a fraction of its intended
//!    position on a scaled Windows or Linux display.
//! 2. **A key event moves exactly one key.** Modifiers arrive as their own
//!    [`remu_proto::KeyCode::ShiftLeft`]-style events, so injecting a key-up
//!    must not also re-apply or clear the modifier flags that came with it —
//!    doing so releases a modifier the remote user is still holding.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod keymap;
pub mod permissions;

use enigo::{Axis, Button, Coordinate, Direction, Enigo, Keyboard, Mouse};
use remu_proto::{InputEvent, KeyCode, MouseButton};

/// Longest [`InputEvent::Text`] payload accepted in one event.
///
/// Text is typed character by character on most backends, so an unbounded
/// payload from a hostile peer would hold the host's keyboard hostage for as
/// long as it took to type. Real IME commits and clipboard pastes are orders of
/// magnitude below this.
pub const MAX_TEXT_BYTES: usize = 4096;

#[derive(Debug, thiserror::Error)]
pub enum InputError {
    #[error("no connection to the desktop input system: {0}")]
    Backend(#[from] enigo::NewConError),
    #[error("the desktop input system rejected the event: {0}")]
    Inject(#[from] enigo::InputError),
    #[error("{0:?} cannot be injected on this platform")]
    UnsupportedKey(KeyCode),
    #[error("text event of {0} bytes is over the host limit")]
    TextTooLong(usize),
    #[error("the display reported an unusable size of {width}x{height}")]
    InvalidGeometry { width: i32, height: i32 },
}

/// The pixel rectangle that normalized coordinates are relative to.
///
/// `origin_x`/`origin_y` place the rectangle in the desktop's global coordinate
/// space, so a session sharing a monitor to the left of the primary one (a
/// negative origin on Windows and Linux) lands its clicks on that monitor
/// rather than on the primary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScreenGeometry {
    pub width: u32,
    pub height: u32,
    pub origin_x: i32,
    pub origin_y: i32,
}

impl ScreenGeometry {
    /// A rectangle at the desktop origin, for the common single-monitor case.
    pub fn primary(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            origin_x: 0,
            origin_y: 0,
        }
    }
}

/// Converts a normalized position into an absolute desktop pixel.
///
/// Kept free of [`InputInjector`] so the arithmetic — the part that silently
/// misplaces every click when it is wrong — can be tested without a desktop
/// session. Out-of-range and NaN inputs are clamped here as well as by
/// [`InputEvent::sanitized`]: this is public API and the next caller may not
/// have sanitized anything.
pub fn absolute_position(x: f32, y: f32, screen: ScreenGeometry) -> (i32, i32) {
    (
        axis_position(x, screen.width, screen.origin_x),
        axis_position(y, screen.height, screen.origin_y),
    )
}

fn axis_position(fraction: f32, extent: u32, origin: i32) -> i32 {
    // NaN survives `clamp`, and casting it to an integer is a silent zero, so
    // fold it to the near edge deliberately instead.
    let fraction = if fraction.is_nan() {
        0.0
    } else {
        f64::from(fraction.clamp(0.0, 1.0))
    };
    // The last addressable pixel is `extent - 1`. Scaling by `extent` would put
    // `1.0` one pixel outside the screen, where the OS clamps it back and the
    // rightmost column becomes unreachable.
    let last = f64::from(extent.saturating_sub(1));
    let offset = (fraction * last).round();
    // `offset` is bounded by `u32::MAX`, which does not fit in an i32 on a wall
    // of displays, and the origin can be negative; saturate rather than wrap.
    let offset = offset.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32;
    origin.saturating_add(offset)
}

/// Opts this process into per-monitor DPI awareness on Windows.
///
/// Until this is set, Win32 hands a non-aware process *virtualized*
/// coordinates on a scaled display: the desktop measures 2560x1440 but reports
/// 1707x960, and an injected click lands at a fraction of where it was aimed.
/// That is the first of the two bugs this crate's module documentation
/// describes, and it is a process-wide mode, so an application with a window of
/// its own should call this during startup, before the window exists.
///
/// Idempotent, and a no-op if an application manifest already chose a mode.
/// macOS and Linux have no such legacy mode and this does nothing there.
pub fn enable_high_dpi_awareness() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        #[cfg(target_os = "windows")]
        match enigo::set_dpi_awareness() {
            // Already chosen elsewhere; the mode that is set wins, and
            // complaining would be noise in every manifest-aware build.
            Err(()) => tracing::debug!("process DPI awareness was already set"),
            Ok(()) => tracing::debug!("process is now per-monitor DPI aware"),
        }
    });
}

/// Keys and buttons currently held down by *this* injector.
///
/// A `Vec` rather than a set: it is never more than a handful of entries, and
/// preserving press order lets [`Held::drain`] unwind newest-first so a
/// modifier pressed before a letter is also released after it.
#[derive(Debug, Default)]
struct Held {
    keys: Vec<enigo::Key>,
    buttons: Vec<Button>,
}

impl Held {
    /// Records a press. Auto-repeat re-sends key-down for a key already held,
    /// which must stay one entry or the matching key-up would leave a remnant.
    fn press_key(&mut self, key: enigo::Key) {
        if !self.keys.contains(&key) {
            self.keys.push(key);
        }
    }

    fn release_key(&mut self, key: enigo::Key) {
        self.keys.retain(|held| *held != key);
    }

    fn press_button(&mut self, button: Button) {
        if !self.buttons.contains(&button) {
            self.buttons.push(button);
        }
    }

    fn release_button(&mut self, button: Button) {
        self.buttons.retain(|held| *held != button);
    }

    fn len(&self) -> usize {
        self.keys.len() + self.buttons.len()
    }

    fn drain(&mut self) -> (Vec<enigo::Key>, Vec<Button>) {
        let mut keys = std::mem::take(&mut self.keys);
        keys.reverse();
        let mut buttons = std::mem::take(&mut self.buttons);
        buttons.reverse();
        (keys, buttons)
    }
}

/// Applies remote input events to this machine.
///
/// One injector owns one connection to the desktop input system and the ledger
/// of what it is holding down, so a session must keep the same instance for its
/// whole life: a new injector does not know what the old one left pressed.
#[derive(Debug)]
pub struct InputInjector {
    enigo: Enigo,
    held: Held,
}

impl InputInjector {
    /// Connects to the desktop input system.
    ///
    /// # Errors
    ///
    /// [`InputError::Backend`] when the OS will not let this process synthesize
    /// input — missing macOS Accessibility permission, no X11/Wayland display,
    /// or a Windows integrity level below the target's. Ask
    /// [`permissions::accessibility`] first to tell the user *why*.
    pub fn new() -> Result<Self, InputError> {
        // Cheap insurance for a host application that forgot to call it at
        // startup: injecting into a scaled Windows desktop without this puts
        // every click at the wrong pixel.
        enable_high_dpi_awareness();
        let settings = enigo::Settings {
            // Prompting here would throw the macOS permission dialog at the
            // user at whatever moment a peer happened to connect;
            // `permissions::request_accessibility` does it where they expect it.
            open_prompt_to_get_permissions: false,
            ..enigo::Settings::default()
        };
        Ok(Self {
            enigo: Enigo::new(&settings)?,
            held: Held::default(),
        })
    }

    /// Injects one event from a remote peer.
    ///
    /// `screen` is the rectangle the peer's normalized coordinates refer to,
    /// in physical pixels as reported by the capture backend.
    ///
    /// # Errors
    ///
    /// [`InputError::UnsupportedKey`] for a key this platform has no equivalent
    /// for, [`InputError::TextTooLong`] for an oversized text payload, and
    /// [`InputError::Inject`] when the OS rejects the synthesized event. None of
    /// these are fatal to a session; they describe one dropped event.
    pub fn inject(&mut self, event: &InputEvent, screen: ScreenGeometry) -> Result<(), InputError> {
        // The event came off the wire from a peer that may be hostile or buggy,
        // so clamp before the values reach any arithmetic.
        match event.clone().sanitized() {
            InputEvent::MouseMove { x, y } => self.move_to(x, y, screen),
            InputEvent::MouseDown { button, x, y } => {
                self.move_to(x, y, screen)?;
                let button = to_enigo_button(button);
                self.enigo.button(button, Direction::Press)?;
                self.held.press_button(button);
                Ok(())
            }
            InputEvent::MouseUp { button, x, y } => {
                self.move_to(x, y, screen)?;
                let button = to_enigo_button(button);
                // Forget it first: if the OS rejects the release we must not
                // keep claiming to hold a button we will never release again.
                self.held.release_button(button);
                self.enigo.button(button, Direction::Release)?;
                Ok(())
            }
            InputEvent::MouseClick {
                button,
                x,
                y,
                double,
            } => {
                self.move_to(x, y, screen)?;
                let button = to_enigo_button(button);
                self.enigo.button(button, Direction::Click)?;
                if double {
                    // Two clicks inside the OS double-click interval; every
                    // enigo backend counts them into a real double click.
                    self.enigo.button(button, Direction::Click)?;
                }
                Ok(())
            }
            InputEvent::Wheel { dx, dy } => {
                // Protocol and enigo agree on sign: positive is down and right.
                let dy = dy.round() as i32;
                let dx = dx.round() as i32;
                if dy != 0 {
                    self.enigo.scroll(dy, Axis::Vertical)?;
                }
                if dx != 0 {
                    self.enigo.scroll(dx, Axis::Horizontal)?;
                }
                Ok(())
            }
            InputEvent::KeyDown { code, .. } => {
                let key = keymap::to_enigo(code).ok_or(InputError::UnsupportedKey(code))?;
                // `modifiers` is deliberately ignored: the peer sends every
                // modifier as its own key event, and re-applying the flags here
                // would press or release a modifier the user is still holding.
                self.enigo.key(key, Direction::Press)?;
                self.held.press_key(key);
                Ok(())
            }
            InputEvent::KeyUp { code, .. } => {
                let key = keymap::to_enigo(code).ok_or(InputError::UnsupportedKey(code))?;
                self.held.release_key(key);
                self.enigo.key(key, Direction::Release)?;
                Ok(())
            }
            InputEvent::Text { value } => {
                check_text(&value)?;
                self.enigo.text(&value)?;
                Ok(())
            }
            InputEvent::ReleaseAll => self.release_all(),
        }
    }

    /// Releases every key and mouse button this injector is still holding.
    ///
    /// Call it when a session ends or the controller loses focus. Without it, a
    /// peer that disconnects mid-shortcut leaves the modifier held down on the
    /// host until a human presses and releases it locally.
    ///
    /// # Errors
    ///
    /// The first rejection from the OS, after having attempted every remaining
    /// release: giving up on the first failure would strand exactly the keys
    /// this method exists to free.
    pub fn release_all(&mut self) -> Result<(), InputError> {
        let (keys, buttons) = self.held.drain();
        let mut first_error = None;
        for key in keys {
            if let Err(error) = self.enigo.key(key, Direction::Release) {
                first_error.get_or_insert(error);
            }
        }
        for button in buttons {
            if let Err(error) = self.enigo.button(button, Direction::Release) {
                first_error.get_or_insert(error);
            }
        }
        match first_error {
            Some(error) => Err(error.into()),
            None => Ok(()),
        }
    }

    /// How many keys and mouse buttons [`InputInjector::release_all`] would
    /// release right now.
    pub fn held_keys(&self) -> usize {
        self.held.len()
    }

    fn move_to(&mut self, x: f32, y: f32, screen: ScreenGeometry) -> Result<(), InputError> {
        let (px, py) = absolute_position(x, y, screen);
        self.enigo.move_mouse(px, py, Coordinate::Abs)?;
        Ok(())
    }
}

/// Rejects a text payload the host should not type.
///
/// Separate from [`InputInjector::inject`] so the limit can be tested without a
/// desktop session, and so a caller that batches text can check it up front.
fn check_text(value: &str) -> Result<(), InputError> {
    // `len` is bytes, not characters: the cost of typing is per byte on the
    // wire and per char on the keyboard, and bytes is the conservative bound.
    if value.len() > MAX_TEXT_BYTES {
        return Err(InputError::TextTooLong(value.len()));
    }
    Ok(())
}

fn to_enigo_button(button: MouseButton) -> Button {
    match button {
        MouseButton::Left => Button::Left,
        MouseButton::Middle => Button::Middle,
        MouseButton::Right => Button::Right,
    }
}

/// The primary display's size in the pixel units input is injected in.
///
/// Note that this is the *input* backend's view of the display. A host that
/// captures with a different backend should pass that backend's geometry to
/// [`InputInjector::inject`] instead, because a mismatch of even a scale factor
/// puts every click at the wrong place.
///
/// # Errors
///
/// [`InputError::Backend`] if the process may not talk to the input system at
/// all (on macOS this needs the Accessibility permission, even just to ask for
/// a size), and [`InputError::InvalidGeometry`] if the display reports a size
/// that cannot be addressed.
pub fn primary_geometry() -> Result<ScreenGeometry, InputError> {
    // Without this the size below is the DPI-scaled one on Windows, which is
    // exactly the mismatch that misplaces every injected click.
    enable_high_dpi_awareness();
    let settings = enigo::Settings {
        open_prompt_to_get_permissions: false,
        // Nothing was pressed through this short-lived connection, and it must
        // not disturb the keys the local user is holding when it drops.
        release_keys_when_dropped: false,
        ..enigo::Settings::default()
    };
    let enigo = Enigo::new(&settings)?;
    let (width, height) = enigo.main_display()?;
    let (Ok(width), Ok(height)) = (u32::try_from(width), u32::try_from(height)) else {
        return Err(InputError::InvalidGeometry { width, height });
    };
    if width == 0 || height == 0 {
        return Err(InputError::InvalidGeometry {
            width: width as i32,
            height: height as i32,
        });
    }
    Ok(ScreenGeometry::primary(width, height))
}

#[cfg(test)]
mod tests {
    use super::*;
    use remu_proto::KeyModifiers;

    const HD: ScreenGeometry = ScreenGeometry {
        width: 1920,
        height: 1080,
        origin_x: 0,
        origin_y: 0,
    };

    #[test]
    fn maps_zero_to_the_origin_and_one_to_the_last_addressable_pixel() {
        assert_eq!(absolute_position(0.0, 0.0, HD), (0, 0));
        assert_eq!(absolute_position(1.0, 1.0, HD), (1919, 1079));
    }

    #[test]
    fn maps_one_half_to_the_centre_of_the_screen() {
        let (x, y) = absolute_position(0.5, 0.5, HD);
        assert_eq!((x, y), (960, 540));
    }

    #[test]
    fn places_coordinates_on_a_monitor_left_of_the_primary_one() {
        // A 1920-wide display to the left of the primary sits at x = -1920 in
        // the desktop coordinate space Windows and Linux use.
        let left = ScreenGeometry {
            width: 1920,
            height: 1080,
            origin_x: -1920,
            origin_y: 200,
        };
        assert_eq!(absolute_position(0.0, 0.0, left), (-1920, 200));
        assert_eq!(absolute_position(1.0, 1.0, left), (-1, 1279));
        assert_eq!(absolute_position(0.5, 0.0, left), (-960, 200));
    }

    #[test]
    fn collapses_a_one_pixel_screen_onto_its_single_pixel() {
        let tiny = ScreenGeometry {
            width: 1,
            height: 1,
            origin_x: 7,
            origin_y: -3,
        };
        assert_eq!(absolute_position(0.0, 0.0, tiny), (7, -3));
        assert_eq!(absolute_position(0.5, 0.5, tiny), (7, -3));
        assert_eq!(absolute_position(1.0, 1.0, tiny), (7, -3));
    }

    #[test]
    fn clamps_out_of_range_coordinates_to_the_screen_edges() {
        assert_eq!(absolute_position(-5.0, 12.0, HD), (0, 1079));
        assert_eq!(
            absolute_position(f32::INFINITY, f32::NEG_INFINITY, HD),
            (1919, 0)
        );
    }

    #[test]
    fn folds_nan_coordinates_onto_the_origin_instead_of_propagating_them() {
        assert_eq!(absolute_position(f32::NAN, f32::NAN, HD), (0, 0));
        assert_eq!(absolute_position(f32::NAN, 1.0, HD), (0, 1079));
    }

    #[test]
    fn treats_a_zero_sized_geometry_as_the_origin_rather_than_underflowing() {
        let empty = ScreenGeometry {
            width: 0,
            height: 0,
            origin_x: 10,
            origin_y: 20,
        };
        assert_eq!(absolute_position(1.0, 1.0, empty), (10, 20));
    }

    #[test]
    fn saturates_instead_of_wrapping_when_the_origin_is_at_the_edge_of_i32() {
        let far = ScreenGeometry {
            width: 4096,
            height: 4096,
            origin_x: i32::MAX - 10,
            origin_y: i32::MIN + 10,
        };
        assert_eq!(absolute_position(1.0, 0.0, far), (i32::MAX, i32::MIN + 10));
    }

    #[test]
    fn counts_a_held_key_once_however_many_repeats_arrive() {
        let mut held = Held::default();
        // Auto-repeat sends key-down again while the key is still down.
        held.press_key(enigo::Key::LShift);
        held.press_key(enigo::Key::LShift);
        assert_eq!(held.len(), 1);
        held.release_key(enigo::Key::LShift);
        assert_eq!(held.len(), 0);
    }

    #[test]
    fn keeps_the_other_keys_held_when_one_is_released() {
        let mut held = Held::default();
        held.press_key(enigo::Key::LControl);
        held.press_key(enigo::Key::Unicode('c'));
        held.release_key(enigo::Key::Unicode('c'));
        assert_eq!(held.len(), 1);
        let (keys, _) = held.drain();
        assert_eq!(keys, vec![enigo::Key::LControl]);
    }

    #[test]
    fn drains_newest_first_so_a_modifier_is_released_after_the_key_it_modified() {
        let mut held = Held::default();
        held.press_key(enigo::Key::LControl);
        held.press_key(enigo::Key::LShift);
        held.press_key(enigo::Key::Unicode('t'));
        let (keys, _) = held.drain();
        assert_eq!(
            keys,
            vec![
                enigo::Key::Unicode('t'),
                enigo::Key::LShift,
                enigo::Key::LControl
            ]
        );
        assert_eq!(held.len(), 0);
    }

    #[test]
    fn draining_an_empty_ledger_yields_nothing_to_release() {
        let mut held = Held::default();
        let (keys, buttons) = held.drain();
        assert!(keys.is_empty() && buttons.is_empty());
        assert_eq!(held.len(), 0);
    }

    #[test]
    fn counts_keys_and_buttons_together_but_tracks_them_apart() {
        let mut held = Held::default();
        held.press_key(enigo::Key::LShift);
        held.press_button(Button::Left);
        held.press_button(Button::Left);
        assert_eq!(held.len(), 2);
        // Releasing a button must not touch the key ledger.
        held.release_button(Button::Left);
        let (keys, buttons) = held.drain();
        assert_eq!(keys, vec![enigo::Key::LShift]);
        assert!(buttons.is_empty());
    }

    #[test]
    fn releasing_a_key_that_was_never_pressed_changes_nothing() {
        let mut held = Held::default();
        held.press_key(enigo::Key::LShift);
        held.release_key(enigo::Key::RShift);
        assert_eq!(held.len(), 1);
    }

    #[test]
    fn enabling_dpi_awareness_is_idempotent() {
        // A host may call it at startup and the injector calls it again; the
        // second call must not fail or re-enter the OS.
        enable_high_dpi_awareness();
        enable_high_dpi_awareness();
    }

    #[test]
    fn maps_each_protocol_mouse_button_to_its_own_enigo_button() {
        assert_eq!(to_enigo_button(MouseButton::Left), Button::Left);
        assert_eq!(to_enigo_button(MouseButton::Middle), Button::Middle);
        assert_eq!(to_enigo_button(MouseButton::Right), Button::Right);
    }

    #[test]
    fn error_messages_name_the_input_that_caused_them() {
        let unsupported = InputError::UnsupportedKey(KeyCode::Pause).to_string();
        assert!(unsupported.contains("Pause"), "{unsupported}");
        let too_long = InputError::TextTooLong(9001).to_string();
        assert!(too_long.contains("9001"), "{too_long}");
    }

    #[test]
    fn rejects_a_text_payload_over_the_size_limit() {
        let over = "x".repeat(MAX_TEXT_BYTES + 1);
        let error = check_text(&over).expect_err("an oversized payload must be refused");
        assert!(matches!(error, InputError::TextTooLong(len) if len == MAX_TEXT_BYTES + 1));
        assert!(check_text(&"x".repeat(MAX_TEXT_BYTES)).is_ok());
        assert!(check_text("").is_ok());
    }

    #[test]
    fn measures_the_text_limit_in_bytes_so_multibyte_input_cannot_slip_past() {
        // 'é' is two bytes: a payload of MAX_TEXT_BYTES chars is over the limit.
        let multibyte = "é".repeat(MAX_TEXT_BYTES);
        assert!(multibyte.chars().count() == MAX_TEXT_BYTES);
        assert!(check_text(&multibyte).is_err());
    }

    // The tests below drive the real desktop. They need a logged-in session
    // (and granted Accessibility on macOS), so they are ignored by default:
    // run with `cargo test -p remu-input -- --ignored` on a machine you are
    // willing to have typed into.

    #[test]
    #[ignore = "moves the real cursor"]
    fn moves_the_cursor_to_the_centre_of_the_primary_display() {
        let screen = primary_geometry().expect("a desktop session");
        let mut injector = InputInjector::new().expect("permission to inject input");
        injector
            .inject(&InputEvent::MouseMove { x: 0.5, y: 0.5 }, screen)
            .expect("the move to be accepted");
    }

    #[test]
    #[ignore = "presses real keys"]
    fn release_all_frees_a_modifier_left_held_by_a_dropped_session() {
        let screen = primary_geometry().expect("a desktop session");
        let mut injector = InputInjector::new().expect("permission to inject input");
        injector
            .inject(
                &InputEvent::KeyDown {
                    code: KeyCode::ShiftLeft,
                    modifiers: KeyModifiers::NONE,
                },
                screen,
            )
            .expect("the press to be accepted");
        assert_eq!(injector.held_keys(), 1);
        injector.release_all().expect("the release to be accepted");
        assert_eq!(injector.held_keys(), 0);
        // A second pass has nothing to do and must still succeed.
        injector
            .release_all()
            .expect("an empty release to be a no-op");
    }
}
