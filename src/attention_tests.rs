//! Focus reporting and BEL regressions through the real terminal parser.
//! UI focus transitions and cooldown behavior are tested in attention.rs.

use super::*;

fn term() -> Terminal {
    let config: crate::config::Config =
        toml::from_str(include_str!("../config/default.toml")).unwrap();
    Terminal::new(16, 4, 20, Palette::try_from(&config.colors).unwrap())
}

#[test]
fn bel_is_an_event_and_does_not_print_or_reply_to_pty() {
    let mut t = term();
    t.advance(b"foo\x07bar");
    assert_eq!(t.screen.take_bells(), 1);
    assert_eq!(t.screen.take_bells(), 0);
    assert_eq!(t.screen.take_replies(), b"");
    assert_eq!(t.screen.grid[0][0].ch, 'f');
    assert_eq!(t.screen.grid[0][3].ch, 'b');
    assert_eq!((t.screen.x, t.screen.y), (6, 0));
}

#[test]
fn bel_does_not_break_utf8_or_csi_when_split_across_reads() {
    let mut t = term();
    t.advance(b"\x1b[31m");
    t.advance("ä".as_bytes());
    t.advance(b"\x07");
    t.advance("Ω".as_bytes());
    assert_eq!(t.screen.take_bells(), 1);
    assert_eq!(t.screen.grid[0][0].ch, 'ä');
    assert_eq!(t.screen.grid[0][1].ch, 'Ω');
    assert_eq!(t.screen.grid[0][0].fg, t.screen.palette.ansi[1]);
    assert_eq!(t.screen.grid[0][1].fg, t.screen.palette.ansi[1]);
}

#[test]
fn osc_bel_terminators_are_not_system_bells() {
    let mut t = term();
    t.advance(b"\x1b]2;hello\x07");
    assert_eq!(t.screen.title, "hello");
    assert_eq!(t.screen.take_bells(), 0);
    t.advance(b"\x1b]4;1;#aabbcc\x07");
    assert_eq!(t.screen.take_bells(), 0);
    assert_eq!(t.screen.palette.ansi[1], 0xaabbcc);
    t.advance(b"\x07");
    assert_eq!(t.screen.take_bells(), 1);
}

#[test]
fn bel_counter_is_bounded_and_drain_is_idempotent() {
    let mut t = term();
    t.advance(&vec![0x07; 1000]);
    assert_eq!(t.screen.take_bells(), u8::MAX);
    assert_eq!(t.screen.take_bells(), 0);
    t.advance(b"\x07\x07");
    assert_eq!(t.screen.take_bells(), 2);
}

#[test]
fn bel_survives_ris_and_decstr_within_same_pty_chunk() {
    let mut t = term();
    t.advance(b"\x07\x1bc");
    assert_eq!(t.screen.take_bells(), 1);
    t.advance(b"\x07\x1b[!p");
    assert_eq!(t.screen.take_bells(), 1);
    t.advance(b"\x1bc");
    assert_eq!(t.screen.take_bells(), 0);
}

#[test]
fn focus_reporting_mode_defaults_off_and_toggles_via_decset_1004() {
    let mut t = term();
    assert!(!t.screen.focus_reporting);
    t.advance(b"\x1b[?1004h");
    assert!(t.screen.focus_reporting);
    t.advance(b"\x1b[?1004$p");
    assert_eq!(t.screen.take_replies(), b"\x1b[?1004;1$y");
    t.advance(b"\x1b[?1004l");
    assert!(!t.screen.focus_reporting);
    t.advance(b"\x1b[?1004$p");
    assert_eq!(t.screen.take_replies(), b"\x1b[?1004;2$y");
}

#[test]
fn focus_reporting_preserved_on_alt_screen_but_disabled_on_soft_and_hard_reset() {
    let mut t = term();
    t.advance(b"\x1b[?1004h\x1b[?1049h");
    assert!(t.screen.focus_reporting);
    t.advance(b"\x1b[?1049l");
    assert!(t.screen.focus_reporting);
    t.advance(b"\x1b[!p");
    assert!(!t.screen.focus_reporting);
    t.advance(b"\x1b[?1004h\x1bc");
    assert!(!t.screen.focus_reporting);
}

#[test]
fn pending_bells_do_not_change_palette_cursor_or_scrollback() {
    let mut t = term();
    let initial_fg = t.screen.palette.foreground;
    let initial_history = t.screen.history.len();
    t.advance(b"\x07\x07");
    assert_eq!(t.screen.palette.foreground, initial_fg);
    assert_eq!(t.screen.history.len(), initial_history);
    assert_eq!((t.screen.x, t.screen.y), (0, 0));
    assert_eq!(t.screen.take_bells(), 2);
}
