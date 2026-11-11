//! Explicit, idempotent shell-integration installer.
//!
//! Only the --install-shell-integration flag modifies shell rc files.
//! Normal startup does not touch user shell configurations.
//! Scripts are embedded via include_str! so app bundles need no resource
//! lookup and work exactly like the Linux executable.

use std::{env, fs, path::{Path, PathBuf}};

use anyhow::{Context, Result};

const BASH: &str = include_str!("../shell-integration/bash.sh");
const ZSH: &str = include_str!("../shell-integration/zsh.zsh");
const FISH: &str = include_str!("../shell-integration/fish.fish");
const MARKER: &str = "# rst shell integration (managed)";

fn shell_hook(script: &str) -> String {
    if script == "fish.fish" {
        format!(r#"
{MARKER}
if test "$TERM_PROGRAM" = rst
    set -l rst_config_dir "$HOME/.config"
    if set -q XDG_CONFIG_HOME
        set rst_config_dir "$XDG_CONFIG_HOME"
    end
    source "$rst_config_dir/rst/shell-integration/fish.fish"
end
"#)
    } else {
        format!(r#"
{MARKER}
if [ "$TERM_PROGRAM" = rst ]; then
    . "${{XDG_CONFIG_HOME:-$HOME/.config}}/rst/shell-integration/{script}"
fi
"#)
    }
}

fn append_once(existing: &str, snippet: &str) -> Option<String> {
    if existing.contains(MARKER) {
        return None;
    }
    let mut updated = existing.to_owned();
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str(snippet);
    Some(updated)
}

fn write_rc(path: &Path, script: &str) -> Result<bool> {
    let existing = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err).with_context(|| format!("read {}", path.display())),
    };
    let Some(content) = append_once(&existing, &shell_hook(script)) else {
        return Ok(false);
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, content).with_context(|| format!("write {}", path.display()))?;
    Ok(true)
}

pub fn install() -> Result<Vec<String>> {
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME not set; cannot find shell initialization files")?;
    let config_base = env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    let scripts_dir = config_base.join("rst").join("shell-integration");
    fs::create_dir_all(&scripts_dir)
        .with_context(|| format!("create {}", scripts_dir.display()))?;

    let mut actions = Vec::new();
    for (name, source) in [("bash.sh", BASH), ("zsh.zsh", ZSH), ("fish.fish", FISH)] {
        let path = scripts_dir.join(name);
        if fs::read_to_string(&path).ok().as_deref() != Some(source) {
            fs::write(&path, source)
                .with_context(|| format!("install {}", path.display()))?;
            actions.push(format!("Installed {}", path.display()));
        }
    }

    let zsh_home = env::var_os("ZDOTDIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.clone());
    let startup_files = [
        (home.join(".bashrc"), "bash.sh"),
        (zsh_home.join(".zshrc"), "zsh.zsh"),
        (config_base.join("fish").join("config.fish"), "fish.fish"),
    ];
    for (path, script) in startup_files {
        if write_rc(&path, script)? {
            actions.push(format!("Enabled in {}", path.display()));
        }
    }

    Ok(actions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hooks_are_idempotent_and_do_not_discard_user_settings() {
        let original = "export PATH=\"$HOME/bin:$PATH\"";
        let snippet = shell_hook("bash.sh");
        let next = append_once(original, &snippet).unwrap();
        assert!(next.starts_with(original));
        assert_eq!(append_once(&next, &snippet), None);
    }

    #[test]
    fn bundled_hooks_emit_osc_133_and_do_not_touch_unrelated_terminals() {
        for script in [BASH, ZSH, FISH] {
            assert!(script.contains("TERM_PROGRAM"));
            assert!(script.contains("133;A"));
            assert!(script.contains("133;B") || script.contains("133;C"));
            assert!(script.contains("133;C"));
            assert!(script.contains("133;D"));
            assert!(script.contains("133;E;"));
            assert!(script.contains("7;file://localhost"));
        }
    }

    #[test]
    fn fish_hook_uses_native_fish_syntax() {
        let snippet = shell_hook("fish.fish");
        assert!(snippet.contains("set -l"));
        assert!(snippet.contains("source"));
        assert!(!snippet.contains("${XDG_CONFIG_HOME:-"));
    }
}
