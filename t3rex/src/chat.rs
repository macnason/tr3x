//! A thread's pane: the conversation, and Reply under it (`compose`), or with the `reply`
//! setting at `pane`, either of them in a pane of its own (`Role`). The conversation is
//! plain terminal output, so Rex's scrollback, find, selection and links work on it. Finished
//! blocks print once and stay. The blocks of a running turn form a live tail at the bottom
//! that redraws in place until they finish. It prints in the rows over Reply's, which a
//! scroll region holds. When it needs to scroll, it clears Reply's rows and scrolls the whole
//! screen instead, because the terminal moves text, not pictures, when a region scrolls; then
//! Reply draws again at the bottom, on a phone as on a Mac.
//!
//! Clicks reach the pane only while Reply has something to click, an approval's or a
//! question's buttons or the slash menu: while they're on, the wheel and drags would reach the
//! pane too, not the scrollback and selection. The first turn of the wheel turns them off
//! for a moment and a half, or until the next key.
//!
//! The pane also tells Rex what the thread is doing with OSC 7501, from the same status word
//! T3's sidebar shows, names itself with OSC 2, and reports the thread's folder with OSC 7, so
//! a split opened beside it starts there.

use std::time::Duration;

use anyhow::Result;
use base64::Engine;
use crossterm::event::{Event, EventStream, KeyEventKind, MouseEventKind};
use futures_util::StreamExt;
use serde_json::Value;
use t3client::env::WatchEvent;
use t3client::inbox::Limit;
use t3client::projection::{Applied, ShellState, ThreadState};
use t3client::transcript::{self, Block, BlockKind};

use std::collections::HashMap;

use tokio::sync::mpsc;

use crate::compose::Composer;
use crate::media::{self, Embed, Media};
use crate::paste::{self, Got};
use crate::render::{self, visible_width};
use crate::t3;
use crate::term::out;
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
    snapshots: mpsc::UnboundedSender<media::Ready>,
    /// What the pane says while the thread's machine is out of reach.
    offline: Option<String>,
    /// The usage-limit line the last frame drew, which time moves on: `tomorrow 9am` becomes
    /// `9am` at midnight, and `resumes` becomes `resuming` at the reset.
    limit: String,
    files: crate::link::FileLinks,
    display: Option<std::sync::Arc<Display>>,
    live: Vec<String>,
}

struct Display {
    sequence: u64,
    blocks: Vec<Block>,
    embeds: HashMap<String, Embed>,
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

/// A moment in local time, as Find Thread and the conversation word it: `3pm` or `3:30pm`
/// today, `tomorrow 9am`, `Tue 9am` within the week, else `12 Nov`. Before today,
/// `yesterday 3pm`, else the date.
pub fn when(at: i64, now: i64, offset_minutes: i64) -> String {
    let local = |ms: i64| ms + offset_minutes * 60_000;
    let day = 86_400_000;
    let days = local(at).div_euclid(day) - local(now).div_euclid(day);
    let minutes = local(at).rem_euclid(day) / 60_000;
    let (h, m) = (minutes / 60, minutes % 60);
    let hour = match h % 12 {
        0 => 12,
        h => h,
    };
    let half = if h < 12 { "am" } else { "pm" };
    let clock = if m == 0 {
        format!("{hour}{half}")
    } else {
        format!("{hour}:{m:02}{half}")
    };
    // 1970-01-01 was a Thursday.
    let weekday = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"]
        [local(at).div_euclid(day).rem_euclid(7) as usize];
    match days {
        0 => clock,
        1 => format!("tomorrow {clock}"),
        -1 => format!("yesterday {clock}"),
        2..=6 => format!("{weekday} {clock}"),
        _ => {
            let date = t3::iso(local(at));
            let month = [
                "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
            ][date[5..7].parse::<usize>().unwrap_or(1) - 1];
            format!("{} {month}", date[8..10].trim_start_matches('0'))
        }
    }
}

/// What a usage limit leaves the thread waiting for, after its status word: `resets 3pm`, or
/// `resumes 3pm` when T3 will send it on then, `resuming` once that time has passed, and
/// `reset 3pm` after a reset nothing resumed. Empty when the provider didn't say when.
fn limit_wait(limit: &Limit, now: i64, offset_minutes: i64) -> String {
    let Some(at) = limit.resets_at else {
        return String::new();
    };
    let time = when(at, now, offset_minutes);
    match (at > now, limit.resumes) {
        (true, true) => format!("resumes {time}"),
        (true, false) => format!("resets {time}"),
        (false, true) => "resuming".to_string(),
        (false, false) => format!("reset {time}"),
    }
}

/// The conversation's last line for a thread stopped on a usage limit, as T3's notice says it:
/// `Usage limit reached · resets 3pm`, with `· resumes then` when auto-resume is on.
fn limit_line(limit: &Limit, now: i64, offset_minutes: i64) -> String {
    let mut line = "Usage limit reached".to_string();
    match limit.resets_at {
        Some(at) if at > now => {
            line.push_str(&format!(" · resets {}", when(at, now, offset_minutes)));
            if limit.resumes {
                line.push_str(" · resumes then");
            }
        }
        Some(_) => line.push_str(&format!(" · {}", limit_wait(limit, now, offset_minutes))),
        None => {}
    }
    line
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
    files: &'a crate::link::FileLinks,
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
        return render::markdown_with_links(text, ctx.width, ctx.files).lines;
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
                    lines.extend(render::markdown_with_links(&chunk, ctx.width, ctx.files).lines);
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
        lines.extend(render::markdown_with_links(&chunk, ctx.width, ctx.files).lines);
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
            let text = render::markdown_with_links(&block.body, width.saturating_sub(2), ctx.files);
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
            for line in render::markdown_with_links(text, width.saturating_sub(2), ctx.files).lines
            {
                lines.push(format!("  {line}"));
            }
        }
        BlockKind::Plan => {
            lines.push(String::new());
            lines.push(format!("{MAGENTA}{BOLD}Plan{RESET}"));
            lines.extend(render::markdown_with_links(&block.body, width, ctx.files).lines);
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
            for line in render::markdown_with_links(text, width.saturating_sub(2), ctx.files).lines
            {
                lines.push(format!("{RED}│{RESET} {line}"));
            }
        }
    }
    lines
}

/// What T3's work log counts a tool call as, when it folds a run into one line.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Command,
    Edit,
    Read,
    WebSearch,
    CodeSearch,
    Other,
}

