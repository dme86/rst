use super::*;

#[test]
fn preedit_is_visible_but_never_emitted_to_the_pty() {
    let mut ime = ImeState::default();
    assert_eq!(ime.handle(Ime::Enabled, false), None);
    assert!(ime.enabled);
    assert_eq!(
        ime.handle(Ime::Preedit("ni".into(), Some((2, 2))), false),
        None
    );
    assert!(ime.composing());
    assert_eq!(ime.preedit.as_ref().unwrap().text, "ni");
    assert_eq!(
        ime.handle(Ime::Preedit("你".into(), Some((3, 3))), false),
        None
    );
    assert_eq!(ime.preedit.as_ref().unwrap().cursor, Some((3, 3)));
    // winit clears Preedit immediately before sending a final Commit.
    assert_eq!(ime.handle(Ime::Preedit(String::new(), None), false), None);
    assert!(!ime.composing());
    assert_eq!(ime.handle(Ime::Commit("你".into()), false), Some("你".as_bytes().to_vec()));
    assert_eq!(ime.preedit, None);
}

#[test]
fn composition_cancellation_never_emits_or_inserts_text() {
    let mut ime = ImeState::default();
    ime.handle(Ime::Enabled, false);
    ime.handle(Ime::Preedit("a b".into(), Some((3, 3))), false);
    assert_eq!(ime.handle(Ime::Disabled, false), None);
    assert!(!ime.enabled);
    assert!(!ime.composing());
    assert_eq!(ime.preedit, None);
    ime.handle(Ime::Enabled, false);
    ime.handle(Ime::Preedit("abc".into(), None), false);
    ime.clear();
    assert_eq!(ime.preedit, None);
}

#[test]
fn commit_sends_utf8_once_and_preserves_unicode() {
    let mut ime = ImeState::default();
    let text = "日本語🙂é";
    assert_eq!(ime.handle(Ime::Commit(text.into()), false), Some(text.as_bytes().to_vec()));
    assert_eq!(ime.preedit, None);
    assert_eq!(ime.handle(Ime::Commit(String::new()), false), None);
}

#[test]
fn commit_normalizes_enter_but_does_not_wrap_as_bracketed_paste() {
    let mut ime = ImeState::default();
    assert_eq!(ime.handle(Ime::Commit("a\r\nb\nc".into()), false),
               Some(b"a\rb\rc".to_vec()));
}

#[test]
fn keyboard_lock_discards_commit_and_clears_preedit() {
    let mut ime = ImeState::default();
    ime.handle(Ime::Enabled, false);
    ime.handle(Ime::Preedit("hello".into(), Some((5, 5))), true);
    assert!(ime.composing());
    assert_eq!(ime.handle(Ime::Commit("hello".into()), true), None);
    assert!(!ime.composing());
}

#[test]
fn byte_offset_mid_multibyte_char_is_floored_safely() {
    let mut ime = ImeState::default();
    ime.handle(Ime::Preedit("你é🙂".into(), Some((1, 999))), false);
    let preedit = ime.preedit.as_ref().unwrap();
    assert_eq!(preedit.cursor, Some((0, "你é🙂".len())));
    let layout = layout(preedit, 12, 3, (0, 0));
    assert_eq!(layout.caret, Some((0, 0)));
    assert_eq!(layout.glyphs.len(), 3);
}

#[test]
fn japanese_preedit_caret_moves_by_cells_not_utf8_bytes() {
    let preedit = Preedit {
        text: "かな漢字".into(),
        cursor: Some((6, 6)), // byte boundary after two CJK graphemes
    };
    let layout = layout(&preedit, 20, 4, (2, 1));
    assert_eq!(layout.glyphs.len(), 4);
    assert_eq!((layout.glyphs[0].col, layout.glyphs[0].row), (2, 1));
    assert_eq!(layout.glyphs[1].col, 4);
    assert_eq!(layout.caret, Some((6, 1)));
    assert_eq!(layout.candidate, (6, 1));
}

