//! The reply pane under the conversation: one input, and the thread's pending approval or
//! question when there is one. Its header, set with OSC 2, names the model, effort and access
//! the next message goes out with.
//!
//! Enter sends. To a running turn it steers or queues, as T3's Follow-up behavior says, and
//! ⌘Enter does the other, as in T3. Alt+Enter or Ctrl+J adds a line. A line that
//! starts with `/` is a command: /settle, /snooze 2h, /wake, /pin, /unpin, /archive, /stop,
//! /model NAME, /effort LEVEL, /open, /width comfortable|wide|full|t3. `/` first opens a menu of
//! those, the provider's commands and skills (`slash`).
//!
//! A pasted picture, or a dropped one, rides on the next message (`paste`). It shows as a chip
//! over the input; its ✕, or Backspace at the start of the input, takes it off.

use std::io::Write;
use std::sync::Arc;

use anyhow::Result;
use crossterm::event::{
    Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::collections::HashMap;
use t3client::env::{Env, IfBusy, WatchEvent};
use t3client::models::{self, Choice};
use t3client::projection::ThreadState;
use tokio::sync::mpsc;
use unicode_width::UnicodeWidthChar;

use crate::chat::{BLUE, BOLD, DIM, RED, RESET, YELLOW, fit};
use crate::icons::{Icon, Painter};
use crate::paste::{self, Got, Image};
use crate::pick::Field;
use crate::picker::{self, Chip, Settings};
use crate::picture::{self, Tray};
use crate::slash::{self, Slash};
use crate::t3::{self, Lifecycle};
use crate::width::Follow;

fn out(text: &str) {
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(text.as_bytes());
    let _ = stdout.flush();
}

#[derive(Default)]
pub(crate) struct Editor {
    pub(crate) text: Vec<char>,
    pub(crate) cursor: usize,
}

impl Editor {
    pub(crate) fn insert(&mut self, s: &str) {
        let s = s.replace('\t', "    ");
        for c in s.chars().filter(|c| *c == '\n' || !c.is_control()) {
            self.text.insert(self.cursor, c);
            self.cursor += 1;
        }
    }
    pub(crate) fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.text.remove(self.cursor);
        }
    }
    pub(crate) fn delete(&mut self) {
        if self.cursor < self.text.len() {
            self.text.remove(self.cursor);
        }
    }
    pub(crate) fn delete_word(&mut self) {
        let start = self.word_left();
        while self.cursor > start {
            self.backspace();
        }
    }
    pub(crate) fn delete_to_line_start(&mut self) {
        let start = self.line_start();
        while self.cursor > start {
            self.backspace();
        }
    }
    /// Where the word before the cursor starts, past any spaces.
    pub(crate) fn word_left(&self) -> usize {
        let mut at = self.cursor;
        while at > 0 && self.text[at - 1].is_whitespace() {
            at -= 1;
        }
        while at > 0 && !self.text[at - 1].is_whitespace() {
            at -= 1;
        }
        at
    }
    /// Where the word after the cursor ends, past any spaces.
    pub(crate) fn word_right(&self) -> usize {
        let mut at = self.cursor;
        while at < self.text.len() && self.text[at].is_whitespace() {
            at += 1;
        }
        while at < self.text.len() && !self.text[at].is_whitespace() {
            at += 1;
        }
        at
    }
    pub(crate) fn line_start(&self) -> usize {
        self.text[..self.cursor]
            .iter()
            .rposition(|c| *c == '\n')
            .map_or(0, |i| i + 1)
    }
    pub(crate) fn line_end(&self) -> usize {
        self.text[self.cursor..]
            .iter()
            .position(|c| *c == '\n')
            .map_or(self.text.len(), |i| self.cursor + i)
    }
    pub(crate) fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.text).into_iter().collect()
    }
    pub(crate) fn string(&self) -> String {
        self.text.iter().collect()
    }
}

/// The `/` commands. Anything else starting with `/`, such as a path, is a message.
const COMMANDS: &[&str] = &[
    "settle", "unsettle", "snooze", "wake", "pin", "unpin", "archive", "stop", "open", "model",
    "effort", "mode", "steer", "width",
];

/// What a click or a focused chip does.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    /// Opens the model picker on a field.
    Field(Field),
    /// Answers the pending approval.
    Decide(&'static str),
    /// Picks the pending question's option, by index.
    Choose(usize),
    /// Takes an attached picture off, by index.
    Remove(usize),
}

/// What came back from work started in the background.
enum Done {
    Note(String, bool),
    /// A message that failed to send, back into the input if it's empty.
    Restore(String, Vec<Image>),
    /// A picture made ready to send.
    Picture(picture::Ready),
    Config(Value),
    Logos(HashMap<String, Icon>),
    /// A link to open the thread with, on the device it's clicked on.
    Link(String),
    /// A resize finished, or gave up.
    Resized,
}

/// The share of the window the conversation has over Reply, as sync and `new` split it.
const SPLIT: f64 = 0.86;
/// Reply grows by at most this many input lines, and to at most this share of the window.
const GROW_LINES: usize = 7;
const GROW_SHARE: f64 = 0.35;
/// How long the line count has to hold before Reply resizes. Each resize reprints the
/// conversation, so typing through a wrap doesn't resize twice.
const GROW_WAIT: std::time::Duration = std::time::Duration::from_millis(250);

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

/// Reply growing with its text: one row per input line past the first, by moving the split
/// between the conversation and Reply (`session.resize_pane`). The split is the session's, so
/// every device looking at it sees the same.
struct Grow {
    session: String,
    block: String,
    /// The extra lines the split was last asked for, and the ones the text wants now.
    asked: usize,
    wants: usize,
    /// When `wants` changed; Reply resizes once it has held for `GROW_WAIT`.
    since: Option<tokio::time::Instant>,
    /// Reply's rows at the starting split, the least it has.
    base: usize,
    busy: bool,
}

