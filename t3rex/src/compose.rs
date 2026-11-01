//! Reply: the bottom rows of a thread's pane, under the conversation. One input, and the
//! thread's pending approval or question when there is one, and under them the chip that
//! names the model, effort and access the next message goes out with. The conversation pane
//! (`chat`) runs it, and keeps the conversation scrolling above it.
//!
//! Enter sends. To a running turn it steers or queues, as T3's Follow-up behavior says, and
//! ⌘Enter does the other, as in T3. Alt+Enter or Ctrl+J adds a line. A line that
//! starts with `/` is a command: /settle, /snooze 2h, /wake, /pin, /unpin, /archive, /stop,
//! /model NAME, /effort LEVEL, /open, /width comfortable|wide|full|t3, /reply inline|pane.
//! `/` first opens a menu of those, the provider's commands and skills (`slash`).
//!
//! A pasted picture, or a dropped one, rides on the next message (`paste`). It shows as a chip
//! over the input; its ✕, or Backspace at the start of the input, takes it off.

use std::io::Write;
use std::sync::Arc;

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent};
use serde_json::{Value, json};
use std::collections::HashMap;
use t3client::env::{Env, IfBusy};
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
use crate::width::ChatWidth;

fn out(text: &str) {
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(text.as_bytes());
    let _ = stdout.flush();
}

#[derive(Default)]
pub(crate) struct Editor {
    pub(crate) text: Vec<char>,
    pub(crate) cursor: usize,
    /// The columns the input last wrapped at, so Up, Down and Cmd with the arrows move by the
    /// rows on screen. 0 before the first draw: by lines.
    pub(crate) width: usize,
    undo: Vec<(Vec<char>, usize)>,
    redo: Vec<(Vec<char>, usize)>,
    /// The edit before this one, so a word typed or a run of deletes undoes in one go.
    last: Run,
    /// What Ctrl+K, Ctrl+U, Ctrl+W and the Delete shortcuts cut last, for Ctrl+Y.
    killed: Vec<char>,
    /// The sent message Up has brought back, and the draft it set aside to get there.
    recalled: Option<(usize, String)>,
}

#[derive(Default, Clone, Copy, PartialEq)]
enum Run {
    #[default]
    None,
    Typing,
    Deleting,
}

