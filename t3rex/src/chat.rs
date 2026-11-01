//! A thread's pane: the conversation, and Reply under it (`compose`). The conversation is
//! plain terminal output, so Rex's scrollback, find, selection and links work on it. Finished
//! blocks print once and stay. The blocks of a running turn form a live tail at the bottom
//! that redraws in place until they finish. It prints in a scroll region over Reply's rows, so
//! what scrolls off its top goes into the scrollback and Reply stays put at the bottom, on a
//! phone as on a Mac.
//!
//! Clicks reach the pane only while Reply has something to click, an approval's or a
//! question's buttons or the slash menu: while they're on, the wheel and drags would reach the
//! pane too, not the scrollback and selection. The first turn of the wheel turns them off
//! until the next key.
//!
//! The pane also tells Rex what the thread is doing with OSC 7501, from the same status word
//! T3's sidebar shows, names itself with OSC 2, and reports the thread's folder with OSC 7, so
//! a split opened beside it starts there.

use std::io::Write;
use std::time::Duration;

use anyhow::Result;
use base64::Engine;
use crossterm::event::{Event, EventStream, KeyEventKind, MouseEventKind};
use futures_util::StreamExt;
use serde_json::Value;
use t3client::env::WatchEvent;
use t3client::projection::{ShellState, ThreadState};
use t3client::transcript::{self, Block, BlockKind};

use std::collections::HashMap;

use tokio::sync::mpsc;

use crate::compose::Composer;
use crate::media::{self, Embed, Media};
use crate::paste::{self, Got};
use crate::render::{self, visible_width};
use crate::t3;
use crate::width::Follow;