#[test]
fn composing_accent_stays_in_one_grapheme_cell() {
    let preedit = Preedit {
        text: "e\u{301}x".into(),
        cursor: Some((3, 3)),
    };
    let layout = layout(&preedit, 10, 3, (0, 0));
    assert_eq!(layout.glyphs.len(), 2);
    assert_eq!(layout.glyphs[0].text, "e\u{301}");
    assert_eq!(layout.glyphs[0].cells, 1);
    assert_eq!(layout.glyphs[1].col, 1);
    assert_eq!(layout.caret, Some((1, 0)));
}

#[test]
fn emoji_zwj_sequence_does_not_split_across_cells() {
    let preedit = Preedit {
        text: "👩‍💻A".into(),
        cursor: Some(("👩‍💻".len(), "👩‍💻".len())),
    };
    let layout = layout(&preedit, 8, 2, (1, 0));
    assert_eq!(layout.glyphs.len(), 2);
    assert_eq!(layout.glyphs[0].text, "👩‍💻");
    assert_eq!(layout.glyphs[0].cells, 2);
    assert_eq!(layout.glyphs[1].col, 3);
    assert_eq!(layout.caret, Some((3, 0)));
}

#[test]
fn ime_selection_highlights_only_graphemes_intersecting_byte_range() {
    let preedit = Preedit {
        text: "你好吗".into(),
        cursor: Some((3, 6)), // select only 好
    };
    let layout = layout(&preedit, 10, 3, (0, 0));
    let marked: Vec<bool> = layout.glyphs.iter().map(|g| g.selected).collect();
    assert_eq!(marked, [false, true, false]);
    assert_eq!(layout.caret, Some((2, 0)));
}

#[test]
fn preedit_wraps_from_bottom_screen_row_by_shifting_temporary_overlay_upward() {
    let preedit = Preedit {
        text: "界界界".into(),
        cursor: Some((9, 9)),
    };
    let layout = layout(&preedit, 4, 3, (3, 2));
    assert_eq!((layout.glyphs[0].col, layout.glyphs[0].row), (0, 1));
    assert_eq!((layout.glyphs[1].col, layout.glyphs[1].row), (2, 1));
    assert_eq!((layout.glyphs[2].col, layout.glyphs[2].row), (0, 2));
    assert_eq!(layout.caret, Some((2, 2)));
}

#[test]
fn disabled_ime_caret_is_hidden_but_candidate_area_stays_near_cursor() {
    let preedit = Preedit {
        text: "a b".into(),
        cursor: None,
    };
    let layout = layout(&preedit, 12, 3, (5, 2));
    assert_eq!(layout.caret, None);
    assert_eq!(layout.candidate, (5, 2));
}

#[test]
fn zero_sized_viewport_and_long_composition_are_bounded() {
    let preedit = Preedit {
        text: "你".repeat(100),
        cursor: Some((300, 300)),
    };
    assert!(layout(&preedit, 0, 5, (0, 0)).glyphs.is_empty());
    assert!(layout(&preedit, 5, 0, (0, 0)).glyphs.is_empty());
    let layout = layout(&preedit, 5, 2, (4, 1));
    assert!(layout.glyphs.len() <= 10);
    assert!(layout.glyphs.iter().all(|g| g.col < 5 && g.row < 2));
    assert!(layout.caret.is_some());
}

#[test]
fn oversized_preedit_is_rejected_without_losing_normal_ime_commits() {
    let mut ime = ImeState::default();
    ime.handle(Ime::Preedit("x".repeat(16 * 1024 + 1), None), false);
    assert!(!ime.composing());
    assert_eq!(ime.handle(Ime::Commit("ok".into()), false), Some(b"ok".to_vec()));
}

#[test]
fn intra_grapheme_caret_snaps_before_combining_mark() {
    let preedit = Preedit {
        text: "e\u{301}x".into(),
        cursor: Some((1, 1)), // byte 1 is inside the first grapheme
    };
    let result = layout(&preedit, 8, 2, (2, 0));
    assert_eq!(result.caret, Some((2, 0)));
}