/// Moves the split so Reply has `base` rows and `extra` more, within `GROW_SHARE` of the
/// window, or back to the starting split for none. None when it couldn't.
async fn resize(session: String, block: String, base: usize, extra: usize) -> Option<()> {
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
    let next = if extra == 0 {
        SPLIT
    } else {
        // Each pane loses a few rows to its header. Work that out from the split as it is:
        // reply = (1 - ratio) * all - header, with all = chat + reply + 2 * header.
        let share = 1.0 - ratio;
        let header =
            ((share * (chat_rows + reply_rows) - reply_rows) / (1.0 - 2.0 * share)).max(0.0);
        let all = chat_rows + reply_rows + 2.0 * header;
        let most = ((chat_rows + reply_rows) * GROW_SHARE).floor();
        let want = (base as f64 + extra as f64).min(most).max(base as f64);
        (1.0 - (want + header + 0.5) / all).clamp(1.0 - GROW_SHARE - 0.1, SPLIT)
    };
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

struct Composer {
    thread_id: String,
    client: Arc<Env>,
    /// What to say while the thread's machine is out of reach.
    offline: Option<String>,
    state: Option<ThreadState>,
    config: Option<Value>,
    editor: Editor,
    /// Answers to the pending question so far, by question id.
    answers: serde_json::Map<String, Value>,
    /// A line under the input: what just happened, or what went wrong, and when it goes.
    note: Option<(String, bool)>,
    note_until: Option<tokio::time::Instant>,
    /// The thread's link, under the input until the next key.
    link: Option<String>,
    cols: usize,
    rows: usize,
    title: String,
    tx: mpsc::UnboundedSender<Done>,
    logos: HashMap<String, Icon>,
    painter: Painter,
    /// Each chip's and button's screen row and columns, for clicks.
    spans: Vec<(usize, (usize, usize), Hit)>,
    /// The approval or question button Tab moved to, by index.
    focus: Option<usize>,
    /// Started again under the provider's name once already; don't loop if that failed.
    renamed: bool,
    /// The chat width setting: the input sits in the conversation's column.
    chat_width: Follow,
    slash: Slash,
    /// None outside a Rex window, where there is no split to move.
    grow: Option<Grow>,
    /// The pictures the next message takes.
    images: Tray,
    clipboard: paste::Reader,
}

impl Composer {
    fn pending(&self) -> Option<(Value, Value)> {
        crate::ask::pending(self.state.as_ref()?)
    }

    /// The buttons that answer what's pending: Approve, Always and Decline for an approval, or
    /// the question's options, numbered as the number keys pick them.
    fn buttons(&self) -> Vec<(String, Hit)> {
        let Some((request, item)) = self.pending() else {
            return Vec::new();
        };
        if !crate::ask::is_question(&request) {
            return vec![
                ("Approve".into(), Hit::Decide("accept")),
                ("Always".into(), Hit::Decide("acceptForSession")),
                ("Decline".into(), Hit::Decide("decline")),
            ];
        }
        let Some(q) = self.question(&item) else {
            return Vec::new();
        };
        q["options"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(i, o)| {
                let label = crate::ask::clean(o["label"].as_str().unwrap_or(""));
                (format!("{} {label}", i + 1), Hit::Choose(i))
            })
            .collect()
    }

    fn press(&mut self, hit: Hit) {
        self.focus = None;
        match hit {
            Hit::Field(field) => self.pick(field),
            Hit::Remove(i) => self.images.remove(i),
            Hit::Decide(decision) => self.respond(decision),
            Hit::Choose(i) => {
                if let Some((request, item)) = self.pending()
                    && let Some(q) = self.question(&item).cloned()
                    && let Some(option) = q["options"].get(i)
                {
                    let value = option["value"]
                        .as_str()
                        .or(option["label"].as_str())
                        .unwrap_or_default()
                        .to_string();
                    self.answer(&request, &item, &q, value);
                }
            }
        }
    }

    fn question<'a>(&self, item: &'a Value) -> Option<&'a Value> {
        item["questions"].as_array()?.iter().find(|q| {
            !self
                .answers
                .contains_key(q["id"].as_str().unwrap_or_default())
        })
    }

    /// The one chip under the input, as T3's composer shows the model: the provider's logo,
    /// then model, effort and access. It opens the picker.
    fn chips(&self, width: usize) -> Vec<(Chip, Field)> {
        let (Some(state), Some(config)) = (self.state.as_ref(), self.config.as_ref()) else {
            return Vec::new();
        };
        let settings = Settings::of_thread(config, state.thread());
        let icon = self.logos.get(&settings.instance).cloned();
        // The logo and the ` ▾` take five columns.
        let room = width.saturating_sub(if icon.is_some() { 5 } else { 2 });
        vec![(
            Chip {
                icon,
                label: settings.summary(config, room),
            },
            Field::Model,
        )]
    }

    /// Opens the model picker over the window, on `field`.
    fn pick(&mut self, field: Field) {
        let Ok(session) = std::env::var("REX_SESSION") else {
            return self.note("Open this in Rex to choose a model", true);
        };
        let thread = format!("{}/{}", self.client.id(), self.thread_id);
        self.spawn(async move {
            crate::pick::float(&session, &thread, field).await?;
            Ok(String::new())
        });
    }

    fn draw(&mut self) {
        let title = "Reply".to_string();
        let mut text = String::from("\x1b[?2026h");
        if title != self.title {
            text.push_str(&format!(
                "\x1b]2;{}\x07",
                title.replace(|c: char| c.is_control(), "")
            ));
            self.title = title;
        }
        text.push_str("\x1b[H\x1b[2J");
        text.push_str(Painter::clear());
        self.slash.sync(
            &self.editor,
            self.config.as_ref(),
            self.state.as_ref().map(|s| s.thread()),
        );
        // The conversation's column, less the prompt's two.
        let (left, column) = self.chat_width.width.column(self.cols);
        let width = column.saturating_sub(2).max(10);
        // The chips take the bottom row, and a blank row above them when there is room.
        let chips: Vec<(Chip, Field)> = self.chips(column);
        let reserved = match (chips.is_empty(), self.rows) {
            (true, _) => 0,
            (false, r) if r >= 5 => 2,
            _ => 1,
        };
        let room = self.rows.saturating_sub(reserved).max(1);
        let mut rows: Vec<String> = Vec::new();
        // The buttons on each row, by the row's index in `rows`.
        let mut hits: Vec<(usize, (usize, usize), Hit)> = Vec::new();
        if let Some((request, item)) = self.pending() {
            let ask = if crate::ask::is_question(&request) {
                self.question(&item).map(|q| {
                    format!(
                        "{YELLOW}{BOLD}◆{RESET} {BOLD}{}{RESET}",
                        fit(&crate::ask::question_text(q), width - 2)
                    )
                })
            } else {
                let state = self.state.as_ref().expect("a pending request has a thread");
                let (verb, what) = crate::ask::approval(state, &request, &item);
                Some(format!(
                    "{YELLOW}{BOLD}◆{RESET} {BOLD}{verb}{RESET} {}?",
                    fit(&what, width.saturating_sub(8))
                ))
            };
            if let Some(ask) = ask {
                rows.push(ask);
                let buttons = self.buttons();
                if self.focus.is_some_and(|f| f >= buttons.len()) {
                    self.focus = None;
                }
                let labels: Vec<String> = buttons.iter().map(|(l, _)| l.clone()).collect();
                let primary = !crate::ask::is_question(&request);
                for (line, spans) in picker::buttons(&labels, self.focus, primary, left, width) {
                    for (cols, i) in spans {
                        hits.push((rows.len(), cols, buttons[i].1));
                    }
                    rows.push(line);
                }
            }
        }
        if !self.images.is_empty() {
            let (line, spans, _) = self.images.row(&mut self.painter, left, width + 2);
            for (i, x) in spans.into_iter().enumerate() {
                hits.push((rows.len(), (x, x), Hit::Remove(i)));
            }
            rows.push(line);
        }
        // The input, wrapped, with the cursor where it is.
        let prompt = format!("{BLUE}{BOLD}›{RESET} ");
        let (lines, cursor) = wrap(&self.editor.text, self.editor.cursor, width);
        if let Some(grow) = self.grow.as_mut() {
            let wants = (lines.len() - 1).min(GROW_LINES);
            if wants != grow.wants {
                grow.wants = wants;
                grow.since = Some(tokio::time::Instant::now());
            }
        }
        let first_input = rows.len();
        for (i, line) in lines.iter().enumerate() {
            let lead = if i == 0 { prompt.clone() } else { "  ".into() };
            rows.push(format!("{lead}{line}"));
        }
        if let Some((note, error)) = &self.note {
            let color = if *error { RED } else { DIM };
            rows.push(format!("{color}{}{RESET}", fit(note, width)));
        } else if let Some(url) = &self.link {
            rows.push(format!(
                "{DIM}{}{RESET}",
                crate::link::osc8(url, "Open in T3 ↗")
            ));
        }
        // Keep the cursor's row on screen in a pane shorter than the text.
        let cursor_row = first_input + cursor.0;
        let start = (cursor_row + 1)
            .saturating_sub(room)
            .min(rows.len().saturating_sub(room.min(rows.len())));
        let shown: Vec<&String> = rows.iter().skip(start).take(room).collect();
        for (i, row) in shown.iter().enumerate() {
            text.push_str(&format!("\x1b[{};{}H{row}", i + 1, left + 1));
        }
        self.spans = hits
            .into_iter()
            .filter(|(row, _, _)| (start..start + room).contains(row))
            .map(|(row, cols, hit)| (row - start + 1, cols, hit))
            .collect();
        if !chips.is_empty() {
            let only: Vec<Chip> = chips
                .iter()
                .map(|(c, _)| Chip {
                    icon: c.icon.clone(),
                    label: c.label.clone(),
                })
                .collect();
            let (line, spans) = picker::chips(&mut self.painter, &only, None, None, left);
            let bottom = self.rows;
            self.spans.extend(
                spans
                    .into_iter()
                    .zip(chips.iter().map(|(_, f)| Hit::Field(*f)))
                    .map(|(cols, hit)| (bottom, cols, hit)),
            );
            text.push_str(&format!("\x1b[{};{}H{line}", self.rows, left + 1));
        }
        let (r, c) = (cursor_row.saturating_sub(start) + 1, left + 3 + cursor.1);
        let input_top = (first_input + 1).saturating_sub(start).max(1);
        let input_bottom = (first_input + lines.len()).saturating_sub(start).min(room);
        text.push_str(&self.slash.draw(
            &mut self.painter,
            (input_top, input_bottom),
            self.rows,
            left,
            column,
        ));
        let hint_room = width.saturating_sub(cursor.1 + 1);
        if let Some(hint) = self.slash.hint().filter(|_| hint_room >= 4) {
            let hint = fit(hint, hint_room);
            text.push_str(&format!("\x1b[{r};{c}H{DIM}{hint}{RESET}"));
        }
        // The cursor hides while a button has focus, as it does on a chip in a new thread.
        let cursor = if self.focus.is_some() { "?25l" } else { "?25h" };
        text.push_str(&format!("\x1b[{r};{c}H\x1b[{cursor}\x1b[?2026l"));
        out(&text);
    }

    /// Pictures to send with the next message, made ready on the way.
    fn attach(&mut self, images: Vec<Image>) {
        let tx = self.tx.clone();
        self.images.add(images, move |ready| {
            let _ = tx.send(Done::Picture(ready));
        });
    }

    /// What a paste came to: pictures, or text to type.
    fn pasted(&mut self, got: Got) {
        match got {
            Got::Offer(request) => out(&request),
            Got::Text(text) => match paste::dropped(&text) {
                Some(got) => self.pasted(got),
                None => self
                    .editor
                    .insert(&text.replace("\r\n", "\n").replace('\r', "\n")),
            },
            Got::Images(images) => self.attach(images),
            Got::Nothing(Some(why)) => self.note(why, true),
            Got::Nothing(None) => {}
        }
    }

    fn note(&mut self, text: impl Into<String>, error: bool) {
        self.note = Some((text.into(), error));
        // A confirmation fades after a few seconds. An error stays until the next key.
        self.note_until =
            (!error).then(|| tokio::time::Instant::now() + std::time::Duration::from_secs(4));
    }

    fn spawn<F>(&self, work: F)
    where
        F: std::future::Future<Output = Result<String>> + Send + 'static,
    {
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let done = match work.await {
                Ok(note) => Done::Note(note, false),
                Err(e) => Done::Note(format!("{e:#}"), true),
            };
            let _ = tx.send(done);
        });
    }

    fn respond(&mut self, decision: &'static str) {
        let Some((request, _)) = self.pending() else {
            return;
        };
        let (client, thread, id) = (
            self.client.clone(),
            self.thread_id.clone(),
            request["id"].as_str().unwrap_or_default().to_string(),
        );
        self.spawn(async move {
            client.respond(&thread, &id, decision).await?;
            Ok(match decision {
                "decline" => "Declined",
                _ => "Accepted",
            }
            .into())
        });
    }

    /// Records the answer to question `q`, and sends every answer once each question has one.
    fn answer(&mut self, request: &Value, item: &Value, q: &Value, answer: String) {
        let answer = if q["multiSelect"] == true {
            json!([answer])
        } else {
            json!(answer)
        };
        self.answers
            .insert(q["id"].as_str().unwrap_or_default().to_string(), answer);
        let total = item["questions"].as_array().map_or(0, Vec::len);
        if self.answers.len() >= total {
            let answers = Value::Object(std::mem::take(&mut self.answers));
            let (client, thread, id) = (
                self.client.clone(),
                self.thread_id.clone(),
                request["id"].as_str().unwrap_or_default().to_string(),
            );
            self.spawn(async move {
                client.answer(&thread, &id, answers).await?;
                Ok("Answered".into())
            });
        }
    }

    /// Sends what's typed, or answers what's pending with it. `other` is ⌘Enter: the follow-up
    /// T3's setting doesn't pick.
    fn submit(&mut self, other: bool) {
        let text = self.editor.string();
        let trimmed = text.trim();
        if trimmed.is_empty() && self.images.is_empty() {
            return;
        }
        if self.images.preparing() {
            return self.note("Getting the picture ready…", false);
        }
        // Out of reach: nothing goes, and the text stays for when the machine is back.
        if let Some(offline) = self.offline.clone() {
            return self.note(offline, true);
        }
        // A pending question takes the input as its answer: a number picks an option.
        if !trimmed.is_empty()
            && let Some((request, item)) = self.pending()
            && request["kind"] == "user_input"
            && let Some(q) = self.question(&item).cloned()
        {
            let options = q["options"].as_array().cloned().unwrap_or_default();
            let answer = match trimmed.parse::<usize>() {
                Ok(n) if (1..=options.len()).contains(&n) => options[n - 1]["value"]
                    .as_str()
                    .or(options[n - 1]["label"].as_str())
                    .unwrap_or_default()
                    .to_string(),
                _ => trimmed.to_string(),
            };
            self.editor.take();
            self.answer(&request, &item, &q, answer);
            return;
        }
        // An approval takes a, s or d and Enter. A reply that starts with one of those letters
        // stays a reply.
        if let Some((request, _)) = self.pending()
            && request["kind"] != "user_input"
            && let Some(decision) = match trimmed {
                "a" => Some("accept"),
                "s" => Some("acceptForSession"),
                "d" => Some("decline"),
                _ => None,
            }
        {
            self.editor.take();
            self.respond(decision);
            return;
        }
        if let Some(command) = trimmed.strip_prefix('/')
            && COMMANDS.contains(&command.split_whitespace().next().unwrap_or(""))
            && !self.slash.is_theirs(trimmed)
        {
            let command = command.to_string();
            if let Some(text) = command.strip_prefix("steer") {
                let text = text.trim().to_string();
                self.editor.take();
                self.editor.insert(&text);
                self.send(IfBusy::Steer);
                return;
            }
            self.editor.take();
            self.command(&command);
            return;
        }
        let steer = crate::width::follow_up_steers() != other;
        self.send(if steer { IfBusy::Steer } else { IfBusy::Queue });
    }

    /// Sends the input. Queue waits for the running turn; Steer goes to it now. A send that
    /// fails puts the text back.
    fn send(&mut self, mode: IfBusy) {
        let Some(state) = self.state.clone() else {
            return;
        };
        let message = self.editor.take();
        let images = self.images.take();
        let uploads: Vec<Value> = images.iter().map(Image::upload).collect();
        let client = self.client.clone();
        let busy = state.active_run().is_some();
        let steer = matches!(mode, IfBusy::Steer);
        let tx = self.tx.clone();
        tokio::spawn(async move {
            // A sent message shows in the conversation, which comes back to the bottom for it. A
            // queued one doesn't until its turn, so only that gets a note.
            let done = match client.send_message(&state, &message, mode, &uploads).await {
                Ok(_) if busy && !steer => Done::Note("Queued".into(), false),
                Ok(_) => return,
                Err(e) => {
                    let _ = tx.send(Done::Restore(message, images));
                    Done::Note(format!("{e:#}"), true)
                }
            };
            let _ = tx.send(done);
        });
    }

    fn command(&mut self, line: &str) {
        let (name, arg) = line
            .split_once(' ')
            .map_or((line, ""), |(a, b)| (a, b.trim()));
        let (client, thread) = (self.client.clone(), self.thread_id.clone());
        let lifecycle = |action: Lifecycle, note: &'static str| {
            let (client, thread) = (client.clone(), thread.clone());
            async move {
                client.dispatch(t3::command(&thread, &action)).await?;
                Ok::<String, anyhow::Error>(note.to_string())
            }
        };
        match name {
            "settle" => self.spawn(lifecycle(Lifecycle::Settle, "Settled")),
            "unsettle" => self.spawn(lifecycle(Lifecycle::Unsettle, "Back in the inbox")),
            "wake" => self.spawn(lifecycle(Lifecycle::Wake, "Awake")),
            "pin" => self.spawn(lifecycle(Lifecycle::Pin, "Pinned")),
            "unpin" => self.spawn(lifecycle(Lifecycle::Unpin, "Unpinned")),
            "archive" => self.spawn(lifecycle(Lifecycle::Archive, "Archived")),
            "snooze" => match t3::snooze_ms(if arg.is_empty() { "1h" } else { arg }) {
                Some(ms) => self.spawn(lifecycle(Lifecycle::Snooze(ms), "Snoozed")),
                None => self.note(
                    format!("Can't read {arg:?} as a duration, such as 2h"),
                    true,
                ),
            },
            "stop" => {
                let run = self
                    .state
                    .as_ref()
                    .and_then(|s| s.active_run())
                    .and_then(|r| r["id"].as_str())
                    .map(str::to_string);
                match run {
                    Some(run) => self.spawn(async move {
                        client
                            .dispatch(json!({"type": "run.interrupt", "threadId": thread, "runId": run, "reason": "Interrupted from Rex"}))
                            .await?;
                        Ok("Stopped".into())
                    }),
                    None => self.note("Nothing is running", false),
                }
            }
            "width" => match crate::width::apply(arg) {
                Ok(note) => {
                    self.chat_width.width = crate::width::current().0;
                    self.note(note, false);
                }
                Err(e) => self.note(format!("{e:#}"), true),
            },
            // Opening goes to the Mac this runs on. With a laptop or phone attached, which
            // may be where this was typed, the note is a link to click there instead.
            "open" => {
                let link =
                    crate::link::thread(self.config.as_ref(), self.client.id(), &self.thread_id);
                let key = format!("{}/{}", self.client.id(), self.thread_id);
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let done = if !crate::link::only_here().await {
                        Done::Link(link)
                    } else {
                        match crate::install::open_in_t3(&key).await {
                            Ok(()) => Done::Note("Opened in T3 Code".into(), false),
                            Err(e) => Done::Note(format!("{e:#}"), true),
                        }
                    };
                    let _ = tx.send(done);
                });
            }
            "model" | "effort" | "mode" if arg.is_empty() => self.pick(match name {
                "model" => Field::Model,
                "effort" => Field::Effort,
                _ => Field::Access,
            }),
            "model" | "effort" | "mode" => {
                let (Some(state), Some(config)) = (self.state.clone(), self.config.clone()) else {
                    return;
                };
                let mut choice = Choice::default();
                match name {
                    "model" => choice.model = Some(arg.to_string()),
                    "effort" => choice.effort = Some(arg.to_string()),
                    _ => {
                        choice.runtime_mode = Some(match arg {
                            "ask" | "supervised" | "approval" => "approval-required".to_string(),
                            "edits" => "auto-accept-edits".to_string(),
                            "full" => "full-access".to_string(),
                            other => other.to_string(),
                        })
                    }
                }
                match models::plan(&config, state.thread(), &choice) {
                    Ok(plan) => self.spawn(async move {
                        client.apply_settings(&state, &plan).await?;
                        Ok("Updated".into())
                    }),
                    Err(e) => self.note(format!("{e:#}"), true),
                }
            }
            _ => self.note(format!("No command /{name}"), true),
        }
    }

    fn key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        self.note = None;
        if self.focus.is_none() {
            match self.slash.key(&mut self.editor, key) {
                slash::Key::Pass => {}
                slash::Key::Done => return,
                slash::Key::Send => return self.submit(false),
            }
        }
        self.link = None;
        // Tab moves through what's pending's buttons, then on to the model picker. On a
        // button, the arrows move and Enter presses it; anything else goes back to typing.
        let buttons = self.buttons();
        match (key.code, self.focus) {
            (KeyCode::Tab, f) if !buttons.is_empty() => {
                self.focus = match f {
                    None => Some(0),
                    Some(i) if i + 1 < buttons.len() => Some(i + 1),
                    Some(_) => {
                        self.focus = None;
                        self.pick(Field::Model);
                        return;
                    }
                };
                return;
            }
            (KeyCode::BackTab, f) if !buttons.is_empty() => {
                self.focus = match f {
                    None => Some(buttons.len() - 1),
                    Some(0) => None,
                    Some(i) => Some(i - 1),
                };
                return;
            }
            (KeyCode::Left, Some(i)) => {
                self.focus = Some(i.saturating_sub(1));
                return;
            }
            (KeyCode::Right, Some(i)) => {
                self.focus = Some((i + 1).min(buttons.len().saturating_sub(1)));
                return;
            }
            (KeyCode::Enter | KeyCode::Char(' '), Some(i)) => {
                if let Some((_, hit)) = buttons.get(i) {
                    self.press(*hit);
                }
                return;
            }
            (KeyCode::Esc | KeyCode::Up, Some(_)) => {
                self.focus = None;
                return;
            }
            (_, Some(_)) => self.focus = None,
            _ => {}
        }
        match key.code {
            KeyCode::Enter
                if !key
                    .modifiers
                    .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT) =>
            {
                self.submit(key.modifiers.contains(KeyModifiers::SUPER))
            }
            KeyCode::Char('c') if ctrl => {
                self.editor.take();
                self.images.clear();
            }
            KeyCode::Backspace if self.editor.cursor == 0 && !self.images.is_empty() => {
                self.images.pop();
            }
            KeyCode::Char('x') if ctrl => self.command("stop"),
            KeyCode::Tab | KeyCode::BackTab => self.pick(Field::Model),
            _ => {
                edit(&mut self.editor, key);
            }
        }
    }
}

