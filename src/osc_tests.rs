//! OSC regression vectors parsed through the real vte byte-stream parser.
use super::*;

fn terminal() -> Terminal {
    let config: crate::config::Config =
        toml::from_str(include_str!("../config/default.toml")).unwrap();
    Terminal::new(8, 3, 20, Palette::try_from(&config.colors).unwrap())
}

#[test]
fn title_and_icon_title_follow_osc_0_1_2_and_both_terminators() {
    let mut t = terminal();
    assert_eq!(t.screen.title, DEFAULT_TITLE);
    t.advance(b"\x1b]0;Hello terminal\x07");
    assert_eq!(t.screen.title, "Hello terminal");
    assert_eq!(t.screen.icon_title, "Hello terminal");
    t.advance(" \x1b]2;Neovim 🦀\x1b\\".as_bytes());
    assert_eq!(t.screen.title, "Neovim 🦀");
    assert_eq!(t.screen.icon_title, "Hello terminal");
    t.advance(b"\x1b]1;Icon only\x07");
    assert_eq!(t.screen.icon_title, "Icon only");
    assert_eq!(t.screen.title, "Neovim 🦀");
    t.advance(b"\x1b]2;\x07");
    assert_eq!(t.screen.title, DEFAULT_TITLE);
    assert_eq!(t.screen.icon_title, "Icon only");
}

#[test]
fn osc_title_works_when_split_across_multiple_reads() {
    let mut t = terminal();
    t.advance(b"\x1b]2;Hello");
    assert_eq!(t.screen.title, DEFAULT_TITLE);
    t.advance(b"; world");
    assert_eq!(t.screen.title, DEFAULT_TITLE);
    // vte 0.15 dispatches OSC on ESC (the first byte of ST), not
    // on the trailing backslash. This is correct for its parser.
    t.advance(b"\x1b");
    assert_eq!(t.screen.title, "Hello; world");
    t.advance(b"\\");
    assert_eq!(t.screen.title, "Hello; world");
}

#[test]
fn osc_4_recolors_existing_palette_cells_not_truecolor() {
    let mut t = terminal();
    let original = t.screen.palette.ansi[1];
    t.advance(b"\x1b[31mA\x1b[38;2;1;2;3mB\x1b[0m");
    assert_eq!(t.screen.grid[0][0].fg, original);
    assert_eq!(t.screen.grid[0][1].fg, 0x010203);
    t.advance(b"\x1b]4;1;#ff3300\x07");
    assert_eq!(t.screen.palette.indexed(1), 0xff3300);
    assert_eq!(t.screen.grid[0][0].fg, 0xff3300);
    assert_eq!(t.screen.grid[0][1].fg, 0x010203);
    t.advance(b"\x1b]4;1;?\x07");
    assert_eq!(t.screen.take_replies(), b"\x1b]4;1;rgb:ffff/3333/0000\x07");
    t.advance(b"\x1b]104;1\x1b\\");
    assert_eq!(t.screen.palette.indexed(1), original);
    assert_eq!(t.screen.grid[0][0].fg, original);
}

#[test]
fn osc_4_supports_256_color_indices_multiple_pairs_and_full_reset() {
    let mut t = terminal();
    let original_196 = t.screen.palette.indexed(196);
    t.advance(b"\x1b[38;5;196mX\x1b[0m");
    assert_eq!(t.screen.grid[0][0].fg, original_196);
    t.advance(b"\x1b]4;196;#112233;3;#00ff99\x1b\\");
    assert_eq!(t.screen.grid[0][0].fg, 0x112233);
    assert_eq!(t.screen.palette.indexed(3), 0x00ff99);
    t.advance(b"\x1b]4;196;?\x07");
    assert_eq!(t.screen.take_replies(), b"\x1b]4;196;rgb:1111/2222/3333\x07");
    t.advance(b"\x1b]104\x07");
    assert_eq!(t.screen.grid[0][0].fg, original_196);
    assert_eq!(t.screen.palette.indexed(3), t.screen.initial_palette.indexed(3));
}

#[test]
fn osc_10_11_12_queries_and_individual_resets() {
    let mut t = terminal();
    let base_fg = t.screen.palette.foreground;
    let base_bg = t.screen.palette.background;
    let base_cursor = t.screen.palette.cursor;
    t.advance(b"A\x1b]10;#123456\x07\x1b]11;rgb:0/f/0\x07\x1b]12;#abcdef\x07");
    assert_eq!(t.screen.palette.foreground, 0x123456);
    assert_eq!(t.screen.palette.background, 0x00ff00);
    assert_eq!(t.screen.palette.cursor, 0xabcdef);
    assert_eq!(t.screen.grid[0][0].fg, 0x123456);
    assert_eq!(t.screen.grid[0][0].bg, 0x00ff00);
    t.advance(b"\x1b]10;?\x07\x1b]11;?\x07\x1b]12;?\x07");
    assert_eq!(t.screen.take_replies(), [
        b"\x1b]10;rgb:1212/3434/5656\x07".as_slice(),
        b"\x1b]11;rgb:0000/ffff/0000\x07",
        b"\x1b]12;rgb:abab/cdcd/efef\x07",
    ].concat());
    t.advance(b"\x1b]110\x07\x1b]111\x07\x1b]112\x07");
    assert_eq!(t.screen.palette.foreground, base_fg);
    assert_eq!(t.screen.palette.background, base_bg);
    assert_eq!(t.screen.palette.cursor, base_cursor);
    assert_eq!(t.screen.grid[0][0].fg, base_fg);
    assert_eq!(t.screen.grid[0][0].bg, base_bg);
}