pub const RESET: &str = "\x1b[0m";
pub const BOLD: &str = "\x1b[1m";
pub const DIM: &str = "\x1b[2m";
pub const ITALIC: &str = "\x1b[3m";
pub const RED: &str = "\x1b[31m";
pub const GREEN: &str = "\x1b[32m";
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
    /// The rows the conversation scrolls in, above Reply's.
    region: usize,
    /// The cursor's row, 1-based: the row after the last one printed.
    row: usize,
    /// The last frame cleared the screen, Reply's rows too.
    cleared: bool,
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
    /// What the pane says while the thread's machine is out of reach.
    offline: Option<String>,
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
    let ms = t3client::inbox::parse_iso(iso)?;
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
            // Its pictures, where this Mac keeps them; elsewhere, their names.
            if block.body.trim().is_empty() && !block.images.is_empty() {
                lines.push(format!("{BLUE}{BOLD}›{RESET}"));
            }
            for image in &block.images {
                let path = ctx
                    .media
                    .and_then(|m| Some((m, m.attachments.as_ref()?.join(&image.file))));
                match path.and_then(|(m, path)| picture_lines(m, &path, width.min(40))) {
                    Some(picture) => lines.extend(picture.into_iter().map(|l| format!("  {l}"))),
                    None => lines.push(format!(
                        "  {DIM}▣ {}{RESET}",
                        fit(&image.name, width.saturating_sub(4))
                    )),
                }
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

/// What a pane says while its machine is out of reach: that it is, or that it needs pairing.
pub fn offline_line(machine: &str, reason: &str, retry_in: Duration) -> String {
    if let Some(at) = reason.find("Run `t3rex pair") {
        return reason[at..].trim_end_matches('.').to_string();
    }
    let secs = retry_in.as_secs().max(1);
    format!("{machine} is offline · retrying in {secs}s")
}

impl Pane {
    /// Before the thread has loaded: the offline line alone, at the top.
    fn waiting(&mut self) {
        if let Some(offline) = &self.offline {
            let (left, width) = self.column();
            out(&format!(
                "\x1b[H\x1b[2J\r\n{}{DIM}○ {}{RESET}\r\n",
                " ".repeat(left),
                fit(offline, width)
            ));
            self.row = 3;
            self.cleared = true;
        }
    }

    /// The conversation's rows become `region`, for Reply growing or shrinking under it.
    /// Growing pushes the conversation up into the scrollback as far as it needs; when that
    /// would take rows of the live tail, which the next frame rewrites, it prints again. So
    /// does shrinking under a conversation that filled its rows, which would leave a gap.
    fn set_region(&mut self, region: usize) -> String {
        if region == self.region {
            return String::new();
        }
        let old = std::mem::replace(&mut self.region, region);
        let margins = format!("\x1b[1;{region}r");
        if region > old && self.header && self.row >= old {
            self.reflow = true;
            return margins;
        }
        // Reply's old rows, the conversation's now.
        if region > old {
            let clear: String = (old + 1..=region)
                .map(|r| format!("\x1b[{r};1H\x1b[2K"))
                .collect();
            return clear + &margins;
        }
        if !self.header {
            return margins;
        }
        let k = self.row.saturating_sub(region);
        if k == 0 {
            return margins;
        }
        if self.live_rows > 0 && self.row - self.live_rows <= k {
            self.reflow = true;
            return margins;
        }
        self.row -= k;
        format!("\x1b[{old};1H{}{margins}", "\n".repeat(k))
    }

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
            region: rows as usize,
            row: 1,
            cleared: false,
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
            offline: None,
        }
    }

    /// The column the conversation draws in: its left edge and its width.
    fn column(&self) -> (usize, usize) {
        self.chat_width.width.column(self.cols)
    }

    /// The thread's blocks: what's new and finished for good, then the live tail.
    fn draw(&mut self, state: &ThreadState, shell_thread: Option<&Value>) -> String {
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
        let mut text = format!("\x1b[{};1H", self.row);
        let reflow = std::mem::take(&mut self.reflow);
        self.cleared = false;
        // The first frame too, which frees pictures an earlier process left in this terminal.
        if !self.header || !matches || self.committed.len() > blocks.len() || reflow {
            text.push_str(self.media.free_all());
            text.push_str(&format!("\x1b[H\x1b[2J\x1b[3J\x1b[1;{}r", self.region));
            self.row = 1;
            self.cleared = true;
            self.committed.clear();
            self.live_rows = 0;
            self.spilled = 0;
            self.header = false;
        }
        // Back to the top of the live tail, and clear it, down to Reply.
        if self.live_rows > 0 {
            self.row -= self.live_rows.min(self.row - 1);
            for r in self.row..=self.region {
                text.push_str(&format!("\x1b[{r};1H\x1b[2K"));
            }
            text.push_str(&format!("\x1b[{};1H", self.row));
        }
        let printed_from = text.len();
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
        if let Some(offline) = &self.offline {
            live.push(String::new());
            live.push(format!("{}{DIM}○ {offline}{RESET}", " ".repeat(left)));
        } else if run_active {
            live.push(String::new());
            let since = state
                .activity_started_at()
                .and_then(t3client::inbox::parse_iso)
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
        let room = self.region.saturating_sub(1).max(1);
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
        let lines = text[printed_from..].matches("\r\n").count();
        self.row = (self.row + lines).min(self.region);
        text
    }

    fn footer(&self, state: &ThreadState) -> Option<String> {
        let run = state.latest_run()?;
        let done = run["completedAt"].as_str()?;
        let started = run["startedAt"].as_str().or(run["requestedAt"].as_str())?;
        let took = t3client::inbox::parse_iso(done)? - t3client::inbox::parse_iso(started)?;
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
        let word = t3client::inbox::status_label(thread, t3::now_ms());
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

/// Clicks on, for Reply's buttons and menu, or off, for the scrollback and selection.
fn clicks(on: bool) -> &'static str {
    if on {
        "\x1b[?1000h\x1b[?1006h"
    } else {
        "\x1b[?1006l\x1b[?1000l"
    }
}

pub async fn run(thread: String, passive: bool) -> Result<()> {
    let (client, thread) = t3::thread(&thread)?;
    let thread_id = thread.thread;
    let mut thread_events = client.watch_thread(&thread_id, None);
    let mut shell_events = client.watch_shell(None);
    let scheme = crate::scheme::listen();
    crossterm::terminal::enable_raw_mode()?;
    // Clear what the login printed, and ask for focus reports and bracketed paste. Paste
    // events too, where the filter can take the clipboard's replies.
    let clipboard = crate::scheme::clipboard();
    out("\x1b[H\x1b[2J\x1b[3J\x1b[?1004h\x1b[?2004h");
    if clipboard.is_some() {
        out(paste::ON);
    }
    if let Ok(cwd) = std::env::current_dir() {
        out(&format!(
            "\x1b]7;file://localhost{}\x1b\\",
            percent(&cwd.to_string_lossy())
        ));
    }
    let keys = crate::scheme::keys();
    let result = event_loop(
        passive,
        &client,
        &thread_id,
        &mut thread_events,
        &mut shell_events,
        scheme,
        clipboard,
    )
    .await;
    drop(keys);
    out(&format!(
        "{}{}\x1b[r\x1b[?2004l\x1b[?1004l\x1b[?25h",
        paste::OFF,
        clicks(false)
    ));
    let _ = crossterm::terminal::disable_raw_mode();
    result
}

async fn event_loop(
    passive: bool,
    client: &std::sync::Arc<t3client::env::Env>,
    thread_id: &str,
    thread_events: &mut tokio::sync::mpsc::UnboundedReceiver<WatchEvent>,
    shell_events: &mut tokio::sync::mpsc::UnboundedReceiver<WatchEvent>,
    mut scheme: tokio::sync::watch::Receiver<Option<bool>>,
    mut clipboard: Option<mpsc::UnboundedReceiver<String>>,
) -> Result<()> {
    let (snap_tx, mut snap_rx) = mpsc::unbounded_channel();
    // Reply asks for the machine's config: the provider's driver for each instance, so the pane
    // can take its provider's name, and the address T3 serves its web app at, for the
    // thread's link.
    let (mut composer, mut done) = Composer::new(client.clone(), thread_id.to_string());
    let link = crate::link::thread(None, client.id(), thread_id);
    let mut pane = Pane::new(Media::new(client.state_dir().as_deref()), link, snap_tx);
    let mut shell = ShellState::default();
    let mut input = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(120));
    let mut visited_for = String::new();
    let mut dirty = false;
    // Reply draws again only when something it shows may have changed.
    let mut reply = true;
    let mut focused = true;
    let mut renamed = false;
    // Whether clicks are on, and whether the wheel turned them off until the next key.
    let (mut clicking, mut wheeled) = (false, false);
    let mut asking = String::new();
    loop {
        tokio::select! {
            event = thread_events.recv() => match event {
                Some(WatchEvent::Item(item)) => {
                    if item["kind"] == "snapshot" {
                        composer.state = ThreadState::from_snapshot(&item);
                    } else if let Some(s) = composer.state.as_mut() {
                        s.apply(&item);
                    }
                    composer.thread_changed();
                    pane.offline = None;
                    composer.offline = None;
                    dirty = true;
                    reply = true;
                }
                Some(WatchEvent::Reconnecting { reason, retry_in }) => {
                    let said = offline_line(client.name(), &reason, retry_in);
                    composer.offline = Some(said.clone());
                    if pane.offline.as_deref() != Some(said.as_str()) {
                        pane.offline = Some(said);
                        dirty = true;
                        if composer.state.is_none() {
                            pane.waiting();
                            reply = true;
                        }
                    }
                }
                Some(WatchEvent::Failed(message)) => anyhow::bail!("T3 refused the thread: {message}"),
                None => anyhow::bail!("the thread's subscription ended"),
            },
            event = shell_events.recv() => if let Some(WatchEvent::Item(item)) = event {
                shell.apply(&item);
                let thread = t3client::inbox::find_thread(&shell, thread_id);
                pane.set_status(thread, composer.state.as_ref());
            },
            event = done.recv() => if let Some(event) = event {
                if let crate::compose::Done::Config(found) = &event {
                    pane.link = crate::link::thread(Some(found), client.id(), thread_id);
                    pane.reflow = true;
                    dirty = true;
                }
                composer.done(event);
                reply = true;
            },
            event = input.next() => {
                reply = true;
                match event {
                    Some(Ok(Event::Resize(cols, rows))) => {
                        if (cols as usize, rows as usize) != (pane.cols, pane.rows) {
                            // Each terminal does its own thing with margins and rows on a
                            // resize, so the thread prints again from the top.
                            pane.reflow = true;
                            pane.cols = cols as usize;
                            pane.rows = rows as usize;
                            dirty = true;
                        }
                    }
                    Some(Ok(Event::FocusGained)) => focused = true,
                    Some(Ok(Event::FocusLost)) => focused = false,
                    Some(Ok(Event::Key(key))) if key.kind != KeyEventKind::Release => {
                        wheeled = false;
                        composer.key(key);
                    }
                    Some(Ok(Event::Mouse(m))) => {
                        if !composer.click(&m)
                            && matches!(m.kind, MouseEventKind::ScrollUp | MouseEventKind::ScrollDown)
                        {
                            wheeled = true;
                        }
                    }
                    Some(Ok(Event::Paste(text))) => composer.pasted(Got::Text(text)),
                    Some(Err(e)) => return Err(e.into()),
                    None => return Ok(()),
                    _ => reply = false,
                }
            },
            Some(packet) = async {
                match clipboard.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => if let Some(got) = composer.clipboard.feed(&packet) {
                composer.pasted(got);
                reply = true;
            },
            // A screen that turns light or dark: snapshots and logos again in its scheme.
            Ok(()) = scheme.changed() => {
                let dark = crate::scheme::dark();
                if dark != pane.media.dark {
                    pane.media.set_dark(dark);
                    pane.reflow = true;
                    dirty = true;
                }
                composer.reload_logos();
            }
            Some((attachment, png)) = snap_rx.recv() => {
                pane.media.snapshots.insert(attachment, png);
                pane.reflow = true;
                dirty = true;
            }
            _ = tokio::time::sleep_until(composer.note_until.unwrap_or_else(tokio::time::Instant::now)), if composer.note_until.is_some() => {
                composer.clear_note();
                reply = true;
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
        if let Some(width) = composer.width_set.take() {
            pane.chat_width.width = width;
            pane.reflow = true;
            dirty = true;
        }
        // A thread that moved to another provider: once nothing is typed, start again under
        // that provider's name, so Rex's sidebar shows its glyph. The thread prints again from
        // the top.
        if !renamed
            && composer.idle()
            && let Some(s) = composer.state.as_ref()
        {
            let instance = s.thread()["modelSelection"]["instanceId"]
                .as_str()
                .unwrap_or_default();
            let driver = crate::icons::driver_of(composer.config.as_ref(), instance);
            let want = crate::icons::program_name(driver).unwrap_or("t3rex");
            if !instance.is_empty()
                && composer.config.is_some()
                && crate::icons::started_as() != want
            {
                renamed = true;
                out(&format!("{}{}\x1b[r", paste::OFF, clicks(false)));
                let _ = crossterm::terminal::disable_raw_mode();
                crate::icons::become_program(driver);
            }
        }
        // Reply's rows at the bottom, and the conversation's above them.
        composer.cols = pane.cols;
        composer.rows = pane.rows;
        composer.column = pane.column();
        let height = composer.height().min(pane.rows.saturating_sub(1)).max(1);
        let mut text = pane.set_region(pane.rows.saturating_sub(height).max(1));
        dirty |= pane.reflow;
        if dirty && let Some(s) = composer.state.as_ref() {
            let thread = t3client::inbox::find_thread(&shell, thread_id);
            let project = thread
                .and_then(|t| t["projectId"].as_str())
                .and_then(|p| shell.projects.iter().find(|x| x["id"] == p))
                .and_then(|p| p["title"].as_str());
            pane.set_title(s, project);
            pane.set_status(thread, Some(s));
            text.push_str(&pane.draw(s, thread));
            dirty = false;
        }
        // A frame that cleared the screen needs Reply again; any frame, the cursor back in the
        // input.
        if reply || pane.cleared || !text.is_empty() && composer.parked.is_empty() {
            text.push_str(&composer.draw(pane.region + 1, pane.cleared));
            reply = false;
        } else if !text.is_empty() {
            text.push_str(&composer.parked);
        }
        // A new question or approval turns clicks back on.
        let ask = crate::ask::pending(
            composer
                .state
                .as_ref()
                .map_or(&ThreadState::default(), |s| s),
        )
        .map(|(r, _)| r["id"].as_str().unwrap_or_default().to_string())
        .unwrap_or_default();
        if ask != asking {
            asking = ask;
            wheeled = false;
        }
        let want = composer.wants_clicks() && !wheeled;
        if want != clicking {
            clicking = want;
            text.push_str(clicks(want));
        }
        if !text.is_empty() {
            out(&format!("\x1b[?2026h\x1b[?25l{text}\x1b[?2026l"));
        }
        // Looking at a finished thread is what clears Done in T3, as opening it in the app does.
        if focused
            && !passive
            && let Some(thread) = t3client::inbox::find_thread(&shell, thread_id)
        {
            let word = t3client::inbox::status_label(thread, t3::now_ms());
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
pub async fn dump(thread: String) -> Result<()> {
    let (client, thread) = t3::thread(&thread)?;
    let state = client.thread(&thread.thread, true).await?;
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
            images: Vec::new(),
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

    fn pane(region: usize, row: usize, live_rows: usize) -> Pane {
        let (tx, _) = mpsc::unbounded_channel();
        let mut pane = Pane::new(Media::new(None), String::new(), tx);
        (pane.region, pane.row, pane.live_rows, pane.header) = (region, row, live_rows, true);
        pane
    }

    #[test]
    fn reply_growing_pushes_the_conversation_up() {
        // Full: the cursor sits on the region's last row. Two more rows for Reply scroll the
        // conversation up two, from the old bottom, before the margins move.
        let mut p = pane(20, 20, 2);
        assert_eq!(p.set_region(18), "\x1b[20;1H\n\n\x1b[1;18r");
        assert_eq!(p.row, 18);
        assert!(!p.reflow);
        // A short conversation has room: only the margins move.
        let mut p = pane(20, 5, 2);
        assert_eq!(p.set_region(18), "\x1b[1;18r");
        assert_eq!(p.row, 5);
    }

    #[test]
    fn reply_growing_into_the_live_tail_prints_again() {
        let mut p = pane(20, 20, 19);
        p.set_region(17);
        assert!(p.reflow);
    }

    #[test]
    fn reply_shrinking_under_a_full_conversation_prints_again() {
        let mut p = pane(18, 18, 2);
        p.set_region(20);
        assert!(p.reflow);
        // Under a short one, Reply's old rows are just cleared.
        let mut p = pane(18, 5, 2);
        assert_eq!(
            p.set_region(20),
            "\x1b[19;1H\x1b[2K\x1b[20;1H\x1b[2K\x1b[1;20r"
        );
        assert!(!p.reflow);
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
