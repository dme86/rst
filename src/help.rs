//! In-terminal keyboard help content, independent of the OS UI toolkit.
//! The popup is painted by the existing pixel renderer and is not fed into
//! PTY input or the terminal scrollback.

pub fn lines(macos: bool) -> Vec<String> {
    let primary = if macos { "Cmd" } else { "Ctrl+Shift" };
    let copy_key = if macos { "Cmd+Shift+O" } else { "Ctrl+Shift+O" };
    let links_key = if macos { "Cmd+Shift+U" } else { "Ctrl+Shift+U" };
    let help_key = if macos { "Cmd+Shift+H" } else { "Ctrl+Shift+H" };
    let new_tab = if macos { "Cmd+T" } else { "Ctrl+Shift+T" };
    let close_tab = if macos { "Cmd+W" } else { "Ctrl+Shift+W" };
    let direct_tab = if macos { "Cmd+1..9" } else { "Alt+1..9" };
    let zoom = if macos { "Cmd" } else { "Ctrl" };
    vec![
        "KEYBOARD SHORTCUTS".into(),
        "".into(),
        "Clipboard & output".into(),
        format!("  Copy last command output      {copy_key}"),
        format!("  Browse recent output URLs     {links_key}"),
        "  Link palette: j/k, arrows; Enter opens; Esc closes".into(),
        format!("  Copy selected text            {primary}+C"),
        format!("  Paste from clipboard          {primary}+V"),
        "".into(),
        "Tabs (enable with [tabs] enabled = true)".into(),
        format!("  New tab                        {new_tab}"),
        format!("  Close focused pane / last tab  {close_tab}"),
        "  Next / previous tab            Ctrl+Tab / Ctrl+Shift+Tab".into(),
        format!("  Switch directly                {direct_tab}"),
        "  Mouse                          Click tab / + / ×".into(),
        "  Tab names                      Program > directory > title".into(),
        "  Configure                      [tabs] name_mode = \"smart\"".into(),
        "".into(),
        "Splits (up to 8 independent shells per tab)".into(),
        format!("  Split left / right             {}", if macos { "Cmd+D" } else { "Ctrl+Shift+D" }),
        format!("  Split top / bottom             {}", if macos { "Cmd+Shift+D" } else { "Ctrl+Shift+E" }),
        format!("  Focus neighboring pane         {}", if macos { "Cmd+Alt+Arrow" } else { "Ctrl+Shift+Arrow" }),
        format!("  Full pane / restore layout     {}", if macos { "Cmd+Shift+F" } else { "Ctrl+Shift+F" }),
        "  Collapsed sidebar              Click a pane / Restore".into(),
        format!("  Close focused pane             {close_tab}"),
        "  Click inside a pane to focus it; tab × closes all panes".into(),
        "".into(),
        "Navigation & display".into(),
        "  Scroll through history        Shift+PgUp/PgDn".into(),
        format!("  Increase / decrease font      {zoom}++ / {zoom}+-"),
        format!("  Reset font zoom               {zoom}+0"),
        "".into(),
        "Mouse selection".into(),
        "  Double-click                  Select word".into(),
        "  Triple-click                  Select line".into(),
        "  Alt/Option + drag             Rectangle selection".into(),
        "  Shift + mouse                 Select inside TUI".into(),
        "".into(),
        "Shell integration".into(),
        "  Install/refresh: rst --install-shell-integration".into(),
        "  Supports fish, zsh, bash using OSC 133.".into(),
        "  After installing, open a new rst shell.".into(),
        "  Copy works while a command is still running.".into(),
        "".into(),
        "Help".into(),
        format!("  Open/close help               F1 / {help_key}"),
        "  Close                          Esc".into(),
        "  Scroll help                    Up/Down, PgUp/PgDn".into(),
    ]
}

pub fn viewport(lines: &[String], first: usize, available: usize) -> &[String] {
    let last = first.saturating_add(available).min(lines.len());
    &lines[first.min(last)..last]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_includes_copy_and_setup_for_both_platforms() {
        for macos in [false, true] {
            let text = lines(macos).join("\n");
            assert!(text.contains(if macos { "Cmd+Shift+O" } else { "Ctrl+Shift+O" }));
            assert!(text.contains(if macos { "Cmd+Shift+U" } else { "Ctrl+Shift+U" }));
            assert!(text.contains("rst --install-shell-integration"));
            assert!(text.contains("F1"));
            assert!(text.contains(if macos { "Cmd+T" } else { "Ctrl+Shift+T" }));
            assert!(text.contains("Alt/Option"));
        }
    }

    #[test]
    fn help_viewport_clips_and_does_not_panic_on_small_windows() {
        let all = lines(true);
        assert!(viewport(&all, 999, 10).is_empty());
        assert_eq!(viewport(&all, 0, 1).len(), 1);
        assert_eq!(viewport(&all, 2, 3).len(), 3);
    }
}