/// The editing keys every input shares: Alt+Enter, Shift+Enter or Ctrl+J for a new line,
/// Ctrl+U, Ctrl+W, Ctrl+A and Ctrl+E as in a shell, the Mac's Cmd and Option with the arrows
/// and Delete, and the arrows. Cmd arrives as SUPER, with the kitty keyboard protocol on
/// (`scheme::keys`). Returns whether the key was one of them.
pub(crate) fn edit(editor: &mut Editor, key: KeyEvent) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let cmd = key.modifiers.contains(KeyModifiers::SUPER);
    match key.code {
        KeyCode::Enter if alt || key.modifiers.contains(KeyModifiers::SHIFT) => editor.insert("\n"),
        KeyCode::Char('j') if ctrl => editor.insert("\n"),
        KeyCode::Char('u') if ctrl => editor.delete_to_line_start(),
        KeyCode::Backspace if cmd => editor.delete_to_line_start(),
        KeyCode::Char('w') if ctrl => editor.delete_word(),
        KeyCode::Backspace if alt => editor.delete_word(),
        KeyCode::Char('a') if ctrl => editor.cursor = editor.line_start(),
        KeyCode::Char('e') if ctrl => editor.cursor = editor.line_end(),
        KeyCode::Home => editor.cursor = editor.line_start(),
        KeyCode::End => editor.cursor = editor.line_end(),
        KeyCode::Left if cmd => editor.cursor = editor.line_start(),
        KeyCode::Right if cmd => editor.cursor = editor.line_end(),
        KeyCode::Up if cmd => editor.cursor = 0,
        KeyCode::Down if cmd => editor.cursor = editor.text.len(),
        // Option and the arrows, and the `Esc b` and `Esc f` some keyboards send for them.
        KeyCode::Left if alt => editor.cursor = editor.word_left(),
        KeyCode::Right if alt => editor.cursor = editor.word_right(),
        KeyCode::Char('b') if alt => editor.cursor = editor.word_left(),
        KeyCode::Char('f') if alt => editor.cursor = editor.word_right(),
        KeyCode::Left => editor.cursor = editor.cursor.saturating_sub(1),
        KeyCode::Right => editor.cursor = (editor.cursor + 1).min(editor.text.len()),
        KeyCode::Up => {
            let start = editor.line_start();
            if start > 0 {
                let col = editor.cursor - start;
                let prev_start = editor.text[..start - 1]
                    .iter()
                    .rposition(|c| *c == '\n')
                    .map_or(0, |i| i + 1);
                editor.cursor = (prev_start + col).min(start - 1);
            }
        }
        KeyCode::Down => {
            let end = editor.line_end();
            if end < editor.text.len() {
                let col = editor.cursor - editor.line_start();
                let next_end = editor.text[end + 1..]
                    .iter()
                    .position(|c| *c == '\n')
                    .map_or(editor.text.len(), |i| end + 1 + i);
                editor.cursor = (end + 1 + col).min(next_end);
            }
        }
        KeyCode::Backspace => editor.backspace(),
        KeyCode::Delete => editor.delete(),
        KeyCode::Char(c) if !ctrl && !alt && !cmd => editor.insert(&c.to_string()),
        _ => return false,
    }
    true
}