/// T3's classifier (`session-logic`): the item type first, then the provider's tool name.
fn action(block: &Block) -> Action {
    match block.item_type.as_str() {
        "command_execution" => return Action::Command,
        "file_change" => return Action::Edit,
        "web_search" => return Action::WebSearch,
        "file_search" => return Action::CodeSearch,
        "image_view" | "file_read" | "read" => return Action::Read,
        _ => {}
    }
    match block.tool_name.to_ascii_lowercase().as_str() {
        "bash" | "terminal" | "shell" => Action::Command,
        "edit" | "write" | "multiedit" | "notebookedit" => Action::Edit,
        "read" | "readfile" => Action::Read,
        "grep" | "glob" | "find" | "rg" | "ls" => Action::CodeSearch,
        "websearch" => Action::WebSearch,
        _ => Action::Other,
    }
}

fn action_label(action: Action, n: usize) -> String {
    let (one, many) = match action {
        Action::Command => ("Ran 1 command", "commands"),
        Action::Edit => ("Changed 1 file", "files"),
        Action::Read => ("Read 1 file", "files"),
        Action::WebSearch => ("Searched the web 1 time", "times"),
        Action::CodeSearch => ("Searched code 1 time", "times"),
        Action::Other => ("Used 1 tool", "tools"),
    };
    if n == 1 {
        return one.to_string();
    }
    let verb = one.rsplitn(3, ' ').last().unwrap_or(one);
    format!("{verb} {n} {many}")
}

/// A run of tool calls as T3 sums it up: its two most telling kinds, in the order they came,
/// and the rest as other actions. "Ran 3 commands and read 1 file".
fn run_summary(run: &[&Block]) -> String {
    let mut groups: Vec<(Action, Vec<&Block>)> = Vec::new();
    for block in run {
        let a = action(block);
        match groups.iter_mut().find(|(g, _)| *g == a) {
            Some((_, list)) => list.push(block),
            None => groups.push((a, vec![block])),
        }
    }
    let priority = |a: Action| match a {
        Action::Command | Action::Edit => 0,
        Action::Other => 2,
        _ => 1,
    };
    let mut top: Vec<usize> = (0..groups.len()).collect();
    top.sort_by_key(|&i| (priority(groups[i].0), i));
    top.truncate(2);
    top.sort();
    let mut parts: Vec<String> = top
        .iter()
        .map(|&i| {
            let (a, list) = &groups[i];
            // Edits count files, not calls.
            let n = if *a == Action::Edit {
                let mut files: Vec<&str> = list.iter().map(|b| b.detail.as_str()).collect();
                let unnamed = files.iter().filter(|f| f.is_empty()).count();
                files.retain(|f| !f.is_empty());
                files.sort();
                files.dedup();
                files.len() + unnamed
            } else {
                list.len()
            };
            action_label(*a, n)
        })
        .collect();
    let rest = run.len() - top.iter().map(|&i| groups[i].1.len()).sum::<usize>();
    if rest > 0 {
        let s = if rest == 1 { "action" } else { "actions" };
        parts.push(format!("Performed {rest} other {s}"));
    }
    for part in parts.iter_mut().skip(1) {
        *part = part[..1].to_lowercase() + &part[1..];
    }
    match parts.len() {
        0..=2 => parts.join(" and "),
        n => format!("{}, and {}", parts[..n - 1].join(", "), parts[n - 1]),
    }
}

/// Whether a block belongs to a run of tool calls. Reasoning between calls doesn't break one.
fn in_run(block: &Block) -> bool {
    matches!(block.kind, BlockKind::Tool | BlockKind::Reasoning)
}

/// The finished calls in a run, as one line. None when nothing in it has finished.
fn run_line(run: &[Block], embeds: &HashMap<String, Embed>, width: usize) -> Option<String> {
    let done: Vec<&Block> = run
        .iter()
        .filter(|b| b.kind == BlockKind::Tool && b.tool_name != "AskUserQuestion")
        .filter(|b| !is_running(b) && !embeds.contains_key(&b.item_id))
        .collect();
    if done.is_empty() {
        return None;
    }
    let failures = done.iter().filter(|b| failed(b)).count();
    let mark = if failures > 0 {
        format!("  {RED}✗ {failures} failed{RESET}")
    } else {
        String::new()
    };
    let room = width.saturating_sub(6 + visible_width(&mark));
    Some(format!(
        "  {DIM}{}{RESET}{mark}",
        fit(&run_summary(&done), room)
    ))
}

/// Where the run of tool calls that ends just before `end` starts.
fn run_start(blocks: &[Block], end: usize) -> usize {
    (0..end)
        .rev()
        .take_while(|&j| in_run(&blocks[j]))
        .last()
        .unwrap_or(end)
}

/// Block `i`'s lines, after the line summing up the run of tool calls it ends. A call
/// draws a row of its own only while it runs, as T3's work log does.
fn lines_at(blocks: &[Block], i: usize, ctx: &Ctx) -> Vec<String> {
    let mut lines = Vec::new();
    let block = &blocks[i];
    if !in_run(block)
        && let Some(line) = run_line(&blocks[run_start(blocks, i)..i], ctx.embeds, ctx.width)
    {
        lines.push(line);
    }
    // An HTML embed is a picture, so it always draws.
    if block.kind != BlockKind::Tool || is_running(block) || ctx.embeds.contains_key(&block.item_id)
    {
        lines.extend(block_lines(block, ctx));
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

/// OSC 7501 for T3's status word, as Rex's tabs and sidebar show it under the window's label,
/// which already names the thread. `msg` is that line: what the thread is waiting on you for,
/// when it is, else the project and the status word, or the project alone when idle. `title`
/// is the project. `shown` is the word the line says, which can differ from `word`, the one
/// the state comes from: a finished run you've already seen still says Done, at rest. Both
/// are cut to the protocol's limits: 2048 and 192 bytes.
fn status_sequence(
    word: Option<&str>,
    shown: Option<&str>,
    ask: Option<&str>,
    project: Option<&str>,
) -> String {
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
    let title = within(project.unwrap_or_default(), 192);
    if !title.is_empty() {
        seq.push_str(&format!(":title={}", b64(&title)));
    }
    let msg = match (state, ask, shown, project) {
        ("blocked", Some(ask), _, _) => within(ask, 2048),
        (_, _, Some(word), Some(project)) => within(&format!("{project} · {word}"), 2048),
        (_, _, Some(word), None) => word.to_string(),
        (_, _, None, project) => within(project.unwrap_or_default(), 2048),
    };
    if !msg.is_empty() {
        seq.push_str(&format!(":msg={}", b64(&msg)));
    }
    seq.push_str("\x1b\\");
    seq
}

/// The name of the project `thread` belongs to.
fn project_name<'a>(shell: &'a ShellState, thread: &Value) -> Option<&'a str> {
    let id = thread["projectId"].as_str()?;
    shell.projects.iter().find(|p| p["id"] == id)?["title"].as_str()
}

