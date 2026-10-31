//! Rex fades inactive panes, which dims the conversation while you type in Reply. The setting
//! is one for the whole app (Settings → Interface → Pane Behavior → Fade inactive panes), so
//! t3rex leaves it alone unless t3rex's settings say `"paneFade": "auto"`. Then sync turns
//! fading off while the Rex app on this Mac shows the T3 Code session, and puts your own
//! setting back everywhere else.
//!
//! Rex's server can't say which session an app is showing: every app is attached to every
//! session. The app's own `Workspace.lastFocusedSession` preference follows it, within a
//! second.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Map, Value};

use crate::state::{self, State};

/// The Rex app's preference domain, which `defaults` reads and writes.
pub const DOMAIN: &str = "com.superlogical.rex";
pub const KEY: &str = "Workspace.inactivePaneFadeMode";
const FOCUSED: &str = "Workspace.lastFocusedSession";
/// What the T3 Code session gets.
const OFF: &str = "none";

/// Whether t3rex's settings ask for it.
pub fn enabled() -> bool {
    crate::width::settings()
        .and_then(|s| s.get("paneFade").cloned())
        .is_some_and(|v| v == "auto")
}

/// Your own setting, kept the first time t3rex changes it: `{KEY: value}`, `null` for unset.
fn backup() -> PathBuf {
    state::dir().join("rex-defaults.json")
}

fn saved() -> Option<Option<String>> {
    let text = std::fs::read(backup()).ok()?;
    let map: Map<String, Value> = serde_json::from_slice(&text).ok()?;
    Some(map.get(KEY)?.as_str().map(str::to_string))
}

fn save(value: Option<&str>) {
    let map = Map::from_iter([(KEY.to_string(), value.map_or(Value::Null, Value::from))]);
    if let Ok(bytes) = serde_json::to_vec_pretty(&map) {
        let _ = std::fs::create_dir_all(state::dir());
        let _ = std::fs::write(backup(), bytes);
    }
}

pub fn read() -> Option<String> {
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

/// Puts your own setting back, if t3rex ever changed it. Returns whether it did.
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

/// The session the Rex app on this Mac last showed, from its preferences.
fn shown() -> Option<String> {
    let out = std::process::Command::new("/usr/bin/defaults")
        .args(["export", DOMAIN, "-"])
        .output()
        .ok()?;
    shown_in(&String::from_utf8_lossy(&out.stdout))
}

/// `FOCUSED` in an exported plist: base64 `<data>` holding `{"sessionID": …}`.
fn shown_in(plist: &str) -> Option<String> {
    use base64::Engine;
    let after = plist.split_once(&format!("<key>{FOCUSED}</key>"))?.1;
    let data = after.trim_start().strip_prefix("<data>")?;
    let data: String = data.split_once("</data>")?.0.split_whitespace().collect();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .ok()?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    value["sessionID"].as_str().map(str::to_string)
}

/// What the setting should be, and whether `current` is now your own: off in the T3 Code
/// session, and elsewhere yours. A value that differs from what t3rex last left (`wrote`) was
/// changed in Rex's settings, so outside T3 Code it becomes yours.
fn decide(
    in_t3: bool,
    current: &Option<String>,
    yours: &Option<String>,
    wrote: Option<&Option<String>>,
) -> (Option<String>, bool) {
    if in_t3 {
        return (Some(OFF.to_string()), false);
    }
    if wrote.is_some_and(|w| w != current) {
        return (current.clone(), true);
    }
    (yours.clone(), false)
}

/// Keeps fading off while the app shows the T3 Code session, for as long as sync runs and
/// t3rex's settings ask for it. Turning it off puts your setting back.
pub async fn follow() {
    let mut wrote: Option<Option<String>> = None;
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if !enabled() {
            if wrote.take().is_some() {
                restore();
            }
            continue;
        }
        let current = read();
        let yours = saved().unwrap_or_else(|| {
            save(current.as_deref());
            current.clone()
        });
        let in_t3 = State::load().session_id.is_some_and(|s| shown() == Some(s));
        let (want, adopt) = decide(in_t3, &current, &yours, wrote.as_ref());
        if adopt {
            save(current.as_deref());
        }
        if want != current {
            write(want.as_deref());
        }
        wrote = Some(want);
    }
}

#[cfg(test)]
mod tests {
    use super::{decide, shown_in};

    fn v(s: &str) -> Option<String> {
        Some(s.to_string())
    }

    #[test]
    fn off_in_t3_code_and_yours_elsewhere() {
        let yours = v("medium");
        // Into T3 Code, and out again.
        assert_eq!(decide(true, &yours, &yours, None), (v("none"), false));
        let wrote = v("none");
        assert_eq!(
            decide(false, &v("none"), &yours, Some(&wrote)),
            (yours.clone(), false)
        );
        // Unset is a setting of its own, put back as unset.
        assert_eq!(
            decide(false, &v("none"), &None, Some(&wrote)),
            (None, false)
        );
    }

    #[test]
    fn a_change_in_rex_settings_outside_t3_code_becomes_yours() {
        let wrote = v("medium");
        assert_eq!(
            decide(false, &v("strong"), &v("medium"), Some(&wrote)),
            (v("strong"), true)
        );
        // In T3 Code, t3rex's off wins.
        assert_eq!(
            decide(true, &v("strong"), &v("medium"), Some(&wrote)),
            (v("none"), false)
        );
    }

    #[test]
    fn reads_the_shown_session_from_an_exported_plist() {
        let plist = "<dict>\n\t<key>Workspace.lastFocusedSession</key>\n\t<data>\n\teyJzZXNzaW9uSUQiOiJzZXNzaW9uOjAxYTE4ZjZlLWU2ZmItN2QwZi04OTQwLTVkYzMx\n\tNWNlOGU4MSJ9\n\t</data>\n</dict>";
        assert_eq!(
            shown_in(plist).as_deref(),
            Some("session:01a18f6e-e6fb-7d0f-8940-5dc315ce8e81")
        );
        assert_eq!(shown_in("<dict></dict>"), None);
    }
}
