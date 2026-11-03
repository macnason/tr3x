//! Where Reply goes: under the conversation in the same pane (`inline`, the default), or in a
//! pane of its own below it (`pane`). One pane suits a phone, where Rex shows each pane of a
//! split as a window of its own; a pane of its own keeps Reply apart on a desktop. The `reply`
//! setting in `~/.config/tr3x/settings.json`, which `/reply` and `tr3x reply` write. Sync
//! reshapes open windows to match within a few seconds.

use std::future::Future;
use std::time::Duration;

use anyhow::{Result, bail};
use serde_json::{Value, json};
use tokio::time::Instant;

/// Whether Reply has a pane of its own.
pub fn own_pane() -> bool {
    crate::width::settings()
        .and_then(|s| s.get("reply").cloned())
        .is_some_and(|v| v == "pane")
}

/// What `/reply` and `tr3x reply` take: `inline` or `pane`, or nothing to say which it is.
pub fn apply(arg: &str) -> Result<String> {
    let name = |pane: bool| if pane { "its own pane" } else { "inline" };
    let pane = match arg.trim() {
        "" => return Ok(format!("Reply: {}", name(own_pane()))),
        "inline" | "one" | "joined" => false,
        "pane" | "split" | "own" => true,
        other => bail!("Can't read {other:?}: inline or pane"),
    };
    crate::width::set_setting("reply", pane.then(|| json!("pane")))?;
    Ok(format!("Reply: {}", name(pane)))
}

/// The share of a window the conversation has over Reply in a pane of its own, as sync and
/// `new` split it.
pub const SPLIT: f64 = 0.84;

/// Whether a window, as `session inspect` prints it, has the shape the setting asks for.
pub fn shaped(window: &Value, pane: bool) -> bool {
    let has_reply = window["layers"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|l| l["kind"] != "floating")
        .flat_map(|l| l["blocks"].as_array().into_iter().flatten())
        .any(|b| b["label"] == "Reply");
    has_reply == pane
}

/// Reply alone takes at most this share of its window.
const MOST: f64 = 0.5;
/// How long what Reply wants has to hold before the split moves, so typing through a wrap
/// doesn't move it twice; and how long to wait after a move that didn't take.
const WAIT: Duration = Duration::from_millis(250);
const RETRY: Duration = Duration::from_secs(3);

/// Reply alone, fitting its pane to its rows by moving the split between it and the
/// conversation (`session.resize_pane`). The split is the session's, so every device looking
/// at it sees the same.
pub struct Fit {
    session: String,
    block: String,
    /// The rows last asked for, 0 when that didn't take, and the ones Reply wants now.
    asked: usize,
    wants: usize,
    /// When to ask next.
    at: Option<Instant>,
    busy: bool,
}

impl Fit {
    /// For the pane this runs in, inside Rex.
    pub fn new() -> Option<Fit> {
        Some(Fit {
            session: std::env::var("REX_SESSION").ok()?,
            block: std::env::var("REX_BLOCK").ok()?,
            asked: 0,
            wants: 0,
            at: None,
            busy: false,
        })
    }

    pub fn want(&mut self, rows: usize) {
        if rows != self.wants {
            self.wants = rows;
            self.at = Some(Instant::now() + WAIT);
        }
    }

    /// The move to make now, if one is due. It comes back true when it took.
    pub fn due(&mut self) -> Option<impl Future<Output = bool> + use<>> {
        if self.busy || self.at.is_none_or(|at| Instant::now() < at) {
            return None;
        }
        self.at = None;
        if self.wants == self.asked {
            return None;
        }
        self.asked = self.wants;
        self.busy = true;
        let (session, block, want) = (self.session.clone(), self.block.clone(), self.wants);
        Some(async move { resize(session, block, want).await.is_some() })
    }

    pub fn finished(&mut self, ok: bool) {
        self.busy = false;
        if !ok {
            self.asked = 0;
            self.at = Some(Instant::now() + RETRY);
        } else if self.wants != self.asked {
            self.at = Some(Instant::now() + WAIT);
        }
    }
}

/// The pane before `block` in the vertical split it's the after side of, and that split's
/// ratio, in a layout as `session inspect` prints it.
fn split_above(layout: &Value, block: &str) -> Option<(String, f64)> {
    let split = &layout["split"];
    if split.is_null() {
        return None;
    }
    if split["direction"] == "vertical"
        && split["after"]["block_id"] == block
        && let Some(before) = split["before"]["block_id"].as_str()
    {
        return Some((before.to_string(), split["ratio"].as_f64()?));
    }
    split_above(&split["before"], block).or_else(|| split_above(&split["after"], block))
}

/// Moves the split so Reply has `want` rows, within `MOST` of the window. None when it
/// couldn't.
async fn resize(session: String, block: String, want: usize) -> Option<()> {
    let rex = crate::rex::Rex::find();
    // The conversation: the pane before Reply in its window's split, and the split as it is.
    let view = rex.inspect(&session).await.ok()?;
    let (chat, ratio) = view["windows"]
        .as_array()?
        .iter()
        .flat_map(|w| w["layers"].as_array().into_iter().flatten())
        .find_map(|l| split_above(&l["layout"], &block))?;
    let size = |b: String| {
        let (rex, session) = (rex.clone(), session.clone());
        async move {
            let out = tokio::process::Command::new(&rex.bin)
                .args(crate::rex::NO_AUTOSTART)
                .args([
                    "do",
                    "-s",
                    &session,
                    "-b",
                    &b,
                    "com.superlogical.terminal.size",
                ])
                .output()
                .await
                .ok()?;
            serde_json::from_slice::<Value>(&out.stdout).ok()
        }
    };
    let (chat_size, reply_size) = (size(chat.clone()).await?, size(block.clone()).await?);
    // Rows mean something only once a device sizes both panes; a pane no one owns reads 24.
    if chat_size["owner"].is_null() || chat_size["owner"] != reply_size["owner"] {
        return None;
    }
    let (chat_rows, reply_rows) = (chat_size["rows"].as_f64()?, reply_size["rows"].as_f64()?);
    // Each pane loses a few rows to its header. Work that out from the split as it is:
    // reply = (1 - ratio) * all - header, with all = chat + reply + 2 * header.
    let share = 1.0 - ratio;
    let header = ((share * (chat_rows + reply_rows) - reply_rows) / (1.0 - 2.0 * share)).max(0.0);
    let all = chat_rows + reply_rows + 2.0 * header;
    let want = (want as f64).min(((chat_rows + reply_rows) * MOST).floor());
    let next = (1.0 - (want + header + 0.5) / all).clamp(1.0 - MOST - 0.1, 0.97);
    if (next - ratio).abs() < 0.001 {
        return Some(());
    }
    let args = json!({
        "session_id": session, "direction": "vertical", "before_block_id": chat,
        "after_block_id": block, "ratio": next,
    });
    rex.call("session.resize_pane", &args.to_string())
        .await
        .ok()
        .map(drop)
}
