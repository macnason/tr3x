//! Keeps one Rex session, "T3 Code", holding one window per open T3 thread, in T3's order:
//! what needs you, then the rest of the inbox, then Working. A settled, snoozed or archived
//! thread's window closes, and comes back when T3 brings the thread back.
//!
//! T3 is the source of truth. t3rex follows T3's shell subscription and never decides a
//! thread's state itself. It only touches the session and windows its state file names.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{Value, json};
use t3term::client::WatchEvent;
use t3term::projection::ShellState;
use t3term::tui::{Inbox, Place};

use crate::rex::{Rex, lua_str};
use crate::state::{State, Window};
use crate::t3;

pub const SESSION_LABEL: &str = "T3 Code";

/// One window t3rex wants, in order.
#[derive(Debug, Clone, PartialEq)]
pub struct Want {
    pub thread_id: String,
    pub label: String,
    pub cwd: Option<String>,
    pub updated_at: String,
    pub place: Place,
}

/// The windows T3's shell asks for at `now`.
pub fn wanted(shell: &ShellState, inbox: &mut Inbox, caps: t3term::tui::Capabilities, now: i64) -> Vec<Want> {
    let roots: HashMap<&str, &str> = shell
        .projects
        .iter()
        .filter_map(|p| Some((p["id"].as_str()?, p["workspaceRoot"].as_str()?)))
        .collect();
    inbox
        .order(&shell.threads, caps, now)
        .into_iter()
        .filter_map(|(thread, place)| {
            let id = thread["id"].as_str()?.to_string();
            let cwd = thread["worktreePath"]
                .as_str()
                .or_else(|| roots.get(thread["projectId"].as_str()?).copied())
                .filter(|dir| std::path::Path::new(dir).is_dir())
                .map(str::to_string);
            Some(Want {
                thread_id: id,
                label: label(thread["title"].as_str().unwrap_or("Untitled")),
                cwd,
                updated_at: thread["updatedAt"].as_str().unwrap_or_default().to_string(),
                place,
            })
        })
        .collect()
}

/// A window label Rex accepts: one line, no control characters, at most 128 characters.
pub fn label(title: &str) -> String {
    let clean: String = title
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let clean = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    let clean = if clean.is_empty() { "Untitled".to_string() } else { clean };
    if clean.chars().count() <= 120 {
        clean
    } else {
        clean.chars().take(119).collect::<String>() + "…"
    }
}

/// What one reconcile does to the session.
#[derive(Debug, Default, PartialEq)]
pub struct Plan {
    pub close: Vec<String>,
    pub create: Vec<Want>,
    pub relabel: Vec<(String, String)>,
    /// Every t3rex window, existing by id or new by thread id, in the order they should end
    /// up. Empty when the order is already right.
    pub order: Vec<Slot>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Slot {
    Window(String),
    New(String),
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.close.is_empty() && self.create.is_empty() && self.relabel.is_empty() && self.order.is_empty()
    }
}

/// Works out the plan from what T3 wants and the session's windows as Rex lists them, and
/// updates `state` for windows the user closed.
pub fn plan(wants: &[Want], present: &[(String, String)], state: &mut State) -> Plan {
    let present_ids: HashSet<&str> = present.iter().map(|(id, _)| id.as_str()).collect();
    let wanted_ids: HashMap<&str, &Want> = wants.iter().map(|w| (w.thread_id.as_str(), w)).collect();

    // A window the user closed: remember the thread as it was, so it stays closed until the
    // thread changes.
    let gone: Vec<String> = state
        .windows
        .iter()
        .filter(|(_, w)| !present_ids.contains(w.window_id.as_str()))
        .map(|(thread, _)| thread.clone())
        .collect();
    for thread in gone {
        state.windows.remove(&thread);
        if let Some(want) = wanted_ids.get(thread.as_str()) {
            state.dismissed.insert(thread, want.updated_at.clone());
        }
    }
    state.dismissed.retain(|thread, at| {
        wanted_ids
            .get(thread.as_str())
            .is_some_and(|want| &want.updated_at == at)
    });

    let mut plan = Plan::default();
    let mut kept: BTreeMap<String, Window> = BTreeMap::new();
    for (thread, window) in std::mem::take(&mut state.windows) {
        if wanted_ids.contains_key(thread.as_str()) {
            kept.insert(thread, window);
        } else {
            plan.close.push(window.window_id);
        }
    }
    let mut order = Vec::new();
    for want in wants {
        if state.dismissed.contains_key(&want.thread_id) {
            continue;
        }
        match kept.get_mut(&want.thread_id) {
            Some(window) => {
                let shown = present
                    .iter()
                    .find(|(id, _)| id == &window.window_id)
                    .map(|(_, label)| label.as_str());
                if shown != Some(want.label.as_str()) {
                    plan.relabel.push((window.window_id.clone(), want.label.clone()));
                    window.label = want.label.clone();
                }
                order.push(Slot::Window(window.window_id.clone()));
            }
            None => {
                plan.create.push(want.clone());
                order.push(Slot::New(want.thread_id.clone()));
            }
        }
    }
    state.windows = kept;

    // Only t3rex's windows count. Others in the session keep their places at the front.
    let mine: HashSet<&str> = state.windows.values().map(|w| w.window_id.as_str()).collect();
    let current: Vec<Slot> = present
        .iter()
        .filter(|(id, _)| mine.contains(id.as_str()))
        .map(|(id, _)| Slot::Window(id.clone()))
        .collect();
    if current != order {
        plan.order = order;
    }
    plan
}

