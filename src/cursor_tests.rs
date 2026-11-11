//! Regression tests for DEC Set Cursor Style parsed as raw PTY bytes.
use super::*;

fn terminal() -> Terminal {
    let config: crate::config::Config =
        toml::from_str(include_str!("../config/default.toml")).unwrap();
    Terminal::new(12, 4, 20, Palette::try_from(&config.colors).unwrap())
}

#[test]
fn decscusr_supports_all_six_shapes_and_blink_variants() {
    let mut term = terminal();
    let cases = [
        (1, CursorShape::Block, true),
        (2, CursorShape::Block, false),
        (3, CursorShape::Underline, true),
        (4, CursorShape::Underline, false),
        (5, CursorShape::Bar, true),
        (6, CursorShape::Bar, false),
    ];
    for (value, shape, blink) in cases {
        term.advance(format!("\x1b[{value} q").as_bytes());
        assert_eq!(term.screen.cursor_style, CursorStyle { shape, blink });
        assert!(term.screen.cursor_blink_on);
    }
}

#[test]
fn decscusr_zero_and_omitted_parameter_restore_configured_cursor() {
    let mut term = terminal();
    let configured = CursorStyle { shape: CursorShape::Underline, blink: false };
    term.screen.set_default_cursor_style(configured);
    term.advance(b"\x1b[5 q");
    assert_eq!(term.screen.cursor_style.shape, CursorShape::Bar);
    term.advance(b"\x1b[0 q");
    assert_eq!(term.screen.cursor_style, configured);
    term.advance(b"\x1b[1 q\x1b[ q");
    assert_eq!(term.screen.cursor_style, configured);
}

#[test]
fn decscusr_does_not_confuse_plain_q_other_intermediates_or_unknown_values() {
    let mut term = terminal();
    term.advance(b"\x1b[6 q");
    let bar = term.screen.cursor_style;
    term.advance(b"\x1b[1q\x1b[2$q\x1b[7 q\x1b[999 q");
    assert_eq!(term.screen.cursor_style, bar);
    term.advance(b"\x1b[4;5 q");
    // Only Ps (the first parameter) determines the style.
    assert_eq!(term.screen.cursor_style,
               CursorStyle { shape: CursorShape::Underline, blink: false });
}

#[test]
fn split_csi_updates_cursor_only_after_complete_sequence() {
    let mut term = terminal();
    term.advance(b"\x1b[5");
    assert_eq!(term.screen.cursor_style, CursorStyle::default());
    term.advance(b" ");
    assert_eq!(term.screen.cursor_style, CursorStyle::default());
    term.advance(b"q");
    assert_eq!(term.screen.cursor_style,
               CursorStyle { shape: CursorShape::Bar, blink: true });
}

#[test]
fn cursor_style_is_independent_from_visibility_text_attributes_and_alternate_screen() {
    let mut term = terminal();
    term.advance(b"\x1b[4 q\x1b[?25l\x1b[1;3;4mA");
    assert_eq!(term.screen.cursor_style,
               CursorStyle { shape: CursorShape::Underline, blink: false });
    assert!(!term.screen.show_cursor);
    assert!(term.screen.grid[0][0].style.contains(TextStyle::ITALIC));
    term.advance(b"\x1b[?1049h\x1b[?25h");
    assert_eq!(term.screen.cursor_style.shape, CursorShape::Underline);
    assert!(term.screen.show_cursor);
    term.advance(b"\x1b[?1049l");
    assert_eq!(term.screen.cursor_style.shape, CursorShape::Underline);
    assert!(term.screen.grid[0][0].style.contains(TextStyle::UNDERLINE));
}

#[test]
fn soft_and_full_reset_restore_user_cursor_default_not_hardcoded_block() {
    let mut term = terminal();
    let configured = CursorStyle { shape: CursorShape::Bar, blink: false };
    term.screen.set_default_cursor_style(configured);
    term.advance(b"\x1b[3 q\x1b[!p");
    assert_eq!(term.screen.cursor_style, configured);
    term.advance(b"\x1b[1 q\x1bc");
    assert_eq!(term.screen.cursor_style, configured);
    assert!(term.screen.show_cursor);
}

#[test]
fn cursor_save_and_restore_preserve_cursor_style_independently() {
    let mut term = terminal();
    term.advance(b"\x1b[2;3H\x1b7\x1b[5 q\x1b[1;1H\x1b8");
    assert_eq!((term.screen.x, term.screen.y), (2, 1));
    assert_eq!(term.screen.cursor_style.shape, CursorShape::Bar);
}
