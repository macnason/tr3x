//! The conversation pane: the thread as plain terminal output, so Rex's scrollback, find,
//! selection and links work on it. Finished blocks print once and stay. The blocks of a
//! running turn form a live tail at the bottom that redraws in place until they finish.
//!
//! The pane also tells Rex what the thread is doing with OSC 7501, from the same status word
//! T3's sidebar shows, names itself with OSC 2, and reports the thread's folder with OSC 7, so
//! a split opened beside it starts there.

use std::io::Write;
use std::time::Duration;

use anyhow::Result;
use base64::Engine;
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures_util::StreamExt;
use serde_json::Value;
use t3term::client::WatchEvent;
use t3term::projection::{ShellState, ThreadState};
use t3term::transcript::{self, Block, BlockKind};

use std::collections::HashMap;

use tokio::sync::mpsc;

use crate::media::{self, Embed, Media};
use crate::render::{self, visible_width};
use crate::t3;
use crate::width::Follow;

pub const RESET: &str = "\x1b[0m";
pub const BOLD: &str = "\x1b[1m";
pub const DIM: &str = "\x1b[2m";
pub const ITALIC: &str = "\x1b[3m";
pub const RED: &str = "\x1b[31m";
pub const YELLOW: &str = "\x1b[33m";
pub const BLUE: &str = "\x1b[34m";
pub const MAGENTA: &str = "\x1b[35m";

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// The terminal's size and what this pane has written to it.
struct Pane {
    cols: usize,
    rows: usize,
    /// The id and version of each block printed for good, in order.
    committed: Vec<(String, String)>,
    /// Rows the live tail takes at the bottom, which the next frame rewrites.
    live_rows: usize,
    /// Lines from the top of the live tail that have printed for good, because the tail
    /// outgrew the screen. Later frames and the commit skip them.
    spilled: usize,
    /// The last status sent with OSC 7501, so it goes out only when it changes.
    status: String,
    title: String,
    header: bool,
    /// Set by a resize or a new chat width: the whole thread prints again at the new width.
    reflow: bool,
    /// The chat width setting, which places the column everything draws in.
    chat_width: Follow,
    /// Whether the last frame drew a spinner or a clock, which the tick moves on.
    animating: bool,
    /// The newest message of yours drawn, so a new one can bring the view to the bottom.
    last_user: String,
    frame: usize,
    offset_minutes: i64,
    media: Media,
    /// `t3code://threads/<environment>/<thread>`, which opens the thread in T3 Code.
    link: String,
    snapshots: mpsc::UnboundedSender<(String, std::path::PathBuf)>,
}

fn out(text: &str) {
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(text.as_bytes());
    let _ = stdout.flush();
}

/// The local time zone's offset, read once from `date`, so timestamps need no time zone crate.
pub fn local_offset_minutes() -> i64 {
    std::process::Command::new("date")
        .arg("+%z")
        .output()
        .ok()
        .and_then(|o| {
            let text = String::from_utf8_lossy(&o.stdout).trim().to_string();
            let sign = if text.starts_with('-') { -1 } else { 1 };
            let digits = text.trim_start_matches(['+', '-']);
            let (h, m) = (
                digits.get(..2)?.parse::<i64>().ok()?,
                digits.get(2..4)?.parse::<i64>().ok()?,
            );
            Some(sign * (h * 60 + m))
        })
        .unwrap_or(0)
}

/// `12:37 PM` for a T3 timestamp, in local time.
fn clock(iso: &str, offset_minutes: i64) -> Option<String> {
    let ms = t3term::tui::parse_iso(iso)?;
    let minutes = (ms / 60_000 + offset_minutes).rem_euclid(24 * 60);
    let (h, m) = (minutes / 60, minutes % 60);
    let (h12, half) = match h {
        0 => (12, "AM"),
        1..=11 => (h, "AM"),
        12 => (12, "PM"),
        _ => (h - 12, "PM"),
    };
    Some(format!("{h12}:{m:02} {half}"))
}

/// `1m 12s`, `45s` or `2h 3m`.
pub fn duration(ms: i64) -> String {
    let s = (ms / 1000).max(0);
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m {}s", s / 60, s % 60),
        _ => format!("{}h {}m", s / 3600, s % 3600 / 60),
    }
}

/// Cuts `text` to `width` columns, with an ellipsis when it was longer. Plain text only.
pub fn fit(text: &str, width: usize) -> String {
    if unicode_width::UnicodeWidthStr::width(text) <= width {
        return text.to_string();
    }
    let mut kept = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if used + w + 1 > width {
            break;
        }
        kept.push(c);
        used += w;
    }
    kept + "…"
}

/// `path` with the home folder written as `~`.
pub fn short_home(path: &str) -> String {
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && path.starts_with(&home) => {
            format!("~{}", &path[home.len()..])
        }
        _ => path.to_string(),
    }
}

