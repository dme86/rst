//! OSC 133 cell tagging regressions: the copy highlight must match only
//! the copied command, not the prompt, typed command or older output.

use super::*;

fn term(cols: usize, rows: usize, history: usize) -> Terminal {
    let config: crate::config::Config =
        toml::from_str(include_str!("../config/default.toml")).unwrap();
    Terminal::new(cols, rows, history, Palette::try_from(&config.colors).unwrap())
}

#[test]
fn only_output_cells_are_tagged_not_prompt_or_command() {
    let mut t = term(32, 3, 4);
    t.advance(b"\x1b]133;A\x07> \x1b]133;B\x07echo hi\r\n");
    t.advance(b"\x1b]133;C\x07hello\x1b]133;D\x07\r\n> ");
    let (text, id) = t.screen.command_capture.latest_output_with_id().unwrap();
    assert_eq!(text, "hello");
    assert_ne!(id, 0);
    for cell in &t.screen.grid[0] {
        assert_eq!(cell.output_id, 0);
    }
    assert!(t.screen.grid[1][..5].iter().all(|cell| cell.output_id == id));
    assert!(t.screen.grid[1][5..].iter().all(|cell| cell.output_id == 0));
    assert!(t.screen.grid[2].iter().all(|cell| cell.output_id == 0));
}

#[test]
fn old_command_tag_survives_scrollback_but_is_not_new_command_id() {
    let mut t = term(12, 2, 4);
    t.advance(b"\x1b]133;C\x07first\r\n\x1b]133;D\x07");
    let (_, first) = t.screen.command_capture.latest_output_with_id().unwrap();
    t.advance(b"\x1b]133;C\x07second\r\n\x1b]133;D\x07");
    let (_, second) = t.screen.command_capture.latest_output_with_id().unwrap();
    assert_ne!(first, second);
    assert!(t.screen.history.iter().flatten().any(|cell| cell.output_id == first));
    assert!(t.screen.grid.iter().flatten().any(|cell| cell.output_id == second));
}

#[test]
fn overwritten_or_erased_cells_lose_the_previous_command_tag() {
    let mut t = term(12, 3, 0);
    t.advance(b"\x1b]133;C\x07hello\x1b]133;D\x07");
    let (_, id) = t.screen.command_capture.latest_output_with_id().unwrap();
    assert_eq!(t.screen.grid[0][0].output_id, id);
    t.advance(b"\rX");
    assert_eq!(t.screen.grid[0][0].output_id, 0);
    assert_eq!(t.screen.grid[0][1].output_id, id);
    t.advance(b"\r\x1b[2K");
    assert!(t.screen.grid[0].iter().all(|cell| cell.output_id == 0));
}

#[test]
fn wide_glyph_lead_and_dummy_share_output_tag() {
    let mut t = term(12, 3, 0);
    t.advance(b"\x1b]133;C\x07");
    t.advance("界".as_bytes());
    let id = t.screen.command_capture.active_output_id().unwrap();
    assert_eq!(t.screen.grid[0][0].output_id, id);
    assert_eq!(t.screen.grid[0][1].output_id, id);
    assert_eq!(t.screen.grid[0][1].width, 0);
}

#[test]
fn plain_output_without_shell_integration_is_never_tagged() {
    let mut t = term(12, 2, 2);
    t.advance(b"hello");
    assert!(t.screen.grid.iter().flatten().all(|cell| cell.output_id == 0));
}
