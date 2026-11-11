//! VT/DEC control mode regressions derived from suckless st's st.c.
//! Exercise the byte stream through vte, not just private helper methods.

use super::*;

fn term(cols: usize, rows: usize) -> Terminal {
    let config: crate::config::Config =
        toml::from_str(include_str!("../config/default.toml")).unwrap();
    Terminal::new(cols, rows, 20, Palette::try_from(&config.colors).unwrap())
}

fn text_row(term: &Terminal, row: usize) -> String {
    term.screen.grid[row].iter().map(|cell| cell.ch).collect()
}

#[test]
fn decom_origin_mode_moves_home_and_clamps_to_margins() {
    let mut t = term(10, 6);
    t.advance(b"\x1b[2;5r\x1b[?6h");
    assert!(t.screen.origin_mode);
    assert_eq!((t.screen.x, t.screen.y), (0, 1));
    t.advance(b"\x1b[3;4H");
    assert_eq!((t.screen.x, t.screen.y), (3, 3));
    t.advance(b"\x1b[99B");
    assert_eq!(t.screen.y, 4);
    t.advance(b"\x1b[99A");
    assert_eq!(t.screen.y, 1);
    t.advance(b"\x1b[6n");
    assert_eq!(t.screen.take_replies(), b"\x1b[1;4R");
    t.advance(b"\x1b[?6l");
    assert!(!t.screen.origin_mode);
    assert_eq!((t.screen.x, t.screen.y), (0, 0));
    t.advance(b"\x1b[5;1H");
    assert_eq!(t.screen.y, 4);
}

#[test]
fn scrolling_region_only_scrolls_inside_declared_margins() {
    let mut t = term(5, 5);
    t.advance(b"0\r\n1\r\n2\r\n3\r\n4");
    t.advance(b"\x1b[2;4r\x1b[4;1H\n");
    // The second through fourth lines scroll; the first and fifth remain.
    assert_eq!(t.screen.grid[0][0].ch, '0');
    assert_eq!(t.screen.grid[4][0].ch, '4');
    assert_eq!(t.screen.grid[1][0].ch, '2');
    assert_eq!(t.screen.grid[2][0].ch, '3');
    assert_eq!(t.screen.grid[3][0].ch, ' ');
    assert_eq!(t.screen.history.len(), 0);
}

#[test]
fn decawm_delays_wrap_until_next_printable_character() {
    let mut t = term(4, 3);
    t.advance(b"abcd");
    assert_eq!((t.screen.x, t.screen.y), (3, 0));
    assert!(t.screen.wrap_pending);
    t.advance(b"\x1b[31m");
    assert!(t.screen.wrap_pending); // SGR does not force an early wrap.
    t.advance(b"e");
    assert_eq!(text_row(&t, 0), "abcd");
    assert_eq!(t.screen.grid[1][0].ch, 'e');
    assert_eq!((t.screen.x, t.screen.y), (1, 1));
}

#[test]
fn no_wrap_overwrites_last_column_and_does_not_scroll() {
    let mut t = term(4, 2);
    t.advance(b"\x1b[?7labcdEF");
    assert!(!t.screen.auto_wrap);
    assert_eq!(text_row(&t, 0), "abcF");
    assert_eq!(t.screen.y, 0);
    assert!(!t.screen.wrap_pending);
    t.advance(b"\x1b[?7hG");
    assert_eq!(text_row(&t, 0), "abcG");
    assert!(t.screen.wrap_pending);
    t.advance(b"H");
    assert_eq!(t.screen.grid[1][0].ch, 'H');
}

#[test]
fn carriage_return_and_cursor_move_cancel_pending_wrap() {
    let mut t = term(4, 2);
    t.advance(b"abcd\rZ");
    assert_eq!(text_row(&t, 0), "Zbcd");
    assert_eq!(t.screen.y, 0);
    t.advance(b"\x1b[4GH\x1b[2DX");
    assert_eq!(t.screen.grid[0][1].ch, 'X');
    assert_eq!(t.screen.y, 0);
}