/// A path for a `file://` URL: anything but unreserved characters and `/` percent-encoded.
pub fn percent(path: &str) -> String {
    let mut out = String::new();
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || b"/-._~".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

pub fn one_line(text: &str) -> String {
    transcript::plain(text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// `lines` moved right by `left` columns, into the conversation's column.
fn pad(lines: Vec<String>, left: usize) -> Vec<String> {
    lines
        .into_iter()
        .map(|l| {
            if l.is_empty() {
                l
            } else {
                format!("{}{l}", " ".repeat(left))
            }
        })
        .collect()
}

/// The glyph a tool row starts with.
fn tool_glyph(block: &Block) -> &'static str {
    match block.item_type.as_str() {
        "command_execution" => "$",
        "file_change" => "✎",
        "web_search" => "⌕",
        "file_read" | "read" => "≡",
        t if t.contains("search") || t.contains("grep") || t.contains("glob") => "⌕",
        t if t.contains("mcp") || t.contains("dynamic") => "◇",
        t if t.contains("subagent") || t.contains("agent") => "◎",
        _ => "·",
    }
}

/// A path as short as it can be and still say where: `~` for home, and only the last two
/// parts of a long one.
fn short_path(word: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let word = match word.strip_prefix(&home) {
        Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => {
            format!("~{rest}")
        }
        _ => word.to_string(),
    };
    if word.len() <= 48 || !word.contains('/') {
        return word;
    }
    let parts: Vec<&str> = word.trim_end_matches('/').rsplitn(3, '/').collect();
    match parts.as_slice() {
        [file, dir, _] => format!("…/{dir}/{file}"),
        _ => word,
    }
}

fn shorten_paths(text: &str) -> String {
    text.split(' ')
        .map(|w| {
            if w.starts_with('/') || w.starts_with("~/") {
                short_path(w)
            } else {
                w.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A tool row's text: what it did, in T3's words where it has them.
/// `mcp__claude_ai_Claude_Docs__update` as `Claude Docs · update`, `ToolSearch` as itself.
fn tool_name(raw: &str) -> String {
    let Some(rest) = raw.strip_prefix("mcp__") else {
        return raw.to_string();
    };
    let (server, tool) = rest.rsplit_once("__").unwrap_or((rest, ""));
    let server = server
        .strip_prefix("claude_ai_")
        .unwrap_or(server)
        .replace(['_', '-'], " ");
    if tool.is_empty() {
        server
    } else {
        format!("{server} · {tool}")
    }
}

fn tool_text(block: &Block, tool: Option<&str>) -> String {
    let first = |t: &str| one_line(t.lines().next().unwrap_or(""));
    // T3's workspace setup reads best as what it ended with.
    if block.detail == "Preparing workspace" && !block.title.is_empty() && !is_running(block) {
        return block.title.clone();
    }
    let text = match block.item_type.as_str() {
        "command_execution" => {
            let mut cmd = first(if block.detail.is_empty() {
                &block.header
            } else {
                &block.detail
            });
            // `cd somewhere && real command` says more as `real command`.
            if let Some(rest) = cmd.strip_prefix("cd ")
                && let Some(at) = rest.find(" && ").or_else(|| rest.find("; "))
            {
                let skip = if rest[at..].starts_with(" && ") { 4 } else { 2 };
                cmd = rest[at + skip..].to_string();
            }
            cmd
        }
        "file_change" => {
            let what = first(&block.header);
            let (verb, rest) = what.split_once(' ').unwrap_or(("edit", &what));
            let verb = match verb {
                "edit" => "Edit",
                "add" | "create" => "Create",
                "delete" => "Delete",
                other => other,
            };
            format!("{verb} {rest}")
        }
        "web_search" => format!("Searched {}", first(&block.detail)),
        _ if !block.title.is_empty() && block.title != "Thinking" => first(&block.title),
        _ if let Some(tool) = tool => tool_name(tool),
        _ => first(&block.header),
    };
    shorten_paths(&text)
}

fn is_running(block: &Block) -> bool {
    block.streaming
        || matches!(
            block.status.as_str(),
            "running" | "inProgress" | "in_progress" | "pending" | "started"
        )
}

fn failed(block: &Block) -> bool {
    matches!(block.status.as_str(), "failed" | "error" | "declined")
        || block.exit_code.is_some_and(|c| c != 0)
}

/// What a block needs besides itself to draw.
struct Ctx<'a> {
    width: usize,
    frame: usize,
    offset: i64,
    embeds: &'a HashMap<String, Embed>,
    /// Pictures draw only in output that prints for good, never in the live tail.
    media: Option<&'a Media>,
    /// Opens the thread in T3, where it's clicked.
    link: &'a str,
}

/// A picture's lines: its kitty sequence on the first, then a blank row for each row it covers.
fn picture_lines(media: &Media, path: &std::path::Path, cols: usize) -> Option<Vec<String>> {
    let (seq, rows) = media.picture(path, cols)?;
    let mut lines = vec![seq];
    lines.extend(std::iter::repeat_n(String::new(), rows.saturating_sub(1)));
    // A row of its own after it, so the text below never touches it.
    lines.push(String::new());
    Some(lines)
}

/// Markdown with its standalone local images drawn as pictures.
fn markdown_with_pictures(text: &str, ctx: &Ctx) -> Vec<String> {
    let Some(media) = ctx.media else {
        return render::markdown(text, ctx.width).lines;
    };
    let mut lines = Vec::new();
    let mut chunk = String::new();
    let mut in_fence = false;
    for line in text.split_inclusive('\n') {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
        }
        if !in_fence && let Some((_, path)) = media::local_image(line) {
            let pictures = picture_lines(media, &path, ctx.width.min(96));
            if let Some(pictures) = pictures {
                if !chunk.trim().is_empty() {
                    lines.extend(render::markdown(&chunk, ctx.width).lines);
                    lines.push(String::new());
                }
                chunk.clear();
                lines.extend(pictures);
                continue;
            }
        }
        chunk.push_str(line);
    }
    if !chunk.trim().is_empty() {
        lines.extend(render::markdown(&chunk, ctx.width).lines);
    }
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines
}

/// What one block looks like, at `ctx.width` columns before the left padding.
fn block_lines(block: &Block, ctx: &Ctx) -> Vec<String> {
    let (width, frame, offset) = (ctx.width, ctx.frame, ctx.offset);
    let mut lines = Vec::new();
    if let Some(embed) = ctx.embeds.get(&block.item_id) {
        lines.push(String::new());
        let open = crate::link::osc8(ctx.link, "Open in T3 Code ↗");
        lines.push(format!(
            "{BOLD}◇ {}{RESET}  {DIM}{open}{RESET}",
            fit(&one_line(&embed.title), width.saturating_sub(22))
        ));
        if let Some(media) = ctx.media
            && let Some(png) = media.snapshots.get(&embed.attachment)
            && let Some(pictures) = picture_lines(media, png, width.min(100))
        {
            lines.extend(pictures);
        }
        return lines;
    }
    match block.kind {
        BlockKind::User => {
            lines.push(String::new());
            let text = render::markdown(&block.body, width.saturating_sub(2));
            for (i, line) in text.lines.into_iter().enumerate() {
                let bar = if i == 0 {
                    format!("{BLUE}{BOLD}›{RESET} ")
                } else {
                    "  ".to_string()
                };
                lines.push(format!("{bar}{line}"));
            }
            if let Some(at) = clock(&block.updated_at, offset) {
                lines.push(format!("  {DIM}{at}{RESET}"));
            }
        }
        BlockKind::Assistant => {
            lines.push(String::new());
            lines.extend(markdown_with_pictures(&block.body, ctx));
            if block.streaming {
                lines.push(format!("{DIM}{}{RESET}", SPINNER[frame % SPINNER.len()]));
            }
        }
        // Finished reasoning stays out of the way; a turn that is thinking says so.
        BlockKind::Reasoning => {
            if is_running(block) {
                lines.push(format!("{DIM}{ITALIC}∴ Thinking{RESET}"));
            }
        }
        BlockKind::Tool => {
            let tool = Some(block.tool_name.as_str()).filter(|t| !t.is_empty());
            // A question shows as its own block, so its tool call needs no row.
            if tool == Some("AskUserQuestion") {
                return lines;
            }
            let mark = if is_running(block) {
                format!(" {BLUE}{}{RESET}", SPINNER[frame % SPINNER.len()])
            } else if failed(block) {
                format!(" {RED}✗{RESET}")
            } else {
                String::new()
            };
            let room = width.saturating_sub(6);
            lines.push(format!(
                "  {DIM}{} {}{RESET}{mark}",
                tool_glyph(block),
                fit(&tool_text(block, tool), room)
            ));
            if failed(block) && !block.body.trim().is_empty() {
                for line in block
                    .body
                    .lines()
                    .rev()
                    .take(3)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                {
                    lines.push(format!(
                        "    {DIM}{RED}{}{RESET}",
                        fit(&transcript::plain(line), width.saturating_sub(6))
                    ));
                }
            }
        }
        BlockKind::Request => {
            let open = matches!(block.status.as_str(), "pending" | "waiting");
            let color = if open { YELLOW } else { DIM };
            let head = if block.item_type.contains("input")
                || block.header.to_lowercase().contains("question")
            {
                "Question"
            } else {
                "Approval"
            };
            lines.push(String::new());
            let state = match block.status.as_str() {
                _ if open => String::new(),
                "accepted" | "approved" | "completed" => format!(" {DIM}· answered{RESET}"),
                other => format!(" {DIM}· {other}{RESET}"),
            };
            lines.push(format!("{color}{BOLD}◆ {head}{RESET}{state}"));
            let text = if block.body.trim().is_empty() {
                &block.header
            } else {
                &block.body
            };
            for line in render::markdown(text, width.saturating_sub(2)).lines {
                lines.push(format!("  {line}"));
            }
        }
        BlockKind::Plan => {
            lines.push(String::new());
            lines.push(format!("{MAGENTA}{BOLD}Plan{RESET}"));
            lines.extend(render::markdown(&block.body, width).lines);
        }
        BlockKind::Notice => {
            let text = one_line(if block.body.is_empty() {
                &block.header
            } else {
                &block.body
            });
            if !text.is_empty() {
                lines.push(String::new());
                lines.push(format!("{DIM}{}{RESET}", fit(&text, width)));
            }
        }
        BlockKind::Error => {
            lines.push(String::new());
            let text = if block.body.is_empty() {
                &block.header
            } else {
                &block.body
            };
            for line in render::markdown(text, width.saturating_sub(2)).lines {
                lines.push(format!("{RED}│{RESET} {line}"));
            }
        }
    }
    lines
}

/// Tool calls shown in a row before the rest of the run folds into one "more" line.
const TOOLS_SHOWN: usize = 3;

/// For each block, whether it shows. A run of tool calls (reasoning between them doesn't
/// break it) shows its first few calls and folds the rest into one "more" line, when that
/// hides at least two. Running and failed calls always show.
fn visible_tools(blocks: &[Block]) -> Vec<bool> {
    let mut shown = vec![true; blocks.len()];
    let mut i = 0;
    while i < blocks.len() {
        if blocks[i].kind != BlockKind::Tool {
            i += 1;
            continue;
        }
        let start = i;
        while i < blocks.len() && matches!(blocks[i].kind, BlockKind::Tool | BlockKind::Reasoning) {
            i += 1;
        }
        let tools: Vec<usize> = (start..i)
            .filter(|&j| blocks[j].kind == BlockKind::Tool)
            .collect();
        if tools.len() >= TOOLS_SHOWN + 2 {
            for &j in &tools[TOOLS_SHOWN..] {
                shown[j] = is_running(&blocks[j]) || failed(&blocks[j]);
            }
        }
    }
    shown
}

/// Block `i`'s lines, after one "more" line when it ends a run with folded tool calls.
fn lines_at(blocks: &[Block], shown: &[bool], i: usize, ctx: &Ctx) -> Vec<String> {
    let mut lines = Vec::new();
    let ends_run = !matches!(blocks[i].kind, BlockKind::Tool | BlockKind::Reasoning);
    if ends_run {
        let hidden = (0..i)
            .rev()
            .take_while(|&j| matches!(blocks[j].kind, BlockKind::Tool | BlockKind::Reasoning))
            .filter(|&j| blocks[j].kind == BlockKind::Tool && !shown[j])
            .count();
        if hidden > 0 {
            lines.push(format!("  {DIM}⋯ {hidden} more tool calls{RESET}"));
        }
    }
    if shown[i] {
        lines.extend(block_lines(&blocks[i], ctx));
    }
    lines
}

/// Whether a block will change no more, so it can print for good.
fn settled(block: &Block, last: bool, run_active: bool) -> bool {
    let waiting =
        block.kind == BlockKind::Request && matches!(block.status.as_str(), "pending" | "waiting");
    !(is_running(block) || (last && run_active) || waiting)
}

/// Text cut to at most `max` bytes of UTF-8, on a character boundary, with no controls.
fn within(text: &str, max: usize) -> String {
    let mut out = String::new();
    for c in text.chars().filter(|c| !c.is_control()) {
        if out.len() + c.len_utf8() > max {
            break;
        }
        out.push(c);
    }
    out
}

/// OSC 7501 for T3's status word, as Rex's tabs and sidebar show it. `msg` says what the
/// thread is waiting on you for, when it is, else the status word; `title` names the thread.
/// Both are cut to the protocol's limits: 2048 and 192 bytes.
fn status_sequence(word: Option<&str>, ask: Option<&str>, title: &str) -> String {
    let (state, extra) = match word {
        Some("Approval") => ("blocked", ":kind=permission"),
        Some("Input") => ("blocked", ":kind=question"),
        Some("Working" | "Goal" | "Waiting") => ("working", ""),
        Some("Failed" | "Limited") => ("error", ""),
        Some("Done" | "Woke") => ("done", ""),
        _ => ("idle", ""),
    };
    let b64 = |text: &str| base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    let mut seq = format!("\x1b]7501;state={state}{extra}:app=t3code");
    let title = within(title, 192);
    if !title.is_empty() {
        seq.push_str(&format!(":title={}", b64(&title)));
    }
    let msg = match (state, ask) {
        ("blocked", Some(ask)) => within(ask, 2048),
        _ => word.map(str::to_string).unwrap_or_default(),
    };
    if !msg.is_empty() {
        seq.push_str(&format!(":msg={}", b64(&msg)));
    }
    seq.push_str("\x1b\\");
    seq
}

impl Pane {
    fn new(
        media: Media,
        link: String,
        snapshots: mpsc::UnboundedSender<(String, std::path::PathBuf)>,
    ) -> Pane {
        let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
        Pane {
            cols: cols as usize,
            rows: rows as usize,
            committed: Vec::new(),
            live_rows: 0,
            spilled: 0,
            status: String::new(),
            title: String::new(),
            header: false,
            reflow: false,
            chat_width: Follow::new(),
            animating: false,
            last_user: String::new(),
            frame: 0,
            offset_minutes: local_offset_minutes(),
            media,
            link,
            snapshots,
        }
    }

    /// The column the conversation draws in: its left edge and its width.
    fn column(&self) -> (usize, usize) {
        self.chat_width.width.column(self.cols)
    }

    /// Prints the thread's blocks: what's new and finished for good, then the live tail.
    fn draw(&mut self, state: &ThreadState, shell_thread: Option<&Value>) {
        let blocks = transcript::blocks(state);
        self.animating = state.active_run().is_some() || blocks.iter().any(is_running);
        let run_active = state.active_run().is_some();
        // A history that no longer starts with what was printed, after a reconnect, prints
        // again from the top.
        let matches = self
            .committed
            .iter()
            .zip(&blocks)
            .all(|((id, _), b)| id == &b.item_id);
        // A message you just sent brings the view back to the bottom. No sequence scrolls a
        // terminal's view, but clearing the scrollback and printing again does.
        let newest = blocks
            .iter()
            .rev()
            .find(|b| b.kind == BlockKind::User)
            .map(|b| b.item_id.clone())
            .unwrap_or_default();
        if newest != self.last_user {
            // Not on the first frame, which prints from the top anyway.
            self.reflow |= self.header;
            self.last_user = newest;
        }
        let mut text = String::from("\x1b[?2026h");
        let reflow = std::mem::take(&mut self.reflow);
        // The first frame too, which frees pictures an earlier process left in this terminal.
        if !self.header || !matches || self.committed.len() > blocks.len() || reflow {
            text.push_str(self.media.free_all());
            text.push_str("\x1b[H\x1b[2J\x1b[3J");
            self.committed.clear();
            self.live_rows = 0;
            self.spilled = 0;
            self.header = false;
        }
        // Back to the top of the live tail, and clear it.
        if self.live_rows > 0 {
            text.push_str(&format!("\x1b[{}F\x1b[J", self.live_rows));
        }
        if !self.header {
            text.push_str(&self.header_lines(state, shell_thread));
            self.header = true;
        }
        let (left, width) = self.column();
        let shown = visible_tools(&blocks);
        let embeds = media::embeds(state);
        if self.media.can_snapshot() {
            for embed in embeds.values() {
                self.media.ask(embed, self.snapshots.clone());
            }
        }
        let fixed = Ctx {
            width,
            frame: 0,
            offset: self.offset_minutes,
            embeds: &embeds,
            media: Some(&self.media),
            link: &self.link,
        };
        let moving = Ctx {
            frame: self.frame,
            media: None,
            ..fixed
        };
        let mut index = self.committed.len();
        while index < blocks.len() && settled(&blocks[index], index + 1 == blocks.len(), run_active)
        {
            let block = &blocks[index];
            let lines = pad(lines_at(&blocks, &shown, index, &fixed), left);
            // The top of this block may already be on screen, printed while it was live.
            let skip = self.spilled.min(lines.len());
            self.spilled -= skip;
            for line in &lines[skip..] {
                text.push_str(line);
                text.push_str("\r\n");
            }
            self.committed
                .push((block.item_id.clone(), block.updated_at.clone()));
            index += 1;
        }
        if index == blocks.len() {
            self.spilled = 0;
        }
        let mut live: Vec<String> = Vec::new();
        for i in index..blocks.len() {
            live.extend(pad(lines_at(&blocks, &shown, i, &moving), left));
        }
        // Lines that already printed for good stay where they are.
        let skip = self.spilled.min(live.len());
        live.drain(..skip);
        if run_active {
            live.push(String::new());
            let since = state
                .active_run()
                .and_then(|r| r["startedAt"].as_str().or(r["requestedAt"].as_str()))
                .and_then(t3term::tui::parse_iso)
                .map(|at| duration(t3::now_ms() - at))
                .unwrap_or_default();
            live.push(format!(
                "{}{BLUE}{}{RESET} {DIM}Working {since}{RESET}",
                " ".repeat(left),
                SPINNER[self.frame % SPINNER.len()]
            ));
        } else if let Some(footer) = self.footer(state) {
            // The finished turn's footer redraws until the next turn starts, so it can stay live.
            live.push(footer);
        }
        // A tail taller than the screen can't be rewritten once its top has scrolled away, so
        // anything above the last screenful prints for good now.
        let room = self.rows.saturating_sub(1).max(1);
        if live.len() > room {
            let cut = live.len() - room;
            for line in live.drain(..cut) {
                text.push_str(&line);
                text.push_str("\r\n");
            }
            self.spilled += cut;
        }
        for line in &live {
            text.push_str(line);
            text.push_str("\r\n");
        }
        self.live_rows = live.len();
        text.push_str("\x1b[?2026l");
        out(&text);
    }

    fn footer(&self, state: &ThreadState) -> Option<String> {
        let run = state.latest_run()?;
        let done = run["completedAt"].as_str()?;
        let started = run["startedAt"].as_str().or(run["requestedAt"].as_str())?;
        let took = t3term::tui::parse_iso(done)? - t3term::tui::parse_iso(started)?;
        let at = clock(done, self.offset_minutes)?;
        let mark = match run["status"].as_str() {
            Some("failed") => format!("{RED}✗{RESET} {DIM}Failed"),
            Some("interrupted" | "cancelled") => format!("{DIM}■ Stopped"),
            _ => format!("{DIM}{at} · {}", duration(took)),
        };
        Some(format!("{}{mark}{RESET}", " ".repeat(self.column().0)))
    }

    fn header_lines(&self, state: &ThreadState, shell_thread: Option<&Value>) -> String {
        let thread = state.thread();
        let (left, width) = self.column();
        let title = thread["title"].as_str().unwrap_or("Untitled");
        let mut meta: Vec<String> = Vec::new();
        if let Some(branch) = thread["branch"].as_str() {
            meta.push(branch.to_string());
        }
        let prs = shell_thread
            .and_then(|t| t["pullRequests"].as_array())
            .or(thread["pullRequests"].as_array());
        for pr in prs.into_iter().flatten().take(3) {
            let number = pr["number"].as_u64().map(|n| format!("PR #{n}"));
            let url = pr["url"].as_str();
            if let (Some(n), Some(url)) = (number, url) {
                meta.push(format!("\x1b]8;;{url}\x1b\\{n}\x1b]8;;\x1b\\"));
            }
        }
        let mut text = String::new();
        let p = " ".repeat(left);
        // The title opens the thread in T3 on whichever device it's clicked on.
        let title = crate::link::osc8(&self.link, &fit(&one_line(title), width));
        text.push_str(&format!("\r\n{p}{BOLD}{title}{RESET}\r\n"));
        if !meta.is_empty() {
            text.push_str(&format!("{p}{DIM}{}{RESET}\r\n", meta.join(" · ")));
        }
        text
    }

    fn set_status(&mut self, thread: Option<&Value>, state: Option<&ThreadState>) {
        let Some(thread) = thread else { return };
        let word = t3term::tui::status_label(thread, t3::now_ms());
        let title = one_line(thread["title"].as_str().unwrap_or(""));
        let ask = state.and_then(crate::ask::line);
        let seq = status_sequence(word, ask.as_deref(), &title);
        if seq != self.status {
            out(&seq);
            self.status = seq;
        }
    }

    fn set_title(&mut self, state: &ThreadState, project: Option<&str>) {
        let thread = state.thread();
        let mut parts = vec!["T3".to_string()];
        if let Some(p) = project {
            parts.push(p.to_string());
        }
        if let Some(b) = thread["branch"].as_str() {
            parts.push(b.to_string());
        }
        let title = parts.join(" · ");
        if title != self.title {
            out(&format!(
                "\x1b]2;{}\x07",
                title.replace(|c: char| c.is_control(), "")
            ));
            self.title = title;
        }
    }
}

/// The sibling pane in this window: the composer.
async fn composer_block() -> Option<String> {
    let me = std::env::var("REX_BLOCK").ok()?;
    let session = std::env::var("REX_SESSION").ok()?;
    let view = crate::rex::Rex::find().inspect(&session).await.ok()?;
    for window in view["windows"].as_array()? {
        let blocks: Vec<&str> = window["layers"]
            .as_array()?
            .iter()
            .flat_map(|l| l["blocks"].as_array().into_iter().flatten())
            .filter_map(|b| b["block_id"].as_str())
            .collect();
        if blocks.contains(&me.as_str()) {
            return blocks.into_iter().find(|b| *b != me).map(str::to_string);
        }
    }
    None
}

/// Typing in the conversation goes to the composer: focus moves there with the keys typed.
/// Keys go through one task in order, so a burst of typing arrives as typed.
fn spawn_handoff() -> mpsc::UnboundedSender<String> {
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        let bin = crate::rex::Rex::find().bin;
        let session = std::env::var("REX_SESSION").unwrap_or_default();
        let mut block: Option<String> = None;
        while let Some(first) = rx.recv().await {
            let mut text = first;
            while let Ok(more) = rx.try_recv() {
                text.push_str(&more);
            }
            if block.is_none() {
                block = composer_block().await;
            }
            let Some(block) = block.as_deref() else {
                continue;
            };
            let _ = tokio::process::Command::new(&bin)
                .args(["focus", "-s", &session, block])
                .output()
                .await;
            if !text.is_empty() {
                let _ = tokio::process::Command::new(&bin)
                    .args(["send", "-b", block, &text])
                    .output()
                    .await;
            }
        }
    });
    tx
}

pub async fn run(thread_id: String) -> Result<()> {
    let client = t3::connect(true).await?;
    let mut thread_events = client.watch_thread(&thread_id, None);
    let mut shell_events = client.watch_shell(None);
    let scheme = crate::scheme::listen();
    crossterm::terminal::enable_raw_mode()?;
    // Clear what the login printed, hide the cursor, and ask for focus reports.
    out("\x1b[H\x1b[2J\x1b[3J\x1b[?25l\x1b[?1004h");
    if let Ok(cwd) = std::env::current_dir() {
        out(&format!(
            "\x1b]7;file://localhost{}\x1b\\",
            percent(&cwd.to_string_lossy())
        ));
    }
    let result = event_loop(
        &client,
        &thread_id,
        &mut thread_events,
        &mut shell_events,
        scheme,
    )
    .await;
    out("\x1b[?1004l\x1b[?25h");
    let _ = crossterm::terminal::disable_raw_mode();
    result
}

async fn event_loop(
    client: &std::sync::Arc<t3term::client::Client>,
    thread_id: &str,
    thread_events: &mut tokio::sync::mpsc::UnboundedReceiver<WatchEvent>,
    shell_events: &mut tokio::sync::mpsc::UnboundedReceiver<WatchEvent>,
    mut scheme: tokio::sync::watch::Receiver<Option<bool>>,
) -> Result<()> {
    let (snap_tx, mut snap_rx) = mpsc::unbounded_channel();
    // The provider's driver for each instance, so the pane can take its provider's name, and
    // the address T3 serves its web app at, for the thread's link.
    let config = client.server_config().await.ok();
    let link = crate::link::thread(config.as_ref(), &client.runtime.environment_id, thread_id);
    let handoff = spawn_handoff();
    let mut pane = Pane::new(
        Media::new(client.runtime.state_dir.as_deref()),
        link,
        snap_tx,
    );
    let mut state: Option<ThreadState> = None;
    let mut shell = ShellState::default();
    let mut input = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(120));
    let mut visited_for = String::new();
    let mut dirty = false;
    let mut focused = true;
    let mut renamed = false;
    loop {
        tokio::select! {
            event = thread_events.recv() => match event {
                Some(WatchEvent::Item(item)) => {
                    if item["kind"] == "snapshot" {
                        state = ThreadState::from_snapshot(&item);
                    } else if let Some(s) = state.as_mut() {
                        s.apply(&item);
                    }
                    dirty = true;
                }
                Some(WatchEvent::Reconnecting { .. }) => {}
                Some(WatchEvent::Failed(message)) => anyhow::bail!("T3 refused the thread: {message}"),
                None => anyhow::bail!("the thread's subscription ended"),
            },
            event = shell_events.recv() => if let Some(WatchEvent::Item(item)) = event {
                shell.apply(&item);
                let thread = t3term::tui::find_thread_in(&shell, thread_id);
                pane.set_status(thread, state.as_ref());
            },
            event = input.next() => match event {
                Some(Ok(Event::Resize(cols, rows))) => {
                    if (cols as usize, rows as usize) != (pane.cols, pane.rows) {
                        pane.reflow = cols as usize != pane.cols;
                        pane.cols = cols as usize;
                        pane.rows = rows as usize;
                        dirty = true;
                    }
                }
                Some(Ok(Event::FocusGained)) => focused = true,
                Some(Ok(Event::FocusLost)) => focused = false,
                Some(Ok(Event::Key(key))) if key.kind != KeyEventKind::Release => {
                    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                        continue;
                    }
                    let typed = match key.code {
                        KeyCode::Char(c) if !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER) => c.to_string(),
                        _ => String::new(),
                    };
                    let _ = handoff.send(typed);
                }
                Some(Err(_)) | None => {}
                _ => {}
            },
            // A screen that turns light or dark: snapshots again in its scheme.
            Ok(()) = scheme.changed() => {
                let dark = crate::scheme::dark();
                if dark != pane.media.dark {
                    pane.media.set_dark(dark);
                    pane.reflow = true;
                    dirty = true;
                }
            }
            Some((attachment, png)) = snap_rx.recv() => {
                pane.media.snapshots.insert(attachment, png);
                pane.reflow = true;
                dirty = true;
            }
            _ = tick.tick() => {
                if pane.animating {
                    pane.frame += 1;
                    dirty = true;
                }
                if pane.chat_width.changed() {
                    pane.reflow = true;
                    dirty = true;
                }
            }
        }
        // A thread that moved to another provider: start again under that provider's name,
        // so Rex's sidebar shows its glyph. The thread prints again from the top.
        if !renamed && let Some(s) = state.as_ref() {
            let instance = s.thread()["modelSelection"]["instanceId"]
                .as_str()
                .unwrap_or_default();
            let driver = crate::icons::driver_of(config.as_ref(), instance);
            let want = crate::icons::program_name(driver).unwrap_or("t3rex");
            if !instance.is_empty() && crate::icons::started_as() != want {
                renamed = true;
                out("\x1b[?1004l\x1b[?25h");
                let _ = crossterm::terminal::disable_raw_mode();
                crate::icons::become_program(driver);
            }
        }
        if dirty && let Some(s) = state.as_ref() {
            let thread = t3term::tui::find_thread_in(&shell, thread_id);
            let project = thread
                .and_then(|t| t["projectId"].as_str())
                .and_then(|p| shell.projects.iter().find(|x| x["id"] == p))
                .and_then(|p| p["title"].as_str());
            pane.set_title(s, project);
            pane.set_status(thread, Some(s));
            pane.draw(s, thread);
            dirty = false;
        }
        // Looking at a finished thread is what clears Done in T3, as opening it in the app does.
        if focused && let Some(thread) = t3term::tui::find_thread_in(&shell, thread_id) {
            let word = t3term::tui::status_label(thread, t3::now_ms());
            let key = thread["updatedAt"].as_str().unwrap_or_default().to_string();
            if matches!(word, Some("Done" | "Woke")) && visited_for != key {
                visited_for = key;
                let (client, id) = (client.clone(), thread_id.to_string());
                tokio::spawn(async move { t3::visit(&client, &id).await });
            }
        }
    }
}

