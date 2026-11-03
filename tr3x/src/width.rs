//! How wide the conversation reads. T3 Code keeps its timeline and composer in one centred
//! lane, `--chat-content-max-width`: 46rem by default ("comfortable"), 72rem ("wide"), or the
//! whole window ("full"). tr3x does the same in columns, from the same setting, so a thread
//! reads at the width you chose in T3.
//!
//! The setting comes from `~/.config/tr3x/settings.json` (`"chatWidth"`), which `/width` and
//! `tr3x width` write, and otherwise from T3's own client settings.

use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result};
use serde_json::{Map, Value};

/// Columns kept clear on each side, however narrow the pane.
pub const PAD: usize = 2;
/// The narrowest column worth drawing in.
const MIN: usize = 20;
/// Comfortable: 46rem of T3's 15–16px sans is about 92–100 characters a line. Monospace
/// reads denser, so 96 sits in the middle and is the classic wide-terminal measure.
const COMFORTABLE: usize = 96;
/// Wide: 72rem, the same 72/46 of comfortable.
const WIDE: usize = 150;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChatWidth {
    Comfortable,
    Wide,
    Full,
    /// A column count of your own, from tr3x's settings.
    Columns(usize),
}

impl ChatWidth {
    pub fn parse(text: &str) -> Option<ChatWidth> {
        match text.trim().to_lowercase().as_str() {
            "comfortable" => Some(ChatWidth::Comfortable),
            "wide" => Some(ChatWidth::Wide),
            "full" => Some(ChatWidth::Full),
            n => n.parse().ok().filter(|n| *n >= MIN).map(ChatWidth::Columns),
        }
    }

    fn from_json(value: &Value) -> Option<ChatWidth> {
        match value {
            Value::String(s) => ChatWidth::parse(s),
            Value::Number(n) => ChatWidth::parse(&n.to_string()),
            _ => None,
        }
    }

    fn to_json(self) -> Value {
        match self {
            ChatWidth::Columns(n) => Value::from(n),
            other => Value::from(other.name()),
        }
    }

    pub fn name(self) -> String {
        match self {
            ChatWidth::Comfortable => "comfortable".into(),
            ChatWidth::Wide => "wide".into(),
            ChatWidth::Full => "full".into(),
            ChatWidth::Columns(n) => format!("{n} columns"),
        }
    }

    fn cap(self) -> Option<usize> {
        match self {
            ChatWidth::Comfortable => Some(COMFORTABLE),
            ChatWidth::Wide => Some(WIDE),
            ChatWidth::Full => None,
            ChatWidth::Columns(n) => Some(n),
        }
    }

    /// The column to draw in, in a pane `cols` wide: its left edge (0-based) and its width,
    /// centred, with at least `PAD` clear on each side.
    pub fn column(self, cols: usize) -> (usize, usize) {
        let room = cols.saturating_sub(PAD * 2).max(MIN);
        let width = self.cap().map_or(room, |cap| cap.min(room));
        let left = (cols.saturating_sub(width) / 2).max(PAD);
        (left, width)
    }
}

fn config_home() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir);
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join(".config")
}

/// tr3x's own settings.
fn ours() -> PathBuf {
    config_home().join("tr3x/settings.json")
}

/// T3 Code's client settings, where its Chat width lives.
fn t3s() -> PathBuf {
    t3client::descriptor::t3_home().join("userdata/client-settings.json")
}

/// Whether a message to a running thread steers it, as T3 Code's Follow-up behavior says.
/// T3's default is to steer.
pub fn follow_up_steers() -> bool {
    read(&t3s())
        .and_then(|s| s.get("followUpBehavior")?.as_str().map(|b| b != "queue"))
        .unwrap_or(true)
}

/// tr3x's own settings, `~/.config/tr3x/settings.json`.
pub fn settings() -> Option<Map<String, Value>> {
    read(&ours())
}

fn read(path: &PathBuf) -> Option<Map<String, Value>> {
    let text = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str(&text).ok()? {
        Value::Object(map) => Some(map),
        _ => None,
    }
}

/// The setting, and where it came from.
pub fn current() -> (ChatWidth, &'static str) {
    let pick = |path: PathBuf| read(&path)?.get("chatWidth").and_then(ChatWidth::from_json);
    if let Some(w) = pick(ours()) {
        return (w, "tr3x");
    }
    if let Some(w) = pick(t3s()) {
        return (w, "T3 Code");
    }
    (ChatWidth::Comfortable, "default")
}

