use super::*;

fn named(key: NamedKey, mods: ModifiersState, modes: KeyboardModes) -> Vec<u8> {
    encode_key(
        &Key::Named(key),
        PhysicalKey::Code(KeyCode::F1),
        KeyLocation::Standard,
        None,
        mods,
        modes,
    )
}

fn text_key(s: &str, mods: ModifiersState) -> Vec<u8> {
    encode_key(
        &Key::Character(s),
        PhysicalKey::Code(KeyCode::KeyA),
        KeyLocation::Standard,
        None,
        mods,
        KeyboardModes::default(),
    )
}

#[test]
fn st_function_keys_f1_to_f12() {
    let cases: [(NamedKey, &[u8]); 12] = [
        (NamedKey::F1, b"\x1bOP"),
        (NamedKey::F2, b"\x1bOQ"),
        (NamedKey::F3, b"\x1bOR"),
        (NamedKey::F4, b"\x1bOS"),
        (NamedKey::F5, b"\x1b[15~"),
        (NamedKey::F6, b"\x1b[17~"),
        (NamedKey::F7, b"\x1b[18~"),
        (NamedKey::F8, b"\x1b[19~"),
        (NamedKey::F9, b"\x1b[20~"),
        (NamedKey::F10, b"\x1b[21~"),
        (NamedKey::F11, b"\x1b[23~"),
        (NamedKey::F12, b"\x1b[24~"),
    ];
    for (key, expected) in cases {
        assert_eq!(named(key, ModifiersState::empty(), KeyboardModes::default()), expected);
    }
}

#[test]
fn all_seven_arrow_modifier_combinations() {
    let cases = [
        (ModifiersState::SHIFT, 2),
        (ModifiersState::ALT, 3),
        (ModifiersState::SHIFT | ModifiersState::ALT, 4),
        (ModifiersState::CONTROL, 5),
        (ModifiersState::SHIFT | ModifiersState::CONTROL, 6),
        (ModifiersState::ALT | ModifiersState::CONTROL, 7),
        (ModifiersState::SHIFT | ModifiersState::ALT | ModifiersState::CONTROL, 8),
    ];
    for (mods, n) in cases {
        for (key, letter) in [
            (NamedKey::ArrowUp, 'A'),
            (NamedKey::ArrowDown, 'B'),
            (NamedKey::ArrowRight, 'C'),
            (NamedKey::ArrowLeft, 'D'),
        ] {
            assert_eq!(
                named(key, mods, KeyboardModes::default()),
                format!("\x1b[1;{n}{letter}").as_bytes()
            );
        }
    }
}

#[test]
fn application_cursor_and_navigation() {
    let app = KeyboardModes { application_cursor: true, application_keypad: false };
    let normal = KeyboardModes::default();
    assert_eq!(named(NamedKey::ArrowUp, ModifiersState::empty(), app), b"\x1bOA");
    assert_eq!(named(NamedKey::ArrowUp, ModifiersState::empty(), normal), b"\x1b[A");
    assert_eq!(named(NamedKey::Home, ModifiersState::empty(), app), b"\x1b[1~");
    assert_eq!(named(NamedKey::Home, ModifiersState::empty(), normal), b"\x1b[H");
    assert_eq!(named(NamedKey::End, ModifiersState::empty(), normal), b"\x1b[4~");
    assert_eq!(named(NamedKey::End, ModifiersState::CONTROL, app), b"\x1b[1;5F");
    assert_eq!(named(NamedKey::ArrowLeft, ModifiersState::CONTROL, app), b"\x1b[1;5D");
}

#[test]
fn insert_delete_page_keys_and_reverse_tab() {
    let modes = KeyboardModes::default();
    assert_eq!(named(NamedKey::Insert, ModifiersState::empty(), modes), b"\x1b[2~");
    assert_eq!(named(NamedKey::Insert, ModifiersState::CONTROL, modes), b"\x1b[2;5~");
    assert_eq!(named(NamedKey::Delete, ModifiersState::empty(), modes), b"\x1b[3~");
    assert_eq!(named(NamedKey::Delete, ModifiersState::SHIFT, modes), b"\x1b[3;2~");
    assert_eq!(named(NamedKey::PageUp, ModifiersState::CONTROL, modes), b"\x1b[5;5~");
    assert_eq!(named(NamedKey::PageDown, ModifiersState::SHIFT, modes), b"\x1b[6;2~");
    assert_eq!(named(NamedKey::Tab, ModifiersState::empty(), modes), b"\t");
    assert_eq!(named(NamedKey::Tab, ModifiersState::SHIFT, modes), b"\x1b[Z");
    assert_eq!(
        named(NamedKey::Tab, ModifiersState::SHIFT | ModifiersState::CONTROL, modes),
        b"\x1b[1;6Z"
    );
}