#[test]
fn irm_inserts_characters_and_reset_restores_replacement() {
    let mut t = term(6, 2);
    t.advance(b"abcd\x1b[1;3H\x1b[4hZ");
    assert!(t.screen.insert_mode);
    assert_eq!(text_row(&t, 0), "abZcd ");
    t.advance(b"\x1b[4l\x1b[3GQ");
    assert!(!t.screen.insert_mode);
    assert_eq!(text_row(&t, 0), "abQcd ");
}

#[test]
fn linefeed_newline_mode_is_independent_of_autowrap() {
    let mut t = term(8, 3);
    t.advance(b"ab\nc");
    assert_eq!(t.screen.grid[1][2].ch, 'c'); // LNM disabled by default
    t.advance(b"\x1b[20h\x1b[2;5H\nZ");
    assert!(t.screen.newline_mode);
    assert_eq!(t.screen.grid[2][0].ch, 'Z');
    t.advance(b"\x1b[20l");
    assert!(!t.screen.newline_mode);
}

#[test]
fn tabs_set_clear_forward_backward_and_reset() {
    let mut t = term(16, 2);
    t.advance(b"\t");
    assert_eq!(t.screen.x, 8);
    t.advance(b"\x1b[Z");
    assert_eq!(t.screen.x, 0);
    t.advance(b"\x1b[3g\t");
    assert_eq!(t.screen.x, 15); // all stops removed
    t.advance(b"\x1b[4G\x1bH\r\t");
    assert_eq!(t.screen.x, 3); // HTS added at column four
    t.advance(b"\x1b[g\r\t");
    assert_eq!(t.screen.x, 15); // TBC removed the current stop
    t.advance(b"\x1bc\t");
    assert_eq!(t.screen.x, 8); // RIS restores every eighth-column stop
}

#[test]
fn tabs_preserve_custom_stops_on_resize_and_add_new_eighths() {
    let mut t = term(16, 2);
    t.advance(b"\x1b[4G\x1bH");
    t.screen.resize(24, 2);
    t.advance(b"\r\t");
    assert_eq!(t.screen.x, 3);
    t.advance(b"\t");
    assert_eq!(t.screen.x, 8);
    t.advance(b"\x1b[2I");
    assert_eq!(t.screen.x, 23); // step via 16 to the last column
}

#[test]
fn desc_decrc_restore_cursor_and_rendering_attributes() {
    let mut t = term(12, 4);
    t.advance(b"\x1b[2;3H\x1b[3;31m\x1b7\x1b[4;8H\x1b[0m\x1b8A");
    assert_eq!(t.screen.grid[1][2].ch, 'A');
    assert!(t.screen.grid[1][2].style.contains(TextStyle::ITALIC));
    assert_eq!(t.screen.grid[1][2].fg, t.screen.palette.ansi[1]);
    t.advance(b"\x1b[s\x1b[1;1H\x1b[0m\x1b[uB");
    assert_eq!(t.screen.grid[1][3].ch, 'B');
    assert!(t.screen.grid[1][3].style.contains(TextStyle::ITALIC));
}

#[test]
fn dec_1048_save_and_restore_cursor_without_switching_screen() {
    let mut t = term(8, 3);
    t.advance(b"\x1b[2;3H\x1b[4m\x1b[?1048h");
    t.advance(b"\x1b[1;1H\x1b[0m\x1b[?1048lQ");
    assert!(!t.screen.in_alternate_screen());
    assert_eq!(t.screen.grid[1][2].ch, 'Q');
    assert!(t.screen.grid[1][2].style.contains(TextStyle::UNDERLINE));
}

#[test]
fn dec_1049_preserves_primary_screen_and_restores_cursor_state() {
    let mut t = term(9, 4);
    t.advance(b"Hello\x1b[2;4H\x1b[3m\x1b[?1049h");
    assert!(t.screen.in_alternate_screen());
    assert_eq!((t.screen.x, t.screen.y), (0, 0));
    assert_eq!(t.screen.grid[0][0].ch, ' ');
    t.advance(b"world\x1b[0m\x1b[?1049l");
    assert!(!t.screen.in_alternate_screen());
    assert_eq!((t.screen.x, t.screen.y), (3, 1));
    assert_eq!(text_row(&t, 0)[..5].to_string(), "Hello");
    t.advance(b"!");
    assert_eq!(t.screen.grid[1][3].ch, '!');
    assert!(t.screen.grid[1][3].style.contains(TextStyle::ITALIC));
}

