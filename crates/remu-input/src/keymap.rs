//! Protocol key codes to `enigo` keys.
//!
//! [`to_enigo`] is one exhaustive `match` with no wildcard arm on purpose: a
//! new variant in [`remu_proto::KeyCode`] must break this build rather than
//! quietly become a key the host ignores.
//!
//! Two kinds of arm return `None`: keys that do not exist on the running
//! platform (macOS has no Pause key), and keys enigo has no way to express
//! there. Both are listed in [`to_enigo`]'s documentation and pinned by test.

use enigo::Key;
use remu_proto::KeyCode;

/// Translates a protocol key code into the key enigo should press.
///
/// Letters, digits and punctuation become [`Key::Unicode`]: the protocol names
/// physical positions by their US legend, and enigo resolves a character back
/// to whatever key produces it under the host's active layout, which is what a
/// remote user actually wants when the host is not on a US keyboard.
///
/// # Substitutions
///
/// - [`KeyCode::NumpadEnter`] presses the main Return key. Enigo exposes no
///   keypad-enter key on any backend; applications that distinguish the two are
///   rare, and losing Enter entirely would be worse.
/// - On macOS, [`KeyCode::Insert`] presses Help, which is the same physical key
///   on that platform — macOS reports an external PC keyboard's Insert as Help.
/// - On Linux, [`KeyCode::AltRight`] and [`KeyCode::MetaRight`] press the left
///   key of the same pair. Enigo's X11/Wayland key list has no `Alt_R` or
///   `Super_R`, so the alternative is dropping the key; the cost is that AltGr
///   behaves as a plain Alt on a Linux host.
///
/// # Returns `None`
///
/// On macOS: `NumLock`, `F21`–`F24`, `PrintScreen`, `ScrollLock`, `Pause` and
/// `Menu`, none of which exist on an Apple keyboard or in enigo's macOS key
/// table. On Windows and Linux every key code maps.
#[allow(clippy::too_many_lines)] // One arm per protocol key; splitting it up would only hide the exhaustiveness.
pub fn to_enigo(code: KeyCode) -> Option<Key> {
    let key = match code {
        KeyCode::A => Key::Unicode('a'),
        KeyCode::B => Key::Unicode('b'),
        KeyCode::C => Key::Unicode('c'),
        KeyCode::D => Key::Unicode('d'),
        KeyCode::E => Key::Unicode('e'),
        KeyCode::F => Key::Unicode('f'),
        KeyCode::G => Key::Unicode('g'),
        KeyCode::H => Key::Unicode('h'),
        KeyCode::I => Key::Unicode('i'),
        KeyCode::J => Key::Unicode('j'),
        KeyCode::K => Key::Unicode('k'),
        KeyCode::L => Key::Unicode('l'),
        KeyCode::M => Key::Unicode('m'),
        KeyCode::N => Key::Unicode('n'),
        KeyCode::O => Key::Unicode('o'),
        KeyCode::P => Key::Unicode('p'),
        KeyCode::Q => Key::Unicode('q'),
        KeyCode::R => Key::Unicode('r'),
        KeyCode::S => Key::Unicode('s'),
        KeyCode::T => Key::Unicode('t'),
        KeyCode::U => Key::Unicode('u'),
        KeyCode::V => Key::Unicode('v'),
        KeyCode::W => Key::Unicode('w'),
        KeyCode::X => Key::Unicode('x'),
        KeyCode::Y => Key::Unicode('y'),
        KeyCode::Z => Key::Unicode('z'),

        KeyCode::Num0 => Key::Unicode('0'),
        KeyCode::Num1 => Key::Unicode('1'),
        KeyCode::Num2 => Key::Unicode('2'),
        KeyCode::Num3 => Key::Unicode('3'),
        KeyCode::Num4 => Key::Unicode('4'),
        KeyCode::Num5 => Key::Unicode('5'),
        KeyCode::Num6 => Key::Unicode('6'),
        KeyCode::Num7 => Key::Unicode('7'),
        KeyCode::Num8 => Key::Unicode('8'),
        KeyCode::Num9 => Key::Unicode('9'),

        // The keypad is its own set of keys, not the number row: Num Lock off
        // makes Numpad4 an arrow, and games bind the two separately.
        KeyCode::Numpad0 => Key::Numpad0,
        KeyCode::Numpad1 => Key::Numpad1,
        KeyCode::Numpad2 => Key::Numpad2,
        KeyCode::Numpad3 => Key::Numpad3,
        KeyCode::Numpad4 => Key::Numpad4,
        KeyCode::Numpad5 => Key::Numpad5,
        KeyCode::Numpad6 => Key::Numpad6,
        KeyCode::Numpad7 => Key::Numpad7,
        KeyCode::Numpad8 => Key::Numpad8,
        KeyCode::Numpad9 => Key::Numpad9,
        KeyCode::NumpadAdd => Key::Add,
        KeyCode::NumpadSubtract => Key::Subtract,
        KeyCode::NumpadMultiply => Key::Multiply,
        KeyCode::NumpadDivide => Key::Divide,
        KeyCode::NumpadDecimal => Key::Decimal,
        KeyCode::NumpadEnter => Key::Return,
        #[cfg(not(target_os = "macos"))]
        KeyCode::NumLock => Key::Numlock,
        #[cfg(target_os = "macos")]
        KeyCode::NumLock => return None,

        KeyCode::F1 => Key::F1,
        KeyCode::F2 => Key::F2,
        KeyCode::F3 => Key::F3,
        KeyCode::F4 => Key::F4,
        KeyCode::F5 => Key::F5,
        KeyCode::F6 => Key::F6,
        KeyCode::F7 => Key::F7,
        KeyCode::F8 => Key::F8,
        KeyCode::F9 => Key::F9,
        KeyCode::F10 => Key::F10,
        KeyCode::F11 => Key::F11,
        KeyCode::F12 => Key::F12,
        KeyCode::F13 => Key::F13,
        KeyCode::F14 => Key::F14,
        KeyCode::F15 => Key::F15,
        KeyCode::F16 => Key::F16,
        KeyCode::F17 => Key::F17,
        KeyCode::F18 => Key::F18,
        KeyCode::F19 => Key::F19,
        KeyCode::F20 => Key::F20,
        #[cfg(not(target_os = "macos"))]
        KeyCode::F21 => Key::F21,
        #[cfg(not(target_os = "macos"))]
        KeyCode::F22 => Key::F22,
        #[cfg(not(target_os = "macos"))]
        KeyCode::F23 => Key::F23,
        #[cfg(not(target_os = "macos"))]
        KeyCode::F24 => Key::F24,
        #[cfg(target_os = "macos")]
        KeyCode::F21 | KeyCode::F22 | KeyCode::F23 | KeyCode::F24 => return None,

        KeyCode::Escape => Key::Escape,
        KeyCode::Tab => Key::Tab,
        KeyCode::CapsLock => Key::CapsLock,
        KeyCode::Space => Key::Space,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Enter => Key::Return,
        // macOS has no Insert; the key in that position on a PC keyboard
        // arrives as Help, and enigo's Help maps to that same key code.
        #[cfg(target_os = "macos")]
        KeyCode::Insert => Key::Help,
        #[cfg(not(target_os = "macos"))]
        KeyCode::Insert => Key::Insert,
        KeyCode::Delete => Key::Delete,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::ArrowUp => Key::UpArrow,
        KeyCode::ArrowDown => Key::DownArrow,
        KeyCode::ArrowLeft => Key::LeftArrow,
        KeyCode::ArrowRight => Key::RightArrow,
        #[cfg(not(target_os = "macos"))]
        KeyCode::PrintScreen => Key::PrintScr,
        #[cfg(target_os = "windows")]
        KeyCode::ScrollLock => Key::Scroll,
        #[cfg(all(unix, not(target_os = "macos")))]
        KeyCode::ScrollLock => Key::ScrollLock,
        #[cfg(not(target_os = "macos"))]
        KeyCode::Pause => Key::Pause,
        // Enigo's Windows table calls the context-menu key Apps; its Linux
        // table calls it LMenu, which maps to the Menu keysym (not to Alt).
        #[cfg(target_os = "windows")]
        KeyCode::Menu => Key::Apps,
        #[cfg(all(unix, not(target_os = "macos")))]
        KeyCode::Menu => Key::LMenu,
        #[cfg(target_os = "macos")]
        KeyCode::PrintScreen | KeyCode::ScrollLock | KeyCode::Pause | KeyCode::Menu => return None,

        KeyCode::Minus => Key::Unicode('-'),
        KeyCode::Equal => Key::Unicode('='),
        KeyCode::BracketLeft => Key::Unicode('['),
        KeyCode::BracketRight => Key::Unicode(']'),
        KeyCode::Backslash => Key::Unicode('\\'),
        KeyCode::Semicolon => Key::Unicode(';'),
        KeyCode::Quote => Key::Unicode('\''),
        KeyCode::Backquote => Key::Unicode('`'),
        KeyCode::Comma => Key::Unicode(','),
        KeyCode::Period => Key::Unicode('.'),
        KeyCode::Slash => Key::Unicode('/'),

        KeyCode::ShiftLeft => Key::LShift,
        KeyCode::ShiftRight => Key::RShift,
        KeyCode::ControlLeft => Key::LControl,
        KeyCode::ControlRight => Key::RControl,
        // Key::Alt and Key::Option are the same arm in enigo on every backend.
        KeyCode::AltLeft => Key::Alt,
        #[cfg(target_os = "macos")]
        KeyCode::AltRight => Key::ROption,
        #[cfg(target_os = "windows")]
        KeyCode::AltRight => Key::RMenu,
        #[cfg(all(unix, not(target_os = "macos")))]
        KeyCode::AltRight => Key::Alt,
        // Key::Meta is enigo's cross-platform left Command / Windows / Super.
        KeyCode::MetaLeft => Key::Meta,
        #[cfg(target_os = "macos")]
        KeyCode::MetaRight => Key::RCommand,
        #[cfg(target_os = "windows")]
        KeyCode::MetaRight => Key::RWin,
        #[cfg(all(unix, not(target_os = "macos")))]
        KeyCode::MetaRight => Key::Meta,
    };
    Some(key)
}