/// The Lua that carries out `plan` in one connection, so the blocks it starts are disowned by
/// the client that made them and size to whoever views them.
pub fn script(plan: &Plan, exe: &str) -> String {
    let create: Vec<Value> = plan
        .create
        .iter()
        .map(|want| {
            json!({
                "thread": want.thread_id,
                "label": want.label,
                "cwd": want.cwd,
                "chat": [exe, "chat", want.thread_id],
                "compose": [exe, "compose", want.thread_id],
            })
        })
        .collect();
    let order: Vec<Value> = plan
        .order
        .iter()
        .map(|slot| match slot {
            Slot::Window(id) => json!({"window": id}),
            Slot::New(thread) => json!({"thread": thread}),
        })
        .collect();
    let data = json!({
        "close": plan.close,
        "create": create,
        "relabel": plan.relabel.iter().map(|(w, l)| json!({"window": w, "label": l})).collect::<Vec<_>>(),
        "order": order,
    });
    format!(
        r#"local plan = rex.json.decode({data})
local term = "com.superlogical.terminal.shell"
local out = {{ created = {{}}, errors = {{}} }}
local function pane(argv, cwd)
  return rex.layout.block{{ flavor = term, options = {{ shell = "none", command = argv, cwd = cwd, exit = {{ on_completion = false }} }} }}
end
for _, id in ipairs(plan.close or {{}}) do
  local _, err = rex.session.close_window{{ window_id = id }}
  if err then table.insert(out.errors, err) end
end
for _, c in ipairs(plan.create or {{}}) do
  local w, err = rex.session.new_window{{
    window_label = c.label,
    focus = false,
    layout = rex.layout.vertical(0.86, pane(c.chat, c.cwd), pane(c.compose, c.cwd)),
  }}
  if w then
    out.created[c.thread] = w.window_id
    rex.session.set_block_label{{ block_id = w.block_ids[1], label = "T3" }}
    rex.session.set_block_label{{ block_id = w.block_ids[2], label = "Reply" }}
  else
    table.insert(out.errors, err)
  end
end
for _, r in ipairs(plan.relabel or {{}}) do
  rex.session.set_window_label{{ window_id = r.window, label = r.label }}
end
for _, o in ipairs(plan.order or {{}}) do
  local id = o.window or out.created[o.thread]
  if id then rex.session.move_window{{ window_id = id }} end
end
rex.session.disown_block{{}}
return out
"#,
        data = lua_str(&data.to_string())
    )
}

pub struct Syncer {
    pub rex: Rex,
    pub exe: PathBuf,
    pub state: State,
    /// Put the session first in Rex's list when it is made.
    pub first: bool,
}

impl Syncer {
    /// The session t3rex keeps, made if it is gone.
    async fn session(&mut self) -> Result<String> {
        let sessions = self.rex.sessions().await?;
        if let Some(id) = &self.state.session_id
            && sessions.iter().any(|s| s["session_id"].as_str() == Some(id))
        {
            return Ok(id.clone());
        }
        let id = self.rex.create_session(SESSION_LABEL).await?;
        if self.first {
            self.rex.move_session(&id, 1).await?;
        }
        self.state = State {
            session_id: Some(id.clone()),
            ..State::default()
        };
        self.state.save()?;
        Ok(id)
    }

