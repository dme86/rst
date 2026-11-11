use super::*;
use crate::terminal::{Palette, Terminal};

fn terminal(cols: usize, rows: usize) -> Terminal {
    let config: crate::config::Config =
        toml::from_str(include_str!("../config/default.toml")).unwrap();
    Terminal::new(cols, rows, 20, Palette::try_from(&config.colors).unwrap())
}

#[test]
fn double_click_selects_whole_word_and_keeps_paths_together() {
    let mut t = terminal(24, 3);
    t.advance(b"run /tmp/hello.rs now");
    let sel = Selection::new(&t.screen, (8, 0), SelectionSnap::Word, false);
    assert_eq!(sel.text(&t.screen), "/tmp/hello.rs");
    assert!(sel.contains(&t.screen, 4, 0));
    assert!(sel.contains(&t.screen, 16, 0));
    assert!(!sel.contains(&t.screen, 3, 0));
    assert!(!sel.contains(&t.screen, 17, 0));
}

#[test]
fn word_snap_follows_soft_wrapped_text_not_hard_line_breaks() {
    let mut t = terminal(4, 4);
    t.advance(b"abcdef Z");
    assert!(t.screen.grid[0][3].wrapped);
    let sel = Selection::new(&t.screen, (2, 0), SelectionSnap::Word, false);
    assert_eq!(sel.text(&t.screen), "abcdef");
    assert!(sel.contains(&t.screen, 0, 1));
    assert!(!sel.contains(&t.screen, 3, 1));
    let line = Selection::new(&t.screen, (0, 1), SelectionSnap::Line, false);
    assert_eq!(line.text(&t.screen), "abcdef Z\n");
    let mut hard = terminal(8, 3);
    hard.advance(b"hello\r\nworld");
    let word = Selection::new(&hard.screen, (0, 0), SelectionSnap::Word, false);
    assert_eq!(word.text(&hard.screen), "hello");
}

#[test]
fn triple_click_selects_logical_line_including_wrapped_rows() {
    let mut t = terminal(5, 4);
    t.advance(b"abcdEFG\r\nxyz");
    let selection = Selection::new(&t.screen, (1, 1), SelectionSnap::Line, false);
    assert!(selection.contains(&t.screen, 0, 0));
    assert!(selection.contains(&t.screen, 4, 1));
    assert!(!selection.contains(&t.screen, 0, 2));
    assert_eq!(selection.text(&t.screen), "abcdEFG\n");
}

#[test]
fn rectangle_drag_forward_backward_highlights_only_columns_in_each_row() {
    let mut t = terminal(10, 4);
    t.advance(b"abcde\r\nvwxyz\r\n12345");
    let mut sel = Selection::new(&t.screen, (3, 2), SelectionSnap::Character, true);
    sel.update(&t.screen, (1, 0), true);
    assert_eq!(sel.kind, SelectionKind::Rectangular);
    assert_eq!(sel.text(&t.screen), "bcd\nwxy\n234");
    assert!(sel.contains(&t.screen, 1, 2));
    assert!(sel.contains(&t.screen, 3, 0));
    assert!(!sel.contains(&t.screen, 0, 1));
    assert!(!sel.contains(&t.screen, 4, 1));
}

#[test]
fn normal_drag_across_rows_uses_linear_not_rectangular_geometry() {
    let mut t = terminal(6, 3);
    t.advance(b"abcde\r\nfghij");
    let mut sel = Selection::new(&t.screen, (3, 1), SelectionSnap::Character, false);
    sel.update(&t.screen, (1, 0), false);
    assert_eq!(sel.text(&t.screen), "bcde\nfghi");
    assert!(sel.contains(&t.screen, 5, 0));
    assert!(sel.contains(&t.screen, 0, 1));
    assert!(!sel.contains(&t.screen, 0, 0));
    assert!(!sel.contains(&t.screen, 4, 1));
}

#[test]
fn rectangular_modifier_can_be_toggled_while_dragging() {
    let mut t = terminal(7, 3);
    t.advance(b"abcdef\r\nghijkl");
    let mut sel = Selection::new(&t.screen, (1, 0), SelectionSnap::Character, false);
    sel.update(&t.screen, (3, 1), true);
    assert_eq!(sel.text(&t.screen), "bcd\nhij");
    sel.update(&t.screen, (3, 1), false);
    assert_eq!(sel.text(&t.screen), "bcdef\nghij");
}