/// Key codes [`to_enigo`] cannot express on the platform being compiled for.
///
/// Public so a host UI can grey out keys it will not be able to deliver, and so
/// the test below can pin the list instead of letting it grow unnoticed.
pub const UNSUPPORTED: &[KeyCode] = &[
    #[cfg(target_os = "macos")]
    KeyCode::NumLock,
    #[cfg(target_os = "macos")]
    KeyCode::F21,
    #[cfg(target_os = "macos")]
    KeyCode::F22,
    #[cfg(target_os = "macos")]
    KeyCode::F23,
    #[cfg(target_os = "macos")]
    KeyCode::F24,
    #[cfg(target_os = "macos")]
    KeyCode::PrintScreen,
    #[cfg(target_os = "macos")]
    KeyCode::ScrollLock,
    #[cfg(target_os = "macos")]
    KeyCode::Pause,
    #[cfg(target_os = "macos")]
    KeyCode::Menu,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_every_key_code_except_the_documented_platform_gaps() {
        let missing: Vec<KeyCode> = KeyCode::ALL
            .iter()
            .copied()
            .filter(|code| to_enigo(*code).is_none())
            .collect();
        // Pinned rather than counted: a gap that appears on a platform where
        // the key does exist is a regression, not a new line in a list.
        assert_eq!(missing, UNSUPPORTED);
    }

    #[test]
    fn maps_left_and_right_modifiers_to_different_keys_where_the_os_can_tell_them_apart() {
        assert_ne!(to_enigo(KeyCode::ShiftLeft), to_enigo(KeyCode::ShiftRight));
        assert_ne!(
            to_enigo(KeyCode::ControlLeft),
            to_enigo(KeyCode::ControlRight)
        );

        #[cfg(not(all(unix, not(target_os = "macos"))))]
        {
            assert_ne!(to_enigo(KeyCode::AltLeft), to_enigo(KeyCode::AltRight));
            assert_ne!(to_enigo(KeyCode::MetaLeft), to_enigo(KeyCode::MetaRight));
        }
        // Linux is the documented exception: enigo has no right-hand Alt or
        // Super, so both sides collapse onto the left key.
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            assert_eq!(to_enigo(KeyCode::AltLeft), to_enigo(KeyCode::AltRight));
            assert_eq!(to_enigo(KeyCode::MetaLeft), to_enigo(KeyCode::MetaRight));
        }
    }

    #[test]
    fn maps_letters_digits_and_punctuation_to_layout_resolved_characters() {
        assert_eq!(to_enigo(KeyCode::A), Some(Key::Unicode('a')));
        assert_eq!(to_enigo(KeyCode::Z), Some(Key::Unicode('z')));
        assert_eq!(to_enigo(KeyCode::Num7), Some(Key::Unicode('7')));
        assert_eq!(to_enigo(KeyCode::Slash), Some(Key::Unicode('/')));
        assert_eq!(to_enigo(KeyCode::Backslash), Some(Key::Unicode('\\')));
        assert_eq!(to_enigo(KeyCode::Quote), Some(Key::Unicode('\'')));
    }

    #[test]
    fn keeps_the_keypad_distinct_from_the_number_row() {
        assert_eq!(to_enigo(KeyCode::Numpad7), Some(Key::Numpad7));
        assert_ne!(to_enigo(KeyCode::Numpad7), to_enigo(KeyCode::Num7));
        assert_eq!(to_enigo(KeyCode::NumpadAdd), Some(Key::Add));
        assert_ne!(to_enigo(KeyCode::NumpadDecimal), to_enigo(KeyCode::Period));
    }

    #[test]
    fn keeps_backspace_and_forward_delete_apart() {
        // Swapping these is the classic macOS port bug: enigo's Backspace is
        // CGKeyCode DELETE and its Delete is FORWARD_DELETE.
        assert_eq!(to_enigo(KeyCode::Backspace), Some(Key::Backspace));
        assert_eq!(to_enigo(KeyCode::Delete), Some(Key::Delete));
        assert_ne!(to_enigo(KeyCode::Backspace), to_enigo(KeyCode::Delete));
    }

    #[test]
    fn assigns_each_enigo_key_to_a_single_key_code_apart_from_documented_collapses() {
        // Guards a 116-arm match against copy-paste: two key codes sharing one
        // enigo key means one of them presses the wrong thing.
        let mut collisions: Vec<(KeyCode, KeyCode)> = Vec::new();
        let mapped: Vec<(KeyCode, Key)> = KeyCode::ALL
            .iter()
            .filter_map(|code| to_enigo(*code).map(|key| (*code, key)))
            .collect();
        for (i, (code, key)) in mapped.iter().enumerate() {
            for (other_code, other_key) in &mapped[i + 1..] {
                if key == other_key {
                    collisions.push((*code, *other_code));
                }
            }
        }

        let mut expected = vec![(KeyCode::NumpadEnter, KeyCode::Enter)];
        if cfg!(all(unix, not(target_os = "macos"))) {
            expected.push((KeyCode::AltLeft, KeyCode::AltRight));
            expected.push((KeyCode::MetaLeft, KeyCode::MetaRight));
        }
        assert_eq!(collisions, expected);
    }
}
