//! DEC ISO-2022 character set regressions through the real vte parser.
//! The expected bytes and mappings follow suckless st's tdeftran/tsetchar.

use super::*;

fn term(cols: usize, rows: usize) -> Terminal {
    let config: crate::config::Config =
        toml::from_str(include_str!("../config/default.toml")).unwrap();
    Terminal::new(cols, rows, 20, Palette::try_from(&config.colors).unwrap())
}

fn row(term: &Terminal, y: usize) -> String {
    term.screen.grid[y].iter().filter(|cell| cell.width != 0)
        .map(|cell| cell.ch).collect()
}

#[test]
fn g0_dec_line_drawing_produces_box_glyphs_and_ascii_restores() {
    let mut t = term(12, 4);
    t.advance(b"\x1b(0lqqk\r\nx  x\r\nmqqj\x1b(B!");
    assert!(row(&t, 0).starts_with("┌──┐"));
    assert!(row(&t, 1).starts_with("│  │"));
    assert!(row(&t, 2).starts_with("└──┘!"));
    t.advance(b"q");
    assert_eq!(t.screen.grid[2][5].ch, 'q');
}

#[test]
fn graphics_mode_maps_ascii_symbol_table_but_not_unrelated_text() {
    let mut t = term(24, 4);
    t.advance(b"\x1b(0ABCDEFG");
    assert!(row(&t, 0).starts_with("↑↓→←█▚☃"));
    t.advance(b"HIZ_");
    assert_eq!(t.screen.grid[0][7].ch, 'H');
    assert_eq!(t.screen.grid[0][8].ch, 'I');
    assert_eq!(t.screen.grid[0][9].ch, 'Z');
    assert_eq!(t.screen.grid[0][10].ch, ' ');
}

#[test]
fn g1_designation_is_inactive_until_so_and_si_reinvokes_g0() {
    let mut t = term(12, 3);
    t.advance(b"\x1b)0q\x0eqx\x0fq");
    assert_eq!(&row(&t, 0)[..0], "");
    assert_eq!(t.screen.grid[0][0].ch, 'q');
    assert_eq!(t.screen.grid[0][1].ch, '─');
    assert_eq!(t.screen.grid[0][2].ch, '│');
    assert_eq!(t.screen.grid[0][3].ch, 'q');
    assert_eq!(t.screen.active_charset, 0);
    assert_eq!(t.screen.charsets[1], CharacterSet::DecSpecialGraphics);
}

#[test]
fn g2_and_g3_can_be_designated_and_invoked_with_ls2_ls3() {
    let mut t = term(12, 3);
    t.advance(b"\x1b*0\x1b+0q\x1bnq\x1boq\x0fq");
    assert_eq!([t.screen.grid[0][0].ch, t.screen.grid[0][1].ch,
                t.screen.grid[0][2].ch, t.screen.grid[0][3].ch],
               ['q', '─', '─', 'q']);
    assert_eq!(t.screen.charsets[2], CharacterSet::DecSpecialGraphics);
    assert_eq!(t.screen.charsets[3], CharacterSet::DecSpecialGraphics);
}

#[test]
fn reassigning_one_register_does_not_change_others() {
    let mut t = term(15, 3);
    t.advance(b"\x1b(0\x1b)0q\x0eq\x1b)Bq\x0fq");
    assert_eq!(t.screen.grid[0][0].ch, '─');
    assert_eq!(t.screen.grid[0][1].ch, '─');
    assert_eq!(t.screen.grid[0][2].ch, 'q');
    assert_eq!(t.screen.grid[0][3].ch, '─');
    assert_eq!(t.screen.charsets[0], CharacterSet::DecSpecialGraphics);
    assert_eq!(t.screen.charsets[1], CharacterSet::Ascii);
}

#[test]
fn unsupported_designations_are_ignored_and_do_not_invoke_plain_esc_actions() {
    let mut t = term(12, 3);
    t.advance(b"\x1b(0q\x1b(Aq\x1b(7q\x1b)Bq");
    assert!(row(&t, 0).starts_with("────"));
    // Unknown ESC ( 8 is NOT the DECRC command ESC 8.
    t.advance(b"\x1b(8q");
    assert_eq!(t.screen.grid[0][4].ch, '─');
}