#[test]
fn dec_47_and_1047_switch_screen_without_restoring_sgr() {
    let mut t = term(8, 3);
    t.advance(b"Hi\x1b[?47hALT\x1b[?47l");
    assert_eq!(t.screen.grid[0][0].ch, 'H');
    assert_eq!(t.screen.grid[0][1].ch, 'i');
    t.advance(b"\x1b[?1047hZ\x1b[?1047l");
    assert!(!t.screen.in_alternate_screen());
    assert_eq!(t.screen.grid[0][0].ch, 'H');
}

#[test]
fn reverse_video_focus_and_keyboard_modes_toggle_independently() {
    let mut t = term(10, 2);
    t.advance(b"\x1b[?5;1004h\x1b[2;4;20h");
    assert!(t.screen.reverse_video);
    assert!(t.screen.focus_reporting);
    assert!(t.screen.keyboard_lock);
    assert!(t.screen.insert_mode);
    assert!(t.screen.newline_mode);
    t.advance(b"\x1b[?5;1004l\x1b[2;4;20l");
    assert!(!t.screen.reverse_video);
    assert!(!t.screen.focus_reporting);
    assert!(!t.screen.keyboard_lock);
    assert!(!t.screen.insert_mode);
    assert!(!t.screen.newline_mode);
    t.advance(b"\x1b[?9999h"); // unknown modes are ignored
    assert!(t.screen.auto_wrap);
}

#[test]
fn ris_resets_modes_tabs_colors_and_margins() {
    let mut t = term(16, 5);
    t.advance(b"\x1b[3;5r\x1b[?6;7l\x1b[4h\x1b[3g\x1b[?5;1004h");
    t.advance(b"\x1bc");
    assert_eq!((t.screen.scroll_top, t.screen.scroll_bottom), (0, 5));
    assert!(!t.screen.origin_mode);
    assert!(t.screen.auto_wrap);
    assert!(!t.screen.insert_mode);
    assert!(!t.screen.reverse_video);
    assert!(!t.screen.focus_reporting);
    assert_eq!((t.screen.x, t.screen.y), (0, 0));
    t.advance(b"\t");
    assert_eq!(t.screen.x, 8);
}

#[test]
fn saved_cursor_restores_decom_state_and_scrolling_region_coordinates() {
    let mut t = term(10, 6);
    t.advance(b"\x1b[2;5r\x1b[?6h\x1b[2;3H\x1b[?1048h");
    assert_eq!((t.screen.x, t.screen.y), (2, 2));
    t.advance(b"\x1b[?6l\x1b[6;8H");
    assert!(!t.screen.origin_mode);
    t.advance(b"\x1b[?1048l");
    assert!(t.screen.origin_mode);
    assert_eq!((t.screen.x, t.screen.y), (2, 2));
    t.advance(b"\x1b[1;1H*");
    assert_eq!(t.screen.grid[1][0].ch, '*');
}

#[test]
fn single_column_terminal_never_moves_cursor_outside_grid() {
    let mut t = term(1, 2);
    t.advance("界A".as_bytes());
    assert!(t.screen.x < t.screen.cols);
    assert!(t.screen.y < t.screen.rows);
    assert_eq!(t.screen.grid[0][0].ch, '?');
    assert_eq!(t.screen.grid[1][0].ch, 'A');
}

#[test]
fn cursor_movement_cancels_pending_wrap_but_sgr_does_not() {
    let mut t = term(3, 3);
    t.advance(b"abc\x1b[1m");
    assert!(t.screen.wrap_pending);
    t.advance(b"\x1b[2GX");
    assert_eq!(text_row(&t, 0), "aXc");
    assert_eq!(t.screen.y, 0);
    assert!(!t.screen.wrap_pending);
}

#[test]
fn dec_saved_cursor_preserves_deferred_wrap_at_right_margin() {
    let mut t = term(3, 2);
    t.advance(b"abc\x1b7\r\x1b8Z");
    assert_eq!(text_row(&t, 0), "abc");
    assert_eq!(t.screen.grid[1][0].ch, 'Z');
    assert_eq!((t.screen.x, t.screen.y), (1, 1));
}

