//! Which Rex session and windows t3rex made, so it never touches anything else.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct State {
    /// The Rex session that holds the threads.
    pub session_id: Option<String>,
    /// Thread id to its window.
    pub windows: BTreeMap<String, Window>,
    /// Threads whose window the user closed, with the thread's `updatedAt` at the time. The
    /// window comes back once the thread changes.
    #[serde(default)]
    pub dismissed: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Window {
    pub window_id: String,
    pub label: String,
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

impl State {
    pub fn load() -> State {
        std::fs::read(path())
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
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

    pub fn thread_for_window(&self, window_id: &str) -> Option<&str> {
        self.windows
            .iter()
            .find(|(_, w)| w.window_id == window_id)
            .map(|(thread, _)| thread.as_str())
    }
}
