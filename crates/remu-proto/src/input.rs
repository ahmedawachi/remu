//! Platform-neutral input events.
//!
//! The controller translates whatever its GUI toolkit reports into these; the
//! host translates these into native injection calls. Neither side ever sees
//! the other's key representation, so a Linux controller can drive a Windows
//! host without either knowing the other's scancode table.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyModifiers {
    #[serde(default)]
    pub shift: bool,
    #[serde(default)]
    pub ctrl: bool,
    #[serde(default)]
    pub alt: bool,
    /// Command on macOS, Windows key elsewhere.
    #[serde(default)]
    pub meta: bool,
}

impl KeyModifiers {
    pub const NONE: Self = Self {
        shift: false,
        ctrl: false,
        alt: false,
        meta: false,
    };

    pub fn any(&self) -> bool {
        self.shift || self.ctrl || self.alt || self.meta
    }
}

/// Physical keys, named by their US-QWERTY legend but meaning the *position*.
///
/// Modifiers are left/right distinct: collapsing them loses AltGr on European
/// layouts, and a host that released the wrong one would strand the other in a
/// held state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum KeyCode {
    // Letters
    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
    I,
    J,
    K,
    L,
    M,
    N,
    O,
    P,
    Q,
    R,
    S,
    T,
    U,
    V,
    W,
    X,
    Y,
    Z,
    // Number row
    Num0,
    Num1,
    Num2,
    Num3,
    Num4,
    Num5,
    Num6,
    Num7,
    Num8,
    Num9,
    // Numeric keypad
    Numpad0,
    Numpad1,
    Numpad2,
    Numpad3,
    Numpad4,
    Numpad5,
    Numpad6,
    Numpad7,
    Numpad8,
    Numpad9,
    NumpadAdd,
    NumpadSubtract,
    NumpadMultiply,
    NumpadDivide,
    NumpadDecimal,
    NumpadEnter,
    NumLock,
    // Function row
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
    F13,
    F14,
    F15,
    F16,
    F17,
    F18,
    F19,
    F20,
    F21,
    F22,
    F23,
    F24,
    // Editing and navigation
    Escape,
    Tab,
    CapsLock,
    Space,
    Backspace,
    Enter,
    Insert,
    Delete,
    Home,
    End,
    PageUp,
    PageDown,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    PrintScreen,
    ScrollLock,
    Pause,
    Menu,
    // Punctuation
    Minus,
    Equal,
    BracketLeft,
    BracketRight,
    Backslash,
    Semicolon,
    Quote,
    Backquote,
    Comma,
    Period,
    Slash,
    // Modifiers
    ShiftLeft,
    ShiftRight,
    ControlLeft,
    ControlRight,
    AltLeft,
    AltRight,
    MetaLeft,
    MetaRight,
}

impl KeyCode {
    pub fn is_modifier(self) -> bool {
        matches!(
            self,
            KeyCode::ShiftLeft
                | KeyCode::ShiftRight
                | KeyCode::ControlLeft
                | KeyCode::ControlRight
                | KeyCode::AltLeft
                | KeyCode::AltRight
                | KeyCode::MetaLeft
                | KeyCode::MetaRight
        )
    }

