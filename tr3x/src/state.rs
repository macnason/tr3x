//! Which Rex sessions and windows tr3x made, so it never touches anything else.
//!
//! Version 2 keys each window by its thread on its machine, `env/thread`, and groups windows
//! by the session that holds them: one group, `""`, for the merged T3 Code session, or one per
//! machine, by its environment id. Version 1 had one session and bare thread ids, all on this
//! Mac; `migrate` moves it over.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Group {
    /// The Rex session that holds the group's threads.
    pub session_id: Option<String>,
    /// `env/thread` to its window.
    #[serde(default)]
    pub windows: BTreeMap<String, Window>,
    /// Threads whose window the user closed, with the thread's `updatedAt` at the time. The
    /// window comes back once the thread changes.
    #[serde(default)]
    pub dismissed: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Window {
    #[serde(alias = "window_id")]
    pub window_id: String,
    pub label: String,
}

/// Where ⌃⌘I goes: the window of the thread that most needs you, on any machine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Next {
    pub session_id: String,
    pub window_id: Option<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct State {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub groups: BTreeMap<String, Group>,
    #[serde(default)]
    pub next: Option<Next>,
}

pub fn dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("TR3X_STATE_DIR") {
        return PathBuf::from(dir);
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join(".local/state/tr3x")
}

fn path() -> PathBuf {
    dir().join("state.json")
}

/// The state file, and the state that last reached it. Sync reconciles every few seconds and
/// on every burst of events, and almost always finds nothing to change: `save` then writes
/// nothing.
pub struct Disk {
    path: PathBuf,
    saved: State,
}

impl Disk {
    /// `found` is what `State::load` read from the file, before any change of ours.
    pub fn new(found: State) -> Disk {
        Disk::at(path(), found)
    }

    pub fn at(path: PathBuf, found: State) -> Disk {
        Disk { path, saved: found }
    }

    /// Writes `state` unless the file already holds it. What counts as held moves only when a
    /// write works, so one that failed is tried again by the next call. A file that has gone
    /// missing is written again. Returns whether it wrote.
    pub fn save(&mut self, state: &State) -> Result<bool> {
        if *state == self.saved && self.path.exists() {
            return Ok(false);
        }
        state.save_to(&self.path)?;
        self.saved = state.clone();
        Ok(true)
    }
}

/// Version 1's whole file.
#[derive(Deserialize)]
struct V1 {
    session_id: Option<String>,
    #[serde(default)]
    windows: BTreeMap<String, Window>,
    #[serde(default)]
    dismissed: BTreeMap<String, String>,
}

impl State {
    pub fn load() -> State {
        State::load_from(&path())
    }

    fn load_from(path: &Path) -> State {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .map(State::read)
            .unwrap_or_default()
    }

    fn read(value: Value) -> State {
        if value["version"].as_u64().unwrap_or(0) >= 2 {
            return serde_json::from_value(value).unwrap_or_default();
        }
        // Version 1: bare thread ids, all on this Mac, in one session. Kept as they are until
        // `migrate` knows this Mac's environment id.
        let Ok(v1) = serde_json::from_value::<V1>(value) else {
            return State::default();
        };
        let mut state = State {
            version: 1,
            ..State::default()
        };
        state.groups.insert(
            String::new(),
            Group {
                session_id: v1.session_id,
                windows: v1.windows,
                dismissed: v1.dismissed,
            },
        );
        state
    }

    /// Puts version 1's bare thread ids on `home`, this Mac's environment.
    pub fn migrate(&mut self, home: &str) {
        if self.version >= 2 {
            return;
        }
        fn keyed<V>(map: &mut BTreeMap<String, V>, home: &str) {
            *map = std::mem::take(map)
                .into_iter()
                .map(|(k, v)| {
                    if k.contains('/') {
                        (k, v)
                    } else {
                        (format!("{home}/{k}"), v)
                    }
                })
                .collect();
        }
        for group in self.groups.values_mut() {
            keyed(&mut group.windows, home);
            keyed(&mut group.dismissed, home);
        }
        self.version = 2;
    }

