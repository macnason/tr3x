//! The status line under Reply: what T3 Code's thread details panel shows, in the terminal.
//!
//! Off, the default, is the one model chip Reply has always had, which warns when the context
//! window runs low. `/status on` adds the context window and the prompt cache beside it, and a
//! second line with the machine, branch, pull request and usage limits. A column too narrow
//! for two lines, as on a phone, gets the most of that on one.
//!
//! What's left reads as T3's panel reads it: the share left, green, yellow at 20% or less and
//! red at 10% or less. Colours are the Rex theme's own design tokens, from the theme for the
//! screen it's seen on, so they match Rex's chrome; without one, the terminal's ANSI colours.
//!
//! The setting is `"statusLine"` in `~/.config/tr3x/settings.json`, which `/status` and
//! `tr3x status` write.

use anyhow::{Context as _, Result};
use serde_json::Value;
use unicode_width::UnicodeWidthStr;

use crate::chat::{BLUE, BOLD, DIM, GREEN, MAGENTA, RED, RESET, YELLOW};
use crate::scheme::Tokens;
use crate::usage::Window;

pub const CYAN: &str = "\x1b[36m";

/// The narrowest column the status line takes two lines in. Below it, one.
const FULL_MIN: usize = 72;
/// Cells in the context bar.
const BAR: usize = 10;
/// The prompt cache turns yellow in its last minutes.
const CACHE_WARN_MS: i64 = 10 * 60_000;
/// The share left at which the chip starts warning, while the status line is off.
const LOW: u8 = 20;

/// How the status line lays out in a column: two lines, or one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Layout {
    Full,
    Compact,
}

/// On or off. Earlier builds wrote a layout's name, so those still read: any of them as on,
/// the chip as off.
pub fn parse(text: &str) -> Option<bool> {
    match text.trim().to_lowercase().as_str() {
        "on" | "auto" | "full" | "compact" => Some(true),
        "off" | "chip" | "default" => Some(false),
        _ => None,
    }
}

