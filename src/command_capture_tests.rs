//! End-to-end OSC 133 output capture using the actual vte parser.
//! Tests cover terminals that truncate history and fragmented PTY reads.

use super::*;
use crate::command_capture::{CommandCapture, CopyError};

fn term(cols: usize, rows: usize, history: usize) -> Terminal {
    let config: crate::config::Config =
        toml::from_str(include_str!("../config/default.toml")).unwrap();
    Terminal::new(cols, rows, history, Palette::try_from(&config.colors).unwrap())
}

#[test]
fn osc133_separates_prompt_command_and_ansi_colored_output() {
    let mut t = term(25, 3, 4);
    t.advance(b"\x1b]133;A\x07prompt> \x1b]133;B\x07kubectl logs pod\r\n");
    t.advance(b"\x1b]133;C\x07hello \x1b[31mred\x1b[0m\nworld\x1b]133;D;0\x07");
    t.advance(b"\r\nprompt> ");
    assert_eq!(t.screen.command_capture.latest_output(), Ok("hello red\nworld".into()));
    assert!(!t.screen.command_capture.running());
}

#[test]
fn osc133_split_st_terminator_only_starts_capture_after_escape() {
    let mut t = term(24, 3, 4);
    t.advance(b"\x1b]133;");
    t.advance(b"C\x1b");
    t.advance(b"\\");
    t.advance(b"payload\x1b]133;D\x1b\\");
    assert_eq!(t.screen.command_capture.latest_output(), Ok("payload".into()));
}

#[test]
fn output_survives_scrollback_eviction() {
    let mut t = term(16, 2, 1);
    t.advance(b"\x1b]133;C\x07");
    for _ in 0..120 {
        t.advance(b"one more log\n");
    }
    t.advance(b"\x1b]133;D\x07");
    assert!(t.screen.history.len() <= 1);
    let copy = t.screen.command_capture.latest_output().unwrap();
    assert_eq!(copy.lines().count(), 120);
    assert!(copy.starts_with("one more log\n"));
}

#[test]
fn copy_running_output_and_then_latest_completed_command() {
    let mut t = term(30, 4, 8);
    t.advance(b"\x1b]133;C\x07first\n\x1b]133;D\x07");
    assert_eq!(t.screen.command_capture.latest_output(), Ok("first\n".into()));
    t.advance(b"\x1b]133;C\x07");
    assert_eq!(t.screen.command_capture.latest_output(), Ok("first\n".into()));
    t.advance(b"second\n");
    assert_eq!(t.screen.command_capture.latest_output(), Ok("second\n".into()));
    t.advance(b"\x1b]133;D\x07");
    assert_eq!(t.screen.command_capture.latest_output(), Ok("second\n".into()));
}

#[test]
fn terminal_carriage_return_and_csi_k_do_not_leave_progress_artifacts() {
    let mut t = term(32, 3, 4);
    t.advance(b"\x1b]133;C\x07progress 10%\r\x1b[2Kdone\n\x1b]133;D\x07");
    assert_eq!(t.screen.command_capture.latest_output(), Ok("done\n".into()));
}

#[test]
fn command_capture_retains_unicode_and_dec_charset_translation() {
    let mut t = term(16, 3, 2);
    t.advance(b"\x1b]133;C\x07\x1b(0lqqk\x1b(B ");
    t.advance("🦀 ü".as_bytes());
    t.advance(b"\x1b]133;D\x07");
    assert_eq!(t.screen.command_capture.latest_output(), Ok("┌──┐ 🦀 ü".into()));
}

#[test]
fn capture_survives_ris_in_command_output() {
    let mut t = term(16, 3, 2);
    t.advance(b"\x1b]133;C\x07before\n\x1bcafter\x1b]133;D\x07");
    assert_eq!(t.screen.command_capture.latest_output(), Ok("before\nafter".into()));
}

#[test]
fn unknown_markers_and_no_shell_integration_never_copy_screen() {
    let mut t = term(12, 2, 1);
    t.advance(b"plain output \x1b]999;C\x07");
    assert_eq!(t.screen.command_capture.latest_output(), Err(CopyError::NoIntegration));
    t.advance(b"\x1b]133;X\x07");
    assert_eq!(t.screen.command_capture.latest_output(), Err(CopyError::NoIntegration));
}

#[test]
fn capture_can_be_limited_independently_of_terminal_history() {
    let mut t = term(24, 2, 999);
    t.screen.command_capture = CommandCapture::new(16);
    t.advance(b"\x1b]133;C\x07much too much data!\x1b]133;D\x07");
    assert_eq!(t.screen.command_capture.latest_output(), Err(CopyError::TooLarge));
    assert_eq!(t.screen.history.len(), 0);
}
