//! Which Rex sessions and windows t3rex made, so it never touches anything else.
//!
//! Version 2 keys each window by its thread on its machine, `env/thread`, and groups windows
//! by the session that holds them: one group, `""`, for the merged T3 Code session, or one per
//! machine, by its environment id. Version 1 had one session and bare thread ids, all on this
//! Mac; `migrate` moves it over.

use std::collections::BTreeMap;
use std::path::PathBuf;

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
    if let Some(dir) = std::env::var_os("T3REX_STATE_DIR") {
        return PathBuf::from(dir);
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join(".local/state/t3rex")
}

fn path() -> PathBuf {
    dir().join("state.json")
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
        std::fs::read(path())
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
    pub fn save(&self) -> Result<()> {
        let path = path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }

    /// Every session t3rex keeps.
    pub fn sessions(&self) -> Vec<String> {
        self.groups
            .values()
            .filter_map(|g| g.session_id.clone())
            .collect()
    }

    /// The merged session, else the first one t3rex keeps.
    pub fn session(&self) -> Option<String> {
        self.groups
            .get("")
            .and_then(|g| g.session_id.clone())
            .or_else(|| self.sessions().into_iter().next())
    }

    pub fn window_count(&self) -> usize {
        self.groups.values().map(|g| g.windows.len()).sum()
    }

    /// The `env/thread` of a window t3rex made, in any of its sessions.
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