#[test]
fn designation_and_lock_shift_can_be_split_across_pty_reads() {
    let mut t = term(12, 3);
    t.advance(b"\x1b");
    t.advance(b"(");
    t.advance(b"0");
    t.advance(b"q");
    assert_eq!(t.screen.grid[0][0].ch, '─');
    t.advance(b"\x1b)");
    t.advance(b"0");
    t.advance(b"\x0e");
    t.advance(b"x");
    assert_eq!(t.screen.grid[0][1].ch, '│');
    t.advance(b"\x0f\x1b(B");
    t.advance(b"q");
    assert_eq!(t.screen.grid[0][2].ch, 'q');
}

#[test]
fn sgr_changes_do_not_reset_character_set_designation() {
    let mut t = term(12, 2);
    t.advance(b"\x1b)0\x0e\x1b[31;1mq\x1b[0mq");
    assert_eq!(t.screen.grid[0][0].ch, '─');
    assert_eq!(t.screen.grid[0][1].ch, '─');
    assert_eq!(t.screen.charsets[1], CharacterSet::DecSpecialGraphics);
    assert_eq!(t.screen.active_charset, 1);
}

#[test]
fn unicode_printables_pass_through_even_with_dec_charset_active() {
    let mut t = term(24, 2);
    t.advance("\x1b(0qÄΩ界🦀".as_bytes());
    assert_eq!(t.screen.grid[0][0].ch, '─');
    assert_eq!(t.screen.grid[0][1].ch, 'Ä');
    assert_eq!(t.screen.grid[0][2].ch, 'Ω');
    assert_eq!(t.screen.grid[0][3].ch, '界');
    assert_eq!(t.screen.grid[0][3].width, 2);
    assert_eq!(t.screen.grid[0][4].width, 0);
    assert_eq!(t.screen.grid[0][5].ch, '🦀');
}

#[test]
fn line_drawing_uses_real_unicode_for_word_selection_and_clipboard() {
    let mut t = term(12, 2);
    t.advance(b"\x1b(0lqqk");
    let sel = Selection::new(&t.screen, (2, 0),
        crate::selection::SelectionSnap::Word, false);
    assert_eq!(sel.text(&t.screen), "┌──┐");
    assert_eq!(t.screen.selected_text(&sel), "┌──┐");
}

#[test]
fn graphics_rendering_tracks_soft_wraps_without_changing_cell_geometry() {
    let mut t = term(4, 3);
    t.advance(b"\x1b(0lqqkq");
    assert_eq!(t.screen.grid[0][0].ch, '┌');
    assert_eq!(t.screen.grid[0][1].ch, '─');
    assert_eq!(t.screen.grid[0][2].ch, '─');
    assert_eq!(t.screen.grid[0][3].ch, '┐');
    assert!(t.screen.grid[0][3].wrapped);
    assert_eq!(t.screen.grid[1][0].ch, '─');
    assert_eq!(t.screen.x, 1);
    assert_eq!(t.screen.y, 1);
}

#[test]
fn soft_reset_and_ris_restore_ascii_designations_without_erasing_on_decstr() {
    let mut t = term(12, 3);
    t.advance(b"\x1b(0\x1b)0\x0eq\x1b[!p\x1b[2Gq");
    assert_eq!(t.screen.grid[0][0].ch, '─'); // DECSTR preserves cells
    assert_eq!(t.screen.grid[0][1].ch, 'q'); // DECSTR restores G0 ASCII
    assert_eq!(t.screen.active_charset, 0);
    assert_eq!(t.screen.charsets, [CharacterSet::Ascii; 4]);
    t.advance(b"\x1b(0q\x1bcq");
    assert_eq!(t.screen.grid[0][0].ch, 'q'); // RIS clears and resets
    assert_eq!(t.screen.charsets, [CharacterSet::Ascii; 4]);
}

#[test]
fn alternate_screen_preserves_global_designations_like_st() {
    let mut t = term(12, 3);
    t.advance(b"\x1b(0q\x1b[?1049h\x1b(Bq\x1b[?1049lq");
    assert_eq!(t.screen.grid[0][0].ch, '─');
    assert_eq!(t.screen.grid[0][1].ch, 'q');
    assert_eq!(t.screen.charsets[0], CharacterSet::Ascii);
}

#[test]
fn save_restore_cursor_does_not_overwrite_charset_registers() {
    let mut t = term(12, 3);
    t.advance(b"\x1b(0\x1b7\x1b(B\x1b8q");
    assert_eq!(t.screen.grid[0][0].ch, 'q');
    assert_eq!(t.screen.charsets[0], CharacterSet::Ascii);
}