    /// Every variant, so a host's key table can be proven exhaustive by test
    /// rather than by reading it.
    pub const ALL: &'static [KeyCode] = &[
        KeyCode::A,
        KeyCode::B,
        KeyCode::C,
        KeyCode::D,
        KeyCode::E,
        KeyCode::F,
        KeyCode::G,
        KeyCode::H,
        KeyCode::I,
        KeyCode::J,
        KeyCode::K,
        KeyCode::L,
        KeyCode::M,
        KeyCode::N,
        KeyCode::O,
        KeyCode::P,
        KeyCode::Q,
        KeyCode::R,
        KeyCode::S,
        KeyCode::T,
        KeyCode::U,
        KeyCode::V,
        KeyCode::W,
        KeyCode::X,
        KeyCode::Y,
        KeyCode::Z,
        KeyCode::Num0,
        KeyCode::Num1,
        KeyCode::Num2,
        KeyCode::Num3,
        KeyCode::Num4,
        KeyCode::Num5,
        KeyCode::Num6,
        KeyCode::Num7,
        KeyCode::Num8,
        KeyCode::Num9,
        KeyCode::Numpad0,
        KeyCode::Numpad1,
        KeyCode::Numpad2,
        KeyCode::Numpad3,
        KeyCode::Numpad4,
        KeyCode::Numpad5,
        KeyCode::Numpad6,
        KeyCode::Numpad7,
        KeyCode::Numpad8,
        KeyCode::Numpad9,
        KeyCode::NumpadAdd,
        KeyCode::NumpadSubtract,
        KeyCode::NumpadMultiply,
        KeyCode::NumpadDivide,
        KeyCode::NumpadDecimal,
        KeyCode::NumpadEnter,
        KeyCode::NumLock,
        KeyCode::F1,
        KeyCode::F2,
        KeyCode::F3,
        KeyCode::F4,
        KeyCode::F5,
        KeyCode::F6,
        KeyCode::F7,
        KeyCode::F8,
        KeyCode::F9,
        KeyCode::F10,
        KeyCode::F11,
        KeyCode::F12,
        KeyCode::F13,
        KeyCode::F14,
        KeyCode::F15,
        KeyCode::F16,
        KeyCode::F17,
        KeyCode::F18,
        KeyCode::F19,
        KeyCode::F20,
        KeyCode::F21,
        KeyCode::F22,
        KeyCode::F23,
        KeyCode::F24,
        KeyCode::Escape,
        KeyCode::Tab,
        KeyCode::CapsLock,
        KeyCode::Space,
        KeyCode::Backspace,
        KeyCode::Enter,
        KeyCode::Insert,
        KeyCode::Delete,
        KeyCode::Home,
        KeyCode::End,
        KeyCode::PageUp,
        KeyCode::PageDown,
        KeyCode::ArrowUp,
        KeyCode::ArrowDown,
        KeyCode::ArrowLeft,
        KeyCode::ArrowRight,
        KeyCode::PrintScreen,
        KeyCode::ScrollLock,
        KeyCode::Pause,
        KeyCode::Menu,
        KeyCode::Minus,
        KeyCode::Equal,
        KeyCode::BracketLeft,
        KeyCode::BracketRight,
        KeyCode::Backslash,
        KeyCode::Semicolon,
        KeyCode::Quote,
        KeyCode::Backquote,
        KeyCode::Comma,
        KeyCode::Period,
        KeyCode::Slash,
        KeyCode::ShiftLeft,
        KeyCode::ShiftRight,
        KeyCode::ControlLeft,
        KeyCode::ControlRight,
        KeyCode::AltLeft,
        KeyCode::AltRight,
        KeyCode::MetaLeft,
        KeyCode::MetaRight,
    ];
}

/// One input action, as produced by the controller.
///
/// Pointer positions are normalized to `0.0..=1.0` of the *shared display*, not
/// pixels: the two machines rarely share a resolution, and normalizing at the
/// source means the host never needs to know the controller's window size.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum InputEvent {
    MouseMove {
        x: f32,
        y: f32,
    },
    MouseDown {
        button: MouseButton,
        x: f32,
        y: f32,
    },
    MouseUp {
        button: MouseButton,
        x: f32,
        y: f32,
    },
    MouseClick {
        button: MouseButton,
        x: f32,
        y: f32,
        #[serde(default)]
        double: bool,
    },
    /// Scroll in notches; positive `dy` scrolls down, positive `dx` scrolls right.
    Wheel {
        dx: f32,
        dy: f32,
    },
    KeyDown {
        code: KeyCode,
        #[serde(default)]
        modifiers: KeyModifiers,
    },
    KeyUp {
        code: KeyCode,
        #[serde(default)]
        modifiers: KeyModifiers,
    },
    /// Literal text, for IME and pasted input that has no single keycode.
    Text {
        value: String,
    },
    /// Release every key and button the host currently holds down.
    ///
    /// The Electron original had no such event, so ending a session while a
    /// modifier was held left it stuck down on the host until a human pressed
    /// and released it locally. The controller sends this on session end and
    /// whenever its window loses focus.
    ReleaseAll,
}