impl Editor {
    pub(crate) fn insert(&mut self, s: &str) {
        let s = s.replace('\t', "    ");
        for c in s.chars().filter(|c| *c == '\n' || !c.is_control()) {
            self.text.insert(self.cursor, c);
            self.cursor += 1;
        }
    }
    /// A paste: undoes on its own.
    pub(crate) fn paste(&mut self, s: &str) {
        let (before, kept) = (self.text.clone(), self.checkpoint(Run::None));
        self.insert(s);
        self.edited(&before, kept);
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
    /// Cuts `from..to` for Ctrl+Y.
    fn kill(&mut self, from: usize, to: usize) {
        if from < to {
            self.killed = self.text.drain(from..to).collect();
            self.cursor = from;
        }
    }
    pub(crate) fn delete_word(&mut self) {
        self.kill(self.word_left(), self.cursor);
    }
    pub(crate) fn delete_to_line_start(&mut self) {
        self.kill(self.line_start(), self.cursor);
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
    /// A row on screen, `offset` from the cursor's: its start, its end, the last place on it
    /// the cursor shows, and the cursor's column on its own row. The end of a row broken
    /// inside a word is after its last character, where the cursor shows on the next row.
    fn row(&self, offset: isize) -> Option<((usize, usize, usize), usize)> {
        let width = if self.width == 0 {
            usize::MAX
        } else {
            self.width
        };
        let rows = layout(&self.text, width);
        let (r, x) = locate(&rows, self.cursor);
        let r = r.checked_add_signed(offset).filter(|r| *r < rows.len())?;
        let start = rows[r].first().map_or(self.text.len(), |(i, _)| *i);
        let (end, last) = match (rows.get(r + 1), rows[r].last()) {
            (Some(next), Some(&(i, c))) => {
                let next = next.first().map_or(self.text.len(), |(i, _)| *i);
                (if c == ' ' || c == '\n' { i } else { next }, i)
            }
            _ => (self.text.len(), self.text.len()),
        };
        Some(((start, end, last), x))
    }
    fn row_start(&self) -> usize {
        self.row(0).map_or(0, |((start, ..), _)| start)
    }
    fn row_end(&self) -> usize {
        self.row(0).map_or(self.text.len(), |((_, end, _), _)| end)
    }
    /// The cursor up or down a row on screen, in the same column where the row is long enough.
    /// From the first row Up goes to the start, and from the last Down goes to the end, as on
    /// the Mac.
    fn vertical(&mut self, down: bool) {
        let Some(((start, _, end), _)) = self.row(if down { 1 } else { -1 }) else {
            self.cursor = if down { self.text.len() } else { 0 };
            return;
        };
        let x = self.row(0).map_or(0, |(_, x)| x);
        let (mut at, mut col) = (start, 0);
        while at < end && col < x {
            col += self.text[at].width().unwrap_or(0);
            at += 1;
        }
        self.cursor = at;
    }
    /// Option and Up: the start of the paragraph, or of the one before from its start.
    fn paragraph_up(&mut self) {
        if self.cursor == self.line_start() && self.cursor > 0 {
            self.cursor -= 1;
        }
        self.cursor = self.line_start();
    }
    /// Option and Down: the end of the paragraph, or of the one after from its end.
    fn paragraph_down(&mut self) {
        if self.cursor == self.line_end() && self.cursor < self.text.len() {
            self.cursor += 1;
        }
        self.cursor = self.line_end();
    }
    /// Keeps the text as it is for Undo, before an edit. A run of the same kind of edit keeps
    /// one. Returns whether it kept one.
    fn checkpoint(&mut self, run: Run) -> bool {
        if run != Run::None && run == self.last {
            return false;
        }
        self.last = run;
        self.undo.push((self.text.clone(), self.cursor));
        if self.undo.len() > 200 {
            self.undo.remove(0);
        }
        true
    }
    /// After an edit: one that changed nothing keeps nothing for Undo and leaves Redo be.
    fn edited(&mut self, before: &[char], kept: bool) {
        if self.text != before {
            self.redo.clear();
        } else if kept {
            self.undo.pop();
            self.last = Run::None;
        }
    }
    pub(crate) fn undo(&mut self) -> bool {
        while let Some((text, cursor)) = self.undo.pop() {
            if text == self.text {
                continue;
            }
            self.redo
                .push((std::mem::replace(&mut self.text, text), self.cursor));
            self.cursor = cursor;
            self.last = Run::None;
            return true;
        }
        false
    }
    pub(crate) fn redo(&mut self) -> bool {
        let Some((text, cursor)) = self.redo.pop() else {
            return false;
        };
        self.undo
            .push((std::mem::replace(&mut self.text, text), self.cursor));
        self.cursor = cursor;
        self.last = Run::None;
        true
    }
    /// Empties the input, which Undo brings back.
    pub(crate) fn clear(&mut self) {
        self.replace("", false);
        self.recalled = None;
    }
    fn replace(&mut self, text: &str, at_end: bool) {
        let (before, kept) = (self.text.clone(), self.checkpoint(Run::None));
        self.text = text.chars().collect();
        self.cursor = if at_end { self.text.len() } else { 0 };
        self.edited(&before, kept);
    }
    /// Up from the start of the input: the message sent before the one showing, oldest last.
    /// What was typed is set aside for Down to bring back; a message brought back and then
    /// changed counts as typed.
    pub(crate) fn older(&mut self, sent: &[String]) -> bool {
        let text = self.string();
        let (i, draft) = match self.recalled.take() {
            Some((i, draft)) if sent.get(i) == Some(&text) => match i.checked_sub(1) {
                Some(older) => (older, draft),
                None => {
                    self.recalled = Some((i, draft));
                    return false;
                }
            },
            _ => match sent.len().checked_sub(1) {
                Some(newest) => (newest, text),
                None => return false,
            },
        };
        self.replace(&sent[i], false);
        self.recalled = Some((i, draft));
        true
    }
    /// Down from the end of a message Up brought back: the next one, then the draft.
    pub(crate) fn newer(&mut self, sent: &[String]) -> bool {
        let text = self.string();
        let Some((i, draft)) = self.recalled.take() else {
            return false;
        };
        if sent.get(i) != Some(&text) {
            return false;
        }
        match sent.get(i + 1) {
            Some(next) => {
                self.replace(next, true);
                self.recalled = Some((i + 1, draft));
            }
            None => self.replace(&draft, true),
        }
        true
    }
    pub(crate) fn take(&mut self) -> String {
        self.cursor = 0;
        self.undo.clear();
        self.redo.clear();
        self.last = Run::None;
        self.recalled = None;
        std::mem::take(&mut self.text).into_iter().collect()
    }
    pub(crate) fn string(&self) -> String {
        self.text.iter().collect()
    }
}

/// Esc twice in a row clears the input, as in Claude Code and Codex.
#[derive(Default)]
pub(crate) struct Escape(Option<std::time::Instant>);

impl Escape {
    const AGAIN: std::time::Duration = std::time::Duration::from_millis(800);
    /// What the first Esc says. It goes when the second would no longer count.
    pub(crate) const NOTE: &str = "Esc again to clear";

    /// Whether an Esc now would be the second.
    pub(crate) fn armed(&self) -> bool {
        self.0.is_some_and(|t| t.elapsed() < Self::AGAIN)
    }

    /// Whether this Esc follows another closely. Any other key in between starts over.
    pub(crate) fn again(&mut self, key: &KeyEvent) -> bool {
        if key.code != KeyCode::Esc {
            self.0 = None;
            return false;
        }
        let now = std::time::Instant::now();
        let again = self.0.is_some_and(|t| now - t < Self::AGAIN);
        self.0 = Some(now);
        again
    }
}

/// What a click or a focused chip does.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Hit {
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
pub(crate) enum Done {
    Note(String, bool),
    /// A message that failed to send, back into the input if it's empty.
    Restore(String, Vec<Image>),
    /// A picture made ready to send.
    Picture(picture::Ready),
    Config(Value),
    Logos(HashMap<String, Icon>),
    /// A link to open the thread with, on the device it's clicked on.
    Link(String),
}

/// Reply's rows at most, as a share of a pane it shares with the conversation, and the
/// input's rows at most within them.
const MOST_SHARE: f64 = 0.5;
const INPUT_ROWS: usize = 8;

/// The input's fill: the terminal's background, a step lighter on a dark screen and a step
/// darker on a light one. None until the terminal says what its background is.
fn fill() -> Option<String> {
    let [r, g, b] = crate::scheme::background()?;
    let dark = (0.299 * r as f64 + 0.587 * g as f64 + 0.114 * b as f64) < 128.0;
    let step = |c: u8| {
        let c = c as f64;
        (if dark {
            c + (255.0 - c) * 0.07
        } else {
            c * 0.95
        })
        .round() as u8
    };
    Some(format!("\x1b[48;2;{};{};{}m", step(r), step(g), step(b)))
}
/// The columns the prompt takes from the text: `› `.
const PROMPT: usize = 2;

pub(crate) struct Composer {
    thread_id: String,
    client: Arc<Env>,
    /// What to say while the thread's machine is out of reach.
    pub(crate) offline: Option<String>,
    pub(crate) state: Option<ThreadState>,
    pub(crate) config: Option<Value>,
    pub(crate) editor: Editor,
    /// Answers to the pending question so far, by question id.
    pub(crate) answers: serde_json::Map<String, Value>,
    /// A line under the input: what just happened, or what went wrong, and when it goes.
    note: Option<(String, bool)>,
    pub(crate) note_until: Option<tokio::time::Instant>,
    /// The thread's link, under the input until the next key.
    link: Option<String>,
    /// The pane's size, and the conversation's column in it: its left edge and its width.
    pub(crate) cols: usize,
    pub(crate) rows: usize,
    pub(crate) column: (usize, usize),
    pub(crate) tx: mpsc::UnboundedSender<Done>,
    pub(crate) logos: HashMap<String, Icon>,
    painter: Painter,
    /// Each chip's and button's screen row and columns, for clicks.
    spans: Vec<(usize, (usize, usize), Hit)>,
    /// The approval or question button Tab moved to, by index.
    focus: Option<usize>,
    pub(crate) slash: Slash,
    /// The rows kept for the slash menu while it's open: the most it has needed, so Reply
    /// doesn't change height as the menu narrows.
    menu_rows: usize,
    /// The pictures the next message takes.
    pub(crate) images: Tray,
    pub(crate) clipboard: paste::Reader,
    /// The last command was `/delete`, so the next one deletes.
    deleting: bool,
    escape: Escape,
    /// A width `/width` chose, for the pane to take up.
    pub(crate) width_set: Option<ChatWidth>,
    /// What puts the cursor back in the input after the conversation draws.
    pub(crate) parked: String,
    /// Reply's rows at most, as a share of its pane: all of it when it has a pane of its own.
    pub(crate) share: f64,
    /// The rows Reply would take with room for all of them, up to `INPUT_ROWS` of input.
    pub(crate) wanted: usize,
    /// The input's background, empty when it sits between rules instead.
    fill: String,
    /// The last frame drawn, less the cursor.
    drawn: String,
}

/// Reply's rows before they're placed: each row, the clickable spans on each by row index,
/// the input's rows, and the cursor's row and column.
struct Laid {
    rows: Vec<String>,
    hits: Vec<(usize, (usize, usize), Hit)>,
    input: (usize, usize),
    cursor: (usize, usize),
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

    /// Reply's rows, top to bottom: what's pending and its buttons, the pictures, room for the
    /// slash menu, the input on its fill (or between two rules, before the terminal says what
    /// its background is), a note, and the chip. At most `share` of the pane;
    /// the input shows the rows around the cursor when there's less room than it has.
    fn layout(&mut self) -> Laid {
        self.slash.sync(
            &self.editor,
            self.config.as_ref(),
            self.state.as_ref().map(|s| s.thread()),
            &self.client.capabilities(),
        );
        self.menu_rows = match self.slash.rows() {
            0 => 0,
            n => n.max(self.menu_rows),
        };
        // The conversation's column, less the prompt.
        let (left, column) = self.column;
        let width = column.saturating_sub(PROMPT).max(10);
        let mut rows: Vec<String> = Vec::new();
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
        // The menu draws over these, right above the input.
        rows.extend(std::iter::repeat_n(String::new(), self.menu_rows));
        let mut below: Vec<String> = Vec::new();
        if let Some((note, error)) = &self.note {
            let color = if *error { RED } else { DIM };
            below.push(format!("{color}{}{RESET}", fit(note, width)));
        } else if let Some(url) = &self.link {
            below.push(format!(
                "{DIM}{}{RESET}",
                crate::link::osc8(url, "Open in T3 ↗")
            ));
        }
        let chips = self.chips(column);
        let chip_row = if chips.is_empty() {
            None
        } else {
            let only: Vec<Chip> = chips
                .iter()
                .map(|(c, _)| Chip {
                    icon: c.icon.clone(),
                    label: c.label.clone(),
                })
                .collect();
            let (line, spans) = picker::chips(&mut self.painter, &only, None, None, left);
            below.push(line);
            Some((spans, chips))
        };
        // The input, wrapped, and as many of its rows as there's room for around the cursor.
        self.editor.width = width;
        let (lines, cursor) = wrap(&self.editor.text, self.editor.cursor, width);
        self.wanted = rows.len() + lines.len().min(INPUT_ROWS) + 2 + below.len();
        let most = ((self.rows as f64 * self.share) as usize).max(3);
        let room = most
            .saturating_sub(rows.len() + below.len() + 2)
            .clamp(1, INPUT_ROWS);
        let start = (cursor.0 + 1)
            .saturating_sub(room)
            .min(lines.len().saturating_sub(room));
        self.fill = fill().unwrap_or_default();
        let bg = self.fill.clone();
        // On a fill, a column of it either side of the conversation's, and a row above and
        // below the input. Without one, a rule above and below.
        let edge = if bg.is_empty() {
            format!("{DIM}{}{RESET}", "─".repeat(column))
        } else {
            format!("\x1b[{left}G{bg}{}{RESET}", " ".repeat(column + 2))
        };
        rows.push(edge.clone());
        let first_input = rows.len();
        let prompt = format!("{BLUE}{BOLD}›{RESET} ");
        for (i, line) in lines.iter().enumerate().skip(start).take(room) {
            let lead = if i == 0 { prompt.clone() } else { "  ".into() };
            if bg.is_empty() {
                rows.push(format!("{lead}{line}"));
            } else {
                let lead = lead.replace(RESET, &format!("{RESET}{bg}"));
                let pad = " ".repeat(
                    width.saturating_sub(unicode_width::UnicodeWidthStr::width(line.as_str())) + 1,
                );
                rows.push(format!("\x1b[{left}G{bg} {lead}{line}{pad}{RESET}"));
            }
        }
        let shown = rows.len() - first_input;
        rows.push(edge);
        let cursor = (first_input + cursor.0 - start, cursor.1);
        if let Some((spans, chips)) = chip_row {
            let at = rows.len() + below.len() - 1;
            hits.extend(
                spans
                    .into_iter()
                    .zip(chips.iter().map(|(_, f)| Hit::Field(*f)))
                    .map(|(cols, hit)| (at, cols, hit)),
            );
        }
        rows.extend(below);
        Laid {
            rows,
            hits,
            input: (first_input, shown),
            cursor,
        }
    }

    /// The rows Reply takes at the bottom of the pane.
    pub(crate) fn height(&mut self) -> usize {
        // Laid out with a painter of its own, so the pictures drawn keep their ids.
        let painter = std::mem::replace(&mut self.painter, Painter::tagged());
        let rows = self.layout().rows.len();
        self.painter = painter;
        rows
    }

    /// Whether anything on Reply needs clicks: buttons to answer what's pending, or the menu.
    pub(crate) fn wants_clicks(&self) -> bool {
        self.slash.is_open() || !self.buttons().is_empty()
    }

    /// Reply drawn from the pane's row `top` (1-based) to its bottom, leaving the cursor in the
    /// input. Only the cursor moves when it would draw the same as last time, unless `force`.
    pub(crate) fn draw(&mut self, top: usize, force: bool) -> String {
        let clear = self.painter.clear_own();
        let mut text = String::new();
        let laid = self.layout();
        let (left, column) = self.column;
        let width = column.saturating_sub(PROMPT).max(10);
        for (i, row) in laid.rows.iter().enumerate() {
            text.push_str(&format!(
                "\x1b[{};1H\x1b[2K\x1b[{}G{row}",
                top + i,
                left + 1
            ));
        }
        // Rows under Reply, which a pane taller than it was may have.
        for row in top + laid.rows.len()..=self.rows {
            text.push_str(&format!("\x1b[{row};1H\x1b[2K"));
        }
        self.spans = laid
            .hits
            .into_iter()
            .map(|(row, cols, hit)| (top + row, cols, hit))
            .collect();
        // The menu goes right above the top rule, so the rules count as the input's.
        let (first, shown) = laid.input;
        text.push_str(&self.slash.draw(
            &mut self.painter,
            (top + first - 1, top + first + shown.max(1)),
            self.rows,
            left,
            column,
        ));
        let (r, c) = (top + laid.cursor.0, left + PROMPT + 1 + laid.cursor.1);
        let hint_room = width.saturating_sub(laid.cursor.1 + 1);
        if let Some(hint) = self.slash.hint().filter(|_| hint_room >= 4) {
            let hint = fit(hint, hint_room);
            text.push_str(&format!("\x1b[{r};{c}H{}{DIM}{hint}{RESET}", self.fill));
        }
        // The cursor hides while a button has focus, as it does on a chip in a new thread.
        let cursor = if self.focus.is_some() { "?25l" } else { "?25h" };
        self.parked = format!("\x1b[{r};{c}H\x1b[{cursor}");
        if !force && text == self.drawn {
            return self.parked.clone();
        }
        self.drawn = text.clone();
        format!("{clear}{text}{}", self.parked)
    }

    /// A click, at the pane's 0-based row and column: the menu's, else a chip's or a button's.
    /// Returns whether it landed on something.
    pub(crate) fn click(&mut self, m: &MouseEvent) -> bool {
        if self.slash.hits(m) {
            self.slash.click(&mut self.editor, m);
            return true;
        }
        let (row, col) = (m.row as usize + 1, m.column as usize + 1);
        let hit = self
            .spans
            .iter()
            .find(|(r, (a, b), _)| *r == row && (*a..=*b).contains(&col))
            .map(|(_, _, hit)| *hit);
        match (m.kind, hit) {
            (
                crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
                Some(hit),
            ) => {
                self.note = None;
                self.press(hit);
                true
            }
            _ => false,
        }
    }

    /// Pictures to send with the next message, made ready on the way.
    fn attach(&mut self, images: Vec<Image>) {
        let tx = self.tx.clone();
        self.images.add(images, move |ready| {
            let _ = tx.send(Done::Picture(ready));
        });
    }

    /// What a paste came to: pictures, or text to type.
    pub(crate) fn pasted(&mut self, got: Got) {
        match got {
            Got::Offer(request) => out(&request),
            Got::Text(text) => match paste::dropped(&text) {
                Some(got) => self.pasted(got),
                None => self
                    .editor
                    .paste(&text.replace("\r\n", "\n").replace('\r', "\n")),
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
            // The `/` commands are actions.rs's. Anything else starting with `/`, such as a
            // path, is a message.
            && crate::actions::is_slash(command.split_whitespace().next().unwrap_or(""))
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
        let deleting = std::mem::take(&mut self.deleting);
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
            "stop" | "force-stop" => {
                let run = self
                    .state
                    .as_ref()
                    .and_then(|s| s.active_run())
                    .and_then(|r| r["id"].as_str())
                    .map(str::to_string);
                if name == "force-stop" {
                    return self.spawn(async move {
                        t3::force_stop(&client, &thread, run.as_deref()).await
                    });
                }
                if run.is_none() && !client.has_capability("threadStop") {
                    return self.note("Nothing is running", false);
                }
                self.spawn(async move {
                    Ok(match t3::stop(&client, &thread, run.as_deref()).await? {
                        true => "Stopped".into(),
                        false => "Nothing is running".into(),
                    })
                })
            }
            // Alone, `/rename` puts the title in the input to edit.
            "rename" if arg.is_empty() => {
                let title = self
                    .state
                    .as_ref()
                    .and_then(|s| s.thread()["title"].as_str().map(str::to_string))
                    .unwrap_or_default();
                self.editor.insert(&format!("/rename {title}"));
            }
            "rename" => {
                let title = arg.to_string();
                self.spawn(async move {
                    t3::rename(&client, &thread, Some(&title)).await?;
                    Ok("Renamed".into())
                })
            }
            "retitle" => self.spawn(async move {
                t3::rename(&client, &thread, None).await?;
                Ok("T3 is writing a title".into())
            }),
            "fork" => {
                let title = (!arg.is_empty()).then(|| arg.to_string());
                let env = client.id().to_string();
                self.spawn(async move {
                    let new = t3::fork(&client, &thread, title.as_deref()).await?;
                    let key = t3client::hub::ThreadRef::new(&env, &new).to_string();
                    crate::find::go_to(&key, None).await?;
                    Ok("Forked".into())
                })
            }
            // Deleting can't be undone, so it takes `/delete` twice in a row.
            "delete" if !deleting => {
                self.deleting = true;
                self.note("Deleting can't be undone. /delete again to delete", true);
            }
            "delete" => self.spawn(lifecycle(Lifecycle::Delete, "Deleted")),
            "reply" => match crate::reply::apply(arg) {
                Ok(note) => self.note(note, false),
                Err(e) => self.note(format!("{e:#}"), true),
            },
            "width" => match crate::width::apply(arg) {
                Ok(note) => {
                    self.width_set = Some(crate::width::current().0);
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
            "pr" if matches!(arg, "" | "create") => {
                let key = format!("{}/{}", self.client.id(), self.thread_id);
                let create = arg == "create";
                match std::env::var("REX_BLOCK") {
                    Ok(block) => self.spawn(async move {
                        crate::pr::open(&key, &block, create).await?;
                        Ok(String::new())
                    }),
                    Err(_) => self.note("Run this in Rex", true),
                }
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

    /// Esc twice clears what's typed and its pictures; Cmd+Z brings the text back. On an empty
    /// input one Esc stops the run, as in Claude Code and Codex, but not an Esc that follows
    /// the two that cleared it.
    fn escape(&mut self, again: bool) {
        if self.editor.text.is_empty() && self.images.is_empty() {
            if !again
                && self
                    .state
                    .as_ref()
                    .is_some_and(|s| s.active_run().is_some())
            {
                self.command("stop");
            }
        } else if again {
            self.editor.clear();
            self.images.clear();
        } else {
            self.note(Escape::NOTE, false);
            self.note_until = Some(tokio::time::Instant::now() + Escape::AGAIN);
        }
    }

    /// The messages sent on the thread, oldest first, for Up to bring back.
    fn sent(&self) -> Vec<String> {
        let mut sent: Vec<String> = self
            .state
            .as_ref()
            .map(|s| {
                s.items()
                    .into_iter()
                    .filter(|i| i["type"] == "user_message")
                    .filter_map(|i| i["text"].as_str())
                    .filter(|t| !t.trim().is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        sent.dedup();
        sent
    }

    pub(crate) fn key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        self.note = None;
        let again = self.escape.again(&key);
        if self.focus.is_none() {
            match self.slash.key(&mut self.editor, key) {
                slash::Key::Pass => {}
                // An Esc that closes the menu isn't the first of two.
                slash::Key::Done => return self.escape = Escape::default(),
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
                self.escape = Escape::default();
                return;
            }
            (_, Some(_)) => self.focus = None,
            _ => {}
        }
        // Up at the start brings back what was sent before, and Down at the end goes forward
        // again, as in Claude Code and Codex.
        if key.modifiers.is_empty() {
            let edge = match key.code {
                KeyCode::Up => self.editor.cursor == 0,
                KeyCode::Down => self.editor.cursor == self.editor.text.len(),
                _ => false,
            };
            if edge {
                let sent = self.sent();
                if key.code == KeyCode::Up && self.editor.older(&sent)
                    || key.code == KeyCode::Down && self.editor.newer(&sent)
                {
                    return;
                }
            }
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
                self.editor.clear();
                self.images.clear();
            }
            KeyCode::Esc => self.escape(again),
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

/// The editing keys every input shares, as in a Mac text field: Alt+Enter, Shift+Enter or
/// Ctrl+J for a new line; Cmd and Option with the arrows and Delete; Cmd+Z and Cmd+Shift+Z;
/// and the Ctrl keys a Mac's text fields and a shell share (A, E, B, F, N, P, D, H, K, Y, T,
/// O, U, W, and Ctrl+_ to undo). Cmd arrives as SUPER, with the kitty keyboard protocol on
/// (`scheme::keys`). Returns whether the key was one of them.
pub(crate) fn edit(editor: &mut Editor, key: KeyEvent) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let cmd = key.modifiers.contains(KeyModifiers::SUPER);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    // What the key does to the text, for Undo to keep what was there before.
    let run = match key.code {
        KeyCode::Char(_) if !ctrl && !alt && !cmd => Some(Run::Typing),
        KeyCode::Backspace | KeyCode::Delete if !alt && !cmd => Some(Run::Deleting),
        KeyCode::Char('d' | 'h') if ctrl => Some(Run::Deleting),
        KeyCode::Enter if alt || shift => Some(Run::None),
        KeyCode::Backspace | KeyCode::Delete => Some(Run::None),
        KeyCode::Char('j' | 'k' | 'u' | 'w' | 'y' | 't' | 'o') if ctrl => Some(Run::None),
        KeyCode::Char('d') if alt => Some(Run::None),
        _ => None,
    };
    let before = run.map(|run| (editor.text.clone(), editor.checkpoint(run)));
    if run.is_none() {
        editor.last = Run::None;
    }
    match key.code {
        KeyCode::Char('z' | 'Z') if cmd && shift => {
            editor.redo();
        }
        KeyCode::Char('z') if cmd => {
            editor.undo();
        }
        KeyCode::Char('_' | '-') if ctrl => {
            editor.undo();
        }
        KeyCode::Enter if alt || shift => editor.insert("\n"),
        KeyCode::Char('j') if ctrl => editor.insert("\n"),
        KeyCode::Char('o') if ctrl => {
            editor.insert("\n");
            editor.cursor -= 1;
        }
        KeyCode::Char('u') if ctrl => editor.delete_to_line_start(),
        KeyCode::Backspace if cmd => editor.kill(editor.row_start(), editor.cursor),
        KeyCode::Delete if cmd => editor.kill(editor.cursor, editor.row_end()),
        KeyCode::Char('w') if ctrl => editor.delete_word(),
        KeyCode::Backspace if alt => editor.delete_word(),
        KeyCode::Delete if alt => editor.kill(editor.cursor, editor.word_right()),
        KeyCode::Char('d') if alt => editor.kill(editor.cursor, editor.word_right()),
        // To the end of the paragraph, or its line break when there already.
        KeyCode::Char('k') if ctrl => {
            let end = editor.line_end();
            editor.kill(
                editor.cursor,
                end.max(editor.cursor + 1).min(editor.text.len()),
            );
        }
        KeyCode::Char('y') if ctrl => {
            let killed: String = editor.killed.iter().collect();
            editor.insert(&killed);
        }
        // Swaps the characters either side of the cursor, or the last two at a line's end.
        KeyCode::Char('t') if ctrl => {
            let at = if editor.cursor == editor.line_end() {
                editor.cursor.saturating_sub(1)
            } else {
                editor.cursor
            };
            if at > 0
                && at < editor.text.len()
                && editor.text[at] != '\n'
                && editor.text[at - 1] != '\n'
            {
                editor.text.swap(at - 1, at);
                editor.cursor = at + 1;
            }
        }
        KeyCode::Char('d') if ctrl => editor.delete(),
        KeyCode::Char('h') if ctrl => editor.backspace(),
        KeyCode::Char('a') if ctrl => editor.cursor = editor.line_start(),
        KeyCode::Char('e') if ctrl => editor.cursor = editor.line_end(),
        KeyCode::Char('b') if ctrl => editor.cursor = editor.cursor.saturating_sub(1),
        KeyCode::Char('f') if ctrl => editor.cursor = (editor.cursor + 1).min(editor.text.len()),
        KeyCode::Char('p') if ctrl => editor.vertical(false),
        KeyCode::Char('n') if ctrl => editor.vertical(true),
        KeyCode::Home => editor.cursor = editor.line_start(),
        KeyCode::End => editor.cursor = editor.line_end(),
        KeyCode::Left if cmd => editor.cursor = editor.row_start(),
        KeyCode::Right if cmd => editor.cursor = editor.row_end(),
        KeyCode::Up if cmd => editor.cursor = 0,
        KeyCode::Down if cmd => editor.cursor = editor.text.len(),
        KeyCode::Up if alt => editor.paragraph_up(),
        KeyCode::Down if alt => editor.paragraph_down(),
        // Option and the arrows, and the `Esc b` and `Esc f` some keyboards send for them.
        KeyCode::Left if alt => editor.cursor = editor.word_left(),
        KeyCode::Right if alt => editor.cursor = editor.word_right(),
        KeyCode::Char('b') if alt => editor.cursor = editor.word_left(),
        KeyCode::Char('f') if alt => editor.cursor = editor.word_right(),
        KeyCode::Left => editor.cursor = editor.cursor.saturating_sub(1),
        KeyCode::Right => editor.cursor = (editor.cursor + 1).min(editor.text.len()),
        KeyCode::Up => editor.vertical(false),
        KeyCode::Down => editor.vertical(true),
        KeyCode::Backspace => editor.backspace(),
        KeyCode::Delete => editor.delete(),
        KeyCode::Char(c) if !ctrl && !alt && !cmd => {
            editor.insert(&c.to_string());
            // A word and the space after it undo together.
            if c.is_whitespace() {
                editor.last = Run::None;
            }
        }
        _ => return false,
    }
    if let Some((before, kept)) = before {
        editor.edited(&before, kept);
    }
    true
}

/// `text` as the rows `wrap` shows it: each character with its index, breaking at spaces or
/// newlines to fit `width` columns. A newline ends its row.
fn layout(text: &[char], width: usize) -> Vec<Vec<(usize, char)>> {
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
    rows
}

/// The row and column of `cursor` in `layout`'s rows: where its character finally is, or the
/// end of the last row.
fn locate(rows: &[Vec<(usize, char)>], cursor: usize) -> (usize, usize) {
    for (r, row) in rows.iter().enumerate() {
        let mut x = 0;
        for &(i, ch) in row {
            if i == cursor {
                return (r, x);
            }
            x += ch.width().unwrap_or(0);
        }
    }
    let last = rows.len() - 1;
    (
        last,
        rows[last].iter().map(|(_, c)| c.width().unwrap_or(0)).sum(),
    )
}

/// `text` broken into rows of at most `width` columns at spaces or newlines, and the cursor's
/// row and column.
pub(crate) fn wrap(text: &[char], cursor: usize, width: usize) -> (Vec<String>, (usize, usize)) {
    let rows = layout(text, width);
    let at = locate(&rows, cursor);
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

impl Composer {
    /// Reply for `thread_id`, and where its background work reports back. It asks for the
    /// machine's config and the providers' logos straight away.
    pub(crate) fn new(
        client: Arc<Env>,
        thread_id: String,
    ) -> (Composer, mpsc::UnboundedReceiver<Done>) {
        let (tx, done) = mpsc::unbounded_channel();
        {
            let (client, tx) = (client.clone(), tx.clone());
            tokio::spawn(async move {
                loop {
                    if let Ok(config) = client.server_config().await {
                        let providers = models::providers(&config).to_vec();
                        let _ = tx.send(Done::Config(config));
                        let _ = tx.send(Done::Logos(crate::new::logos(&providers).await));
                        return;
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            });
        }
        let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
        let slash = Slash::new(client.clone());
        let composer = Composer {
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
            column: (0, cols as usize),
            tx,
            logos: HashMap::new(),
            painter: Painter::tagged(),
            spans: Vec::new(),
            focus: None,
            slash,
            menu_rows: 0,
            images: Tray::default(),
            clipboard: paste::Reader::default(),
            deleting: false,
            escape: Escape::default(),
            width_set: None,
            parked: String::new(),
            drawn: String::new(),
            share: MOST_SHARE,
            wanted: 0,
            fill: String::new(),
        };
        (composer, done)
    }

    /// After the thread changes: answers to a question that's gone go too.
    pub(crate) fn thread_changed(&mut self) {
        if self.pending().is_none() {
            self.answers.clear();
        }
    }

    /// Whether nothing is typed or half-answered, so the pane can start again.
    pub(crate) fn idle(&self) -> bool {
        self.editor.text.is_empty() && self.answers.is_empty() && self.images.is_empty()
    }

    /// What background work came back with.
    pub(crate) fn done(&mut self, done: Done) {
        match done {
            Done::Note(text, error) => {
                if !text.is_empty() {
                    self.note(text, error)
                }
            }
            Done::Restore(text, images) => {
                if self.editor.text.is_empty() {
                    self.editor.insert(&text);
                }
                if self.images.is_empty() {
                    self.attach(images);
                }
            }
            Done::Picture(ready) => {
                if let Some(why) = self.images.ready(ready) {
                    self.note(why, true);
                }
            }
            Done::Config(config) => self.config = Some(config),
            Done::Link(url) => self.link = Some(url),
            Done::Logos(logos) => self.logos = logos,
        }
    }

    /// The logos again, for a screen that turned light or dark.
    pub(crate) fn reload_logos(&self) {
        if let Some(config) = self.config.as_ref() {
            let (tx, providers) = (self.tx.clone(), models::providers(config).to_vec());
            tokio::spawn(async move {
                let _ = tx.send(Done::Logos(crate::new::logos(&providers).await));
            });
        }
    }

    pub(crate) fn clear_note(&mut self) {
        self.note = None;
        self.note_until = None;
    }
}

#[cfg(test)]
mod tests {
    use super::{Editor, Escape, edit, wrap};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn press(ed: &mut Editor, code: KeyCode, mods: KeyModifiers) -> bool {
        edit(ed, KeyEvent::new(code, mods))
    }

    fn typed(ed: &mut Editor, text: &str) {
        for c in text.chars() {
            press(ed, KeyCode::Char(c), KeyModifiers::NONE);
        }
    }

    #[test]
    fn up_and_down_move_by_the_rows_on_screen() {
        let mut ed = Editor::default();
        ed.insert("hello there world");
        ed.width = 11;
        // "hello " over "there world", the cursor at the end.
        press(&mut ed, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(ed.cursor, 5);
        press(&mut ed, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(ed.cursor, 0, "Up from the first row goes to the start");
        press(&mut ed, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(ed.cursor, 6);
        press(&mut ed, KeyCode::Right, KeyModifiers::SUPER);
        assert_eq!(ed.cursor, 17);
        press(&mut ed, KeyCode::Left, KeyModifiers::SUPER);
        assert_eq!(
            ed.cursor, 6,
            "Cmd+Left goes to the start of the row, not the line"
        );
        press(&mut ed, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(ed.cursor, 17, "Down from the last row goes to the end");
        press(&mut ed, KeyCode::Char('p'), KeyModifiers::CONTROL);
        assert_eq!(ed.cursor, 5);
    }

    #[test]
    fn a_row_broken_inside_a_word_ends_after_it() {
        let mut ed = Editor::default();
        ed.insert("abcdefghij");
        ed.width = 5;
        ed.cursor = 0;
        press(&mut ed, KeyCode::Right, KeyModifiers::SUPER);
        assert_eq!(ed.cursor, 5);
        ed.cursor = 0;
        press(&mut ed, KeyCode::Delete, KeyModifiers::SUPER);
        assert_eq!(ed.string(), "fghij");
        // Up from the end of a full row stays on the row above.
        let mut ed = Editor::default();
        ed.insert("abcdefghij");
        ed.width = 5;
        press(&mut ed, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(ed.cursor, 4);
    }

    #[test]
    fn an_edit_that_changes_nothing_keeps_redo() {
        let mut ed = Editor::default();
        typed(&mut ed, "ab");
        press(&mut ed, KeyCode::Char('z'), KeyModifiers::SUPER);
        press(&mut ed, KeyCode::Char('k'), KeyModifiers::CONTROL);
        press(&mut ed, KeyCode::Backspace, KeyModifiers::NONE);
        press(
            &mut ed,
            KeyCode::Char('z'),
            KeyModifiers::SUPER | KeyModifiers::SHIFT,
        );
        assert_eq!(ed.string(), "ab");
        // Ctrl+T at a line's start leaves the line break where it is.
        let mut ed = Editor::default();
        ed.insert("ab\ncd");
        ed.cursor = 3;
        press(&mut ed, KeyCode::Char('t'), KeyModifiers::CONTROL);
        assert_eq!(ed.string(), "ab\ncd");
    }

    #[test]
    fn undo_takes_back_a_word_at_a_time_and_a_clear() {
        let mut ed = Editor::default();
        typed(&mut ed, "fix the build");
        press(&mut ed, KeyCode::Char('z'), KeyModifiers::SUPER);
        assert_eq!(ed.string(), "fix the ");
        press(&mut ed, KeyCode::Char('_'), KeyModifiers::CONTROL);
        assert_eq!(ed.string(), "fix ");
        press(
            &mut ed,
            KeyCode::Char('Z'),
            KeyModifiers::SUPER | KeyModifiers::SHIFT,
        );
        assert_eq!(ed.string(), "fix the ");
        ed.clear();
        assert_eq!(ed.string(), "");
        press(&mut ed, KeyCode::Char('z'), KeyModifiers::SUPER);
        assert_eq!(ed.string(), "fix the ", "Undo brings back a cleared input");
        // Sending starts over.
        ed.take();
        assert!(!ed.undo());
    }

    #[test]
    fn the_macs_control_keys() {
        let mut ed = Editor::default();
        ed.insert("one two\nthree");
        ed.cursor = 4;
        press(&mut ed, KeyCode::Char('k'), KeyModifiers::CONTROL);
        assert_eq!(ed.string(), "one \nthree");
        press(&mut ed, KeyCode::Char('k'), KeyModifiers::CONTROL);
        assert_eq!(
            ed.string(),
            "one three",
            "Ctrl+K at a line's end joins the next"
        );
        press(&mut ed, KeyCode::Char('e'), KeyModifiers::CONTROL);
        press(&mut ed, KeyCode::Char('y'), KeyModifiers::CONTROL);
        assert_eq!(ed.string(), "one three\n");
        press(&mut ed, KeyCode::Up, KeyModifiers::SUPER);
        assert_eq!(ed.cursor, 0);
        press(&mut ed, KeyCode::Char('f'), KeyModifiers::CONTROL);
        press(&mut ed, KeyCode::Char('t'), KeyModifiers::CONTROL);
        assert_eq!(ed.string(), "noe three\n");
        press(&mut ed, KeyCode::Char('d'), KeyModifiers::CONTROL);
        assert_eq!(ed.string(), "no three\n");
        press(&mut ed, KeyCode::Delete, KeyModifiers::ALT);
        assert_eq!(ed.string(), "no\n");
        ed.cursor = 0;
        press(&mut ed, KeyCode::Delete, KeyModifiers::SUPER);
        assert_eq!(ed.string(), "\n");
        press(&mut ed, KeyCode::Char('o'), KeyModifiers::CONTROL);
        assert_eq!((ed.string().as_str(), ed.cursor), ("\n\n", 0));
    }

    #[test]
    fn up_brings_back_what_was_sent_and_down_the_draft() {
        let sent = vec!["first".to_string(), "second".to_string()];
        let mut ed = Editor::default();
        ed.insert("draft");
        ed.cursor = 0;
        assert!(ed.older(&sent));
        assert_eq!(ed.string(), "second");
        assert!(ed.older(&sent));
        assert_eq!(ed.string(), "first");
        assert!(!ed.older(&sent), "nothing before the first");
        assert!(ed.newer(&sent));
        assert_eq!((ed.string().as_str(), ed.cursor), ("second", 6));
        assert!(ed.newer(&sent));
        assert_eq!(ed.string(), "draft");
        assert!(!ed.newer(&sent));
        // A message brought back and changed is a draft of its own.
        ed.cursor = 0;
        ed.older(&sent);
        ed.insert("re: ");
        assert!(!ed.newer(&sent));
        ed.cursor = 0;
        assert!(ed.older(&sent));
        assert!(ed.newer(&sent));
        assert_eq!(ed.string(), "re: second");
    }

    #[test]
    fn esc_twice_close_together() {
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        let mut escape = Escape::default();
        assert!(!escape.again(&esc));
        assert!(escape.again(&esc));
        escape.again(&KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        assert!(!escape.again(&esc), "a key in between starts over");
    }

    #[test]
    fn mac_editing_keys() {
        let mut ed = Editor::default();
        ed.insert("one two\nthree  four");
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
