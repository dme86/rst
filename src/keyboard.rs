//! Encode winit keys as st/xterm-compatible PTY bytes.
//! Reference: https://git.suckless.org/st/file/config.def.h.html

use winit::keyboard::{Key, KeyCode, KeyLocation, ModifiersState, NamedKey, PhysicalKey};

#[derive(Clone, Copy, Debug, Default)]
pub struct KeyboardModes {
    pub application_cursor: bool,
    pub application_keypad: bool,
}

fn modifier_parameter(mods: ModifiersState) -> u8 {
    1 + u8::from(mods.shift_key())
        + 2 * u8::from(mods.alt_key())
        + 4 * u8::from(mods.control_key())
}

fn csi_letter(letter: char, modifier: u8, application_cursor: bool) -> Vec<u8> {
    if modifier != 1 {
        format!("\x1b[1;{modifier}{letter}").into_bytes()
    } else if application_cursor {
        format!("\x1bO{letter}").into_bytes()
    } else {
        format!("\x1b[{letter}").into_bytes()
    }
}

fn csi_tilde(number: u8, modifier: u8) -> Vec<u8> {
    if modifier == 1 {
        format!("\x1b[{number}~").into_bytes()
    } else {
        format!("\x1b[{number};{modifier}~").into_bytes()
    }
}

fn with_alt(mut bytes: Vec<u8>, alt: bool) -> Vec<u8> {
    if alt && !bytes.is_empty() {
        bytes.insert(0, 0x1b);
    }
    bytes
}

fn control_character(ch: char) -> Option<u8> {
    match ch.to_ascii_lowercase() {
        'a'..='z' => Some(ch.to_ascii_lowercase() as u8 - b'a' + 1),
        ' ' | '@' | '2' | '\u{60}' => Some(0),
        '[' | '3' => Some(27),
        '\\' | '4' => Some(28),
        ']' | '5' => Some(29),
        '^' | '6' => Some(30),
        '_' | '-' | '7' => Some(31),
        '?' | '8' => Some(127),
        _ => None,
    }
}

fn keypad_code(key: KeyCode) -> Option<char> {
    Some(match key {
        KeyCode::Numpad0 => 'p', KeyCode::Numpad1 => 'q',
        KeyCode::Numpad2 => 'r', KeyCode::Numpad3 => 's',
        KeyCode::Numpad4 => 't', KeyCode::Numpad5 => 'u',
        KeyCode::Numpad6 => 'v', KeyCode::Numpad7 => 'w',
        KeyCode::Numpad8 => 'x', KeyCode::Numpad9 => 'y',
        KeyCode::NumpadMultiply => 'j',
        KeyCode::NumpadAdd => 'k',
        KeyCode::NumpadSubtract => 'm',
        KeyCode::NumpadDecimal => 'n',
        KeyCode::NumpadDivide => 'o',
        KeyCode::NumpadEnter => 'M',
        KeyCode::NumpadEqual => 'X',
        _ => return None,
    })
}

fn function_number(key: NamedKey) -> Option<usize> {
    Some(match key {
        NamedKey::F1 => 1, NamedKey::F2 => 2, NamedKey::F3 => 3,
        NamedKey::F4 => 4, NamedKey::F5 => 5, NamedKey::F6 => 6,
        NamedKey::F7 => 7, NamedKey::F8 => 8, NamedKey::F9 => 9,
        NamedKey::F10 => 10, NamedKey::F11 => 11, NamedKey::F12 => 12,
        NamedKey::F13 => 13, NamedKey::F14 => 14, NamedKey::F15 => 15,
        NamedKey::F16 => 16, NamedKey::F17 => 17, NamedKey::F18 => 18,
        NamedKey::F19 => 19, NamedKey::F20 => 20, NamedKey::F21 => 21,
        NamedKey::F22 => 22, NamedKey::F23 => 23, NamedKey::F24 => 24,
        NamedKey::F25 => 25, NamedKey::F26 => 26, NamedKey::F27 => 27,
        NamedKey::F28 => 28, NamedKey::F29 => 29, NamedKey::F30 => 30,
        NamedKey::F31 => 31, NamedKey::F32 => 32, NamedKey::F33 => 33,
        NamedKey::F34 => 34, NamedKey::F35 => 35,
        _ => return None,
    })
}

fn function_key(number: usize, modifier: u8) -> Vec<u8> {
    // st treats F13-F24 as Shift+F1-F12 and F25-F35 as Ctrl+F1-F11.
    let (base, inherent) = match number {
        1..=12 => (number, 1),
        13..=24 => (number - 12, 2),
        25..=35 => (number - 24, 5),
        _ => return Vec::new(),
    };
    let modifier = if modifier == 1 { inherent } else { modifier };
    if base <= 4 {
        let letter = ['P', 'Q', 'R', 'S'][base - 1];
        if modifier == 1 {
            format!("\x1bO{letter}").into_bytes()
        } else {
            format!("\x1b[1;{modifier}{letter}").into_bytes()
        }
    } else {
        csi_tilde([15, 17, 18, 19, 20, 21, 23, 24][base - 5], modifier)
    }
}