    /// Brings the session in line with `wants`. Returns whether anything changed.
    pub async fn reconcile(&mut self, wants: &[Want]) -> Result<bool> {
        let session = self.session().await?;
        let view = self.rex.inspect(&session).await?;
        let present: Vec<(String, String)> = view["windows"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter_map(|w| Some((w["window_id"].as_str()?.to_string(), w["label"].as_str().unwrap_or_default().to_string())))
            .collect();
        let plan = plan(wants, &present, &mut self.state);
        if plan.is_empty() {
            self.state.save()?;
            return Ok(false);
        }
        let exe = self.exe.to_string_lossy().into_owned();
        let result = self.rex.lua(&session, &script(&plan, &exe)).await?;
        for (thread, id) in result["created"].as_object().into_iter().flatten() {
            if let (Some(id), Some(want)) = (id.as_str(), plan.create.iter().find(|w| &w.thread_id == thread)) {
                self.state.windows.insert(
                    thread.clone(),
                    Window { window_id: id.to_string(), label: want.label.clone() },
                );
            }
        }
        for error in result["errors"].as_array().into_iter().flatten() {
            eprintln!("t3rex: rex: {error}");
        }
        self.state.save()?;
        Ok(true)
    }
}

/// Follows T3 until interrupted, reconciling after each burst of changes and every few
/// seconds, which picks up snoozes that end and windows closed in Rex.
pub async fn run(first: bool) -> Result<()> {
    let client = t3::connect(false).await.context("couldn't reach T3")?;
    let caps = t3::capabilities(&client);
    let mut syncer = Syncer {
        rex: Rex::find(),
        exe: std::env::current_exe()?,
        state: State::load(),
        first,
    };
    let mut events = client.watch_shell(None);
    let mut shell = ShellState::default();
    let mut inbox = Inbox::default();
    let mut ready = false;
    let mut dirty = false;
    let mut last: Vec<Want> = Vec::new();
    let mut tick = tokio::time::interval(Duration::from_secs(5));
    loop {
        tokio::select! {
            event = events.recv() => match event {
                Some(WatchEvent::Item(item)) => {
                    shell.apply(&item);
                    ready |= item["kind"] == "synchronized";
                    dirty = true;
                }
                Some(WatchEvent::Reconnecting { reason, .. }) => eprintln!("t3rex: T3 reconnecting: {reason}"),
                Some(WatchEvent::Failed(message)) => anyhow::bail!("T3 refused the subscription: {message}"),
                None => anyhow::bail!("T3's subscription ended"),
            },
            _ = tick.tick() => dirty = true,
        }
        // Drain a burst before acting, so a snapshot's items reconcile once.
        while let Ok(event) = events.try_recv() {
            if let WatchEvent::Item(item) = event {
                shell.apply(&item);
                ready |= item["kind"] == "synchronized";
            }
        }
        if !ready || !dirty {
            continue;
        }
        dirty = false;
        let wants = wanted(&shell, &mut inbox, caps, t3::now_ms());
        let changed_wants = wants != last;
        match syncer.reconcile(&wants).await {
            Ok(changed) if changed || (changed_wants && wants.len() != last.len()) => {
                let names: Vec<&str> = wants.iter().map(|w| w.label.as_str()).collect();
                eprintln!("t3rex: {} thread(s): {}", wants.len(), names.join(" | "));
            }
            Ok(_) => {}
            Err(e) => eprintln!("t3rex: {e:#}"),
        }
        last = wants;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn want(id: &str, label: &str) -> Want {
        Want {
            thread_id: id.into(),
            label: label.into(),
            cwd: None,
            updated_at: "u1".into(),
            place: Place::Open,
        }
    }

    fn win(id: &str, label: &str) -> Window {
        Window { window_id: id.into(), label: label.into() }
    }

    #[test]
    fn creates_closes_and_orders() {
        let mut state = State::default();
        state.windows.insert("a".into(), win("w-a", "A"));
        state.windows.insert("gone".into(), win("w-g", "G"));
        let present = vec![("w-g".into(), "G".into()), ("w-a".into(), "A".into()), ("user".into(), "mine".into())];
        let plan = plan(&[want("b", "B"), want("a", "A2")], &present, &mut state);
        assert_eq!(plan.close, vec!["w-g".to_string()]);
        assert_eq!(plan.create.len(), 1);
        assert_eq!(plan.relabel, vec![("w-a".to_string(), "A2".to_string())]);
        assert_eq!(plan.order, vec![Slot::New("b".into()), Slot::Window("w-a".into())]);
        assert!(!state.windows.contains_key("gone"));
    }

    #[test]
    fn a_closed_window_stays_closed_until_the_thread_changes() {
        let mut state = State::default();
        state.windows.insert("a".into(), win("w-a", "A"));
        let plan1 = plan(&[want("a", "A")], &[], &mut state);
        assert!(plan1.create.is_empty());
        assert_eq!(state.dismissed.get("a").map(String::as_str), Some("u1"));
        let mut changed = want("a", "A");
        changed.updated_at = "u2".into();
        let plan2 = plan(&[changed], &[], &mut state);
        assert_eq!(plan2.create.len(), 1);
        assert!(state.dismissed.is_empty());
    }

    #[test]
    fn nothing_to_do_when_in_order() {
        let mut state = State::default();
        state.windows.insert("a".into(), win("w-a", "A"));
        state.windows.insert("b".into(), win("w-b", "B"));
        let present = vec![("w-a".into(), "A".into()), ("w-b".into(), "B".into())];
        assert!(plan(&[want("a", "A"), want("b", "B")], &present, &mut state).is_empty());
    }

    #[test]
    fn labels_are_one_clean_line() {
        assert_eq!(label("  fix\tthe\nthing "), "fix the thing");
        assert_eq!(label(""), "Untitled");
        assert_eq!(label(&"x".repeat(300)).chars().count(), 120);
    }
}