#[test]
fn decrqm_reports_standard_and_dec_private_modes_without_changing_them() {
    let mut t = term(12, 5);
    t.advance(b"\x1b[4$p\x1b[?6$p\x1b[?7$p\x1b[?1006$p\x1b[?9999$p");
    assert_eq!(
        t.screen.take_replies(),
        b"\x1b[4;2$y\x1b[?6;2$y\x1b[?7;1$y\x1b[?1006;2$y\x1b[?9999;0$y"
    );
    assert!(!t.screen.insert_mode);
    assert!(t.screen.auto_wrap);

    t.advance(b"\x1b[4;20h\x1b[?6;1002;1006;1004h");
    t.advance(b"\x1b[4$p\x1b[20$p\x1b[?6$p\x1b[?1002$p\x1b[?1000$p\x1b[?1006$p\x1b[?1004$p");
    assert_eq!(
        t.screen.take_replies(),
        b"\x1b[4;1$y\x1b[20;1$y\x1b[?6;1$y\x1b[?1002;1$y\x1b[?1000;2$y\x1b[?1006;1$y\x1b[?1004;1$y"
    );
    assert_eq!(t.screen.mouse_tracking, MouseTracking::ButtonMotion);
}

#[test]
fn decrqm_cannot_toggle_modes_and_unknown_queries_are_safe() {
    let mut t = term(10, 3);
    t.advance(b"\x1b[?7l");
    t.advance(b"\x1b[?7$p\x1b[?1234$p\x1b[9999$p");
    assert!(!t.screen.auto_wrap);
    assert_eq!(
        t.screen.take_replies(),
        b"\x1b[?7;2$y\x1b[?1234;0$y\x1b[9999;0$y"
    );
}

#[test]
fn decstr_soft_reset_preserves_main_screen_and_scrollback() {
    let mut t = term(8, 3);
    t.advance(b"0\r\n1\r\n2\r\n3");
    let previous_history = t.screen.history.len();
    t.advance(b"\x1b[2;3r\x1b[?6;7l\x1b[4h\x1b[3;31m");
    t.advance(b"\x1b[?1002;1006;1004h\x1b[1;3H");
    assert!(!t.screen.auto_wrap);
    assert!(t.screen.origin_mode);
    assert!(t.screen.insert_mode);
    t.advance(b"\x1b[!p");
    assert!(!t.screen.origin_mode);
    assert!(t.screen.auto_wrap);
    assert!(!t.screen.insert_mode);
    assert!(!t.screen.focus_reporting);
    assert!(!t.screen.mouse_sgr);
    assert_eq!(t.screen.mouse_tracking, MouseTracking::Off);
    assert_eq!((t.screen.x, t.screen.y), (0, 0));
    assert_eq!((t.screen.scroll_top, t.screen.scroll_bottom), (0, 3));
    assert_eq!(t.screen.history.len(), previous_history);
    assert_eq!(t.screen.grid[0][0].ch, '1');
    assert_eq!(t.screen.grid[1][0].ch, '2');
    assert_eq!(t.screen.grid[2][0].ch, '3');
    t.advance(b"X");
    assert_eq!(t.screen.grid[0][0].fg, t.screen.palette.foreground);
    assert_eq!(t.screen.grid[0][0].style, TextStyle::default());
}

#[test]
fn decstr_does_not_destroy_alternate_screen_buffer() {
    let mut t = term(8, 3);
    t.advance(b"main\x1b[?1049halt\x1b[3;31m");
    assert!(t.screen.in_alternate_screen());
    t.advance(b"\x1b[!p");
    assert!(t.screen.in_alternate_screen());
    assert_eq!(t.screen.grid[0][0].ch, 'a');
    assert_eq!((t.screen.x, t.screen.y), (0, 0));
    t.advance(b"\x1b[?1049l");
    assert_eq!(t.screen.grid[0][0].ch, 'm');
}

#[test]
fn decstr_retains_custom_tab_stops_but_ris_clears_them() {
    let mut t = term(16, 2);
    t.advance(b"\x1b[3g\x1b[4G\x1bH\x1b[!p\t");
    assert_eq!(t.screen.x, 3);
    t.advance(b"\x1bc\t");
    assert_eq!(t.screen.x, 8);
}
