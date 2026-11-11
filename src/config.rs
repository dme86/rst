use std::{fs, path::PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    pub window: WindowConfig,
    pub font: FontConfig,
    pub cursor: CursorConfig,
    pub scrollback: ScrollbackConfig,
    pub terminal: TerminalConfig,
    pub clipboard: ClipboardConfig,
    pub colors: Colors,
}

#[derive(Clone, Debug, Deserialize)]
pub struct WindowConfig {
    pub columns: usize,
    pub rows: usize,
    pub padding: u32,
}

#[derive(Clone, Debug, Deserialize)]
pub struct FontConfig {
    pub family: String,
    pub size: f32,
    pub fallback: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CursorConfig {
    pub style: String,
    pub blink: bool,
    pub blink_interval_ms: u64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ScrollbackConfig {
    pub lines: usize,
}

#[derive(Clone, Debug, Deserialize)]
pub struct TerminalConfig {
    pub term: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ClipboardConfig {
    pub enabled: bool,
    pub copy_on_select: bool,
    /// Allow applications to replace the host clipboard via OSC 52.
    /// Disabled by default; clipboard reads are never supported.
    #[serde(default)]
    pub osc52_write: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Colors {
    pub foreground: String,
    pub background: String,
    pub cursor: String,
    pub selection: String,
    pub palette: Vec<String>,
}

pub fn path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME").unwrap_or_else(|| ".".into());
            PathBuf::from(home).join(".config")
        });
    base.join("rst").join("config.toml")
}

// Merge user tables recursively; unspecified settings retain the embedded defaults.
fn merge(base: &mut toml::Value, override_value: toml::Value) {
    if let (Some(base_table), toml::Value::Table(override_table)) =
        (base.as_table_mut(), &override_value)
    {
        for (key, value) in override_table {
            match base_table.get_mut(key) {
                Some(existing) => merge(existing, value.clone()),
                None => {
                    base_table.insert(key.clone(), value.clone());
                }
            }
        }
    } else {
        *base = override_value;
    }
}

pub fn load() -> Result<Config> {
    let mut settings: toml::Value = toml::from_str(include_str!("../config/default.toml"))
        .context("invalid embedded default config")?;

    let user_path = path();
    if user_path.exists() {
        let contents = fs::read_to_string(&user_path)
            .with_context(|| format!("cannot read {}", user_path.display()))?;
        let user: toml::Value = toml::from_str(&contents)
            .with_context(|| format!("invalid TOML in {}", user_path.display()))?;
        merge(&mut settings, user);
    }

    let config: Config = settings.try_into().context("invalid rst config values")?;
    anyhow::ensure!(config.window.columns > 0 && config.window.rows > 0, "window size must be positive");
    anyhow::ensure!(config.font.size >= 6.0 && config.font.size <= 72.0, "font size must be 6..72");
    anyhow::ensure!(config.window.padding <= 128, "padding must be <= 128");
    anyhow::ensure!(config.cursor.style == "block", "only cursor.style = 'block' is supported in v0.1");
    anyhow::ensure!((100..=3000).contains(&config.cursor.blink_interval_ms), "cursor.blink_interval_ms must be 100..3000");
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_default_uses_requested_background() {
        let defaults: Config =
            toml::from_str(include_str!("../config/default.toml")).unwrap();
        assert_eq!(defaults.colors.background, "#282828");
    }

    #[test]
    fn default_padding_and_cursor_blink_are_enabled() {
        let config: Config =
            toml::from_str(include_str!("../config/default.toml")).unwrap();
        assert_eq!(config.window.padding, 6);
        assert!(config.cursor.blink);
        assert_eq!(config.cursor.blink_interval_ms, 700);
    }

    #[test]
    fn bundled_default_font_size_matches_alacritty() {
        let defaults: Config =
            toml::from_str(include_str!("../config/default.toml")).unwrap();
        assert_eq!(defaults.font.size, 16.5);
    }

    #[test]
    fn bundled_default_prefers_0xproto_regular() {
        let defaults: Config =
            toml::from_str(include_str!("../config/default.toml")).unwrap();
        assert_eq!(defaults.font.family, "0xProto Nerd Font");
        assert!(defaults.font.fallback.iter().any(|name| name == "Symbols Nerd Font Mono"));
    }

    #[test]
    fn partial_configuration_inherits_defaults() {
        let mut defaults: toml::Value =
            toml::from_str(include_str!("../config/default.toml")).unwrap();
        let overrides = toml::from_str("[font]\nsize = 18.0\n").unwrap();
        merge(&mut defaults, overrides);
        let config: Config = defaults.try_into().unwrap();
        assert_eq!(config.font.size, 18.0);
        assert_eq!(config.scrollback.lines, 10_000);
    }
}