/// Prints a thread's blocks, for working on this pane.
pub async fn dump(thread_id: String) -> Result<()> {
    let client = t3::connect(false).await?;
    let state = client.thread(&thread_id, true).await?;
    for b in transcript::blocks(&state) {
        println!(
            "{:?} type={} status={} streaming={} exit={:?} header={:?} detail={:?} title={:?} body={:?}",
            b.kind,
            b.item_type,
            b.status,
            b.streaming,
            b.exit_code,
            fit(&b.header, 60),
            fit(&b.detail, 60),
            fit(&b.title, 40),
            fit(&one_line(&b.body), 80)
        );
    }
    let _ = visible_width("");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(item_type: &str, header: &str, detail: &str, title: &str) -> Block {
        Block {
            item_id: "i".into(),
            kind: BlockKind::Tool,
            header: header.into(),
            body: String::new(),
            streaming: false,
            status: "completed".into(),
            item_type: item_type.into(),
            detail: detail.into(),
            title: title.into(),
            output_omitted: false,
            updated_at: String::new(),
            exit_code: Some(0),
            run_id: String::new(),
            tool_name: String::new(),
            request_id: String::new(),
            change: None,
        }
    }

    #[test]
    fn commands_drop_their_cd() {
        let b = tool(
            "command_execution",
            "$ cd ~/x && pnpm test",
            "cd ~/x && pnpm test",
            "",
        );
        assert_eq!(tool_text(&b, None), "pnpm test");
    }

    #[test]
    fn workspace_setup_reads_as_its_result() {
        let b = tool(
            "command_execution",
            "$ Preparing workspace",
            "Preparing workspace",
            "Workspace ready",
        );
        assert_eq!(tool_text(&b, None), "Workspace ready");
    }

    #[test]
    fn dynamic_tools_are_named() {
        let b = tool("dynamic_tool", "dynamic_tool", "", "");
        assert_eq!(
            tool_text(&b, Some("mcp__claude_ai_Claude_Docs__update")),
            "Claude Docs · update"
        );
        assert_eq!(tool_name("ToolSearch"), "ToolSearch");
    }

    #[test]
    fn long_paths_keep_their_end() {
        assert_eq!(
            short_path(
                "/private/tmp/claude-501/some-very-long-scratch-directory-name/scratchpad/v2.png"
            ),
            "…/scratchpad/v2.png"
        );
        assert_eq!(short_path("/usr/bin/env"), "/usr/bin/env");
    }

    #[test]
    fn long_runs_of_tools_fold() {
        let blocks: Vec<Block> = (0..6)
            .map(|_| tool("command_execution", "$ ls", "ls", ""))
            .collect();
        let shown = visible_tools(&blocks);
        assert_eq!(shown, vec![true, true, true, false, false, false]);
        // Folding one call saves nothing, so a run of four shows all four.
        assert!(visible_tools(&blocks[..4]).iter().all(|s| *s));
    }

    #[test]
    fn status_maps_to_rex_states() {
        assert!(
            status_sequence(Some("Approval"), None, "t").contains("state=blocked:kind=permission")
        );
        assert!(status_sequence(Some("Input"), None, "t").contains("state=blocked:kind=question"));
        assert!(status_sequence(Some("Working"), None, "t").contains("state=working"));
        assert!(status_sequence(Some("Done"), None, "t").contains("state=done"));
        assert!(status_sequence(None, None, "t").contains("state=idle"));
    }

    #[test]
    fn status_says_what_is_asked_within_the_limits() {
        let b64 = |t: &str| base64::engine::general_purpose::STANDARD.encode(t);
        let seq = status_sequence(Some("Approval"), Some("Run pnpm test?"), "Fix the build");
        assert_eq!(
            seq,
            format!(
                "\x1b]7501;state=blocked:kind=permission:app=t3code:title={}:msg={}\x1b\\",
                b64("Fix the build"),
                b64("Run pnpm test?")
            )
        );
        // Not blocked: the status word, whatever was asked before.
        assert!(
            status_sequence(Some("Working"), Some("x"), "t")
                .ends_with(&format!(":msg={}\x1b\\", b64("Working")))
        );
        // Over the limits: cut on a character, never mid-way through one.
        let long = "é".repeat(2000);
        let seq = status_sequence(Some("Input"), Some(&long), &long);
        let field = |key: &str| {
            let at = seq.find(&format!(":{key}=")).unwrap() + key.len() + 2;
            let end = seq[at..].find([':', '\x1b']).unwrap() + at;
            base64::engine::general_purpose::STANDARD
                .decode(&seq[at..end])
                .unwrap()
        };
        assert_eq!(field("title").len(), 192);
        assert_eq!(field("msg").len(), 2048);
        assert!(seq.len() < 4096);
    }

    #[test]
    fn clocks_are_local_twelve_hour() {
        assert_eq!(
            clock("2026-10-30T15:29:13.895Z", 11 * 60).as_deref(),
            Some("2:29 AM")
        );
        assert_eq!(
            clock("2026-10-30T01:05:00.000Z", 0).as_deref(),
            Some("1:05 AM")
        );
        assert_eq!(
            clock("2026-10-30T12:00:00.000Z", 0).as_deref(),
            Some("12:00 PM")
        );
    }
}
