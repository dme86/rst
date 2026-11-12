//! Persist layout and directories only; never replay commands or terminal output.
use std::{collections::{BTreeMap, BTreeSet}, fs, io::{Read, Write}, path::{Path, PathBuf}};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use crate::panes::{Layout, Node, MAX_PANES};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Directory { pub id: u64, pub cwd: String }
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub version: u32,
    pub active: u64,
    pub tabs: Vec<u64>,
    pub layouts: BTreeMap<String, Layout>,
    pub directories: Vec<Directory>,
}
impl Snapshot {
    pub fn validate(&self, max_tabs: usize) -> Result<()> {
        anyhow::ensure!(self.version == 1 && !self.tabs.is_empty() && self.tabs.len() <= max_tabs, "unsupported session or tab count");
        let mut all = BTreeSet::new();
        let mut tabs = BTreeSet::new();
        for tab in &self.tabs {
            anyhow::ensure!(tabs.insert(*tab), "duplicate tab");
            let layout = self.layouts.get(&tab.to_string()).context("missing layout")?;
            let mut ids = Vec::new();
            fn visit(node: &Node, depth: usize, ids: &mut Vec<u64>) -> Result<()> {
                anyhow::ensure!(depth <= MAX_PANES && ids.len() < MAX_PANES, "too many panes");
                match node { Node::Leaf(id) => ids.push(*id), Node::Split { first, second, .. } => {
                    visit(first, depth + 1, ids)?; visit(second, depth + 1, ids)?;
                }}
                Ok(())
            }
            visit(&layout.root, 0, &mut ids)?;
            anyhow::ensure!(ids.contains(&layout.focused), "invalid focused pane");
            if ids.contains(&self.active) {
                anyhow::ensure!(layout.focused == self.active, "active pane must be focused");
            }
            for id in ids { anyhow::ensure!(id > 0 && id < u64::MAX && all.insert(id), "invalid pane ID"); }
        }
        anyhow::ensure!(all.contains(&self.active) && self.layouts.len() == self.tabs.len(), "invalid active pane");
        let mut dirs = BTreeSet::new();
        for dir in &self.directories {
            anyhow::ensure!(all.contains(&dir.id) && dirs.insert(dir.id) && dir.cwd.len() <= 4096
                && Path::new(&dir.cwd).is_absolute() && !dir.cwd.chars().any(char::is_control), "invalid directory");
        }
        Ok(())
    }
}
pub fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME").map(PathBuf::from).filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))?;
    Some(base.join("rst/session.toml"))
}
pub fn load(path: &Path, max_tabs: usize) -> Result<Option<Snapshot>> {
    let file = match fs::File::open(path) {
        Ok(file) => file, Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None), Err(err) => return Err(err.into()),
    };
    let mut data = String::new();
    file.take(131073).read_to_string(&mut data)?;
    anyhow::ensure!(data.len() <= 131072, "session file too large");
    let state: Snapshot = toml::from_str(&data)?;
    state.validate(max_tabs)?;
    Ok(Some(state))
}
pub fn save(path: &Path, data: &str) -> Result<()> {
    anyhow::ensure!(data.len() <= 131072, "session file too large");
    let parent = path.parent().context("missing session directory")?;
    fs::create_dir_all(parent)?;
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_nanos();
    let temp = parent.join(format!("session-{}-{nonce}.tmp", std::process::id()));
    let mut created = false;
    let result = (|| -> Result<()> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)] {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        created = true;
        file.write_all(data.as_bytes())?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        #[cfg(unix)] fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() && created { let _ = fs::remove_file(temp); }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Snapshot {
        let mut layout = Layout::new(1);
        layout.split(1, 2, crate::panes::Axis::Vertical);
        layout.zoomed = true;
        Snapshot { version: 1, active: 2, tabs: vec![1],
            layouts: BTreeMap::from([("1".into(), layout)]),
            directories: vec![Directory { id: 2, cwd: "/tmp/projects with spaces".into() }] }
    }
    #[test]
    fn nested_layout_round_trips_without_commands_or_screen_content() {
        let state = fixture();
        state.validate(32).unwrap();
        let data = toml::to_string(&state).unwrap();
        let restored: Snapshot = toml::from_str(&data).unwrap();
        restored.validate(32).unwrap();
        assert_eq!(restored.layouts["1"].ids(), vec![1, 2]);
        assert!(restored.layouts["1"].zoomed);
        assert_eq!(restored.layouts["1"].focused, 2);
        assert_eq!(restored.directories, state.directories);
    }
    #[test]
    fn rejects_invalid_versions_ids_focus_and_directories() {
        let mut state = fixture(); state.version = 2; assert!(state.validate(32).is_err());
        let mut state = fixture(); state.tabs.push(1); assert!(state.validate(32).is_err());
        let mut state = fixture(); state.active = 999; assert!(state.validate(32).is_err());
        let mut state = fixture(); state.layouts.get_mut("1").unwrap().focused = 999;
        assert!(state.validate(32).is_err());
        let mut state = fixture(); state.directories[0].cwd = "relative/path".into();
        assert!(state.validate(32).is_err());
        let mut state = fixture(); state.layouts.get_mut("1").unwrap().root = Node::Split {
            axis: crate::panes::Axis::Horizontal, first: Box::new(Node::Leaf(1)), second: Box::new(Node::Leaf(1)) };
        assert!(state.validate(32).is_err());
    }
    #[test]
    fn atomic_save_and_bounded_load() {
        let root = std::env::temp_dir().join(format!("rst-session-test-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let path = root.join("session.toml");
        assert!(load(&path, 32).unwrap().is_none());
        let data = toml::to_string(&fixture()).unwrap();
        save(&path, &data).unwrap();
        assert_eq!(load(&path, 32).unwrap().unwrap().active, 2);
        save(&path, &data).unwrap();
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        fs::write(&path, "x".repeat(131073)).unwrap();
        assert!(load(&path, 32).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