/// `text` broken into rows of at most `width` columns at spaces or newlines, and the cursor's
/// row and column.
pub(crate) fn wrap(text: &[char], cursor: usize, width: usize) -> (Vec<String>, (usize, usize)) {
    // Each character's row and column, kept up to date as words move down, so the cursor
    // lands where its character finally is.
    let mut rows: Vec<Vec<(usize, char)>> = vec![Vec::new()];
    let mut col = 0;
    for (i, &c) in text.iter().enumerate() {
        if c == '\n' {
            rows.last_mut().unwrap().push((i, c));
            rows.push(Vec::new());
            col = 0;
            continue;
        }
        let w = c.width().unwrap_or(0);
        if col + w > width {
            // Move the word being typed down with it, when there is a space to break at.
            let row = rows.last_mut().unwrap();
            let carried = match row.iter().rposition(|(_, ch)| *ch == ' ') {
                Some(space) if space + 1 < row.len() => row.split_off(space + 1),
                _ => Vec::new(),
            };
            col = carried.iter().map(|(_, ch)| ch.width().unwrap_or(0)).sum();
            rows.push(carried);
        }
        rows.last_mut().unwrap().push((i, c));
        col += w;
    }
    let mut at = (rows.len() - 1, col);
    for (r, row) in rows.iter().enumerate() {
        let mut x = 0;
        for &(i, ch) in row {
            if i == cursor {
                at = (r, x);
            }
            x += ch.width().unwrap_or(0);
        }
    }
    let lines = rows
        .into_iter()
        .map(|row| {
            row.into_iter()
                .filter(|(_, c)| *c != '\n')
                .map(|(_, c)| c)
                .collect()
        })
        .collect();
    (lines, at)
}