fn name(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

/// The setting, off unless set.
pub fn current() -> bool {
    set_in(crate::width::settings().as_ref())
}

fn set_in(settings: Option<&serde_json::Map<String, Value>>) -> bool {
    settings
        .and_then(|s| s.get("statusLine")?.as_str().and_then(parse))
        .unwrap_or_default()
}

/// What `/status` and `tr3x status` take: on or off, or nothing to say which is set.
pub fn apply(arg: &str) -> Result<String> {
    let arg = arg.trim();
    if arg.is_empty() {
        return Ok(format!("Status line: {}", name(current())));
    }
    let on = parse(arg).with_context(|| format!("Can't read {arg:?}: on or off"))?;
    // Off is the default, so it leaves nothing behind.
    crate::width::set_setting("statusLine", on.then(|| Value::from("on")))?;
    Ok(format!("Status line: {}", name(on)))
}

/// The context window of the thread's provider session, as its latest turn reported it.
#[derive(Clone, Debug, PartialEq)]
pub struct ContextWindow {
    pub used: u64,
    pub max: Option<u64>,
    /// When the provider's prompt cache runs out, in ms since the epoch.
    pub cache_until: Option<i64>,
}

impl ContextWindow {
    /// The share left, 0 to 100, when the window's size is known.
    pub fn left(&self) -> Option<u8> {
        let max = self.max.filter(|m| *m > 0)?;
        let used = self.used.min(max) as f64 / max as f64;
        Some((100.0 - used * 100.0).round() as u8)
    }
}

/// The context window of the provider thread T3 is using now. The provider thread keeps the
/// last count; its turns carry the newest report and when the prompt cache runs out. A
/// provider switch leaves the old provider's turns behind, so only the active one's count.
pub fn context(projection: &serde_json::Map<String, Value>) -> Option<ContextWindow> {
    let thread = projection.get("thread")?;
    let active = thread["activeProviderThreadId"].as_str();
    let list = |key: &str| {
        projection
            .get(key)
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
    };
    let latest = list("providerTurns")
        .iter()
        .filter(|t| active.is_none() || t["providerThreadId"].as_str() == active)
        .filter_map(|t| {
            let usage = t.get("tokenUsage").filter(|u| u.is_object())?;
            let at = t3client::inbox::parse_iso(usage["updatedAt"].as_str()?)?;
            Some((at, usage))
        })
        // Ties go to the later entry, since live upserts append.
        .fold(
            None,
            |best: Option<(i64, &Value)>, (at, usage)| match best {
                Some((b, _)) if b > at => best,
                _ => Some((at, usage)),
            },
        )
        .map(|(_, usage)| usage);
    let kept = active
        .and_then(|id| list("providerThreads").iter().find(|p| p["id"] == id))
        .map(|p| &p["contextUsage"])
        .filter(|u| u.is_object());
    let counted = latest.or(kept)?;
    let used = counted["usedTokens"].as_u64()?;
    let max = counted["maxTokens"]
        .as_u64()
        .or_else(|| kept.and_then(|k| k["maxTokens"].as_u64()));
    let cache_until = latest
        .and_then(|u| u["promptCacheExpiresAt"].as_str())
        .and_then(t3client::inbox::parse_iso);
    Some(ContextWindow {
        used,
        max,
        cache_until,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Checks {
    Passing,
    Failing,
    Pending,
}

/// The thread's pull request, from the snapshot T3 keeps of it.
#[derive(Clone, Debug, PartialEq)]
pub struct PullRequest {
    pub number: u64,
    pub url: String,
    /// `open`, `merged` or `closed`.
    pub state: String,
    pub draft: bool,
    pub additions: Option<u64>,
    pub deletions: Option<u64>,
    pub checks: Option<Checks>,
}

/// The pull request to show: the newest open one the thread links, else its newest.
pub fn pull_request(thread: &Value) -> Option<PullRequest> {
    let prs: Vec<&Value> = thread["pullRequests"].as_array()?.iter().collect();
    let newest = |open: bool| {
        prs.iter()
            .filter(|p| !open || p["snapshot"]["state"].as_str().unwrap_or("open") == "open")
            .max_by_key(|p| p["linkedAt"].as_str().unwrap_or_default().to_string())
            .copied()
    };
    let pr = newest(true).or_else(|| newest(false))?;
    let snapshot = &pr["snapshot"];
    Some(PullRequest {
        number: pr["number"].as_u64()?,
        url: pr["url"].as_str()?.to_string(),
        state: snapshot["state"].as_str().unwrap_or("open").to_string(),
        draft: snapshot["isDraft"].as_bool().unwrap_or(false),
        additions: snapshot["additions"].as_u64(),
        deletions: snapshot["deletions"].as_u64(),
        checks: match snapshot["checksState"].as_str() {
            Some("passing") => Some(Checks::Passing),
            Some("failing") => Some(Checks::Failing),
            Some("pending") => Some(Checks::Pending),
            _ => None,
        },
    })
}

/// Everything the status line says, gathered before it's laid out.
#[derive(Clone, Debug, Default)]
pub struct Facts {
    pub model: String,
    pub effort: Option<String>,
    /// The runtime mode's id (`full-access`) and its label (`Full access`).
    pub mode: (String, String),
    pub context: Option<ContextWindow>,
    pub machine: String,
    pub branch: Option<String>,
    pub pr: Option<PullRequest>,
    pub windows: Vec<Window>,
    /// In place of the usage limits: a provider that isn't ready, or an update waiting.
    pub notice: Option<(String, &'static str)>,
}

/// A piece of a line: its text with colours, its width, and how much it matters. The least
/// that matters goes first when the line is too long.
#[derive(Clone, Debug)]
struct Part {
    text: String,
    /// The text without its colours, to cut it by.
    plain: String,
    width: usize,
    keep: u8,
}

fn part(text: String, plain: &str, keep: u8) -> Part {
    Part {
        text,
        plain: plain.to_string(),
        width: plain.width(),
        keep,
    }
}

/// What the status line draws in, as escapes: the Rex theme's tokens where it has them, the
/// terminal's ANSI colours where it doesn't.
#[derive(Clone, Debug, PartialEq)]
pub struct Ink {
    /// Labels.
    pub secondary: String,
    /// What matters least: separators, the token count.
    pub tertiary: String,
    pub success: String,
    pub warning: String,
    pub danger: String,
    /// The effort and the prompt cache, which the terminal draws cyan.
    pub accent: String,
}

impl Ink {
    pub fn from(tokens: Option<Tokens>) -> Ink {
        let t = tokens.unwrap_or_default();
        let pick = |rgb: Option<[u8; 3]>, ansi: &str| match rgb {
            Some([r, g, b]) => format!("\x1b[38;2;{r};{g};{b}m"),
            None => ansi.to_string(),
        };
        Ink {
            secondary: pick(t.secondary, DIM),
            tertiary: pick(t.tertiary, DIM),
            success: pick(t.success, GREEN),
            warning: pick(t.warning, YELLOW),
            danger: pick(t.danger, RED),
            accent: pick(t.accent, CYAN),
        }
    }

    /// The theme on the screen now.
    pub fn current() -> Ink {
        Ink::from(crate::scheme::tokens())
    }

    /// The colour of a share left, as Reply warns of usage limits.
    fn tone(&self, left: u8) -> &str {
        match left {
            0..=10 => &self.danger,
            11..=20 => &self.warning,
            _ => &self.success,
        }
    }
}

/// `ctx 14%`, for the chip to show while the status line is off, when what's left is low
/// enough to act on. Above that the chip stays as it was. With its width.
pub fn low_context(context: Option<&ContextWindow>, ink: &Ink) -> Option<(String, usize)> {
    let left = context?.left().filter(|l| *l <= LOW)?;
    let plain = format!("ctx {left}%");
    let text = format!(
        "{}ctx{RESET} {}{left}%{RESET}",
        ink.secondary,
        ink.tone(left)
    );
    Some((text, plain.width()))
}

/// `100k`, `1M`, `1.5M`, `950`.
pub fn tokens(n: u64) -> String {
    let short = |v: f64, unit: &str| {
        if v < 10.0 && v.fract() >= 0.05 {
            format!("{v:.1}{unit}")
        } else {
            format!("{}{unit}", v.round() as u64)
        }
    };
    match n {
        0..=999 => n.to_string(),
        1_000..=999_499 => short(n as f64 / 1e3, "k"),
        _ => short(n as f64 / 1e6, "M"),
    }
}

/// `42m`, `1h 5m`, or `<1m`.
fn minutes(ms: i64) -> String {
    let m = ms / 60_000;
    match m {
        0 => "<1m".into(),
        1..=59 => format!("{m}m"),
        _ => format!("{}h {}m", m / 60, m % 60),
    }
}

/// The chip's text: `Opus 5.5 · Medium · Full access`, model bold, effort in the accent, and
/// full access in the warning colour as T3 marks it.
fn chip(facts: &Facts, with_mode: bool, ink: &Ink) -> Part {
    let Ink {
        secondary,
        tertiary,
        accent,
        ..
    } = ink;
    let mut text = format!("{BOLD}{}{RESET}", facts.model);
    let mut plain = facts.model.clone();
    if let Some(effort) = &facts.effort {
        text.push_str(&format!(" {tertiary}·{RESET} {accent}{effort}{RESET}"));
        plain.push_str(&format!(" · {effort}"));
    }
    if with_mode {
        let color = if facts.mode.0 == "full-access" {
            &ink.warning
        } else {
            secondary
        };
        text.push_str(&format!(
            " {tertiary}·{RESET} {color}{}{RESET}",
            facts.mode.1
        ));
        plain.push_str(&format!(" · {}", facts.mode.1));
    }
    part(text, &plain, 9)
}

fn bar(left: u8, ink: &Ink) -> (String, String) {
    let filled = ((left as usize * BAR) + 50) / 100;
    let tertiary = &ink.tertiary;
    let text = format!(
        "{tertiary}▕{RESET}{}{}{RESET}{tertiary}{}▏{RESET}",
        ink.tone(left),
        "█".repeat(filled),
        "░".repeat(BAR - filled)
    );
    let plain = format!("▕{}{}▏", "█".repeat(filled), "░".repeat(BAR - filled));
    (text, plain)
}

fn cache(context: &ContextWindow, now: i64, ink: &Ink) -> Option<Part> {
    let rest = context.cache_until? - now;
    if rest <= 0 {
        return None;
    }
    let color = if rest <= CACHE_WARN_MS {
        &ink.warning
    } else {
        &ink.accent
    };
    let plain = format!("cache {}", minutes(rest));
    Some(part(format!("{color}{plain}{RESET}"), &plain, 3))
}

fn checks(checks: Checks, word: bool, ink: &Ink) -> (String, String) {
    let (mark, color) = match checks {
        Checks::Passing => ("✓", &ink.success),
        Checks::Failing => ("✗", &ink.danger),
        Checks::Pending => ("◷", &ink.warning),
    };
    let plain = if word {
        format!("{mark} checks")
    } else {
        mark.to_string()
    };
    (format!("{color}{plain}{RESET}"), plain)
}

/// `PR #31`, a link, with its state when it isn't open, and what it changes when `stats`.
fn pr_part(pr: &PullRequest, stats: bool, keep: u8, ink: &Ink) -> Part {
    let label = format!("PR #{}", pr.number);
    let mut text = format!("{BLUE}{}{RESET}", crate::link::osc8(&pr.url, &label));
    let mut plain = label;
    let state = match pr.state.as_str() {
        "merged" => Some(("merged", MAGENTA)),
        "closed" => Some(("closed", ink.secondary.as_str())),
        _ if pr.draft => Some(("draft", ink.secondary.as_str())),
        _ => None,
    };
    if let Some((word, color)) = state {
        text.push_str(&format!(" {color}{word}{RESET}"));
        plain.push_str(&format!(" {word}"));
    }
    if stats && let (Some(a), Some(d)) = (pr.additions, pr.deletions) {
        let (success, danger) = (&ink.success, &ink.danger);
        text.push_str(&format!(" {success}+{a}{RESET} {danger}−{d}{RESET}"));
        plain.push_str(&format!(" +{a} −{d}"));
    }
    if pr.state == "open"
        && let Some(c) = pr.checks
    {
        let (t, p) = checks(c, stats, ink);
        text.push_str(&format!(" {t}"));
        plain.push_str(&format!(" {p}"));
    }
    part(text, &plain, keep)
}

/// `Session 87% · Weekly 68%`, or `Session resets 3pm` for a window that's used up.
fn windows_part(windows: &[Window], now: i64, offset_minutes: i64, ink: &Ink) -> Option<Part> {
    if windows.is_empty() {
        return None;
    }
    let mut text = Vec::new();
    let mut plain = Vec::new();
    for w in windows {
        match w.resets_at {
            Some(at) if w.left == 0 && at > now => {
                let p = format!(
                    "{} resets {}",
                    w.label,
                    crate::chat::when(at, now, offset_minutes)
                );
                text.push(format!("{}{p}{RESET}", ink.danger));
                plain.push(p);
            }
            _ => {
                text.push(format!(
                    "{}{}{RESET} {}{}%{RESET}",
                    ink.secondary,
                    w.label,
                    ink.tone(w.left),
                    w.left
                ));
                plain.push(format!("{} {}%", w.label, w.left));
            }
        }
    }
    let sep = format!(" {}·{RESET} ", ink.tertiary);
    Some(part(text.join(&sep), &plain.join(" · "), 6))
}

fn sep(ink: &Ink) -> Part {
    part(format!(" {}·{RESET} ", ink.tertiary), " · ", u8::MAX)
}

/// Parts joined by ` · `.
fn joined(parts: Vec<Part>, ink: &Ink) -> Vec<Part> {
    let mut out = Vec::new();
    for (i, p) in parts.into_iter().enumerate() {
        if i > 0 {
            out.push(sep(ink));
        }
        out.push(p);
    }
    out
}

fn width(parts: &[Part]) -> usize {
    parts.iter().map(|p| p.width).sum()
}

/// The parts that aren't separators.
fn real(parts: &[Part]) -> usize {
    parts.iter().filter(|p| p.keep != u8::MAX).count()
}

/// How much the part that matters least matters, if there's one.
fn least(parts: &[Part]) -> Option<u8> {
    parts.iter().map(|p| p.keep).filter(|k| *k != u8::MAX).min()
}

/// Takes out the part that matters least, the later of equals, and a separator beside it.
fn drop_least(parts: &mut Vec<Part>) {
    let Some((i, _)) = parts
        .iter()
        .enumerate()
        .filter(|(_, p)| p.keep != u8::MAX)
        .min_by_key(|(i, p)| (p.keep, usize::MAX - i))
    else {
        return;
    };
    parts.remove(i);
    // The separator before it, else the one after it.
    if i > 0 && parts[i - 1].keep == u8::MAX {
        parts.remove(i - 1);
    } else if i < parts.len() && parts[i].keep == u8::MAX {
        parts.remove(i);
    }
}

/// One line: what's on the left, and what's on the right pushed to the edge of `room`, two
/// columns apart at least. Whichever side has the part that matters least gives it up first.
fn line(mut left: Vec<Part>, mut right: Vec<Part>, room: usize) -> (String, usize) {
    let total = |l: &[Part], r: &[Part]| width(l) + if r.is_empty() { 0 } else { 2 + width(r) };
    while total(&left, &right) > room {
        // The left keeps its last part; the right can go entirely.
        let l = (real(&left) > 1).then(|| least(&left)).flatten();
        match (l, least(&right)) {
            (Some(l), Some(r)) if l < r => drop_least(&mut left),
            (_, Some(_)) => drop_least(&mut right),
            (Some(_), None) => drop_least(&mut left),
            (None, None) => {
                // The one part left is still too long: it's cut to fit, without its colours.
                if let Some(p) = left.iter_mut().find(|p| p.keep != u8::MAX) {
                    p.text = crate::chat::fit(&p.plain, room);
                    p.width = p.text.width();
                }
                break;
            }
        }
    }
    let used = width(&left);
    let mut text: String = left.iter().map(|p| p.text.as_str()).collect();
    if !right.is_empty() {
        let gap = room - used - width(&right);
        text.push_str(&" ".repeat(gap));
        text.extend(right.iter().map(|p| p.text.as_str()));
    }
    (text, used)
}

/// The status line's rows in a column `room` wide, after the provider's logo, which takes
/// `logo` columns: two lines when there's room for them, else one. The first part of the
/// first row is the chip, which opens the picker: its width comes back with the rows.
pub fn rows(
    facts: &Facts,
    room: usize,
    logo: usize,
    now: i64,
    offset_minutes: i64,
) -> (Vec<String>, usize) {
    let layout = if room >= FULL_MIN {
        Layout::Full
    } else {
        Layout::Compact
    };
    laid_out(
        facts,
        layout,
        room,
        logo,
        now,
        offset_minutes,
        &Ink::current(),
    )
}

fn laid_out(
    facts: &Facts,
    layout: Layout,
    room: usize,
    logo: usize,
    now: i64,
    offset_minutes: i64,
    ink: &Ink,
) -> (Vec<String>, usize) {
    let room = room.saturating_sub(logo);
    match layout {
        Layout::Full => full(facts, room, now, offset_minutes, ink),
        Layout::Compact => compact(facts, room, now, offset_minutes, ink),
    }
}

fn full(
    facts: &Facts,
    room: usize,
    now: i64,
    offset_minutes: i64,
    ink: &Ink,
) -> (Vec<String>, usize) {
    let Ink {
        secondary,
        tertiary,
        ..
    } = ink;
    let chip = chip(facts, true, ink);
    let chip_width = chip.width;
    let mut right = Vec::new();
    if let Some(ctx) = &facts.context {
        let mut text = format!("{secondary}ctx{RESET}");
        let mut plain = "ctx".to_string();
        if let Some(left) = ctx.left() {
            let (b, bp) = bar(left, ink);
            text.push_str(&format!(" {b} {}{left}%{RESET}", ink.tone(left)));
            plain.push_str(&format!(" {bp} {left}%"));
        }
        right.push(part(text, &plain, 8));
        let count = match ctx.max {
            Some(max) => format!("{}/{}", tokens(ctx.used), tokens(max)),
            None => tokens(ctx.used),
        };
        right.push(part(
            format!(" {tertiary}{count}{RESET}"),
            &format!(" {count}"),
            4,
        ));
        if let Some(c) = cache(ctx, now, ink) {
            right.push(sep(ink));
            right.push(c);
        }
    }
    let (first, used) = line(vec![chip], right, room);
    let chip_width = chip_width.min(used);

    let mut left = Vec::new();
    if !facts.machine.is_empty() {
        let m = &facts.machine;
        left.push(part(format!("{secondary}{m}{RESET}"), m, 2));
    }
    if let Some(b) = &facts.branch {
        left.push(part(format!("{MAGENTA}{b}{RESET}"), b, 3));
    }
    if let Some(pr) = &facts.pr {
        left.push(pr_part(pr, true, 5, ink));
    }
    let right = match &facts.notice {
        Some((text, color)) => vec![part(format!("{color}{text}{RESET}"), text, 6)],
        None => windows_part(&facts.windows, now, offset_minutes, ink)
            .into_iter()
            .collect(),
    };
    let mut rows = vec![first];
    if !left.is_empty() || !right.is_empty() {
        let (second, _) = line(joined(left, ink), right, room);
        rows.push(second);
    }
    (rows, chip_width)
}

fn compact(
    facts: &Facts,
    room: usize,
    now: i64,
    offset_minutes: i64,
    ink: &Ink,
) -> (Vec<String>, usize) {
    let chip = chip(facts, false, ink);
    let chip_width = chip.width;
    let mut parts = vec![chip];
    if let Some(left) = facts.context.as_ref().and_then(ContextWindow::left) {
        let plain = format!("ctx {left}%");
        parts.push(part(
            format!(
                "{}ctx{RESET} {}{left}%{RESET}",
                ink.secondary,
                ink.tone(left)
            ),
            &plain,
            8,
        ));
    }
    if let Some((text, color)) = &facts.notice {
        parts.push(part(format!("{color}{text}{RESET}"), text, 6));
    } else if !facts.windows.is_empty() {
        // `87%/68%`, or a used-up window's reset.
        let (text, plain) = if let Some(w) = facts
            .windows
            .iter()
            .find(|w| w.left == 0 && w.resets_at.is_some_and(|at| at > now))
        {
            let p = format!(
                "{} resets {}",
                w.label,
                crate::chat::when(w.resets_at.unwrap_or_default(), now, offset_minutes)
            );
            (format!("{}{p}{RESET}", ink.danger), p)
        } else {
            let text: Vec<String> = facts
                .windows
                .iter()
                .map(|w| format!("{}{}%{RESET}", ink.tone(w.left), w.left))
                .collect();
            let plain: Vec<String> = facts
                .windows
                .iter()
                .map(|w| format!("{}%", w.left))
                .collect();
            (
                text.join(&format!("{}/{RESET}", ink.tertiary)),
                plain.join("/"),
            )
        };
        parts.push(part(text, &plain, 7));
    }
    if let Some(pr) = &facts.pr {
        parts.push(pr_part(pr, false, 4, ink));
    }
    if !facts.machine.is_empty() {
        let m = &facts.machine;
        parts.push(part(format!("{}{m}{RESET}", ink.secondary), m, 2));
    }
    let (row, used) = line(joined(parts, ink), Vec::new(), room);
    (vec![row], chip_width.min(used))
}

/// Text without its escapes: SGR colours and OSC 8 links.
#[cfg(test)]
pub(crate) fn plain(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => {
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\x1b' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ansi() -> Ink {
        Ink::from(None)
    }

    fn now() -> i64 {
        t3client::inbox::parse_iso("2026-11-22T12:00:00.000Z").unwrap()
    }

    fn facts() -> Facts {
        Facts {
            model: "Opus 5.5".into(),
            effort: Some("Medium".into()),
            mode: ("full-access".into(), "Full access".into()),
            context: Some(ContextWindow {
                used: 100_136,
                max: Some(1_000_000),
                cache_until: Some(now() + 42 * 60_000 + 5_000),
            }),
            machine: "Mac mini".into(),
            branch: Some("fix/adopt-restored".into()),
            pr: Some(PullRequest {
                number: 31,
                url: "https://github.com/macnason/tr3x/pull/31".into(),
                state: "open".into(),
                draft: false,
                additions: Some(154),
                deletions: Some(8),
                checks: Some(Checks::Pending),
            }),
            windows: vec![
                Window {
                    label: "Session".into(),
                    left: 87,
                    resets_at: None,
                },
                Window {
                    label: "Weekly".into(),
                    left: 68,
                    resets_at: None,
                },
            ],
            notice: None,
        }
    }

    fn shown(facts: &Facts, layout: Layout, room: usize) -> Vec<String> {
        laid_out(facts, layout, room, 3, now(), 0, &ansi())
            .0
            .iter()
            .map(|r| plain(r))
            .collect()
    }

    #[test]
    fn full_shows_two_lines_with_the_right_side_at_the_edge() {
        let rows = shown(&facts(), Layout::Full, 100);
        assert_eq!(rows.len(), 2);
        assert!(rows[0].starts_with("Opus 5.5 · Medium · Full access"));
        assert!(
            rows[0].ends_with("ctx ▕█████████░▏ 90% 100k/1M · cache 42m"),
            "{}",
            rows[0]
        );
        assert!(rows[1].starts_with("Mac mini · fix/adopt-restored · PR #31 +154 −8 ◷ checks"));
        assert!(rows[1].ends_with("Session 87% · Weekly 68%"), "{}", rows[1]);
        for row in &rows {
            assert_eq!(row.width(), 97, "{row}");
        }
    }

    #[test]
    fn full_gives_way_from_the_least_that_matters() {
        let rows = shown(&facts(), Layout::Full, 72);
        assert!(!rows[0].contains("cache"), "{}", rows[0]);
        assert!(rows[0].contains("ctx ▕"), "{}", rows[0]);
        assert!(!rows[1].contains("Mac mini"), "{}", rows[1]);
        assert!(rows[1].contains("PR #31"), "{}", rows[1]);
        for row in &rows {
            assert!(row.width() <= 69, "{row}");
        }
    }

    #[test]
    fn compact_drops_parts_from_the_right() {
        let wide = shown(&facts(), Layout::Compact, 100);
        assert_eq!(
            wide,
            ["Opus 5.5 · Medium · ctx 90% · 87%/68% · PR #31 ◷ · Mac mini"]
        );
        let narrow = shown(&facts(), Layout::Compact, 40);
        assert_eq!(narrow, ["Opus 5.5 · Medium · ctx 90% · 87%/68%"]);
    }

    #[test]
    fn on_is_two_lines_when_they_fit_and_one_when_they_dont() {
        assert_eq!(rows(&facts(), 100, 3, now(), 0).0.len(), 2);
        assert_eq!(rows(&facts(), 50, 3, now(), 0).0.len(), 1);
    }

    #[test]
    fn running_low_turns_yellow_then_red() {
        assert_eq!(ansi().tone(21), GREEN);
        assert_eq!(ansi().tone(20), YELLOW);
        assert_eq!(ansi().tone(10), RED);
        let mut f = facts();
        f.context = Some(ContextWindow {
            used: 820_000,
            max: Some(1_000_000),
            cache_until: Some(now() + 6 * 60_000),
        });
        let (rows, _) = laid_out(&f, Layout::Full, 100, 0, now(), 0, &ansi());
        assert!(rows[0].contains(&format!("{YELLOW}18%")), "{:?}", rows[0]);
        assert!(
            rows[0].contains(&format!("{YELLOW}cache 6m")),
            "{:?}",
            rows[0]
        );
    }

    #[test]
    fn a_cold_cache_and_an_unknown_window_say_less() {
        let mut f = facts();
        f.context = Some(ContextWindow {
            used: 64_900,
            max: None,
            cache_until: Some(now() - 1),
        });
        let rows = shown(&f, Layout::Full, 100);
        assert!(rows[0].ends_with("ctx 65k"), "{}", rows[0]);
        assert!(!shown(&f, Layout::Compact, 100)[0].contains("ctx"));
    }

    #[test]
    fn a_notice_takes_the_usage_limits_place() {
        let mut f = facts();
        f.notice = Some(("Claude isn't ready.".into(), RED));
        let rows = shown(&f, Layout::Full, 100);
        assert!(rows[1].ends_with("Claude isn't ready."), "{}", rows[1]);
    }

    #[test]
    fn the_chip_width_covers_the_chip() {
        let (_, w) = laid_out(&facts(), Layout::Full, 100, 3, now(), 0, &ansi());
        assert_eq!(w, "Opus 5.5 · Medium · Full access".width());
        let (_, w) = laid_out(&facts(), Layout::Compact, 100, 3, now(), 0, &ansi());
        assert_eq!(w, "Opus 5.5 · Medium".width());
    }

    #[test]
    fn tokens_read_short() {
        assert_eq!(tokens(950), "950");
        assert_eq!(tokens(64_900), "65k");
        assert_eq!(tokens(9_500), "9.5k");
        assert_eq!(tokens(100_136), "100k");
        assert_eq!(tokens(1_000_000), "1M");
        assert_eq!(tokens(1_500_000), "1.5M");
    }

    #[test]
    fn context_comes_from_the_active_provider_threads_newest_turn() {
        let projection = json!({
            "thread": {"activeProviderThreadId": "pt-2"},
            "providerThreads": [
                {"id": "pt-1", "contextUsage": {"usedTokens": 5, "maxTokens": 10}},
                {"id": "pt-2", "contextUsage": {"usedTokens": 90_000, "maxTokens": 1_000_000}},
            ],
            "providerTurns": [
                {"providerThreadId": "pt-2", "tokenUsage": {"usedTokens": 88_000, "maxTokens": 1_000_000,
                    "updatedAt": "2026-11-22T11:00:00.000Z", "promptCacheExpiresAt": "2026-11-22T12:00:00.000Z"}},
                {"providerThreadId": "pt-2", "tokenUsage": {"usedTokens": 100_136, "maxTokens": 1_000_000,
                    "updatedAt": "2026-11-22T11:30:00.000Z", "promptCacheExpiresAt": "2026-11-22T12:30:00.000Z"}},
                {"providerThreadId": "pt-1", "tokenUsage": {"usedTokens": 7, "maxTokens": 10,
                    "updatedAt": "2026-11-22T11:59:00.000Z"}},
            ],
        });
        let ctx = context(projection.as_object().unwrap()).unwrap();
        assert_eq!(ctx.used, 100_136);
        assert_eq!(ctx.max, Some(1_000_000));
        assert_eq!(ctx.left(), Some(90));
        assert_eq!(
            ctx.cache_until,
            t3client::inbox::parse_iso("2026-11-22T12:30:00.000Z")
        );
    }

    #[test]
    fn context_falls_back_to_the_provider_threads_count() {
        let projection = json!({
            "thread": {"activeProviderThreadId": "pt-1"},
            "providerThreads": [{"id": "pt-1", "contextUsage": {"usedTokens": 122_032, "maxTokens": 1_000_000}}],
            "providerTurns": [],
        });
        let ctx = context(projection.as_object().unwrap()).unwrap();
        assert_eq!((ctx.used, ctx.cache_until), (122_032, None));
        let none = json!({"thread": {}, "providerThreads": [], "providerTurns": []});
        assert_eq!(context(none.as_object().unwrap()), None);
    }

    #[test]
    fn the_newest_open_pull_request_shows() {
        let thread = json!({"pullRequests": [
            {"number": 112, "url": "u112", "linkedAt": "2026-11-21T18:17:23.006Z",
             "snapshot": {"state": "merged", "checksState": "passing"}},
            {"number": 31, "url": "u31", "linkedAt": "2026-11-21T14:32:38.124Z",
             "snapshot": {"state": "open", "additions": 114, "deletions": 0, "checksState": "failing"}},
        ]});
        let pr = pull_request(&thread).unwrap();
        assert_eq!(
            (pr.number, pr.checks, pr.additions),
            (31, Some(Checks::Failing), Some(114))
        );
        let merged =
            json!({"pullRequests": [{"number": 112, "url": "u", "snapshot": {"state": "merged"}}]});
        let mut f = facts();
        f.pr = pull_request(&merged);
        let rows = shown(&f, Layout::Full, 100);
        assert!(rows[1].contains("PR #112 merged"), "{}", rows[1]);
        assert_eq!(pull_request(&json!({})), None);
    }

    #[test]
    fn narrow_columns_cut_the_last_part_to_fit() {
        for layout in [None, Some(Layout::Full), Some(Layout::Compact)] {
            for room in [4, 10, 16, 21, 24, 27, 30, 40] {
                let (rows, chip_width) = match layout {
                    Some(layout) => laid_out(&facts(), layout, room, 3, now(), 0, &ansi()),
                    None => rows(&facts(), room, 3, now(), 0),
                };
                for row in &rows {
                    let row = plain(row);
                    assert!(row.width() <= room - 3, "{layout:?} at {room}: {row}");
                }
                assert!(chip_width <= room - 3, "{layout:?} at {room}");
            }
        }
        assert_eq!(
            shown(&facts(), Layout::Full, 30),
            ["Opus 5.5 · Medium · Full a…", "PR #31 +154 −8 ◷ checks"]
        );
    }

    #[test]
    fn the_setting_parses() {
        assert_eq!(parse("On"), Some(true));
        assert_eq!(parse("off"), Some(false));
        assert_eq!(parse("big"), None);
        // What earlier builds wrote, or a habit typed: a layout is on, the chip is off.
        for on in ["auto", "full", "compact"] {
            assert_eq!(parse(on), Some(true), "{on}");
        }
        for off in ["chip", "default"] {
            assert_eq!(parse(off), Some(false), "{off}");
        }
        // Opt-in: nothing changes until someone asks for more.
        assert!(!set_in(None));
        assert!(!set_in(json!({}).as_object()));
        assert!(!set_in(json!({"statusLine": "big"}).as_object()));
        assert!(set_in(json!({"statusLine": "auto"}).as_object()));
    }

    #[test]
    fn a_themes_tokens_colour_it_and_ansi_stands_in_for_the_rest() {
        let theme = Ink::from(Some(Tokens {
            secondary: Some([118, 136, 148]),
            tertiary: Some([91, 107, 119]),
            success: Some([82, 199, 154]),
            warning: Some([212, 178, 94]),
            danger: Some([226, 106, 106]),
            accent: None,
        }));
        assert_eq!(theme.tone(21), "\x1b[38;2;82;199;154m");
        assert_eq!(theme.tone(20), "\x1b[38;2;212;178;94m");
        assert_eq!(theme.tone(10), "\x1b[38;2;226;106;106m");
        // A token the theme doesn't have falls back alone.
        assert_eq!(theme.accent, CYAN);
        assert_eq!(ansi(), Ink::from(Some(Tokens::default())));
        let (rows, _) = laid_out(&facts(), Layout::Full, 100, 0, now(), 0, &theme);
        assert!(
            rows[0].contains("\x1b[38;2;118;136;148mctx"),
            "{:?}",
            rows[0]
        );
        assert!(
            rows[0].contains("\x1b[38;2;82;199;154m90%"),
            "{:?}",
            rows[0]
        );
        assert!(
            rows[0].contains("\x1b[38;2;91;107;119m100k/1M"),
            "{:?}",
            rows[0]
        );
        let (text, _) = low_context(
            Some(&ContextWindow {
                used: 920_000,
                max: Some(1_000_000),
                cache_until: None,
            }),
            &theme,
        )
        .unwrap();
        assert!(text.contains("\x1b[38;2;226;106;106m8%"), "{text:?}");
    }

    #[test]
    fn the_chip_warns_only_when_the_context_runs_low() {
        let at = |left: u64| ContextWindow {
            used: 1_000_000 - left * 10_000,
            max: Some(1_000_000),
            cache_until: None,
        };
        assert_eq!(low_context(Some(&at(90)), &ansi()), None);
        assert_eq!(low_context(Some(&at(21)), &ansi()), None);
        assert_eq!(low_context(None, &ansi()), None);
        let unknown = ContextWindow {
            used: 5,
            max: None,
            cache_until: None,
        };
        assert_eq!(low_context(Some(&unknown), &ansi()), None);
        let (text, width) = low_context(Some(&at(15)), &ansi()).unwrap();
        assert_eq!((plain(&text).as_str(), width), ("ctx 15%", 7));
        assert!(text.contains(&format!("{YELLOW}15%")), "{text:?}");
        let (text, _) = low_context(Some(&at(20)), &ansi()).unwrap();
        assert!(text.contains(&format!("{YELLOW}20%")), "{text:?}");
        let (text, _) = low_context(Some(&at(8)), &ansi()).unwrap();
        assert!(text.contains(&format!("{RED}8%")), "{text:?}");
    }
}