#[test]
fn osc_palette_refreshes_history_and_saved_alternate_screen() {
    let mut t = terminal();
    t.advance(b"\x1b[31mA\r\nB\r\nC\r\nD");
    assert_eq!(t.screen.history[0][0].ch, 'A');
    t.advance(b"\x1b[?1049h\x1b[31mZ");
    t.advance(b"\x1b]4;1;#abcdef\x07");
    assert_eq!(t.screen.grid[0][0].fg, 0xabcdef);
    assert_eq!(t.screen.history[0][0].fg, 0xabcdef);
    t.advance(b"\x1b[?1049l");
    assert_eq!(t.screen.grid[0][0].fg, 0xabcdef); // saved primary
}

#[test]
fn invalid_osc_colors_and_unknown_commands_do_not_mutate_state() {
    let mut t = terminal();
    let original = t.screen.palette.indexed(1);
    t.advance(b"\x1b]4;1;bad;600;#ffffff\x07");
    t.advance(b"\x1b]4;1;?\x07");
    assert_eq!(t.screen.palette.indexed(1), original);
    assert!(!t.screen.take_replies().is_empty());
    t.advance(b"\x1b]9999;arbitrary\x07");
    assert_eq!(t.screen.palette.indexed(1), original);
}

#[test]
fn osc_52_is_default_deny_and_read_queries_never_leak_clipboard() {
    let mut t = terminal();
    t.advance(b"\x1b]52;c;aGVsbG8=\x07");
    assert!(t.screen.take_clipboard_writes().is_empty());
    t.screen.osc52_write_allowed = true;
    t.advance(b"\x1b]52;c;?\x07");
    assert!(t.screen.take_clipboard_writes().is_empty());
    assert!(t.screen.take_replies().is_empty());
    t.advance(b"\x1b]52;c;aGVsbG8=\x1b\\");
    assert_eq!(t.screen.take_clipboard_writes(), vec!["hello".to_string()]);
    t.advance(b"\x1b]52;c;bad!base64\x07");
    assert!(t.screen.take_clipboard_writes().is_empty());
}

#[test]
fn ris_resets_palette_and_title_but_preserves_osc52_permission() {
    let mut t = terminal();
    t.screen.osc52_write_allowed = true;
    let original = t.screen.palette.ansi[2];
    t.advance(b"\x1b]2;custom\x07\x1b]4;2;#112233\x07");
    t.advance(b"\x1bc");
    assert_eq!(t.screen.title, DEFAULT_TITLE);
    assert_eq!(t.screen.palette.ansi[2], original);
    assert!(t.screen.osc52_write_allowed);
    t.advance(b"\x1b]52;c;aGVsbG8=\x07");
    assert_eq!(t.screen.take_clipboard_writes(), vec!["hello".to_string()]);
}

#[test]
fn saved_cursor_palette_references_update_after_color_change() {
    let mut t = terminal();
    t.advance(b"\x1b[31m\x1b7\x1b[0m\x1b]4;1;#fedcba\x07\x1b8Q");
    assert_eq!(t.screen.grid[0][0].fg, 0xfedcba);
}


#[test]
fn osc7_updates_cwd_without_altering_window_title_or_command_output() {
    let mut t = terminal();
    let original = t.screen.title.clone();
    t.advance(b"\x1b]7;file://localhost/Users/dan/code/rst\x07");
    assert_eq!(t.screen.cwd.as_deref(), Some("/Users/dan/code/rst"));
    assert_eq!(t.screen.title, original);
    let revision = t.screen.tab_label_revision;
    t.advance(b"\x1b]7;file://localhost/Users/dan/code/rst\x07");
    assert_eq!(t.screen.tab_label_revision, revision);
    t.advance(b"\x1b]7;file://localhost/Users/dan/code/hello%20world\x1b\\");
    assert_eq!(t.screen.cwd.as_deref(), Some("/Users/dan/code/hello world"));
    t.advance(b"\x1b]7;file://localhost/path%0Aevil\x07");
    assert_eq!(t.screen.cwd.as_deref(), Some("/Users/dan/code/hello world"));
}