/// What a pane says while its machine is out of reach: that it is, or that it needs pairing.
/// This Mac's T3 runs inside T3 Code, so when it's gone the app was quit (or is restarting),
/// and its agents stopped with it. Saying so beats a retry count nobody can act on.
pub fn offline_line(machine: &str, local: bool, reason: &str, retry_in: Duration) -> String {
    if let Some(at) = reason.find("Run `t3rex pair") {
        return reason[at..].trim_end_matches('.').to_string();
    }
    if local {
        return LOCAL_OFFLINE.to_string();
    }
    let secs = retry_in.as_secs().max(1);
    format!("{machine} is offline · retrying in {secs}s")
}

const LOCAL_OFFLINE: &str =
    "T3 Code isn't running · agents stop when it quits · open T3 Code to carry on";

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
        // The whole screen scrolls, not the region: pictures stay put when a region scrolls.
        self.cleared = true;
        format!("{}{margins}", self.scroll_up(k))
    }

    /// Scrolls the whole screen up `k` rows, taking pictures with the text, and clears what
    /// was Reply's. Within margins the terminal moves the text but not its pictures, so the
    /// conversation never scrolls in its region; Reply draws again after.
    fn scroll_up(&self, k: usize) -> String {
        let mut text = format!("\x1b[r\x1b[{};1H{}", self.rows, "\n".repeat(k));
        if self.region < self.rows {
            text.push_str(&format!("\x1b[{};1H\x1b[J", self.region + 1));
        }
        text
    }

    fn new(media: Media, link: String, snapshots: mpsc::UnboundedSender<media::Ready>) -> Pane {
        let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
        let mut media = media;
        media.listen(snapshots.clone());
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
            limit: String::new(),
            files: crate::link::FileLinks::default(),
            display: None,
            live: Vec::new(),
        }
    }

    /// The column the conversation draws in: its left edge and its width.
    fn column(&self) -> (usize, usize) {
        self.chat_width.width.column(self.cols)
    }

    /// The thread's blocks: what's new and finished for good, then the live tail.
    fn draw(&mut self, state: &ThreadState, shell_thread: Option<&Value>) -> String {
        let display = self.display.get_or_insert_with(|| {
            std::sync::Arc::new(Display {
                sequence: state.sequence,
                blocks: transcript::blocks(state),
                embeds: media::embeds(state),
            })
        });
        if display.sequence != state.sequence {
            *display = std::sync::Arc::new(Display {
                sequence: state.sequence,
                blocks: transcript::blocks(state),
                embeds: media::embeds(state),
            });
        }
        let display = display.clone();
        let blocks = &display.blocks;
        self.limit = self.limit_now(shell_thread);
        self.animating = state.active_run().is_some() || blocks.iter().any(is_running);
        let run_active = state.active_run().is_some();
        // A history that no longer starts with what was printed, after a reconnect, prints
        // again from the top.
        let matches = self
            .committed
            .iter()
            .zip(blocks)
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
        // The first frame too, which frees pictures an earlier process left in this terminal.
        let reset = !self.header || !matches || self.committed.len() > blocks.len() || reflow;
        if reset {
            text.push_str(self.media.free_all());
            text.push_str(&format!("\x1b[H\x1b[2J\x1b[3J\x1b[1;{}r", self.region));
            self.row = 1;
            self.cleared = true;
            self.committed.clear();
            self.live_rows = 0;
            self.spilled = 0;
            self.header = false;
        }
        let tail_control_from = text.len();
        let committed_before = self.committed.len();
        // Back to the top of the live tail, and clear it, down to Reply.
        if self.live_rows > 0 {
            self.row -= self.live_rows.min(self.row - 1);
        }
        let mut printed_from = text.len();
        if !self.header {
            text.push_str(&self.header_lines(state, shell_thread));
            self.header = true;
        }
        let (left, width) = self.column();
        let embeds = &display.embeds;
        if self.media.can_snapshot() {
            for embed in embeds.values() {
                self.media.ask(embed, self.snapshots.clone());
            }
        }
        let fixed = Ctx {
            width,
            frame: 0,
            offset: self.offset_minutes,
            embeds,
            media: Some(&self.media),
            link: &self.link,
            files: &self.files,
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
            let lines = pad(lines_at(blocks, index, &fixed), left);
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
        // A run still going sums up what it has done so far, above what it's doing now.
        let open = run_start(blocks, blocks.len());
        let summary = run_line(&blocks[open..], embeds, width).map(|l| pad(vec![l], left));
        let summary_at = open.max(index);
        for i in index..blocks.len() {
            if i == summary_at {
                live.extend(summary.iter().flatten().cloned());
            }
            live.extend(pad(lines_at(blocks, i, &moving), left));
        }
        if summary_at == blocks.len() {
            live.extend(summary.into_iter().flatten());
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
        if !reset
            && committed_before == self.committed.len()
            && self.spilled == 0
            && live.len() == self.live.len()
            && self.row + live.len() <= self.region
        {
            text.truncate(tail_control_from);
            let mut changed = false;
            for (i, line) in live.iter().enumerate() {
                if self.live[i] != *line {
                    changed = true;
                    text.push_str(&format!("\x1b[{};1H\x1b[2K{line}", self.row + i));
                }
            }
            self.live_rows = live.len();
            self.row += self.live_rows;
            self.live = live;
            return if changed { text } else { String::new() };
        }
        if self.live_rows > 0 {
            let mut clear = String::new();
            for r in self.row..=self.region {
                clear.push_str(&format!("\x1b[{r};1H\x1b[2K"));
            }
            clear.push_str(&format!("\x1b[{};1H", self.row));
            printed_from += clear.len();
            text.insert_str(tail_control_from, &clear);
        }
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
        self.live = live;
        let body = text.split_off(printed_from);
        let lines = body.matches("\r\n").count();
        if self.row + lines <= self.region {
            text.push_str(&body);
            self.row += lines;
            return text;
        }
        // Past the region's bottom: printed over the whole screen, with Reply's rows cleared
        // first, so what scrolls takes its pictures along, then scrolled up off Reply's rows.
        // Claude Code prints this way; Codex scrolls a region but never shows pictures.
        text.push_str("\x1b[r");
        if self.region < self.rows {
            text.push_str(&format!("\x1b[{};1H\x1b[J", self.region + 1));
        }
        text.push_str(&format!("\x1b[{};1H", self.row));
        text.push_str(&body);
        let end = (self.row + lines).min(self.rows);
        if end > self.region {
            text.push_str(&self.scroll_up(end - self.region));
        }
        text.push_str(&format!("\x1b[1;{}r", self.region));
        self.row = self.region;
        self.cleared = true;
        text
    }

    /// The usage limit the thread stopped on, as its last line says it now, or empty.
    fn limit_now(&self, shell_thread: Option<&Value>) -> String {
        shell_thread
            .and_then(t3client::inbox::limit)
            .map(|limit| limit_line(&limit, t3::now_ms(), self.offset_minutes))
            .unwrap_or_default()
    }

    /// The finished turn's last line: when it finished and how long it took, or that it
    /// failed or stopped. A turn stopped on a usage limit says when the limit resets, and
    /// whether T3 resumes the thread then.
    fn footer(&self, state: &ThreadState) -> Option<String> {
        if !self.limit.is_empty() {
            return Some(format!(
                "{}{YELLOW}◷{RESET} {DIM}{}{RESET}",
                " ".repeat(self.column().0),
                fit(&self.limit, self.column().1.saturating_sub(2))
            ));
        }
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

    fn set_status(
        &mut self,
        shell: &ShellState,
        thread: Option<&Value>,
        state: Option<&ThreadState>,
    ) {
        let Some(thread) = thread else { return };
        let now = t3::now_ms();
        let word = t3client::inbox::status_label(thread, now);
        // A usage limit says what it waits for: `Limited · resumes 3pm`.
        let limited = t3client::inbox::limit(thread)
            .map(|limit| limit_wait(&limit, now, self.offset_minutes))
            .filter(|wait| !wait.is_empty())
            .map(|wait| format!("Limited · {wait}"));
        let shown = limited
            .as_deref()
            .or(word)
            .or(t3client::inbox::finished(thread).then_some("Done"));
        let project = project_name(shell, thread).map(one_line);
        let ask = state.and_then(crate::ask::line);
        let seq = status_sequence(word, shown, ask.as_deref(), project.as_deref());
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

/// What a thread's pane shows.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The conversation, with Reply in its bottom rows.
    Inline,
    /// The conversation alone, over a Reply pane.
    Conversation,
    /// Reply alone, under the conversation's pane, sized to fit.
    Reply,
}

/// How long after a turn of the wheel the one-pane window takes clicks again. The wheel turns
/// them off, so the terminal scrolls its scrollback.
const WHEEL_REST: Duration = Duration::from_millis(1500);

/// Clicks on, for Reply's buttons and menu, or off, for the scrollback and selection.
fn clicks(on: bool) -> &'static str {
    if on {
        "\x1b[?1000h\x1b[?1006h"
    } else {
        "\x1b[?1006l\x1b[?1000l"
    }
}

pub async fn run(thread: String, passive: bool, role: Role) -> Result<()> {
    let (client, thread) = t3::thread(&thread)?;
    let thread_id = thread.thread;
    let scheme = crate::scheme::listen();
    crossterm::terminal::enable_raw_mode()?;
    // Clear what the login printed, and ask for focus reports and bracketed paste. Paste
    // events too, where the filter can take the clipboard's replies.
    let clipboard = crate::scheme::clipboard();
    // Reply alone keeps nothing to scroll back to, so it takes the alternate screen. Its
    // title is its job: the window's label above already names the thread and project.
    if role == Role::Reply {
        out("\x1b]2;Reply\x07\x1b[?1049h");
    }
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
    let result = event_loop(passive, role, &client, &thread_id, scheme, clipboard).await;
    drop(keys);
    out(&format!(
        "{}{}\x1b[r\x1b[?2004l\x1b[?1004l\x1b[?25h{}",
        paste::OFF,
        clicks(false),
        if role == Role::Reply {
            "\x1b[?1049l"
        } else {
            ""
        }
    ));
    let _ = crossterm::terminal::disable_raw_mode();
    result
}

async fn event_loop(
    passive: bool,
    role: Role,
    client: &std::sync::Arc<t3client::env::Env>,
    thread_id: &str,
    mut scheme: tokio::sync::watch::Receiver<Option<bool>>,
    mut clipboard: Option<mpsc::UnboundedReceiver<String>>,
) -> Result<()> {
    let mut thread_events = client.watch_thread(thread_id, None);
    let mut shell_events = (role != Role::Reply).then(|| client.watch_shell(None));
    let (snap_tx, mut snap_rx) = mpsc::unbounded_channel();
    // Reply asks for the machine's config: the provider's driver for each instance, so the pane
    // can take its provider's name, and the address T3 serves its web app at, for the
    // thread's link.
    let (mut composer, mut done) = Composer::new(client.clone(), thread_id.to_string());
    if role == Role::Reply {
        composer.share = 1.0;
    }
    // Reply alone fits its pane to what it shows, by moving the split above it.
    let mut fit = (role == Role::Reply).then(crate::reply::Fit::new).flatten();
    let (fitted_tx, mut fitted) = mpsc::unbounded_channel();
    // Where Reply alone starts in its pane, to clear above it when that moves.
    let mut reply_top = 0;
    let link = crate::link::thread(None, client.id(), thread_id);
    let mut pane = Pane::new(Media::new(client.state_dir().as_deref()), link, snap_tx);
    let mut shell = ShellState::default();
    let mut input = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(120));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Every eighth tick, about a second: a model the picker held for the next message.
    let mut ticks = 0u32;
    let mut visited_for = String::new();
    let mut dirty = false;
    // Reply draws again only when something it shows may have changed.
    let mut reply = true;
    let mut focused = true;
    let mut renamed = false;
    // The binary was replaced by an update: start again on it once nothing is typed.
    let mut stale = false;
    // Whether clicks are on, and when the wheel last turned them off for the scrollback.
    let mut clicking = false;
    let mut wheeled: Option<tokio::time::Instant> = None;
    let mut asking = String::new();
    let mut next_frame = tokio::time::Instant::now();
    let mut resize = None;
    let mut resize_at = tokio::time::Instant::now();
    let mut perf = crate::perf::Frames::new();
    let (file_tx, mut file_rx) = mpsc::channel(4);
    let mut files_checked = tokio::time::Instant::now();

    loop {
        let mut content_event = false;
        tokio::select! {
            event = thread_events.recv() => match event {
                Some(WatchEvent::Item(item)) => {
                    let changed = if item["kind"] == "snapshot" {
                        composer.state = ThreadState::from_snapshot(&item);
                        pane.display = None;
                        true
                    } else {
                        composer.state.as_mut().is_some_and(|s| matches!(s.apply(&item), Applied::Event(_) | Applied::Snapshot))
                    };
                    let recovered = pane.offline.is_some() || composer.offline.is_some();
                    pane.offline = None;
                    composer.offline = None;
                    if !changed && !recovered { continue; }
                    composer.thread_changed();
                    content_event = true;
                    pane.offline = None;
                    composer.offline = None;
                    dirty = true;
                    reply = true;
                }
                Some(WatchEvent::Reconnecting { reason, retry_in }) => {
                    let said = offline_line(client.name(), client.is_local(), &reason, retry_in);
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
            event = async {
                match shell_events.as_mut() {
                    Some(events) => events.recv().await,
                    None => std::future::pending().await,
                }
            } => if let Some(WatchEvent::Item(item)) = event {
                shell.apply(&item);
                if role != Role::Reply {
                    let thread = t3client::inbox::find_thread(&shell, thread_id);
                    pane.set_status(&shell, thread, composer.state.as_ref());
                }
            },
            Some(ok) = fitted.recv() => if let Some(fit) = fit.as_mut() {
                fit.finished(ok);
            },
            event = done.recv() => if let Some(event) = event {
                // The config comes again with each change, so the thread prints again only
                // for a new link.
                let config = matches!(event, crate::compose::Done::Config(_));
                let shown = composer.config_shown();
                if let crate::compose::Done::Config(found) = &event {
                    let link = crate::link::thread(Some(found), client.id(), thread_id);
                    if link != pane.link {
                        pane.link = link;
                        pane.reflow = true;
                        dirty = true;
                    }
                }
                composer.done(event);
                // A config that changes nothing Reply shows draws nothing: T3 sends one each
                // time a provider's status is checked.
                reply |= !config || shown.is_none() || composer.config_shown() != shown;
            },
            event = input.next() => {
                // A content event may have left the composer waiting for the next frame.
                let pending = reply;
                reply = true;
                match event {
                    Some(Ok(Event::Resize(cols, rows))) => {
                        resize = Some((cols as usize, rows as usize));
                        resize_at = tokio::time::Instant::now() + Duration::from_millis(50);
                        reply = false;
                    }
                    Some(Ok(Event::FocusGained)) => { focused = true; dirty = true; },
                    Some(Ok(Event::FocusLost)) => focused = false,
                    // The conversation alone has nothing to type into.
                    Some(Ok(Event::Key(_) | Event::Mouse(_) | Event::Paste(_)))
                        if role == Role::Conversation => reply = false,
                    Some(Ok(Event::Key(key))) if key.kind != KeyEventKind::Release => {
                        wheeled = None;
                        composer.key(key);
                    }
                    Some(Ok(Event::Mouse(m))) => {
                        if !composer.click(&m)
                            && matches!(m.kind, MouseEventKind::ScrollUp | MouseEventKind::ScrollDown)
                        {
                            wheeled = Some(tokio::time::Instant::now());
                        }
                    }
                    Some(Ok(Event::Paste(text))) => composer.pasted(Got::Text(text)),
                    Some(Err(e)) => return Err(e.into()),
                    None => return Ok(()),
                    _ => reply = false,
                }
                reply |= pending;
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
                // The fill under the input follows the new background.
                crate::scheme::ask_background();
                let dark = crate::scheme::dark();
                if dark != pane.media.dark {
                    pane.media.set_dark(dark);
                    pane.reflow = true;
                    dirty = true;
                }
                composer.reload_logos();
            }
            Some(result) = file_rx.recv() => {
                let mut changed = pane.files.finish(result, t3::now_ms());
                while let Ok(result) = file_rx.try_recv() {
                    changed |= pane.files.finish(result, t3::now_ms());
                }
                pane.reflow |= changed;
                dirty |= changed;
            }
            Some(ready) = snap_rx.recv() => {
                let mut changed = pane.media.ready(ready);
                while let Ok(ready) = snap_rx.try_recv() {
                    changed |= pane.media.ready(ready);
                }
                pane.reflow |= changed;
                dirty |= changed;
            }
            _ = tokio::time::sleep_until(composer.note_until.unwrap_or_else(tokio::time::Instant::now)), if composer.note_until.is_some() => {
                composer.clear_note();
                reply = true;
            }
            _ = tokio::time::sleep_until(resize_at), if resize.is_some() => {
                if let Some((cols, rows)) = resize.take() && (cols, rows) != (pane.cols, pane.rows) {
                    pane.cols = cols;
                    pane.rows = rows;
                    pane.reflow = true;
                    dirty = true;
                    reply = true;
                }
            }
            _ = tokio::time::sleep_until(next_frame), if dirty && composer.state.is_some() || reply && role != Role::Conversation => {},
            _ = tick.tick() => {
                if let Some(fit) = fit.as_mut()
                    && let Some(job) = fit.due()
                {
                    let tx = fitted_tx.clone();
                    tokio::spawn(async move {
                        let _ = tx.send(job.await);
                    });
                }
                if pane.animating && focused {
                    pane.frame += 1;
                    dirty = true;
                }
                if pane.chat_width.changed() {
                    pane.reflow = true;
                    dirty = true;
                }
                // Clicks come back once the wheel has rested, for the buttons.
                if wheeled.is_some_and(|at| at.elapsed() >= WHEEL_REST) {
                    wheeled = None;
                }
                ticks = ticks.wrapping_add(1);
                if ticks.is_multiple_of(8) && composer.reload_draft() {
                    reply = true;
                    dirty = true;
                }
                // A usage limit's times read differently as the clock moves on.
                if ticks.is_multiple_of(8) && role != Role::Reply {
                    let thread = t3client::inbox::find_thread(&shell, thread_id);
                    if pane.limit_now(thread) != pane.limit {
                        dirty = true;
                    }
                }
                if ticks.is_multiple_of(8) && role != Role::Conversation && composer.usage_moved() {
                    reply = true;
                }
                if ticks.is_multiple_of(40) && !stale && crate::icons::replaced() {
                    stale = true;
                }
            }
        }
        if content_event && tokio::time::Instant::now() < next_frame {
            continue;
        }
        let started = std::time::Instant::now();
        if let Some(width) = composer.width_set.take() {
            pane.chat_width.width = width;
            pane.reflow = true;
            dirty = true;
        }
        if stale && composer.idle() {
            stale = false;
            out(&format!("{}{}\x1b[r", paste::OFF, clicks(false)));
            if role == Role::Reply {
                out("\x1b[?1049l");
            }
            let _ = crossterm::terminal::disable_raw_mode();
            crate::icons::start_again();
            // Still here: it couldn't start again, so it carries on as it was.
            let _ = crossterm::terminal::enable_raw_mode();
            pane.reflow = true;
            dirty = true;
            reply = true;
        }
        // A thread that moved to another provider: once nothing is typed, start again under
        // that provider's name, so Rex's sidebar shows its glyph. The thread prints again from
        // the top.
        if !renamed && composer.idle() && composer.state.is_some() {
            // The provider the next message goes to: the thread's, or the one picked for it
            // while it worked.
            let instance = composer.next_instance();
            let driver = crate::icons::driver_of(composer.config.as_ref(), &instance);
            let want = crate::icons::program_name(driver).unwrap_or("t3rex");
            if !instance.is_empty()
                && composer.config.is_some()
                && crate::icons::started_as() != want
            {
                renamed = true;
                out(&format!("{}{}\x1b[r", paste::OFF, clicks(false)));
                let _ = crossterm::terminal::disable_raw_mode();
                crate::icons::become_program(driver);
                // Still here: the pane couldn't start again, so it carries on as it was.
                let _ = crossterm::terminal::enable_raw_mode();
                pane.reflow = true;
                dirty = true;
                reply = true;
            }
        }
        // Reply's rows at the bottom, and the conversation's above them.
        composer.cols = pane.cols;
        composer.rows = pane.rows;
        composer.column = pane.column();
        let height = match role {
            Role::Inline => composer.height().min(pane.rows.saturating_sub(1)).max(1),
            Role::Conversation => 0,
            Role::Reply => composer.height().min(pane.rows).max(1),
        };
        let mut text = String::new();
        if role == Role::Reply {
            if let Some(fit) = fit.as_mut() {
                fit.want(composer.wanted);
            }
            // Reply alone sits at the bottom of its pane, over blank rows.
            let top = pane.rows + 1 - height;
            let force = std::mem::take(&mut pane.reflow) || top != reply_top;
            if force {
                text.push_str("\x1b[H\x1b[2J");
            }
            reply_top = top;
            if reply || force {
                text.push_str(&composer.draw(top, force));
                reply = false;
            }
            dirty = false;
        } else {
            text = pane.set_region(pane.rows.saturating_sub(height).max(1));
        }
        dirty |= pane.reflow;
        if dirty && let Some(s) = composer.state.as_ref() {
            let thread = t3client::inbox::find_thread(&shell, thread_id);
            pane.set_title(s, thread.and_then(|t| project_name(&shell, t)));
            pane.set_status(&shell, thread, Some(s));
            text.push_str(&pane.draw(s, thread));
            dirty = false;
        }
        if role != Role::Reply
            && let Some(s) = composer.state.as_ref()
            && files_checked.elapsed() >= Duration::from_secs(1)
            && let Some(display) = pane.display.as_ref()
        {
            let root = s.thread()["worktreePath"]
                .as_str()
                .or_else(|| {
                    let project = s.thread()["projectId"].as_str()?;
                    shell.projects.iter().find(|p| p["id"] == project)?["workspaceRoot"].as_str()
                })
                .unwrap_or_default();
            for request in pane.files.requests(
                display.blocks.iter().map(|b| b.body.as_str()),
                composer.config.as_ref(),
                thread_id,
                root,
                t3::now_ms(),
            ) {
                let (tx, client) = (file_tx.clone(), client.clone());
                tokio::spawn(async move {
                    tokio::select! {
                        result = request.run(client) => { let _ = tx.send(result).await; },
                        _ = tx.closed() => {},
                    }
                });
            }
            files_checked = tokio::time::Instant::now();
        }
        // A frame that cleared the screen needs Reply again; any frame, the cursor back in the
        // input.
        if role == Role::Inline {
            if reply || pane.cleared || !text.is_empty() && composer.parked.is_empty() {
                text.push_str(&composer.draw(pane.region + 1, pane.cleared));
                pane.cleared = false;
                reply = false;
            } else if !text.is_empty() {
                text.push_str(&composer.parked);
            }
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
            wheeled = None;
        }
        // Reply alone has no scrollback for the wheel, so clicks stay on there.
        let want = match role {
            Role::Inline => composer.wants_clicks() && wheeled.is_none(),
            Role::Conversation => false,
            Role::Reply => true,
        };
        if want != clicking {
            clicking = want;
            text.push_str(clicks(want));
        }
        if !text.is_empty() {
            perf.record(started.elapsed(), text.len());
            out(&format!("\x1b[?2026h\x1b[?25l{text}\x1b[?2026l"));
            next_frame = tokio::time::Instant::now() + Duration::from_millis(33);
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
    fn a_lost_mac_says_t3_quit_and_a_lost_machine_says_offline() {
        let lost = "T3 connection lost: the server closed the connection";
        let wait = Duration::from_secs(4);
        assert_eq!(offline_line("Mac mini", true, lost, wait), LOCAL_OFFLINE);
        assert_eq!(
            offline_line("studio", false, lost, wait),
            "studio is offline · retrying in 4s"
        );
        let unpaired = "Rex isn't paired. Run `t3rex pair studio`.";
        assert_eq!(
            offline_line("studio", true, unpaired, wait),
            "Run `t3rex pair studio`"
        );
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

    fn dynamic(name: &str, detail: &str) -> Block {
        Block {
            tool_name: name.into(),
            ..tool("dynamic_tool", name, detail, "")
        }
    }

    #[test]
    fn runs_of_tools_sum_up_as_t3_does() {
        let cmd = || tool("command_execution", "$ ls", "ls", "");
        let (a, b, c) = (cmd(), cmd(), cmd());
        assert_eq!(run_summary(&[&a]), "Ran 1 command");
        let read = dynamic("Read", "/x/a.rs");
        assert_eq!(
            run_summary(&[&a, &b, &read]),
            "Ran 2 commands and read 1 file"
        );
        let (t1, t2) = (dynamic("ToolSearch", ""), dynamic("TaskStop", ""));
        assert_eq!(
            run_summary(&[&a, &b, &t1, &t2]),
            "Ran 2 commands and used 2 tools"
        );
        // Past two kinds: the two most telling, then the rest.
        assert_eq!(
            run_summary(&[&t1, &read, &a, &c]),
            "Read 1 file, ran 2 commands, and performed 1 other action"
        );
        // Edits count files.
        let (e1, e2) = (dynamic("Edit", "a.rs"), dynamic("Edit", "a.rs"));
        assert_eq!(run_summary(&[&e1, &e2]), "Changed 1 file");
    }

    #[test]
    fn a_run_shows_one_line_and_only_running_calls_draw_rows() {
        let mut running = tool("command_execution", "$ sleep 9", "sleep 9", "");
        running.status = "inProgress".into();
        let mut blocks = vec![
            tool("command_execution", "$ ls", "ls", ""),
            tool("command_execution", "$ ls", "ls", ""),
            running,
        ];
        let embeds = HashMap::new();
        let ctx = Ctx {
            width: 80,
            frame: 0,
            offset: 0,
            embeds: &embeds,
            media: None,
            link: "",
            files: &crate::link::FileLinks::default(),
        };
        assert!(lines_at(&blocks, 0, &ctx).is_empty());
        assert_eq!(lines_at(&blocks, 2, &ctx).len(), 1);
        assert!(
            run_line(&blocks, &embeds, 80)
                .unwrap()
                .contains("Ran 2 commands")
        );
        blocks[2].status = "completed".into();
        let mut reply = tool("", "", "", "");
        reply.kind = BlockKind::Assistant;
        reply.body = "done".into();
        blocks.push(reply);
        assert!(lines_at(&blocks, 3, &ctx)[0].contains("Ran 3 commands"));
    }

    #[test]
    fn an_html_embed_draws_beside_a_folded_run() {
        let mut blocks = vec![
            tool("command_execution", "$ ls", "ls", ""),
            tool("command_execution", "$ ls", "ls", ""),
            tool("command_execution", "$ ls", "ls", ""),
        ];
        blocks[1].item_id = "embed".into();
        let embed = media::Embed {
            attachment: "a".into(),
            title: "Options".into(),
            height: 400,
        };
        let embeds = HashMap::from([("embed".to_string(), embed)]);
        let ctx = Ctx {
            width: 80,
            frame: 0,
            offset: 0,
            embeds: &embeds,
            media: None,
            link: "",
            files: &crate::link::FileLinks::default(),
        };
        assert!(lines_at(&blocks, 0, &ctx).is_empty());
        assert!(
            lines_at(&blocks, 1, &ctx)
                .iter()
                .any(|l| l.contains("Options"))
        );
        assert!(
            run_line(&blocks, &embeds, 80)
                .unwrap()
                .contains("Ran 2 commands")
        );
    }

    #[test]
    fn status_maps_to_rex_states() {
        assert!(
            status_sequence(Some("Approval"), Some("Approval"), None, Some("t"))
                .contains("state=blocked:kind=permission")
        );
        assert!(
            status_sequence(Some("Input"), Some("Input"), None, Some("t"))
                .contains("state=blocked:kind=question")
        );
        assert!(
            status_sequence(Some("Working"), Some("Working"), None, Some("t"))
                .contains("state=working")
        );
        assert!(
            status_sequence(Some("Done"), Some("Done"), None, Some("t")).contains("state=done")
        );
        assert!(status_sequence(None, None, None, Some("t")).contains("state=idle"));
    }

    #[test]
    fn status_says_what_is_asked_within_the_limits() {
        let b64 = |t: &str| base64::engine::general_purpose::STANDARD.encode(t);
        let seq = status_sequence(
            Some("Approval"),
            Some("Approval"),
            Some("Run pnpm test?"),
            Some("t3rex"),
        );
        assert_eq!(
            seq,
            format!(
                "\x1b]7501;state=blocked:kind=permission:app=t3code:title={}:msg={}\x1b\\",
                b64("t3rex"),
                b64("Run pnpm test?")
            )
        );
        // Not blocked: the project and the status word, whatever was asked before.
        assert!(
            status_sequence(Some("Working"), Some("Working"), Some("x"), Some("t3rex"))
                .ends_with(&format!(":msg={}\x1b\\", b64("t3rex · Working")))
        );
        // Idle: the project alone, on the line under the label; no project, the word alone.
        assert_eq!(
            status_sequence(None, None, None, Some("t3rex")),
            format!(
                "\x1b]7501;state=idle:app=t3code:title={}:msg={}\x1b\\",
                b64("t3rex"),
                b64("t3rex")
            )
        );
        assert_eq!(
            status_sequence(None, None, None, None),
            "\x1b]7501;state=idle:app=t3code\x1b\\"
        );
        // A finished run you've seen: Done on the line, at rest in Rex.
        assert_eq!(
            status_sequence(None, Some("Done"), None, Some("t3rex")),
            format!(
                "\x1b]7501;state=idle:app=t3code:title={}:msg={}\x1b\\",
                b64("t3rex"),
                b64("t3rex · Done")
            )
        );
        assert!(
            status_sequence(Some("Done"), Some("Done"), None, None)
                .ends_with(&format!(":app=t3code:msg={}\x1b\\", b64("Done")))
        );
        // Over the limits: cut on a character, never mid-way through one.
        let long = "é".repeat(2000);
        let seq = status_sequence(Some("Input"), Some("Input"), Some(&long), Some(&long));
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
        p.rows = 24;
        assert_eq!(
            p.set_region(18),
            "\x1b[r\x1b[24;1H\n\n\x1b[19;1H\x1b[J\x1b[1;18r"
        );
        assert_eq!(p.row, 18);
        assert!(p.cleared);
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
    fn moments_read_in_local_time() {
        let h = 3_600_000;
        // 2026-10-31T00:00:00Z, a Saturday, at UTC.
        let now = t3client::inbox::parse_iso("2026-10-31T00:00:00.000Z").unwrap();
        assert_eq!(when(now + 15 * h, now, 0), "3pm");
        assert_eq!(when(now + 15 * h + 30 * 60_000, now, 0), "3:30pm");
        assert_eq!(when(now + 33 * h, now, 0), "tomorrow 9am");
        assert_eq!(when(now + 3 * 24 * h, now, 0), "Tue 12am");
        assert_eq!(when(now + 12 * 24 * h + 9 * h, now, 0), "12 Nov");
        assert_eq!(when(now - 9 * h, now, 0), "yesterday 3pm");
        assert_eq!(when(now - 3 * 24 * h, now, 0), "28 Oct");
        // Ten hours ahead, 3pm UTC is 1am the next day.
        assert_eq!(when(now + 15 * h, now, 600), "tomorrow 1am");
    }

    #[test]
    fn a_usage_limit_says_when_it_resets_and_whether_it_resumes() {
        let h = 3_600_000;
        let now = t3client::inbox::parse_iso("2026-10-31T00:00:00.000Z").unwrap();
        let limit = |resets_at: Option<i64>, resumes: bool| Limit { resets_at, resumes };
        let later = Some(now + 15 * h);
        assert_eq!(
            limit_line(&limit(later, true), now, 0),
            "Usage limit reached · resets 3pm · resumes then"
        );
        assert_eq!(limit_wait(&limit(later, true), now, 0), "resumes 3pm");
        assert_eq!(
            limit_line(&limit(later, false), now, 0),
            "Usage limit reached · resets 3pm"
        );
        assert_eq!(limit_wait(&limit(later, false), now, 0), "resets 3pm");
        // The reset has come: T3 is about to send it on, or it's your move.
        let passed = Some(now - h);
        assert_eq!(
            limit_line(&limit(passed, true), now, 0),
            "Usage limit reached · resuming"
        );
        assert_eq!(
            limit_line(&limit(passed, false), now, 0),
            "Usage limit reached · reset yesterday 11pm"
        );
        // The provider didn't say when.
        assert_eq!(
            limit_line(&limit(None, false), now, 0),
            "Usage limit reached"
        );
        assert_eq!(limit_wait(&limit(None, false), now, 0), "");
    }

    #[test]
    fn a_limited_thread_says_what_it_waits_for_under_its_label() {
        let b64 = |t: &str| base64::engine::general_purpose::STANDARD.encode(t);
        assert_eq!(
            status_sequence(
                Some("Limited"),
                Some("Limited · resumes 3pm"),
                None,
                Some("t3rex")
            ),
            format!(
                "\x1b]7501;state=error:app=t3code:title={}:msg={}\x1b\\",
                b64("t3rex"),
                b64("t3rex · Limited · resumes 3pm")
            )
        );
    }

    /// A thread whose only run stopped on a usage limit, and the shell's word on it.
    fn limited(recovery: Value, resets_at: i64) -> (ThreadState, Value) {
        let state = ThreadState::from_snapshot(&serde_json::json!({
            "kind": "snapshot",
            "snapshotSequence": 3,
            "projection": {
                "thread": {"id": "t1", "title": "Ship it"},
                "runs": [{"id": "r1", "ordinal": 1, "status": "failed", "userMessageId": "m1",
                    "startedAt": "2026-10-31T00:00:00.000Z", "completedAt": "2026-10-31T00:01:00.000Z"}],
                "runtimeRequests": [],
                "turnItems": [],
                "visibleTurnItems": []
            }
        }))
        .unwrap();
        let reset = t3::iso(resets_at);
        let mut recovery = recovery;
        if recovery.is_object() {
            recovery["resetAt"] = Value::String(reset.clone());
        }
        let shell = serde_json::json!({
            "id": "t1", "latestRunId": "r1", "status": "failed", "lastErrorClass": "usage_limit",
            "usageLimitResetAt": reset, "limitRecovery": recovery,
            "pendingRuntimeRequest": null, "archivedAt": null, "settledOverride": null,
        });
        (state, shell)
    }

    #[test]
    fn the_conversation_ends_on_the_limit_not_failed() {
        let resets = t3::now_ms() + 2 * 3_600_000;
        let armed = serde_json::json!({"runId": "r1", "autoResume": true});
        let (state, shell) = limited(armed, resets);
        let mut p = pane(20, 1, 0);
        p.header = false;
        let drawn = p.draw(&state, Some(&shell));
        let time = when(resets, t3::now_ms(), p.offset_minutes);
        assert!(
            drawn.contains(&format!(
                "Usage limit reached · resets {time} · resumes then"
            )),
            "{drawn:?}"
        );
        assert!(!drawn.contains("Failed"), "{drawn:?}");
        // Without auto-resume, only when it resets.
        let (state, shell) = limited(Value::Null, resets);
        let drawn = p.draw(&state, Some(&shell));
        assert!(drawn.contains(&format!("Usage limit reached · resets {time}")));
        assert!(!drawn.contains("resumes"));
        // Another failure keeps its own line.
        let mut other = shell.clone();
        other["lastErrorClass"] = Value::String("provider_error".into());
        let drawn = p.draw(&state, Some(&other));
        assert!(drawn.contains("Failed") && !drawn.contains("Usage limit"));
        assert!(p.limit.is_empty());
    }

    #[test]
    fn an_animation_frame_does_not_rewrite_unchanged_live_text() {
        let state = ThreadState::from_snapshot(&serde_json::json!({
            "snapshotSequence": 1,
            "projection": {
                "thread": {"id": "t1", "title": "Working"},
                "runs": [{"id": "r1", "ordinal": 1, "status": "running"}],
                "turnItems": [{"id": "a1", "type": "assistant_message", "ordinal": 1,
                    "status": "in_progress", "text": "A long answer stays visible while the spinner moves."}]
            }
        })).unwrap();
        let mut p = pane(40, 1, 0);
        p.rows = 44;
        p.header = false;
        let first = p.draw(&state, None);
        assert!(first.contains("A long answer"));
        p.frame += 1;
        let next = p.draw(&state, None);
        assert!(!next.contains("A long answer"), "{next:?}");
        assert!(next.contains(SPINNER[1]));
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
