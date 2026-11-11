//! Tab names from native foreground executables and OSC metadata.
//! No shell invocation or per-frame process lookup is needed.

const MAX_METADATA_BYTES: usize = 4096;
const MAX_PROGRAM_BYTES: usize = 128;

/// Decode OSC 7 file://host/absolute/path, including percent-encoded UTF-8.
/// A remote hostname is allowed; only the path is displayed. Invalid/control
/// payloads are ignored rather than changing tab labels.
pub fn cwd_from_osc7(raw: &[u8]) -> Option<String> {
    if raw.is_empty() || raw.len() > MAX_METADATA_BYTES {
        return None;
    }
    let rest = raw.strip_prefix(b"file://")?;
    let start = rest.iter().position(|b| *b == b'/')?;
    let path = &rest[start..];
    let mut decoded = Vec::with_capacity(path.len());
    let mut index = 0;
    while index < path.len() {
        if path[index] == b'%' && index + 2 < path.len() {
            if let (Some(a), Some(b)) =
                (hex_value(path[index + 1]), hex_value(path[index + 2]))
            {
                decoded.push((a << 4) | b);
                index += 3;
                continue;
            }
        }
        decoded.push(path[index]);
        index += 1;
    }
    let value = String::from_utf8(decoded).ok()?;
    if !value.starts_with('/')
        || value.chars().any(|ch| ch.is_control())
    {
        return None;
    }
    let without_trailing_slashes = value.trim_end_matches('/');
    Some(if without_trailing_slashes.is_empty() {
        "/".to_owned()
    } else {
        without_trailing_slashes.to_owned()
    })
}

fn hex_value(ch: u8) -> Option<u8> {
    match ch {
        b'0'..=b'9' => Some(ch - b'0'),
        b'a'..=b'f' => Some(ch - b'a' + 10),
        b'A'..=b'F' => Some(ch - b'A' + 10),
        _ => None,
    }
}

/// OSC 133;E is a short, sanitized executable label. Reject shell escape
/// syntax and arbitrary control characters from untrusted PTY output.
pub fn program_from_osc133(raw: &[u8]) -> Option<String> {
    if raw.is_empty() || raw.len() > MAX_PROGRAM_BYTES {
        return None;
    }
    let text = std::str::from_utf8(raw).ok()?;
    let name = text.rsplit('/').next()?;
    if name.is_empty() || name.len() > 64 ||
        !name.bytes().all(|ch| ch.is_ascii_alphanumeric() || b"._+-".contains(&ch))
    {
        return None;
    }
    Some(name.to_owned())
}

/// Fish and other shells can update OSC 0/2 window titles before a TUI
/// starts (for instance "k9s ~"). If an OSC 133;E executable hint is
/// absent, extract a safe program token from that title *only while a
/// command or alternate-screen application is running*. At the prompt,
/// directory labels still win over an outdated application title.
fn program_from_running_title(title: &str, shell: &str) -> Option<String> {
    if title == crate::terminal::DEFAULT_TITLE {
        return None;
    }
    let first = title.split_whitespace().next()?;
    // Never mistake a CWD or user@host prompt title for a program.
    if first == shell
        || first.chars().next().is_some_and(|ch| matches!(ch, '/' | '~' | '.'))
        || first.chars().any(|ch| matches!(ch, '@' | ':' | '/' | '\\'))
    {
        return None;
    }
    program_from_osc133(first.as_bytes())
}

/// Max characters for a tab label, excluding tab number, space and X icon.
pub fn label_columns(tab_columns: usize, close_buttons: bool) -> usize {
    tab_columns.saturating_sub(if close_buttons { 7 } else { 4 }).max(1)
}

