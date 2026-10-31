//! The model picker: a floating layer over a thread's window, opened from the Reply pane with
//! /model, /effort or /mode alone, or ⌃⌘M. It lists every enabled provider's models under the
//! provider's logo, then the model's efforts, and the access modes. A choice applies to the
//! thread's next message at once, as T3's composer chips do, and the Reply header follows.

use std::collections::HashMap;
use std::io::Write;

use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, MouseButton, MouseEventKind};
use futures_util::StreamExt;
use serde_json::json;
use t3client::models;

use crate::chat::{RED, RESET, fit};
use crate::icons::Painter;
use crate::picker::{self, Chip, Menu, MenuKey, Settings};
use crate::rex::{Rex, lua_str};
use crate::t3;

fn out(text: &str) {
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(text.as_bytes());
    let _ = stdout.flush();
}

#[derive(Clone, Copy, PartialEq, Debug, clap::ValueEnum)]
pub enum Field {
    Model,
    Effort,
    Access,
}

/// Opens the picker for `thread` in a floating layer over the session's active window.
pub async fn float(session: &str, thread: &str, field: Field) -> Result<()> {
    let exe = crate::icons::real_exe().to_string_lossy().into_owned();
    let field = match field {
        Field::Model => "model",
        Field::Effort => "effort",
        Field::Access => "access",
    };
    let data = json!({"argv": [exe, "pick", thread, "--field", field]});
    let script = format!(
        r#"local d = rex.json.decode({})
local _, err = rex.session.new_layer{{ bounds = {{ 0.2, 0.12, 0.6, 0.76 }}, focus = true,
  layout = {{ block = {{ flavor = "com.superlogical.terminal.shell", label = "Model",
    options = {{ shell = "none", login = false, command = d.argv }} }} }} }}
rex.session.disown_block{{}}
if err then error(err, 0) end
return {{}}"#,
        lua_str(&data.to_string())
    );
    Rex::find().lua(session, &script).await.map(drop)
}