    /// Writes through a temporary file, so a reader never sees half a file.
    fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }

    /// Every session tr3x keeps.
    pub fn sessions(&self) -> Vec<String> {
        self.groups
            .values()
            .filter_map(|g| g.session_id.clone())
            .collect()
    }

    /// The merged session, else the first one tr3x keeps.
    pub fn session(&self) -> Option<String> {
        self.groups
            .get("")
            .and_then(|g| g.session_id.clone())
            .or_else(|| self.sessions().into_iter().next())
    }

    pub fn window_count(&self) -> usize {
        self.groups.values().map(|g| g.windows.len()).sum()
    }

    /// The `env/thread` of a window tr3x made, in any of its sessions.
    pub fn thread_for_window(&self, window_id: &str) -> Option<&str> {
        self.groups
            .values()
            .flat_map(|g| g.windows.iter())
            .find(|(_, w)| w.window_id == window_id)
            .map(|(thread, _)| thread.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn group_with(thread: &str, window: &str) -> State {
        let mut state = State {
            version: 2,
            ..State::default()
        };
        let mut group = Group::default();
        group.windows.insert(
            thread.into(),
            Window {
                window_id: window.into(),
                label: "A".into(),
            },
        );
        state.groups.insert(String::new(), group);
        state
    }

    #[test]
    fn unchanged_state_is_not_written_again() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("state.json");
        let state = group_with("e/a", "w1");
        let mut disk = Disk::at(file.clone(), State::default());
        assert!(disk.save(&state).unwrap());
        let written = std::fs::read_to_string(&file).unwrap();
        assert_eq!(State::read(serde_json::from_str(&written).unwrap()), state);

        // A write would replace this; a skipped one leaves it alone.
        std::fs::write(&file, "untouched").unwrap();
        assert!(!disk.save(&state).unwrap());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "untouched");

        let mut changed = state.clone();
        changed
            .groups
            .get_mut("")
            .unwrap()
            .dismissed
            .insert("e/b".into(), "u1".into());
        assert!(disk.save(&changed).unwrap());
        assert_eq!(State::load_from(&file), changed);
        assert!(!disk.save(&changed).unwrap());

        // Found as it is on disk (a restart), nothing to do; found as it was before a
        // migration, the migrated state is written.
        let mut restarted = Disk::at(file.clone(), State::load_from(&file));
        assert!(!restarted.save(&changed).unwrap());
        let mut migrating = Disk::at(
            file.clone(),
            State {
                version: 1,
                ..changed.clone()
            },
        );
        assert!(migrating.save(&changed).unwrap());
    }

    #[test]
    fn a_missing_file_is_written_again() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("state.json");
        let state = group_with("e/a", "w1");
        let mut disk = Disk::at(file.clone(), State::default());
        disk.save(&state).unwrap();
        std::fs::remove_file(&file).unwrap();
        assert!(disk.save(&state).unwrap());
        assert_eq!(State::load_from(&file), state);
    }

    #[test]
    fn a_failed_write_is_tried_again() {
        let dir = tempfile::tempdir().unwrap();
        // The state file's folder is a file: nothing can be written there.
        let blocked = dir.path().join("dir");
        std::fs::write(&blocked, "in the way").unwrap();
        let file = blocked.join("state.json");
        let state = group_with("e/a", "w1");
        let mut disk = Disk::at(file.clone(), State::default());
        assert!(disk.save(&state).is_err());
        assert!(disk.save(&state).is_err());

        std::fs::remove_file(&blocked).unwrap();
        assert!(disk.save(&state).unwrap());
        assert_eq!(State::load_from(&file), state);
        assert!(!disk.save(&state).unwrap());
    }

    #[test]
    fn version_one_moves_to_this_mac() {
        let v1 = json!({
            "session_id": "s1",
            "windows": {"t-1": {"window_id": "w1", "label": "A"}},
            "dismissed": {"t-2": "2026-10-30T00:00:00.000Z"},
        });
        let mut state = State::read(v1);
        state.migrate("home");
        let group = &state.groups[""];
        assert_eq!(group.session_id.as_deref(), Some("s1"));
        assert_eq!(group.windows["home/t-1"].window_id, "w1");
        assert!(group.dismissed.contains_key("home/t-2"));
        assert_eq!(state.version, 2);
        assert_eq!(state.thread_for_window("w1"), Some("home/t-1"));
        // Written and read again, it stays version 2.
        let again = State::read(serde_json::to_value(&state).unwrap());
        assert_eq!(again, state);
    }
}