pub async fn run(thread: String) -> Result<()> {
    let (client, thread) = t3::thread(&thread)?;
    let thread_id = thread.thread;
    // The scheme first, so the logos loaded next are for the screen they're seen on.
    let mut scheme = crate::scheme::listen();
    let mut events = client.watch_thread(&thread_id, None);
    let (tx, mut done) = mpsc::unbounded_channel();
    {
        let (client, tx) = (client.clone(), tx.clone());
        tokio::spawn(async move {
            if let Ok(config) = client.server_config().await {
                let providers = models::providers(&config).to_vec();
                let _ = tx.send(Done::Config(config));
                let _ = tx.send(Done::Logos(crate::new::logos(&providers).await));
            }
        });
    }
    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 5));
    let slash = Slash::new(client.clone());
    let mut composer = Composer {
        thread_id,
        client,
        offline: None,
        state: None,
        config: None,
        editor: Editor::default(),
        answers: serde_json::Map::new(),
        note: None,
        note_until: None,
        link: None,
        cols: cols as usize,
        rows: rows as usize,
        title: String::new(),
        tx,
        logos: HashMap::new(),
        painter: Painter,
        spans: Vec::new(),
        focus: None,
        renamed: false,
        chat_width: Follow::new(),
        slash,
        images: Tray::default(),
        clipboard: paste::Reader::default(),
        grow: match (std::env::var("REX_SESSION"), std::env::var("REX_BLOCK")) {
            (Ok(session), Ok(block)) => Some(Grow {
                session,
                block,
                asked: 0,
                wants: 0,
                since: None,
                base: rows as usize,
                busy: false,
            }),
            _ => None,
        },
    };
    crossterm::terminal::enable_raw_mode()?;
    // The alternate screen, bracketed paste, and clicks on the chips. Paste events too, where
    // the filter can take the clipboard's replies.
    let mut clipboard = crate::scheme::clipboard();
    out("\x1b[?1049h\x1b[?2004h\x1b[?1000h\x1b[?1006h");
    if clipboard.is_some() {
        out(paste::ON);
    }
    let keys = crate::scheme::keys();
    let mut input = EventStream::new();
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(2));
    composer.draw();
    let result: Result<()> = async {
        loop {
            tokio::select! {
                event = events.recv() => match event {
                    Some(WatchEvent::Item(item)) => {
                        if item["kind"] == "snapshot" {
                            composer.state = ThreadState::from_snapshot(&item);
                        } else if let Some(s) = composer.state.as_mut() {
                            s.apply(&item);
                        }
                        if composer.pending().is_none() {
                            composer.answers.clear();
                        }
                        composer.offline = None;
                    }
                    Some(WatchEvent::Reconnecting { reason, retry_in }) => {
                        composer.offline = Some(crate::chat::offline_line(composer.client.name(), &reason, retry_in));
                    }
                    Some(WatchEvent::Failed(m)) => anyhow::bail!("T3 refused the thread: {m}"),
                    None => anyhow::bail!("the thread's subscription ended"),
                },
                event = done.recv() => match event {
                    Some(Done::Note(text, error)) => if !text.is_empty() { composer.note(text, error) },
                    Some(Done::Restore(text, images)) => {
                        if composer.editor.text.is_empty() {
                            composer.editor.insert(&text);
                        }
                        if composer.images.is_empty() {
                            composer.attach(images);
                        }
                    }
                    Some(Done::Picture(ready)) => if let Some(why) = composer.images.ready(ready) {
                        composer.note(why, true);
                    },
                    Some(Done::Config(config)) => composer.config = Some(config),
                    Some(Done::Link(url)) => composer.link = Some(url),
                    Some(Done::Logos(logos)) => composer.logos = logos,
                    Some(Done::Resized) => if let Some(grow) = composer.grow.as_mut() {
                        grow.busy = false;
                        // The text may have changed while it resized.
                        if grow.wants != grow.asked {
                            grow.since = Some(tokio::time::Instant::now());
                        }
                        continue;
                    },
                    None => {}
                },
                Ok(()) = scheme.changed() => if let Some(config) = composer.config.as_ref() {
                    let (tx, providers) = (composer.tx.clone(), models::providers(config).to_vec());
                    tokio::spawn(async move {
                        let _ = tx.send(Done::Logos(crate::new::logos(&providers).await));
                    });
                    continue;
                } else {
                    continue;
                },
                Some(packet) = async {
                    match clipboard.as_mut() {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => match composer.clipboard.feed(&packet) {
                    Some(got) => composer.pasted(got),
                    None => continue,
                },
                _ = tick.tick() => if !composer.chat_width.changed() {
                    continue;
                },
                _ = tokio::time::sleep_until(composer.grow.as_ref().and_then(|g| g.since).unwrap_or_else(tokio::time::Instant::now) + GROW_WAIT),
                    if composer.grow.as_ref().is_some_and(|g| g.since.is_some() && !g.busy) => {
                    let grow = composer.grow.as_mut().expect("checked above");
                    grow.since = None;
                    if grow.wants != grow.asked {
                        grow.asked = grow.wants;
                        grow.busy = true;
                        let (session, block, base, extra, tx) = (grow.session.clone(), grow.block.clone(), grow.base, grow.wants, composer.tx.clone());
                        tokio::spawn(async move {
                            resize(session, block, base, extra).await;
                            let _ = tx.send(Done::Resized);
                        });
                    }
                    continue;
                }
                _ = tokio::time::sleep_until(composer.note_until.unwrap_or_else(tokio::time::Instant::now)), if composer.note_until.is_some() => {
                    composer.note = None;
                    composer.note_until = None;
                }
                event = input.next() => match event {
                    Some(Ok(Event::Key(key))) if key.kind != KeyEventKind::Release => composer.key(key),
                    Some(Ok(Event::Mouse(m))) if composer.slash.hits(&m) => composer.slash.click(&mut composer.editor, &m),
                    Some(Ok(Event::Mouse(m))) => {
                        let (row, col) = (m.row as usize + 1, m.column as usize + 1);
                        let hit = composer
                            .spans
                            .iter()
                            .find(|(r, (a, b), _)| *r == row && (*a..=*b).contains(&col))
                            .map(|(_, _, hit)| *hit);
                        match (m.kind, hit) {
                            (MouseEventKind::Down(MouseButton::Left), Some(hit)) => {
                                composer.note = None;
                                composer.press(hit);
                            }
                            _ => continue,
                        }
                    }
                    Some(Ok(Event::Paste(text))) => composer.pasted(Got::Text(text)),
                    Some(Ok(Event::Resize(c, r))) => {
                        composer.cols = c as usize;
                        composer.rows = r as usize;
                        // At the starting split, Reply's rows are its least: a window that changes
                        // size, or another device's, changes that.
                        if let Some(grow) = composer.grow.as_mut()
                            && grow.asked == 0
                            && !grow.busy
                        {
                            grow.base = r as usize;
                        }
                    }
                    Some(Err(e)) => return Err(e.into()),
                    None => return Ok(()),
                    _ => continue,
                },
            }
            // A thread that moved to another provider: once the input is empty, start again
            // under that provider's name, so Rex's sidebar shows its glyph.
            if composer.editor.text.is_empty()
                && composer.answers.is_empty()
                && let (Some(state), Some(config)) = (composer.state.as_ref(), composer.config.as_ref())
            {
                let instance = state.thread()["modelSelection"]["instanceId"].as_str().unwrap_or_default();
                let driver = crate::icons::driver_of(Some(config), instance).to_string();
                let want = crate::icons::program_name(&driver).unwrap_or("t3rex");
                if !instance.is_empty() && crate::icons::started_as() != want && !composer.renamed {
                    composer.renamed = true;
                    crate::scheme::keys_off();
                    out("\x1b[?1006l\x1b[?1000l\x1b[?2004l\x1b[?1049l");
                    let _ = crossterm::terminal::disable_raw_mode();
                    crate::icons::become_program(&driver);
                    out("\x1b[?1049h\x1b[?2004h\x1b[?1000h\x1b[?1006h");
                    crate::scheme::keys_on();
                    let _ = crossterm::terminal::enable_raw_mode();
                }
            }
            composer.draw();
        }
    }
    .await;
    drop(keys);
    out(paste::OFF);
    out("\x1b[?1006l\x1b[?1000l\x1b[?2004l\x1b[?1049l");
    let _ = crossterm::terminal::disable_raw_mode();
    result
}

#[cfg(test)]
mod tests {
    use super::{Editor, edit, wrap};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn mac_editing_keys() {
        let mut ed = Editor::default();
        ed.insert("one two\nthree  four");
        let press = |ed: &mut Editor, code, mods| edit(ed, KeyEvent::new(code, mods));
        press(&mut ed, KeyCode::Left, KeyModifiers::SUPER);
        assert_eq!(ed.cursor, 8);
        press(&mut ed, KeyCode::Right, KeyModifiers::SUPER);
        assert_eq!(ed.cursor, 19);
        press(&mut ed, KeyCode::Left, KeyModifiers::ALT);
        assert_eq!(ed.cursor, 15);
        press(&mut ed, KeyCode::Left, KeyModifiers::ALT);
        assert_eq!(ed.cursor, 8);
        press(&mut ed, KeyCode::Right, KeyModifiers::ALT);
        assert_eq!(ed.cursor, 13);
        press(&mut ed, KeyCode::Backspace, KeyModifiers::ALT);
        assert_eq!(ed.string(), "one two\n  four");
        ed.cursor = 14;
        press(&mut ed, KeyCode::Backspace, KeyModifiers::SUPER);
        assert_eq!(ed.string(), "one two\n");
        // Shift+Enter is a new line, and a Cmd shortcut types nothing.
        press(&mut ed, KeyCode::Enter, KeyModifiers::SHIFT);
        assert!(!press(&mut ed, KeyCode::Char('k'), KeyModifiers::SUPER));
        assert_eq!(ed.string(), "one two\n\n");
    }

    #[test]
    fn wraps_at_spaces_and_tracks_the_cursor() {
        let text: Vec<char> = "hello there world".chars().collect();
        let (lines, at) = wrap(&text, text.len(), 11);
        assert_eq!(lines, vec!["hello ".to_string(), "there world".to_string()]);
        assert_eq!(at, (1, 11));
        // The cursor inside a word that a later character carries down goes down with it.
        let text: Vec<char> = "hello there".chars().collect();
        assert_eq!(wrap(&text, 8, 10).1, (1, 2));
        let (lines, at) = wrap(&"a\nb".chars().collect::<Vec<_>>(), 2, 10);
        assert_eq!(lines, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(at, (1, 0));
    }
}
