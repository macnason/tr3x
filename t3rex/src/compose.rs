//! The reply pane under the conversation: one input, and the thread's pending approval or
//! question when there is one. Its header, set with OSC 2, names the model, effort and access
//! the next message goes out with.
//!
//! Enter sends, and queues behind a running turn. Alt+Enter or Ctrl+J adds a line. A line that
//! starts with `/` is a command: /settle, /snooze 2h, /wake, /pin, /unpin, /archive, /stop,
//! /model NAME, /effort LEVEL, /open.

use std::io::Write;
use std::sync::Arc;

use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures_util::StreamExt;
use serde_json::{Value, json};
use t3term::client::{Client, IfBusy, WatchEvent};
use t3term::models::{self, Choice};
use t3term::projection::ThreadState;
use tokio::sync::mpsc;
use unicode_width::UnicodeWidthChar;

use crate::chat::{BLUE, BOLD, DIM, RED, RESET, YELLOW, fit};
use crate::t3::{self, Lifecycle};

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
    "effort", "mode", "steer",
];

/// Agent-written text as one plain line: no escape sequences or other controls, which could
/// redraw the pane and show something other than what is being asked.
fn clean(text: &str) -> String {
    t3term::transcript::plain(text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// What came back from work started in the background.
enum Done {
    Note(String, bool),
    /// A message that failed to send, back into the input if it's empty.
    Restore(String),
    Config(Value),
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
    cols: usize,
    rows: usize,
    title: String,
    tx: mpsc::UnboundedSender<Done>,
}

impl Composer {
    fn pending(&self) -> Option<(Value, Value)> {
        let state = self.state.as_ref()?;
        let request = state.pending_requests().into_iter().next()?.clone();
        let item = state
            .request_item(request["id"].as_str().unwrap_or_default())
            .cloned()
            .unwrap_or(Value::Null);
        Some((request, item))
    }

    fn question<'a>(&self, item: &'a Value) -> Option<&'a Value> {
        item["questions"].as_array()?.iter().find(|q| {
            !self
                .answers
                .contains_key(q["id"].as_str().unwrap_or_default())
        })
    }

    /// The header: model, effort and access, as the desktop composer's chips show them.
    fn header(&self) -> String {
        let (Some(state), Some(config)) = (self.state.as_ref(), self.config.as_ref()) else {
            return "Reply".into();
        };
        let settings = models::thread_settings(config, state.thread());
        let mut parts: Vec<String> = Vec::new();
        let model = settings["modelName"]
            .as_str()
            .or(settings["model"].as_str())
            .unwrap_or("Model");
        parts.push(model.to_string());
        for key in ["effort", "reasoningEffort", "thinking"] {
            if let Some(v) = settings["options"][key].as_str() {
                let mut c = v.chars();
                parts.push(
                    c.next()
                        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                        .unwrap_or_default(),
                );
            }
        }
        if settings["options"]["fastMode"] == true {
            parts.push("Fast".into());
        }
        if let Some(mode) = settings["runtimeModeLabel"].as_str() {
            parts.push(mode.to_string());
        }
        if state.active_run().is_some() {
            parts.push("Working".into());
        }
        parts.join(" · ")
    }

    fn draw(&mut self) {
        let title = self.header();
        let mut text = String::from("\x1b[?2026h");
        if title != self.title {
            text.push_str(&format!(
                "\x1b]2;{}\x07",
                title.replace(|c: char| c.is_control(), "")
            ));
            self.title = title;
        }
        text.push_str("\x1b[H\x1b[2J");
        let width = self.cols.saturating_sub(4).max(10);
        let mut rows: Vec<String> = Vec::new();
        if let Some((request, item)) = self.pending() {
            if request["kind"] == "user_input" {
                if let Some(q) = self.question(&item) {
                    let prompt = clean(
                        q["question"]
                            .as_str()
                            .or(q["header"].as_str())
                            .unwrap_or("Question"),
                    );
                    rows.push(format!(
                        "{YELLOW}{BOLD}◆{RESET} {BOLD}{}{RESET}",
                        fit(&prompt, width - 2)
                    ));
                    let opts: Vec<String> = q["options"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .enumerate()
                        .take(6)
                        .map(|(i, o)| {
                            format!(
                                "{DIM}{}{RESET} {}",
                                i + 1,
                                clean(o["label"].as_str().unwrap_or(""))
                            )
                        })
                        .collect();
                    if !opts.is_empty() {
                        rows.push(fit_ansi(&opts.join("   "), width));
                    }
                }
            } else {
                let what = item["prompt"]
                    .as_str()
                    .or(item["title"].as_str())
                    .or(request["summary"].as_str())
                    .unwrap_or("The agent is asking to continue");
                let what = clean(what);
                let verb = match request["kind"].as_str().unwrap_or_default() {
                    k if k.contains("command") => "Run",
                    k if k.contains("file-change") || k.contains("file_change") => "Change",
                    k if k.contains("read") => "Read",
                    k if k.contains("network") || k.contains("fetch") => "Fetch",
                    _ => "Allow",
                };
                rows.push(format!(
                    "{YELLOW}{BOLD}◆{RESET} {BOLD}{verb}{RESET} {}?",
                    fit(&what, width.saturating_sub(8))
                ));
                rows.push(format!(
                    "{BOLD}a ⏎{RESET} Accept   {BOLD}s ⏎{RESET} Accept for session   {BOLD}d ⏎{RESET} Decline"
                ));
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
        }
        // Keep the cursor's row on screen in a pane shorter than the text.
        let cursor_row = first_input + cursor.0;
        let start = (cursor_row + 1)
            .saturating_sub(self.rows)
            .min(rows.len().saturating_sub(self.rows.min(rows.len())));
        let shown: Vec<&String> = rows.iter().skip(start).take(self.rows).collect();
        for (i, row) in shown.iter().enumerate() {
            text.push_str(&format!("\x1b[{};1H {row}", i + 1));
        }
        let (r, c) = (cursor_row.saturating_sub(start) + 1, cursor.1 + 4);
        text.push_str(&format!("\x1b[{r};{c}H\x1b[?25h\x1b[?2026l"));
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
            let answer = if q["multiSelect"] == true {
                json!([answer])
            } else {
                json!(answer)
            };
            self.answers
                .insert(q["id"].as_str().unwrap_or_default().to_string(), answer);
            self.editor.take();
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
            "open" => self.spawn(async move {
                crate::install::open_in_t3(&thread).await?;
                Ok("Opened in T3 Code".into())
            }),
            "model" | "effort" | "mode" if arg.is_empty() => {
                let field = match name {
                    "model" => crate::pick::Field::Model,
                    "effort" => crate::pick::Field::Effort,
                    _ => crate::pick::Field::Access,
                };
                let Ok(session) = std::env::var("REX_SESSION") else {
                    return self.note("Open this in Rex to choose a model", true);
                };
                self.spawn(async move {
                    crate::pick::float(&session, &thread, field).await?;
                    Ok(String::new())
                });
            }
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
        match key.code {
            KeyCode::Enter if !key.modifiers.intersects(KeyModifiers::ALT | KeyModifiers::SHIFT) => {
                self.submit()
            }
            KeyCode::Char('c') if ctrl => {
                self.editor.take();
            }
            KeyCode::Char('x') if ctrl => self.command("stop"),
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

/// Cuts a styled line to `width` visible columns.
fn fit_ansi(line: &str, width: usize) -> String {
    if crate::render::visible_width(line) <= width {
        return line.to_string();
    }
    fit(&crate::render::strip_ansi(line), width)
}

pub async fn run(thread_id: String) -> Result<()> {
    let client = t3::connect(true).await?;
    let mut events = client.watch_thread(&thread_id, None);
    let (tx, mut done) = mpsc::unbounded_channel();
    {
        let (client, tx) = (client.clone(), tx.clone());
        tokio::spawn(async move {
            if let Ok(config) = client.server_config().await {
                let _ = tx.send(Done::Config(config));
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
        cols: cols as usize,
        rows: rows as usize,
        title: String::new(),
        tx,
    };
    crossterm::terminal::enable_raw_mode()?;
    out("\x1b[?1049h\x1b[?2004h");
    let mut input = EventStream::new();
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
                    None => {}
                },
                _ = tokio::time::sleep_until(composer.note_until.unwrap_or_else(tokio::time::Instant::now)), if composer.note_until.is_some() => {
                    composer.note = None;
                    composer.note_until = None;
                }
                event = input.next() => match event {
                    Some(Ok(Event::Key(key))) if key.kind != KeyEventKind::Release => composer.key(key),
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
            composer.draw();
        }
    }
    .await;
    out("\x1b[?2004l\x1b[?1049l");
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
