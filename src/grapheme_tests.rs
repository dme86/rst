//! Combining marks and joined Unicode sequences are stored with the base
//! Cell so rasterization can shape the *actual* grapheme, not only its first
//! scalar. These tests use the same vte parser as ordinary PTY data.

use super::*;

fn term(cols: usize, rows: usize) -> Terminal {
    let config: crate::config::Config =
        toml::from_str(include_str!("../config/default.toml")).unwrap();
    Terminal::new(cols, rows, 20, Palette::try_from(&config.colors).unwrap())
}

#[test]
fn combining_accent_survives_pty_and_clipboard_without_cursor_advance() {
    let mut t = term(12, 3);
    t.advance("Cafe\u{301}!".as_bytes());
    assert_eq!((t.screen.x, t.screen.y), (5, 0));
    assert_eq!(t.screen.grid[0][3].ch, 'e');
    assert_eq!(t.screen.grid[0][3].combining_len, 1);
    assert_eq!(t.screen.grid[0][3].text(), "e\u{301}");
    let sel = crate::selection::Selection::new(
        &t.screen, (1, 0), crate::selection::SelectionSnap::Word, false,
    );
    assert_eq!(sel.text(&t.screen), "Cafe\u{301}!");
}

#[test]
fn wide_cjk_glyph_receives_combining_mark_on_lead_cell() {
    let mut t = term(10, 2);
    t.advance("界\u{20dd}X".as_bytes());
    assert_eq!(t.screen.grid[0][0].width, 2);
    assert_eq!(t.screen.grid[0][0].text(), "界\u{20dd}");
    assert_eq!(t.screen.grid[0][1].width, 0);
    assert_eq!(t.screen.grid[0][2].ch, 'X');
}

#[test]
fn emoji_skin_tone_and_zwj_joiners_are_not_extra_grid_cells() {
    let mut t = term(16, 2);
    t.advance("👩‍💻!".as_bytes());
    assert_eq!(t.screen.grid[0][0].text(), "👩‍💻");
    assert_eq!(t.screen.grid[0][0].width, 2);
    assert_eq!(t.screen.grid[0][2].ch, '!');
    assert_eq!((t.screen.x, t.screen.y), (3, 0));
}

#[test]
fn accent_after_delayed_autowrap_keeps_base_on_previous_row() {
    let mut t = term(3, 3);
    t.advance("abe\u{301}X".as_bytes());
    assert_eq!(t.screen.grid[0][2].text(), "e\u{301}");
    assert!(t.screen.grid[0][2].wrapped);
    assert_eq!(t.screen.grid[1][0].ch, 'X');
}

#[test]
fn erasing_or_overwriting_cell_clears_stale_combining_marks() {
    let mut t = term(8, 2);
    t.advance("e\u{301}".as_bytes());
    assert_eq!(t.screen.grid[0][0].combining_len, 1);
    t.advance(b"\r\x1b[2K");
    assert_eq!(t.screen.grid[0][0].combining_len, 0);
    t.advance(b"x");
    assert_eq!(t.screen.grid[0][0].text(), "x");
}

#[test]
fn composed_grapheme_is_preserved_in_scrollback_and_alternate_screen() {
    let mut t = term(8, 2);
    t.advance("e\u{301}\r\nline\r\nend".as_bytes());
    assert_eq!(t.screen.history[0][0].text(), "e\u{301}");
    t.advance(b"\x1b[?1049h");
    t.advance("ö\u{302}".as_bytes());
    assert_eq!(t.screen.grid[0][0].text(), "ö\u{302}");
    t.advance(b"\x1b[?1049l");
    assert_eq!(t.screen.history[0][0].text(), "e\u{301}");
}

#[test]
fn combining_storage_is_bounded_and_does_not_mutate_adjacent_cells() {
    let mut t = term(8, 2);
    t.advance("a".as_bytes());
    t.advance("\u{0301}".repeat(12).as_bytes());
    assert_eq!(t.screen.grid[0][0].combining_len, 8);
    assert_eq!(t.screen.grid[0][1].ch, ' ');
    assert_eq!((t.screen.x, t.screen.y), (1, 0));
}
