//! Find Thread: every thread on every machine in one list, in a floating layer, to get
//! back what Rex no longer shows. Typing filters it, each word by its letters in order. The
//! list is grouped Open, Snoozed, Settled and Archived, newest first, with the project of an
//! open thread, when a snoozed one wakes, or how long ago the rest were put away.
//!
//! Enter brings the thread back: it unarchives, wakes or unsettles it, as it needs, and asks
//! sync to show a window it hid. Sync makes the tab, and the finder goes to it. Option+Enter
//! (or Cmd+Enter, where Rex passes it on) opens the thread in T3. Reopen Last Settled
//! brings back the thread settled last.
//!
//! The shell lists open, snoozed and recently settled threads. Older settled threads come from
//! `orchestration.getOlderSettledThreads` a page at a time, and archived ones from
//! `orchestration.getArchivedShellSnapshot`.

use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use crossterm::event::{
    Event, EventStream, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use futures_util::StreamExt;
use serde_json::{Value, json};
use t3client::env::Env;
use t3client::hub::ThreadRef;
use tokio::sync::mpsc;

use crate::chat::{BLUE, BOLD, DIM, RED, RESET, fit};
use crate::icons::Painter;
use crate::picker::{Item, Menu, MenuKey};
use crate::rex::Rex;
use crate::state::State;
use crate::t3::{self, Lifecycle};

fn out(text: &str) {
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(text.as_bytes());
    let _ = stdout.flush();
}

/// Where a thread is, as the finder groups them, in the order it lists them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Group {
    Open,
    Snoozed,
    Settled,
    Archived,
}

impl Group {
    fn of(thread: &Value, now: i64) -> Group {
        if !thread["archivedAt"].is_null() {
            Group::Archived
        } else if t3client::inbox::effective_snoozed(thread, now) {
            Group::Snoozed
        } else if thread["settledOverride"] == "settled" {
            Group::Settled
        } else {
            Group::Open
        }
    }

    fn title(self) -> &'static str {
        match self {
            Group::Open => "Open",
            Group::Snoozed => "Snoozed",
            Group::Settled => "Settled",
            Group::Archived => "Archived",
        }
    }
}

/// One thread the finder lists.
struct Found {
    env: Arc<Env>,
    thread: Value,
    group: Group,
    /// The project's title, and the machine's name once there's more than one.
    project: String,
}

impl Found {
    fn key(&self) -> String {
        format!(
            "{}/{}",
            self.env.id(),
            self.thread["id"].as_str().unwrap_or_default()
        )
    }

    /// What the list sorts on, newest first: the latest change, the wake (soonest first, as a
    /// negative), or when it was settled or archived.
    fn when(&self) -> i64 {
        let at = |key: &str| t3client::inbox::parse_iso(self.thread[key].as_str().unwrap_or(""));
        match self.group {
            Group::Open => at("updatedAt").unwrap_or(0),
            Group::Snoozed => -at("snoozedUntil").unwrap_or(0),
            Group::Settled => t3client::inbox::settled_ms(&self.thread),
            Group::Archived => at("archivedAt").unwrap_or(0),
        }
    }

    /// The right column: the project, when it wakes, or how long ago it was put away.
    fn detail(&self, now: i64, offset_minutes: i64) -> String {
        match self.group {
            Group::Open => self.project.clone(),
            Group::Snoozed => until(-self.when(), now, offset_minutes),
            _ => ago(self.when(), now),
        }
    }
}

/// `until 3pm`, `until tomorrow 9am`, `until Tue 9am` or `until 12 Nov`, in local time.
fn until(at: i64, now: i64, offset_minutes: i64) -> String {
    format!("until {}", crate::chat::when(at, now, offset_minutes))
}

/// `just now`, `5m ago`, `3h ago`, `2d ago`, `6w ago`, `1y ago`.
fn ago(at: i64, now: i64) -> String {
    let minutes = (now - at).max(0) / 60_000;
    match minutes {
        0 => "just now".into(),
        1..=59 => format!("{minutes}m ago"),
        60..=1439 => format!("{}h ago", minutes / 60),
        1440..=20159 => format!("{}d ago", minutes / 1440),
        20160..=525_599 => format!("{}w ago", minutes / 10080),
        _ => format!("{}y ago", minutes / 525_600),
    }
}

