//! Earlier versions could turn Rex's pane fading off while the T3 Code session showed
//! (`"paneFade": "auto"`). Rex's own Settings → Interface → Active Pane covers it now, so tr3x
//! only puts back the setting it saved, if it ever changed it.

use std::path::PathBuf;

use serde_json::{Map, Value};

use crate::state;

/// The Rex app's preference domain, which `defaults` reads and writes.
pub const DOMAIN: &str = "com.superlogical.rex";
const KEY: &str = "Workspace.inactivePaneFadeMode";

/// Your own setting, kept the first time tr3x changed it: `{KEY: value}`, `null` for unset.
fn backup() -> PathBuf {
    state::dir().join("rex-defaults.json")
}

fn saved() -> Option<Option<String>> {
    let text = std::fs::read(backup()).ok()?;
    let map: Map<String, Value> = serde_json::from_slice(&text).ok()?;
    Some(map.get(KEY)?.as_str().map(str::to_string))
}

fn read() -> Option<String> {
    let out = std::process::Command::new("/usr/bin/defaults")
        .args(["read", DOMAIN, KEY])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn write(value: Option<&str>) {
    let mut command = std::process::Command::new("/usr/bin/defaults");
    match value {
        Some(v) => command.args(["write", DOMAIN, KEY, "-string", v]),
        None => command.args(["delete", DOMAIN, KEY]),
    };
    let _ = command.output();
}

/// Puts your own setting back, if tr3x ever changed it. Returns whether it did.
pub fn restore() -> bool {
    let Some(value) = saved() else {
        return false;
    };
    if read() != value {
        write(value.as_deref());
    }
    let _ = std::fs::remove_file(backup());
    true
}