pub fn directory_label(cwd: &str, home: Option<&str>, max_chars: usize) -> String {
    let abbreviated = home.filter(|h| !h.is_empty() && *h != "/")
        .and_then(|home| cwd.strip_prefix(home).and_then(|rest| {
            if rest.is_empty() || rest.starts_with('/') {
                Some(format!("~{rest}"))
            } else {
                None
            }
        }))
        .unwrap_or_else(|| cwd.to_owned());
    if abbreviated.chars().count() <= max_chars {
        return abbreviated;
    }
    // Keep a useful basename rather than cutting a path in the middle.
    let leaf = cwd.rsplit('/').find(|part| !part.is_empty()).unwrap_or("/");
    truncate(leaf, max_chars)
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    if max_chars <= 1 {
        return "…".into();
    }
    let mut shortened: String = text.chars().take(max_chars - 1).collect();
    shortened.push('…');
    shortened
}

#[allow(clippy::too_many_arguments)]
pub fn label(
    cwd: Option<&str>,
    program: Option<&str>,
    title: &str,
    running: bool,
    shell: &str,
    mode: &str,
    home: Option<&str>,
    max_chars: usize,
) -> String {
    let max_chars = max_chars.max(1);
    let dir = || cwd.map(|cwd| directory_label(cwd, home, max_chars));
    let prog = || running.then_some(program).flatten()
        .filter(|value| !value.is_empty())
        .map(|value| truncate(value, max_chars));
    let normal_title = || (!title.is_empty()
        && title != crate::terminal::DEFAULT_TITLE)
        .then(|| truncate(title, max_chars));
    let title_program = || running.then(||
        program_from_running_title(title, shell)
            .map(|name| truncate(&name, max_chars))
    ).flatten();
    match mode {
        "directory" => dir(),
        "title" => normal_title(),
        "shell" => Some(truncate(shell, max_chars)),
        _ => prog().or_else(title_program).or_else(dir).or_else(normal_title),
    }
    .unwrap_or_else(|| truncate(shell, max_chars))
}