/// The finder's list: by group, newest first in each.
fn items(found: &[Found], now: i64, offset_minutes: i64) -> Vec<Item> {
    let mut order: Vec<&Found> = found.iter().collect();
    order.sort_by_key(|f| (f.group, std::cmp::Reverse(f.when())));
    order
        .into_iter()
        .map(|f| {
            let mut item = Item::new(
                f.key(),
                crate::sync::label(f.thread["title"].as_str().unwrap_or("")),
            );
            item.detail = f.detail(now, offset_minutes);
            item.section = Some(f.group.title().into());
            item
        })
        .collect()
}

/// A page of threads from one machine, as the finder loads them.
enum Loaded {
    Threads(Arc<Env>, Vec<Value>, Vec<Value>),
    Failed(String),
    Done,
}

/// Every thread on `env`: the shell's, then archived ones, then older settled ones a page at a
/// time, each sent as it comes. Projects come with the shell and the archive.
async fn load(env: Arc<Env>, tx: mpsc::UnboundedSender<Loaded>) {
    let name = env.name().to_string();
    let send = |threads: Vec<Value>, projects: Vec<Value>| {
        let _ = tx.send(Loaded::Threads(env.clone(), threads, projects));
    };
    match env.shell().await {
        Ok(shell) => send(shell.threads, shell.projects),
        Err(e) => {
            let _ = tx.send(Loaded::Failed(format!("{name}: {e:#}")));
            return;
        }
    }
    if let Ok(archive) = env
        .call("orchestration.getArchivedShellSnapshot", json!({}))
        .await
    {
        let list = |key: &str| archive[key].as_array().cloned().unwrap_or_default();
        send(list("threads"), list("projects"));
    }
    // Older settled threads, newest first, 50 at a time, up to a thousand.
    let mut before = json!({"settledAt": t3::iso(t3::now_ms())});
    for _ in 0..20 {
        let Ok(page) = env
            .call(
                "orchestration.getOlderSettledThreads",
                json!({"before": before, "limit": 50}),
            )
            .await
        else {
            break;
        };
        let threads = page["threads"].as_array().cloned().unwrap_or_default();
        let last = threads.last().cloned();
        let full = threads.len() == 50;
        send(threads, Vec::new());
        match last {
            Some(last) if full && last["settledAt"].is_string() => {
                before = json!({"settledAt": last["settledAt"], "threadId": last["id"]});
            }
            _ => break,
        }
    }
    let _ = tx.send(Loaded::Done);
}

/// How a choice turned out.
enum Outcome {
    /// Done: the finder closes.
    Close,
    Failed(String),
    /// The thread's link, to click on the device the finder is on.
    Link(String),
}

/// Brings `thread` back where sync shows it: out of the archive, awake and unsettled, as it
/// needs, and no longer hidden by a closed window. Then goes to its tab, for `client`.
async fn bring_back(env: &Env, thread: &Value, client: Option<&str>) -> Result<()> {
    let id = thread["id"].as_str().context("a thread without an id")?;
    if !thread["archivedAt"].is_null() {
        let command = json!({"type": "thread.unarchive", "commandId": format!("t3rex:{}", uuid::Uuid::new_v4()), "threadId": id});
        env.dispatch(command).await?;
    }
    if t3client::inbox::effective_snoozed(thread, t3::now_ms()) {
        env.dispatch(t3::command(id, &Lifecycle::Wake)).await?;
    }
    if thread["settledOverride"] == "settled" {
        env.dispatch(t3::command(id, &Lifecycle::Unsettle)).await?;
    }
    let key = ThreadRef::new(env.id(), id).to_string();
    crate::sync::restore(&key)?;
    go_to(&key, client).await
}