#[test]
fn osc133_metadata_tracks_running_program_and_resets_to_cwd() {
    let mut t = terminal();
    t.advance(b"\x1b]7;file://localhost/Users/dan/code/rst\x07");
    t.advance(b"\x1b]133;A\x07\x1b]133;B\x07");
    t.advance(b"\x1b]133;E;k9s\x07\x1b]133;C\x07");
    assert_eq!(t.screen.foreground_program.as_deref(), Some("k9s"));
    assert!(t.screen.command_capture.running());
    let title = crate::tab_naming::label(
        t.screen.cwd.as_deref(), t.screen.foreground_program.as_deref(),
        &t.screen.title, t.screen.command_capture.running(),
        "fish", "smart", Some("/Users/dan"), 11,
    );
    assert_eq!(title, "k9s");
    t.advance(b"some output\n\x1b]133;D\x07");
    assert_eq!(t.screen.foreground_program, None);
    assert!(!t.screen.command_capture.running());
    let title = crate::tab_naming::label(
        t.screen.cwd.as_deref(), t.screen.foreground_program.as_deref(),
        &t.screen.title, t.screen.command_capture.running(),
        "fish", "smart", Some("/Users/dan"), 11,
    );
    assert_eq!(title, "~/code/rst");
    assert_eq!(t.screen.command_capture.latest_output(), Ok("some output\n".into()));
}

#[test]
fn osc133_program_metadata_is_not_part_of_output_capture() {
    let mut t = terminal();
    t.advance(b"\x1b]133;E;nvim\x07");
    assert_eq!(t.screen.foreground_program.as_deref(), Some("nvim"));
    assert!(!t.screen.command_capture.saw_markers());
    t.advance(b"\x1b]133;C\x07\x1b]133;E;malicious;extra\x07");
    assert_eq!(t.screen.foreground_program.as_deref(), Some("nvim"));
    t.advance(b"text\x1b]133;D\x07");
    assert_eq!(t.screen.foreground_program, None);
    assert_eq!(t.screen.command_capture.latest_output(), Ok("text".into()));
}


#[test]
fn fullscreen_k9s_uses_window_title_when_program_marker_is_missing() {
    let mut t = terminal();
    t.advance(b"\x1b]7;file://localhost/Users/dan\x07");
    t.advance(b"\x1b]133;C\x07");
    // fish can publish a useful OSC title even when no OSC 133;E
    // executable hint reaches the terminal (e.g. older shell hooks).
    t.advance(b"\x1b]0;k9s ~\x07\x1b[?1049h");
    assert_eq!(t.screen.foreground_program, None);
    let label = crate::tab_naming::label(
        t.screen.cwd.as_deref(), t.screen.foreground_program.as_deref(),
        &t.screen.title,
        t.screen.command_capture.running() || t.screen.in_alternate_screen(),
        "fish", "smart", Some("/Users/dan"), 13,
    );
    assert_eq!(label, "k9s");
    t.advance(b"\x1b[?1049l\x1b]133;D\x07");
    let label = crate::tab_naming::label(
        t.screen.cwd.as_deref(), t.screen.foreground_program.as_deref(),
        &t.screen.title,
        t.screen.command_capture.running() || t.screen.in_alternate_screen(),
        "fish", "smart", Some("/Users/dan"), 13,
    );
    assert_eq!(label, "~", "finished applications must not leave stale titles");
}

#[test]
fn fullscreen_app_title_works_without_any_shell_markers() {
    let mut t = terminal();
    t.advance(b"\x1b]7;file://localhost/Users/dan\x07");
    t.advance(b"\x1b[?1049h\x1b]2;nvim README.md\x07");
    assert!(!t.screen.command_capture.running());
    assert_eq!(
        crate::tab_naming::label(
            t.screen.cwd.as_deref(), t.screen.foreground_program.as_deref(),
            &t.screen.title,
            t.screen.command_capture.running() || t.screen.in_alternate_screen(),
            "fish", "smart", Some("/Users/dan"), 13,
        ), "nvim",
    );
}

#[test]
fn oversized_osc_clipboard_is_discarded_and_parser_recovers() {
    let mut t = terminal();
    t.screen.osc52_write_allowed = true;
    t.advance(b"\x1b]52;c;");
    // The unterminated payload must remain bounded across PTY reads.
    for _ in 0..32 {
        t.advance(&vec![b'A'; 8192]);
    }
    t.advance(b"\x07OK\x1b]2;recovered\x07");
    assert!(t.screen.take_clipboard_writes().is_empty());
    assert_eq!(t.screen.title, "recovered");
    assert_eq!(t.screen.grid[0][0].ch, 'O');
    assert_eq!(t.screen.grid[0][1].ch, 'K');
}

#[test]
fn maximum_supported_clipboard_survives_fixed_parser_buffer() {
    use base64::Engine;
    let mut t = terminal();
    t.screen.osc52_write_allowed = true;
    let text = "x".repeat(osc::MAX_OSC52_BYTES);
    let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    t.advance(b"\x1b]52;c;");
    for chunk in encoded.as_bytes().chunks(8192) { t.advance(chunk); }
    t.advance(b"\x1b\\");
    assert_eq!(t.screen.take_clipboard_writes(), vec![text]);
}

#[test]
fn prompt_readiness_requires_a_completed_osc_marker() {
    let mut t = terminal();
    t.advance(b"ordinary prompt> ");
    assert!(!t.screen.prompt_seen);
    t.advance(b"\x1b]133;A\x07prompt> \x1b]133;");
    t.advance(b"B");
    assert!(!t.screen.prompt_seen, "an incomplete OSC is not a ready prompt");
    t.advance(b"\x07");
    assert!(t.screen.prompt_seen);
}