/// Sets tr3x's own width, or with `None` goes back to following T3's.
pub fn set(width: Option<ChatWidth>) -> Result<()> {
    set_setting("chatWidth", width.map(|w| w.to_json()))
}

/// Sets one of tr3x's own settings, or with `None` takes it out.
pub fn set_setting(key: &str, value: Option<Value>) -> Result<()> {
    let path = ours();
    let mut map = read(&path).unwrap_or_default();
    match value {
        Some(v) => map.insert(key.into(), v),
        None => map.remove(key),
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(&Value::Object(map))? + "\n";
    std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))
}

/// What `/width` and `tr3x width` take: a width, or `t3` to follow T3 Code again.
pub fn apply(arg: &str) -> Result<String> {
    let arg = arg.trim();
    if arg.is_empty() {
        let (w, from) = current();
        return Ok(format!("Chat width: {} ({from})", w.name()));
    }
    if matches!(arg, "t3" | "auto" | "default") {
        set(None)?;
        return Ok(format!("Chat width: {} (T3 Code)", current().0.name()));
    }
    let width = ChatWidth::parse(arg).with_context(|| {
        format!("Can't read {arg:?} as a width: comfortable, wide, full, t3 or a column count")
    })?;
    set(Some(width))?;
    Ok(format!("Chat width: {}", width.name()))
}

/// Whether finished turns show their work (`"details": true`), which `/details` and
/// `tr3x details` set. Off by default: each turn folds to `Worked for …`, as in T3 Code.
pub fn details() -> bool {
    read(&ours())
        .and_then(|s| s.get("details")?.as_bool())
        .unwrap_or(false)
}

/// What `/details` and `tr3x details` take: `on`, `off`, or nothing to switch it.
pub fn apply_details(arg: &str) -> Result<String> {
    let on = match arg.trim().to_lowercase().as_str() {
        "" => !details(),
        "on" | "show" | "true" => true,
        "off" | "hide" | "false" => false,
        other => anyhow::bail!("Can't read {other:?}: on or off"),
    };
    set_setting("details", on.then_some(Value::Bool(true)))?;
    Ok(if on {
        "Details shown"
    } else {
        "Details folded"
    }
    .into())
}

/// The setting as a pane follows it: read again when either file changes, checked at most
/// every couple of seconds.
pub struct Follow {
    pub width: ChatWidth,
    pub details: bool,
    stamps: [Option<SystemTime>; 2],
    checked: Instant,
}

fn stamps() -> [Option<SystemTime>; 2] {
    [ours(), t3s()].map(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
}

impl Follow {
    pub fn new() -> Follow {
        Follow {
            width: current().0,
            details: details(),
            stamps: stamps(),
            checked: Instant::now(),
        }
    }

    /// Whether the width or the details setting changed since the last look.
    pub fn changed(&mut self) -> bool {
        if self.checked.elapsed() < Duration::from_secs(2) {
            return false;
        }
        self.checked = Instant::now();
        let now = stamps();
        if now == self.stamps {
            return false;
        }
        self.stamps = now;
        let (width, details) = (current().0, details());
        let changed = width != self.width || details != self.details;
        self.width = width;
        self.details = details;
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comfortable_centres_a_readable_column() {
        assert_eq!(ChatWidth::Comfortable.column(200), (52, 96));
        // An odd remainder goes to the right.
        assert_eq!(ChatWidth::Comfortable.column(201), (52, 96));
        assert_eq!(ChatWidth::Wide.column(200), (25, 150));
    }

    #[test]
    fn narrow_panes_keep_their_padding() {
        assert_eq!(ChatWidth::Comfortable.column(80), (PAD, 76));
        assert_eq!(ChatWidth::Comfortable.column(100), (PAD, 96));
        assert_eq!(ChatWidth::Wide.column(120), (PAD, 116));
        assert_eq!(ChatWidth::Comfortable.column(10), (PAD, MIN));
    }

    #[test]
    fn full_is_the_whole_pane() {
        assert_eq!(ChatWidth::Full.column(240), (PAD, 236));
    }

    #[test]
    fn settings_parse() {
        assert_eq!(ChatWidth::parse("Wide"), Some(ChatWidth::Wide));
        assert_eq!(ChatWidth::parse("110"), Some(ChatWidth::Columns(110)));
        assert_eq!(ChatWidth::parse("8"), None);
        assert_eq!(ChatWidth::parse("roomy"), None);
        let n = ChatWidth::from_json(&ChatWidth::Columns(88).to_json());
        assert_eq!(n, Some(ChatWidth::Columns(88)));
    }
}
