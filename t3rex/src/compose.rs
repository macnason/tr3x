//! The reply pane under the conversation: one input, and the thread's pending approval or
//! question when there is one. Its header, set with OSC 2, names the model, effort and access
//! the next message goes out with.
//!
//! Enter sends, and queues behind a running turn. Alt+Enter or Ctrl+J adds a line. A line that
//! starts with `/` is a command: /settle, /snooze 2h, /wake, /pin, /unpin, /archive, /stop,
//! /model NAME, /effort LEVEL, /open, /width comfortable|wide|full|t3.

use std::io::Write;
use std::sync::Arc;

use anyhow::Result;
use crossterm::event::{
    Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::collections::HashMap;
use t3term::client::{Client, IfBusy, WatchEvent};
use t3term::models::{self, Choice};
use t3term::projection::ThreadState;
use tokio::sync::mpsc;
use unicode_width::UnicodeWidthChar;

use crate::chat::{BLUE, BOLD, DIM, RED, RESET, YELLOW, fit};
use crate::icons::{Icon, Painter};
use crate::pick::Field;
use crate::picker::{self, Chip, Settings};
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
        while self.cursor > 0 && self.text[self.cursor - 1].is_whitespace() {
            self.backspace();
        }
        while self.cursor > 0 && !self.text[self.cursor - 1].is_whitespace() {
            self.backspace();
        }
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
}

/// What came back from work started in the background.
enum Done {
    Note(String, bool),
    /// A message that failed to send, back into the input if it's empty.
    Restore(String),
    Config(Value),
    Logos(HashMap<String, Icon>),
    /// A link to open the thread with, on the device it's clicked on.
    Link(String),
}

struct Composer {
    thread_id: String,
    client: Arc<Client>,
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

    /// The model, effort and access chips under the input, as T3's composer shows them, with
    /// the provider's logo.
    fn chips(&self) -> Vec<(Chip, Field)> {
        let (Some(state), Some(config)) = (self.state.as_ref(), self.config.as_ref()) else {
            return Vec::new();
        };
        let settings = Settings::of_thread(config, state.thread());
        let mut chips = vec![(
            Chip {
                icon: self.logos.get(&settings.instance).cloned(),
                label: settings.model_label(config),
            },
            Field::Model,
        )];
        if let Some(effort) = settings.effort_label(config) {
            chips.push((
                Chip {
                    icon: None,
                    label: effort,
                },
                Field::Effort,
            ));
        }
        chips.push((
            Chip {
                icon: None,
                label: settings.mode_label(),
            },
            Field::Access,
        ));
        chips
    }

    /// Opens the model picker over the window, on `field`.
    fn pick(&mut self, field: Field) {
        let Ok(session) = std::env::var("REX_SESSION") else {
            return self.note("Open this in Rex to choose a model", true);
        };
        let thread = self.thread_id.clone();
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
        // The conversation's column, less the prompt's two.
        let (left, column) = self.chat_width.width.column(self.cols);
        let width = column.saturating_sub(2).max(10);
        // The chips take the bottom row, and a blank row above them when there is room.
        let chips: Vec<(Chip, Field)> = self.chips();
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
        // The input, wrapped, with the cursor where it is.
        let prompt = format!("{BLUE}{BOLD}›{RESET} ");
        let (lines, cursor) = wrap(&self.editor.text, self.editor.cursor, width);
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
        // The cursor hides while a button has focus, as it does on a chip in a new thread.
        let cursor = if self.focus.is_some() { "?25l" } else { "?25h" };
        text.push_str(&format!("\x1b[{r};{c}H\x1b[{cursor}\x1b[?2026l"));
        out(&text);
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

    fn submit(&mut self) {
        let text = self.editor.string();
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return;
        }
        // A pending question takes the input as its answer: a number picks an option.
        if let Some((request, item)) = self.pending()
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
        self.send(IfBusy::Queue);
    }

