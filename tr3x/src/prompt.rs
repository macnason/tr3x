//! One question in a floating layer, for the palette's actions that ask before they act:
//! Rename Thread… edits the title in place, Delete Thread… asks to be sure.

use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures_util::StreamExt;

use crate::chat::{BLUE, BOLD, DIM, RED, RESET, fit};
use crate::compose::{Editor, edit};
use crate::term::out;

/// What came of the question.
pub enum Answer {
    Text(String),
    Yes,
    /// Esc, or No.
    Cancel,
}

/// Edits `initial` on one line. Enter answers with the text, Esc cancels. `confirm` asks a
/// yes-or-no question instead, with `initial` as its subject: y or Enter on Yes answers.
pub async fn ask(title: &str, initial: &str, confirm: bool) -> Result<Answer> {
    let _scheme = crate::scheme::listen();
    let mut editor = Editor::default();
    if !confirm {
        editor.insert(initial);
    }
    let mut note: Option<String> = None;
    crossterm::terminal::enable_raw_mode()?;
    out(&format!("\x1b]2;{title}\x07\x1b[?1049h"));
    let keys = crate::scheme::keys();
    let mut input = EventStream::new();
    let result: Result<Answer> = async {
        loop {
            let (cols, rows) = crossterm::terminal::size().unwrap_or((60, 10));
            let width = (cols as usize).saturating_sub(4).max(20);
            let mut text = String::from("\x1b[?2026h\x1b[H\x1b[2J");
            text.push_str(&format!("\x1b[2;3H{BOLD}{}{RESET}", fit(title, width)));
            if confirm {
                text.push_str(&format!("\x1b[4;3H{}", fit(initial, width)));
                text.push_str(&format!(
                    "\x1b[6;3H{RED}{BOLD}y{RESET} Delete   {DIM}n or Esc{RESET} Keep"
                ));
                text.push_str("\x1b[?25l");
            } else {
                // The line scrolls to keep the cursor in view.
                let shown: String = editor.text.iter().collect();
                let start = editor.cursor.saturating_sub(width.saturating_sub(4));
                let visible: String = shown.chars().skip(start).take(width - 2).collect();
                let before: String = editor.text[start..editor.cursor].iter().collect();
                let cursor = 5 + unicode_width::UnicodeWidthStr::width(before.as_str());
                text.push_str(&format!("\x1b[4;3H{BLUE}{BOLD}›{RESET} {visible}"));
                if let Some(n) = &note {
                    text.push_str(&format!("\x1b[{rows};3H{RED}{}{RESET}", fit(n, width)));
                } else {
                    text.push_str(&format!("\x1b[6;3H{DIM}Enter saves · Esc cancels{RESET}"));
                }
                text.push_str(&format!("\x1b[4;{cursor}H\x1b[?25h"));
            }
            text.push_str("\x1b[?2026l");
            out(&text);
            let Some(Ok(event)) = input.next().await else {
                return Ok(Answer::Cancel);
            };
            let Event::Key(key) = event else { continue };
            if key.kind == KeyEventKind::Release {
                continue;
            }
            let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
            note = None;
            if confirm {
                match key.code {
                    KeyCode::Char('y' | 'Y') => return Ok(Answer::Yes),
                    KeyCode::Char('n' | 'N') | KeyCode::Esc => return Ok(Answer::Cancel),
                    KeyCode::Char('c') if ctrl => return Ok(Answer::Cancel),
                    _ => {}
                }
                continue;
            }
            match key.code {
                KeyCode::Esc => return Ok(Answer::Cancel),
                KeyCode::Char('c') if ctrl => return Ok(Answer::Cancel),
                KeyCode::Enter => {
                    let text = editor.string();
                    if text.trim().is_empty() {
                        note = Some("A title can't be empty".into());
                    } else {
                        return Ok(Answer::Text(text.trim().to_string()));
                    }
                }
                // One line: the keys that make another do nothing.
                KeyCode::Char('j' | 'o') if ctrl => {}
                _ => {
                    edit(&mut editor, key);
                }
            }
        }
    }
    .await;
    drop(keys);
    out("\x1b[?25h\x1b[?1049l");
    let _ = crossterm::terminal::disable_raw_mode();
    result
}