#[test]
fn single_click_is_only_an_anchor_and_does_not_copy_empty_cells() {
    let mut t = terminal(8, 2);
    t.advance(b"hello");
    let sel = Selection::new(&t.screen, (1, 0), SelectionSnap::Character, false);
    assert!(!sel.contains(&t.screen, 1, 0));
    assert_eq!(sel.text(&t.screen), "");
    let word = Selection::new(&t.screen, (1, 0), SelectionSnap::Word, false);
    assert_eq!(word.text(&t.screen), "hello");
}

#[test]
fn double_width_glyphs_are_canonicalized_and_copied_once() {
    let mut t = terminal(12, 2);
    t.advance("界好 world".as_bytes());
    let sel = Selection::new(&t.screen, (1, 0), SelectionSnap::Word, false);
    assert_eq!(sel.text(&t.screen), "界好");
    assert!(sel.contains(&t.screen, 0, 0));
    assert!(sel.contains(&t.screen, 1, 0));
    assert!(sel.contains(&t.screen, 3, 0));
    assert!(!sel.contains(&t.screen, 4, 0));
    let mut rect = Selection::new(&t.screen, (1, 0), SelectionSnap::Character, true);
    rect.update(&t.screen, (3, 0), true);
    assert_eq!(rect.text(&t.screen), "界好");
}

#[test]
fn selection_tracks_scrollback_view_and_invalidates_on_scroll_or_alt_screen() {
    let mut t = terminal(8, 2);
    t.advance(b"old\r\nnew\r\nmore");
    t.screen.scroll_view(1);
    assert!(t.screen.view_offset > 0);
    let sel = Selection::new(&t.screen, (0, 0), SelectionSnap::Word, false);
    assert_eq!(sel.text(&t.screen), "old");
    t.screen.scroll_view(-1);
    assert_eq!(sel.text(&t.screen), "");
    t.screen.scroll_view(1);
    t.advance(b"\x1b[?1049h");
    assert_eq!(sel.text(&t.screen), "");
}

#[test]
fn multiple_click_thresholds_follow_st_and_reset_after_triple() {
    let mut clicks = ClickTracker::default();
    let now = Instant::now();
    let at = (3, 1);
    assert_eq!(clicks.press(now, at, false), SelectionSnap::Character);
    assert_eq!(clicks.press(now + Duration::from_millis(200), at, false), SelectionSnap::Word);
    assert_eq!(clicks.press(now + Duration::from_millis(400), at, false), SelectionSnap::Line);
    assert_eq!(clicks.press(now + Duration::from_millis(450), at, false), SelectionSnap::Character);
    assert_eq!(clicks.press(now + Duration::from_millis(800), at, false), SelectionSnap::Character);
    assert_eq!(clicks.press(now + Duration::from_millis(850), (7, 1), false), SelectionSnap::Character);
}

#[test]
fn click_tracker_separates_alt_rectangles_and_clears_on_reset() {
    let mut clicks = ClickTracker::default();
    let now = Instant::now();
    assert_eq!(clicks.press(now, (1, 0), false), SelectionSnap::Character);
    assert_eq!(clicks.press(now + Duration::from_millis(100), (1, 0), true), SelectionSnap::Character);
    assert_eq!(clicks.press(now + Duration::from_millis(200), (1, 0), true), SelectionSnap::Word);
    clicks.reset();
    assert_eq!(clicks.press(now + Duration::from_millis(210), (1, 0), true), SelectionSnap::Character);
}

#[test]
fn selection_of_blank_rectangle_retains_line_structure() {
    let mut t = terminal(8, 3);
    t.advance(b"ab\r\nc");
    let mut sel = Selection::new(&t.screen, (1, 0), SelectionSnap::Character, true);
    sel.update(&t.screen, (3, 1), true);
    assert_eq!(sel.text(&t.screen), "b\n");
}

#[test]
fn resizing_keeps_history_addressable_by_selection() {
    let mut t = terminal(4, 2);
    t.advance(b"one\r\ntwo\r\nend");
    assert!(!t.screen.history.is_empty());
    t.screen.resize(7, 2);
    assert_eq!(t.screen.history[0].len(), 7);
    assert_eq!(t.screen.display_row(0).len(), 7);
    t.screen.scroll_view(1);
    let sel = Selection::new(&t.screen, (1, 0), SelectionSnap::Word, false);
    assert_eq!(sel.text(&t.screen), "one");
    t.screen.resize(3, 2);
    assert_eq!(t.screen.history[0].len(), 3);
    assert_eq!(t.screen.display_row(0).len(), 3);
}