    /// Sends the input. Queue waits for the running turn; Steer goes to it now. A send that
    /// fails puts the text back.
    fn send(&mut self, mode: IfBusy) {
        let Some(state) = self.state.clone() else {
            return;
        };
        let message = self.editor.take();
        let client = self.client.clone();
        let busy = state.active_run().is_some();
        let steer = matches!(mode, IfBusy::Steer);
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let done = match client.send_message(&state, &message, mode).await {
                Ok(_) if busy && steer => Done::Note("Sent to the running turn".into(), false),
                Ok(_) if busy => Done::Note("Queued".into(), false),
                Ok(_) => Done::Note("Sent".into(), false),
                Err(e) => {
                    let _ = tx.send(Done::Restore(message));
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
                let link = crate::link::thread(
                    self.config.as_ref(),
                    &self.client.runtime.environment_id,
                    &self.thread_id,
                );
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let done = if !crate::link::only_here().await {
                        Done::Link(link)
                    } else {
                        match crate::install::open_in_t3(&thread).await {
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
                self.submit()
            }
            KeyCode::Char('c') if ctrl => {
                self.editor.take();
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
/// Ctrl+U, Ctrl+W, Ctrl+A and Ctrl+E as in a shell, and the arrows. Returns whether the key
/// was one of them.
pub(crate) fn edit(editor: &mut Editor, key: KeyEvent) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    match key.code {
        KeyCode::Enter if alt || key.modifiers.contains(KeyModifiers::SHIFT) => editor.insert("\n"),
        KeyCode::Char('j') if ctrl => editor.insert("\n"),
        KeyCode::Char('u') if ctrl => {
            let start = editor.line_start();
            while editor.cursor > start {
                editor.backspace();
            }
        }
        KeyCode::Char('w') if ctrl => editor.delete_word(),
        KeyCode::Backspace if alt => editor.delete_word(),
        KeyCode::Char('a') if ctrl => editor.cursor = editor.line_start(),
        KeyCode::Char('e') if ctrl => editor.cursor = editor.line_end(),
        KeyCode::Home => editor.cursor = editor.line_start(),
        KeyCode::End => editor.cursor = editor.line_end(),
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
        KeyCode::Char(c) if !ctrl && !alt => editor.insert(&c.to_string()),
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

pub async fn run(thread_id: String) -> Result<()> {
    let client = t3::connect(true).await?;
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
    let mut composer = Composer {
        thread_id,
        client,
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
    };
    crossterm::terminal::enable_raw_mode()?;
    // The alternate screen, bracketed paste, and clicks on the chips.
    out("\x1b[?1049h\x1b[?2004h\x1b[?1000h\x1b[?1006h");
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
                    }
                    Some(WatchEvent::Failed(m)) => anyhow::bail!("T3 refused the thread: {m}"),
                    None => anyhow::bail!("the thread's subscription ended"),
                    _ => {}
                },
                event = done.recv() => match event {
                    Some(Done::Note(text, error)) => if !text.is_empty() { composer.note(text, error) },
                    Some(Done::Restore(text)) => {
                        if composer.editor.text.is_empty() {
                            composer.editor.insert(&text);
                        }
                    }
                    Some(Done::Config(config)) => composer.config = Some(config),
                    Some(Done::Link(url)) => composer.link = Some(url),
                    Some(Done::Logos(logos)) => composer.logos = logos,
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
                _ = tick.tick() => if !composer.chat_width.changed() {
                    continue;
                },
                _ = tokio::time::sleep_until(composer.note_until.unwrap_or_else(tokio::time::Instant::now)), if composer.note_until.is_some() => {
                    composer.note = None;
                    composer.note_until = None;
                }
                event = input.next() => match event {
                    Some(Ok(Event::Key(key))) if key.kind != KeyEventKind::Release => composer.key(key),
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
                    Some(Ok(Event::Paste(text))) => composer.editor.insert(&text.replace("\r\n", "\n").replace('\r', "\n")),
                    Some(Ok(Event::Resize(c, r))) => {
                        composer.cols = c as usize;
                        composer.rows = r as usize;
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
                    out("\x1b[?1006l\x1b[?1000l\x1b[?2004l\x1b[?1049l");
                    let _ = crossterm::terminal::disable_raw_mode();
                    crate::icons::become_program(&driver);
                    out("\x1b[?1049h\x1b[?2004h\x1b[?1000h\x1b[?1006h");
                    let _ = crossterm::terminal::enable_raw_mode();
                }
            }
            composer.draw();
        }
    }
    .await;
    out("\x1b[?1006l\x1b[?1000l\x1b[?2004l\x1b[?1049l");
    let _ = crossterm::terminal::disable_raw_mode();
    result
}

#[cfg(test)]
mod tests {
    use super::wrap;

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