/// Prefer the executable resolved from the actual foreground PTY process
/// group over the user's typed command or an alias. Keep all old naming
/// modes and OSC fallback behavior unchanged when OS lookup is unavailable.
#[allow(clippy::too_many_arguments)]
pub fn label_with_process(
    actual_process: Option<&str>,
    cwd: Option<&str>,
    command_hint: Option<&str>,
    title: &str,
    running: bool,
    shell: &str,
    mode: &str,
    home: Option<&str>,
    max_chars: usize,
) -> String {
    if mode == "smart" && running {
        if let Some(name) = actual_process
            .and_then(|value| program_from_osc133(value.as_bytes()))
        {
            return truncate(&name, max_chars.max(1));
        }
    }
    label(cwd, command_hint, title, running, shell, mode, home, max_chars)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_cwd_osc7_and_utf8_percent_encoding() {
        assert_eq!(cwd_from_osc7(b"file://localhost/Users/dan/code/rst"),
            Some("/Users/dan/code/rst".into()));
        assert_eq!(cwd_from_osc7(b"file:///home/user/Projekts%20%26%20Tests"),
            Some("/home/user/Projekts & Tests".into()));
        assert_eq!(cwd_from_osc7(b"file://host/%C3%BCbung"),
            Some("/übung".into()));
        assert_eq!(cwd_from_osc7(b"file:///"), Some("/".into()));
        assert_eq!(cwd_from_osc7(b"https://example.org/"), None);
        assert_eq!(cwd_from_osc7(b"file://host"), None);
        assert_eq!(cwd_from_osc7(b"file://localhost/path%0Ahello"), None);
        assert_eq!(cwd_from_osc7(&vec![b'x'; 5000]), None);
    }

    #[test]
    fn accepts_only_safe_executable_names() {
        assert_eq!(program_from_osc133(b"/opt/bin/k9s"), Some("k9s".into()));
        assert_eq!(program_from_osc133(b"nvim"), Some("nvim".into()));
        assert_eq!(program_from_osc133(b"rust-analyzer"), Some("rust-analyzer".into()));
        assert_eq!(program_from_osc133(b"rm;foo"), None);
        assert_eq!(program_from_osc133(b"k9s\x07"), None);
        assert_eq!(program_from_osc133(b""), None);
    }

    #[test]
    fn smart_mode_prioritizes_running_program_then_directory() {
        let title = crate::terminal::DEFAULT_TITLE;
        let make = |mode, running, prog| label(
            Some("/Users/dan/code/rst"), prog, title,
            running, "fish", mode, Some("/Users/dan"), 11,
        );
        assert_eq!(make("smart", true, Some("k9s")), "k9s");
        assert_eq!(make("smart", true, Some("nvim")), "nvim");
        assert_eq!(make("smart", false, Some("k9s")), "~/code/rst");
        assert_eq!(make("directory", true, Some("k9s")), "~/code/rst");
        assert_eq!(make("shell", true, Some("k9s")), "fish");
    }

    #[test]
    fn smart_mode_uses_running_fish_title_when_preexec_hint_is_missing() {
        let cwd = Some("/Users/dan");
        let title = "k9s ~";
        assert_eq!(
            label(cwd, None, title, true, "fish", "smart", cwd, 13),
            "k9s",
        );
        assert_eq!(
            label(cwd, None, title, false, "fish", "smart", cwd, 13),
            "~",
        );
        assert_eq!(
            label(cwd, None, title, true, "fish", "directory", cwd, 13),
            "~",
        );
        assert_eq!(
            label(cwd, None, title, true, "fish", "title", cwd, 13),
            "k9s ~",
        );
    }

    #[test]
    fn title_fallback_rejects_cwd_and_shell_prompt_titles() {
        for title in ["~/code/rst", "/Users/dan", "fish ~",
                      "dan@mac: ~/code", "rst — Rust Simple Terminal"] {
            assert_eq!(
                label(Some("/Users/dan"), None, title, true,
                      "fish", "smart", Some("/Users/dan"), 13),
                "~",
                "must not interpret {title:?} as a program",
            );
        }
        assert_eq!(
            label(Some("/Users/dan"), Some("nvim"), "k9s ~", true,
                  "fish", "smart", Some("/Users/dan"), 13),
            "nvim",
            "explicit OSC 133 metadata takes priority over the window title",
        );
    }

    #[test]
    fn actual_process_wins_over_alias_and_fish_window_title() {
        let base = |running, mode| label_with_process(
            Some("nvim"),
            Some("/Users/dan/code/rst"),
            Some("vim"),
            "vim ~/code/rst",
            running, "fish", mode, Some("/Users/dan"), 13,
        );
        assert_eq!(base(true, "smart"), "nvim");
        assert_eq!(base(false, "smart"), "~/code/rst");
        assert_eq!(base(true, "directory"), "~/code/rst");
        assert_eq!(base(true, "title"), "vim ~/code/r…");
        assert_eq!(base(true, "shell"), "fish");
    }

    #[test]
    fn process_lookup_failure_falls_back_to_existing_hints() {
        assert_eq!(label_with_process(
            None, Some("/Users/dan"), Some("vim"), "vim ~",
            true, "fish", "smart", Some("/Users/dan"), 13,
        ), "vim");
        assert_eq!(label_with_process(
            Some("not a valid executable"), Some("/Users/dan"), None, "k9s ~",
            true, "fish", "smart", Some("/Users/dan"), 13,
        ), "k9s");
    }

    #[test]
    fn long_paths_use_basename_and_long_names_ellipsis() {
        assert_eq!(directory_label("/Users/dan/very-long-directory",
                                  Some("/Users/dan"), 10), "very-long…");
        assert_eq!(directory_label("/Users/dan", Some("/Users/dan"), 11), "~");
        assert_eq!(directory_label("/", Some("/Users/dan"), 11), "/");
        assert_eq!(label_columns(16, true), 9);
        assert_eq!(label_columns(20, true), 13);
    }
}