/// Waits for sync to give `key` a window, then selects it on `client`, else on every app.
pub async fn go_to(key: &str, client: Option<&str>) -> Result<()> {
    for _ in 0..60 {
        let state = State::load();
        let found = state
            .groups
            .values()
            .find_map(|g| Some((g.session_id.clone()?, g.windows.get(key)?.window_id.clone())));
        if let Some((session, window)) = found {
            let rex = Rex::find();
            let pairs = [
                ("session_id", session.as_str()),
                ("window_id", window.as_str()),
            ];
            return match client {
                Some(client) => {
                    let kv: Vec<String> = pairs.iter().map(|(k, v)| format!("{k}={v}")).collect();
                    let mut args = vec!["-C", client, "do", "session.select"];
                    args.extend(kv.iter().map(String::as_str));
                    let _ = tokio::process::Command::new(&rex.bin)
                        .args(crate::rex::NO_AUTOSTART)
                        .args(&args)
                        .output()
                        .await;
                    Ok(())
                }
                None => rex.client_do("session.select", &pairs).await,
            };
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    bail!("sync hasn't made its tab yet");
}

/// Whether `client` is an app on this Mac, so opening T3 Code here opens it in front of them.
async fn is_here(client: Option<&str>) -> bool {
    let Some(client) = client else {
        return crate::link::only_here().await;
    };
    let Ok(list) = Rex::find().call("client.list", "{}").await else {
        return false;
    };
    list["clients"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|c| c["client_id"] == client)
        .is_some_and(|c| c["principal"]["transport"] == "unix")
}

/// Reopen Last Settled: the thread settled last, on any machine, back in its tab.
pub async fn reopen_last(client: Option<String>) -> Result<()> {
    let hub = t3::hub();
    let mut last: Option<(i64, Arc<Env>, Value)> = None;
    for env in hub.envs() {
        let mut threads = match env.shell().await {
            Ok(shell) => shell.threads,
            Err(_) => continue,
        };
        let before = json!({"settledAt": t3::iso(t3::now_ms())});
        if let Ok(page) = env
            .call(
                "orchestration.getOlderSettledThreads",
                json!({"before": before, "limit": 1}),
            )
            .await
        {
            threads.extend(page["threads"].as_array().cloned().unwrap_or_default());
        }
        for thread in threads {
            if thread["settledOverride"] != "settled"
                || !thread["archivedAt"].is_null()
                || thread["lineage"]["relationshipToParent"] == "subagent"
            {
                continue;
            }
            let at = t3client::inbox::settled_ms(&thread);
            if last.as_ref().is_none_or(|(best, _, _)| at > *best) {
                last = Some((at, env.clone(), thread));
            }
        }
    }
    let (_, env, thread) = last.context("no settled thread to reopen")?;
    bring_back(&env, &thread, client.as_deref()).await
}

pub async fn run(client: Option<String>) -> Result<()> {
    let _scheme = crate::scheme::listen();
    let hub = t3::hub();
    let badges = hub.paired() >= 2;
    let home = hub.home_id().map(str::to_string);
    let offset_minutes = crate::chat::local_offset_minutes();
    let (tx, mut loaded) = mpsc::unbounded_channel();
    let mut loading = hub.envs().len();
    for env in hub.envs() {
        tokio::spawn(load(env.clone(), tx.clone()));
    }
    drop(tx);
    // Each thread once, by `env/thread`; what the shell said first wins over older pages.
    let mut found: Vec<Found> = Vec::new();
    let mut at: HashMap<String, usize> = HashMap::new();
    let mut projects: HashMap<(String, String), String> = HashMap::new();
    let mut menu = Menu::default();
    (menu.fuzzy, menu.details_right, menu.hide_filter) = (true, true, true);
    let mut painter = Painter::default();
    let mut note: Option<(String, bool)> = None;
    let mut busy = false;
    let (done_tx, mut done) = mpsc::unbounded_channel::<Outcome>();
    let (mut cols, mut rows) = crossterm::terminal::size().unwrap_or((60, 20));
    crossterm::terminal::enable_raw_mode()?;
    out("\x1b]2;Find Thread\x07\x1b[?1049h\x1b[?1000h\x1b[?1006h");
    let keys = crate::scheme::keys();
    let mut input = EventStream::new();
    let result: Result<()> = async {
        loop {
            // Draw: the filter, then the list, then what's happening.
            let width = (cols as usize).saturating_sub(4).max(20);
            let mut text = String::from("\x1b[?2026h\x1b[H\x1b[2J");
            text.push_str(Painter::clear());
            let room = (rows as usize).saturating_sub(4).max(3);
            for (i, line) in menu.lines(&mut painter, 2, width, room).iter().enumerate() {
                text.push_str(&format!("\x1b[{};1H  {line}", i + 3));
            }
            let status = match (&note, loading) {
                (Some((n, error)), _) => {
                    let color = if *error { RED } else { DIM };
                    // A link is drawn whole; fitting it would cut its escape codes.
                    let n = if n.starts_with('\x1b') { n.clone() } else { fit(n, width) };
                    Some(format!("{color}{n}{RESET}"))
                }
                (None, 0) => None,
                (None, _) => Some(format!("{DIM}Loading…{RESET}")),
            };
            if let Some(status) = status {
                text.push_str(&format!("\x1b[{rows};1H  {status}"));
            }
            let filter = fit(&menu.filter, width.saturating_sub(2));
            text.push_str(&format!("\x1b[1;1H  {BLUE}{BOLD}›{RESET} {filter}"));
            let cursor = 5 + unicode_width::UnicodeWidthStr::width(filter.as_str());
            text.push_str(&format!("\x1b[1;{cursor}H\x1b[?25h\x1b[?2026l"));
            out(&text);

            let chosen: Option<(String, bool)> = tokio::select! {
                page = loaded.recv(), if loading > 0 => {
                    match page {
                        Some(Loaded::Threads(env, threads, more)) => {
                            for p in more {
                                if let (Some(id), Some(title)) = (p["id"].as_str(), p["title"].as_str()) {
                                    projects.insert((env.id().to_string(), id.to_string()), title.to_string());
                                }
                            }
                            let now = t3::now_ms();
                            for thread in threads {
                                if thread["lineage"]["relationshipToParent"] == "subagent"
                                    || !thread["deletedAt"].is_null()
                                {
                                    continue;
                                }
                                let project = projects
                                    .get(&(env.id().to_string(), thread["projectId"].as_str().unwrap_or_default().to_string()))
                                    .cloned()
                                    .unwrap_or_default();
                                // As in a tab's label, only another machine's threads say which it is.
                                let badge = badges && Some(env.id()) != home.as_deref();
                                let project = match (badge, project.is_empty()) {
                                    (true, true) => env.name().to_string(),
                                    (true, false) => format!("{project} · {}", env.name()),
                                    _ => project,
                                };
                                let f = Found { env: env.clone(), group: Group::of(&thread, now), thread, project };
                                if let std::collections::hash_map::Entry::Vacant(slot) = at.entry(f.key()) {
                                    slot.insert(found.len());
                                    found.push(f);
                                }
                            }
                        }
                        Some(Loaded::Failed(e)) => {
                            loading -= 1;
                            note = Some((e, true));
                        }
                        Some(Loaded::Done) | None => loading = loading.saturating_sub(1),
                    }
                    // Keep the selection on the same thread as the list grows.
                    let selected = menu.shown().get(menu.selected).map(|&i| menu.items[i].id.clone());
                    menu.items = items(&found, t3::now_ms(), offset_minutes);
                    if let Some(id) = selected
                        && let Some(n) = menu.shown().iter().position(|&i| menu.items[i].id == id)
                    {
                        menu.selected = n;
                    }
                    None
                }
                Some(outcome) = done.recv() => {
                    busy = false;
                    match outcome {
                        Outcome::Close => return Ok(()),
                        Outcome::Failed(e) => note = Some((e, true)),
                        Outcome::Link(url) => note = Some((crate::link::osc8(&url, "Open in T3 ↗"), false)),
                    }
                    None
                }
                event = input.next() => match event {
                    Some(Ok(Event::Key(key))) if key.kind != KeyEventKind::Release && !busy => {
                        note = None;
                        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                        let alt = key.modifiers.contains(KeyModifiers::ALT);
                        let cmd = key.modifiers.contains(KeyModifiers::SUPER);
                        let selected = || menu.shown().get(menu.selected).map(|&i| menu.items[i].id.clone());
                        match key.code {
                            // Rex keeps Cmd+Enter for itself, so Option+Enter does the same.
                            KeyCode::Enter => selected().map(|id| (id, cmd || alt)),
                            // Backspace on an empty filter leaves the finder open.
                            KeyCode::Backspace if menu.filter.is_empty() => None,
                            KeyCode::Backspace if cmd => {
                                menu.filter.clear();
                                None
                            }
                            KeyCode::Char('u') if ctrl => {
                                menu.filter.clear();
                                None
                            }
                            KeyCode::Backspace if alt => {
                                let kept = menu.filter.trim_end().rfind(' ').map_or(0, |i| i + 1);
                                menu.filter.truncate(kept);
                                None
                            }
                            KeyCode::Char('w') if ctrl => {
                                let kept = menu.filter.trim_end().rfind(' ').map_or(0, |i| i + 1);
                                menu.filter.truncate(kept);
                                None
                            }
                            _ => match menu.key(key) {
                                MenuKey::Close => return Ok(()),
                                _ => None,
                            },
                        }
                    }
                    Some(Ok(Event::Paste(text))) => {
                        menu.filter.push_str(&text.replace(['\r', '\n'], " "));
                        menu.selected = 0;
                        None
                    }
                    Some(Ok(Event::Mouse(m))) if matches!(m.kind, MouseEventKind::Down(MouseButton::Left)) && !busy => {
                        menu.click(m.row).map(|id| (id, false))
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
            let Some((id, in_t3)) = chosen else { continue };
            let Some(f) = at.get(&id).map(|&i| &found[i]) else { continue };
            let (env, thread, client, done_tx) = (f.env.clone(), f.thread.clone(), client.clone(), done_tx.clone());
            busy = true;
            if in_t3 {
                note = Some(("Opening in T3…".into(), false));
                tokio::spawn(async move {
                    let outcome = if is_here(client.as_deref()).await {
                        match crate::install::open_in_t3(&id).await {
                            Ok(()) => Outcome::Close,
                            Err(e) => Outcome::Failed(format!("{e:#}")),
                        }
                    } else {
                        // Opening here would open it on the wrong screen: a link that device opens.
                        let config = env.server_config().await.ok();
                        let thread_id = thread["id"].as_str().unwrap_or_default();
                        Outcome::Link(crate::link::thread(config.as_ref(), env.id(), thread_id))
                    };
                    let _ = done_tx.send(outcome);
                });
            } else {
                note = Some(("Bringing it back…".into(), false));
                tokio::spawn(async move {
                    let outcome = match bring_back(&env, &thread, client.as_deref()).await {
                        Ok(()) => Outcome::Close,
                        Err(e) => Outcome::Failed(format!("{e:#}")),
                    };
                    let _ = done_tx.send(outcome);
                });
            }
        }
    }
    .await;
    drop(keys);
    out("\x1b[?1006l\x1b[?1000l\x1b[?1049l");
    let _ = crossterm::terminal::disable_raw_mode();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_read_as_the_list_shows_them() {
        let h = 3_600_000;
        // 2026-10-31T00:00:00Z, at UTC.
        let now = t3client::inbox::parse_iso("2026-10-31T00:00:00.000Z").unwrap();
        assert_eq!(until(now + 15 * h, now, 0), "until 3pm");
        assert_eq!(until(now + 15 * h + 30 * 60_000, now, 0), "until 3:30pm");
        assert_eq!(until(now + 33 * h, now, 0), "until tomorrow 9am");
        assert_eq!(until(now + 3 * 24 * h, now, 0), "until Tue 12am");
        assert_eq!(until(now + 12 * 24 * h + 9 * h, now, 0), "until 12 Nov");
        // Local time: ten hours ahead, 3pm UTC is 1am the next day.
        assert_eq!(until(now + 15 * h, now, 600), "until tomorrow 1am");
        // A wake that passed while the list was open, on an earlier day.
        assert_eq!(until(now - 9 * h, now, 0), "until yesterday 3pm");
        assert_eq!(ago(now - 30_000, now), "just now");
        assert_eq!(ago(now - 5 * 60_000, now), "5m ago");
        assert_eq!(ago(now - 3 * h, now), "3h ago");
        assert_eq!(ago(now - 50 * h, now), "2d ago");
        assert_eq!(ago(now - 15 * 24 * h, now), "2w ago");
    }

    #[test]
    fn groups() {
        let now = t3client::inbox::parse_iso("2026-10-31T00:00:00.000Z").unwrap();
        assert_eq!(Group::of(&json!({}), now), Group::Open);
        assert_eq!(
            Group::of(&json!({"settledOverride": "settled"}), now),
            Group::Settled
        );
        assert_eq!(
            Group::of(&json!({"snoozedUntil": "2026-11-01T00:00:00.000Z"}), now),
            Group::Snoozed
        );
        assert_eq!(
            Group::of(
                &json!({"archivedAt": "2026-10-01T00:00:00.000Z", "settledOverride": "settled"}),
                now
            ),
            Group::Archived
        );
    }
}