#[test]
fn modified_and_extended_function_keys() {
    let modes = KeyboardModes::default();
    assert_eq!(named(NamedKey::F1, ModifiersState::SHIFT, modes), b"\x1b[1;2P");
    assert_eq!(named(NamedKey::F2, ModifiersState::CONTROL, modes), b"\x1b[1;5Q");
    assert_eq!(named(NamedKey::F4, ModifiersState::ALT, modes), b"\x1b[1;3S");
    assert_eq!(
        named(NamedKey::F12, ModifiersState::SHIFT | ModifiersState::ALT, modes),
        b"\x1b[24;4~"
    );
    assert_eq!(named(NamedKey::F13, ModifiersState::empty(), modes), b"\x1b[1;2P");
    assert_eq!(named(NamedKey::F24, ModifiersState::empty(), modes), b"\x1b[24;2~");
    assert_eq!(named(NamedKey::F25, ModifiersState::empty(), modes), b"\x1b[1;5P");
    assert_eq!(named(NamedKey::F35, ModifiersState::empty(), modes), b"\x1b[23;5~");
}

#[test]
fn application_keypad_uses_numpad_physical_identity_only() {
    let app = KeyboardModes { application_cursor: false, application_keypad: true };
    let cases: [(KeyCode, &[u8]); 6] = [
        (KeyCode::Numpad0, b"\x1bOp"),
        (KeyCode::Numpad7, b"\x1bOw"),
        (KeyCode::NumpadAdd, b"\x1bOk"),
        (KeyCode::NumpadEnter, b"\x1bOM"),
        (KeyCode::NumpadDecimal, b"\x1bOn"),
        (KeyCode::NumpadDivide, b"\x1bOo"),
    ];
    for (code, expected) in cases {
        assert_eq!(
            encode_key(
                &Key::Character("7"),
                PhysicalKey::Code(code),
                KeyLocation::Numpad,
                None,
                ModifiersState::empty(),
                app,
            ),
            expected
        );
    }
    assert_eq!(
        encode_key(
            &Key::Character("7"),
            PhysicalKey::Code(KeyCode::Numpad7),
            KeyLocation::Numpad,
            None,
            ModifiersState::empty(),
            KeyboardModes::default(),
        ),
        b"7"
    );
    assert_eq!(text_key("7", ModifiersState::empty()), b"7");
}

#[test]
fn control_meta_utf8_and_space() {
    let modes = KeyboardModes::default();
    assert_eq!(text_key("c", ModifiersState::CONTROL), b"\x03");
    assert_eq!(text_key("C", ModifiersState::CONTROL | ModifiersState::SHIFT), b"\x03");
    assert_eq!(text_key("[", ModifiersState::CONTROL), b"\x1b");
    assert_eq!(text_key("6", ModifiersState::CONTROL), b"\x1e");
    assert_eq!(text_key("a", ModifiersState::CONTROL | ModifiersState::ALT), b"\x1b\x01");
    assert_eq!(text_key("ä", ModifiersState::empty()), "ä".as_bytes());
    assert_eq!(text_key("ä", ModifiersState::ALT), "\x1bä".as_bytes());
    assert_eq!(named(NamedKey::Backspace, ModifiersState::ALT, modes), b"\x1b\x7f");
    assert_eq!(named(NamedKey::Space, ModifiersState::CONTROL, modes), b"\0");
    assert_eq!(named(NamedKey::Enter, ModifiersState::ALT, modes), b"\x1b\r");
    assert!(text_key("x", ModifiersState::SUPER).is_empty());
}

#[test]
fn altgr_keeps_international_printable_characters() {
    let modifiers = ModifiersState::CONTROL | ModifiersState::ALT;
    for printable in ["@", "{", "[", "\\", "€"] {
        assert_eq!(
            encode_key(
                &Key::Character(printable),
                PhysicalKey::Code(KeyCode::KeyQ),
                KeyLocation::Standard,
                Some(printable),
                modifiers,
                KeyboardModes::default(),
            ),
            printable.as_bytes()
        );
    }
    // A true Ctrl+Alt chord still encodes control bytes with the Meta prefix.
    assert_eq!(
        encode_key(
            &Key::Character("c"),
            PhysicalKey::Code(KeyCode::KeyC),
            KeyLocation::Standard,
            Some("\x03"),
            modifiers,
            KeyboardModes::default(),
        ),
        b"\x1b\x03"
    );
}