pub async fn run(thread: String, first: Field) -> Result<()> {
    let (client, thread) = t3::thread(&thread)?;
    let thread_id = thread.thread;
    let _scheme = crate::scheme::listen();
    let (config, state) =
        tokio::try_join!(client.server_config(), client.thread(&thread_id, true))?;
    let mut logos: HashMap<String, crate::icons::Icon> = HashMap::new();
    let mut settings = Settings::of_thread(&config, state.thread());
    let (tx, mut loaded) = tokio::sync::mpsc::unbounded_channel();
    {
        let providers = models::providers(&config).to_vec();
        tokio::spawn(async move {
            let _ = tx.send(crate::new::logos(&providers).await);
        });
    }
    let fields = |s: &Settings| {
        let mut f = vec![Field::Model];
        if !picker::effort_items(&config, s).is_empty() {
            f.push(Field::Effort);
        }
        f.push(Field::Access);
        f
    };
    let open = |field: Field, s: &Settings, logos: &HashMap<String, crate::icons::Icon>| {
        let (items, current) = match field {
            Field::Model => (
                picker::model_items(&config, logos),
                Some(format!("{}/{}", s.instance, s.model)),
            ),
            Field::Effort => (
                picker::effort_items(&config, s),
                picker::effort_now(&config, s),
            ),
            Field::Access => (picker::mode_items(&config, s), Some(s.mode.clone())),
        };
        Menu::new(items, current.as_deref())
    };
    let mut field = first;
    let mut menu = open(field, &settings, &logos);
    let mut painter = Painter;
    let mut note: Option<String> = None;
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let (mut cols, mut rows) = crossterm::terminal::size().unwrap_or((60, 20));
    crossterm::terminal::enable_raw_mode()?;
    out("\x1b]2;Model\x07\x1b[?1049h\x1b[?25l\x1b[?1000h\x1b[?1006h");
    let mut input = EventStream::new();
    let result: Result<()> = async {
        loop {
            // Draw: the chips, then the open menu.
            let width = (cols as usize).saturating_sub(4).max(20);
            let fs = fields(&settings);
            let chips: Vec<Chip> = fs
                .iter()
                .map(|f| match f {
                    Field::Model => Chip {
                        icon: logos.get(&settings.instance).cloned(),
                        label: settings.model_label(&config),
                    },
                    Field::Effort => Chip {
                        icon: None,
                        label: settings.effort_label(&config).unwrap_or_default(),
                    },
                    Field::Access => Chip { icon: None, label: settings.mode_label() },
                })
                .collect();
            let at = fs.iter().position(|f| *f == field);
            let (row, s) = picker::chips(&mut painter, &chips, None, at, 2);
            spans = s;
            let mut text = String::from("\x1b[?2026h\x1b[H\x1b[2J");
            text.push_str(Painter::clear());
            text.push_str(&format!("\x1b[2;1H  {row}"));
            let room = (rows as usize).saturating_sub(5).max(3);
            for (i, line) in menu.lines(&mut painter, 3, width, room).iter().enumerate() {
                text.push_str(&format!("\x1b[{};1H  {line}", i + 4));
            }
            if let Some(n) = &note {
                text.push_str(&format!("\x1b[{rows};1H  {RED}{}{RESET}", fit(n, width)));
            }
            text.push_str("\x1b[?2026l");
            out(&text);

            let chosen = tokio::select! {
                Some(l) = loaded.recv() => {
                    logos = l;
                    menu.items = open(field, &settings, &logos).items;
                    None
                }
                event = input.next() => match event {
                    Some(Ok(Event::Key(key))) if key.kind != KeyEventKind::Release => {
                        note = None;
                        let fs = fields(&settings);
                        let i = fs.iter().position(|f| *f == field).unwrap_or(0);
                        match key.code {
                            KeyCode::Left if menu.filter.is_empty() => {
                                field = fs[i.saturating_sub(1)];
                                menu = open(field, &settings, &logos);
                                None
                            }
                            KeyCode::Right | KeyCode::Tab if menu.filter.is_empty() => {
                                field = fs[(i + 1) % fs.len()];
                                menu = open(field, &settings, &logos);
                                None
                            }
                            _ => match menu.key(key) {
                                MenuKey::Chose(id) => Some(id),
                                MenuKey::Close => return Ok(()),
                                MenuKey::Moved => None,
                            },
                        }
                    }
                    Some(Ok(Event::Mouse(m))) if matches!(m.kind, MouseEventKind::Down(MouseButton::Left)) => {
                        let col = m.column as usize + 1;
                        if m.row == 1 && let Some(i) = spans.iter().position(|(a, b)| (*a..=*b).contains(&col)) {
                            field = fields(&settings)[i];
                            menu = open(field, &settings, &logos);
                            None
                        } else {
                            menu.click(m.row)
                        }
                    }
                    Some(Ok(Event::Resize(c, r))) => {
                        cols = c;
                        rows = r;
                        None
                    }
                    Some(Err(e)) => return Err(e.into()),
                    None => return Ok(()),
                    _ => None,
                },
            };
            let Some(id) = chosen else { continue };
            let mut next = settings.clone();
            match field {
                Field::Model => next.set_model(&config, &id),
                Field::Effort => next.effort = Some(id),
                Field::Access => next.mode = id,
            }
            match models::plan(&config, state.thread(), &next.choice()) {
                Ok(plan) => match client.apply_settings(&state, &plan).await {
                    Ok(_) => {
                        settings = next;
                        // After a model, its efforts; after anything else, done.
                        if field == Field::Model && fields(&settings).contains(&Field::Effort) {
                            field = Field::Effort;
                            menu = open(field, &settings, &logos);
                        } else {
                            return Ok(());
                        }
                    }
                    Err(e) => note = Some(format!("{e:#}")),
                },
                Err(e) => note = Some(format!("{e:#}")),
            }
        }
    }
    .await;
    out("\x1b[?1006l\x1b[?1000l\x1b[?25h\x1b[?1049l");
    let _ = crossterm::terminal::disable_raw_mode();
    result
}