impl InputEvent {
    /// Clamps pointer coordinates into `0.0..=1.0`.
    ///
    /// Applied on the host, never trusting the controller: a hostile or buggy
    /// peer that sent `x: 1e9` would otherwise drive the cursor off-screen or
    /// overflow the host's pixel conversion.
    pub fn sanitized(self) -> Self {
        fn c(v: f32) -> f32 {
            if v.is_nan() {
                0.0
            } else {
                v.clamp(0.0, 1.0)
            }
        }
        fn s(v: f32) -> f32 {
            if v.is_nan() {
                0.0
            } else {
                v.clamp(-64.0, 64.0)
            }
        }
        match self {
            InputEvent::MouseMove { x, y } => InputEvent::MouseMove { x: c(x), y: c(y) },
            InputEvent::MouseDown { button, x, y } => InputEvent::MouseDown {
                button,
                x: c(x),
                y: c(y),
            },
            InputEvent::MouseUp { button, x, y } => InputEvent::MouseUp {
                button,
                x: c(x),
                y: c(y),
            },
            InputEvent::MouseClick {
                button,
                x,
                y,
                double,
            } => InputEvent::MouseClick {
                button,
                x: c(x),
                y: c(y),
                double,
            },
            InputEvent::Wheel { dx, dy } => InputEvent::Wheel {
                dx: s(dx),
                dy: s(dy),
            },
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamps_out_of_range_pointer_coordinates() {
        let ev = InputEvent::MouseMove { x: 9.0, y: -3.0 }.sanitized();
        assert_eq!(ev, InputEvent::MouseMove { x: 1.0, y: 0.0 });
    }

    #[test]
    fn maps_nan_coordinates_to_zero_rather_than_propagating_them() {
        let ev = InputEvent::MouseDown {
            button: MouseButton::Left,
            x: f32::NAN,
            y: f32::INFINITY,
        }
        .sanitized();
        assert_eq!(
            ev,
            InputEvent::MouseDown {
                button: MouseButton::Left,
                x: 0.0,
                y: 1.0
            }
        );
    }

    #[test]
    fn bounds_wheel_deltas_so_one_event_cannot_scroll_forever() {
        let ev = InputEvent::Wheel { dx: 1e9, dy: -1e9 }.sanitized();
        assert_eq!(
            ev,
            InputEvent::Wheel {
                dx: 64.0,
                dy: -64.0
            }
        );
    }

    #[test]
    fn text_and_release_all_pass_through_sanitization_unchanged() {
        let text = InputEvent::Text {
            value: "hello".into(),
        };
        assert_eq!(text.clone().sanitized(), text);
        assert_eq!(InputEvent::ReleaseAll.sanitized(), InputEvent::ReleaseAll);
    }

    #[test]
    fn round_trips_every_event_shape_through_json() {
        let cases = vec![
            InputEvent::MouseMove { x: 0.5, y: 0.25 },
            InputEvent::MouseDown {
                button: MouseButton::Right,
                x: 0.1,
                y: 0.2,
            },
            InputEvent::MouseUp {
                button: MouseButton::Middle,
                x: 0.0,
                y: 1.0,
            },
            InputEvent::MouseClick {
                button: MouseButton::Left,
                x: 0.3,
                y: 0.4,
                double: true,
            },
            InputEvent::Wheel { dx: -1.0, dy: 3.0 },
            InputEvent::KeyDown {
                code: KeyCode::F13,
                modifiers: KeyModifiers {
                    shift: true,
                    ..KeyModifiers::NONE
                },
            },
            InputEvent::KeyUp {
                code: KeyCode::MetaRight,
                modifiers: KeyModifiers::NONE,
            },
            InputEvent::Text {
                value: "héllo 👋".into(),
            },
            InputEvent::ReleaseAll,
        ];
        for case in cases {
            let json = serde_json::to_string(&case).unwrap();
            let back: InputEvent = serde_json::from_str(&json).unwrap();
            assert_eq!(back, case, "round trip failed for {json}");
        }
    }

    #[test]
    fn all_lists_every_keycode_exactly_once() {
        let mut seen = std::collections::HashSet::new();
        for key in KeyCode::ALL {
            assert!(seen.insert(*key), "{key:?} appears twice in KeyCode::ALL");
        }
        // Guards ALL against duplicates and against silently losing an entry.
        // The complementary guard -- a variant added to the enum but never to
        // ALL -- lives in remu-input, whose key table matches on KeyCode
        // exhaustively and so fails to compile when a new variant appears.
        assert_eq!(seen.len(), 116);
    }

    #[test]
    fn identifies_exactly_the_eight_modifier_keys() {
        let mods: Vec<_> = KeyCode::ALL.iter().filter(|k| k.is_modifier()).collect();
        assert_eq!(mods.len(), 8);
    }
}
