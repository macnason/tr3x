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
    /// The provider instance the thread runs on, for the window's icon.
    pub instance: String,
    /// Closing the window settles the thread. A thread that is running or waiting on you only
    /// hides, since T3 won't settle it without stopping the run.
    pub settles: bool,
}

/// The windows T3's shell asks for at `now`: T3's sidebar order, with each subagent thread
/// worth showing right after the window of the thread it works for.
pub fn wanted(
    shell: &ShellState,
    inbox: &mut Inbox,
    caps: t3term::tui::Capabilities,
    now: i64,
) -> Vec<Want> {
    let roots: HashMap<&str, &str> = shell
        .projects
        .iter()
        .filter_map(|p| Some((p["id"].as_str()?, p["workspaceRoot"].as_str()?)))
        .collect();
    let want = |thread: &Value, place: Place, child: bool| {
        let id = thread["id"].as_str()?.to_string();
        let cwd = thread["worktreePath"]
            .as_str()
            .or_else(|| roots.get(thread["projectId"].as_str()?).copied())
            .filter(|dir| std::path::Path::new(dir).is_dir())
            .map(str::to_string);
        let title = label(thread["title"].as_str().unwrap_or("Untitled"));
        Some(Want {
            thread_id: id,
            label: if child {
                label(&format!("↳ {title}"))
            } else {
                title
            },
            cwd,
            updated_at: thread["updatedAt"].as_str().unwrap_or_default().to_string(),
            place,
            instance: thread["modelSelection"]["instanceId"]
                .as_str()
                .or(thread["providerInstanceId"].as_str())
                .unwrap_or_default()
                .to_string(),
            settles: caps.settlement
                && !matches!(
                    t3term::tui::status_label(thread, now),
                    Some("Approval" | "Input" | "Working" | "Goal" | "Waiting")
                ),
        })
    };
    let by_id: HashMap<&str, &Value> = shell
        .threads
        .iter()
        .filter_map(|t| Some((t["id"].as_str()?, t)))
        .collect();
    // Each shown subagent under the nearest ancestor that isn't one, in the shell's order.
    let mut children: HashMap<&str, Vec<&Value>> = HashMap::new();
    for thread in shell.threads.iter().filter(|t| shown_subagent(t, now)) {
        if let Some(root) = top_ancestor(thread, &by_id) {
            children.entry(root).or_default().push(thread);
        }
    }
    let mut out = Vec::new();
    for (thread, place) in inbox.order(&shell.threads, caps, now) {
        out.extend(want(thread, place, false));
        let id = thread["id"].as_str().unwrap_or_default();
        for child in children.remove(id).unwrap_or_default() {
            out.extend(want(child, place, true));
        }
    }
    out
}

/// A subagent gets a window while it works or needs you. A task delegated through T3 keeps
/// its window until it settles, as a thread of its own would. A provider's own subagent at
/// rest is history, which T3 shows inside the parent thread.
fn shown_subagent(thread: &Value, now: i64) -> bool {
    if thread["lineage"]["relationshipToParent"] != "subagent" || !thread["archivedAt"].is_null() {
        return false;
    }
    let active = matches!(
        t3term::tui::status_label(thread, now),
        Some("Approval" | "Input" | "Working" | "Goal" | "Failed" | "Limited")
    );
    let delegated = thread["creationSource"] == "mcp"
        || thread["id"]
            .as_str()
            .is_some_and(|id| id.starts_with("thread:delegated-task:"));
    let settled = thread["settledOverride"] == "settled" || !thread["settledAt"].is_null();
    active || (delegated && !settled)
}

/// The thread a subagent works for at the top of its chain: the first ancestor that isn't a
/// subagent itself. None for a broken chain.
fn top_ancestor<'a>(thread: &Value, by_id: &HashMap<&'a str, &'a Value>) -> Option<&'a str> {
    let mut parent = thread["lineage"]["parentThreadId"].as_str()?;
    for _ in 0..16 {
        let (id, t) = by_id.get_key_value(parent)?;
        if t["lineage"]["relationshipToParent"] != "subagent" {
            return Some(id);
        }
        parent = t["lineage"]["parentThreadId"].as_str()?;
    }
    None
}

/// A window label Rex accepts: one line, no control characters, at most 128 characters.
pub fn label(title: &str) -> String {
    let clean: String = title
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let clean = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    let clean = if clean.is_empty() {
        "Untitled".to_string()
    } else {
        clean
    };
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
    /// Threads whose window the user closed, to settle in T3.
    pub settle: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Slot {
    Window(String),
    New(String),
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.close.is_empty()
            && self.create.is_empty()
            && self.relabel.is_empty()
            && self.order.is_empty()
    }
}

/// Works out the plan from what T3 wants and the session's windows as Rex lists them, and
/// updates `state` for windows the user closed. Closing a window settles its thread; the
/// window stays closed until T3 reports it settled, or until the thread changes if T3 won't.
pub fn plan(wants: &[Want], present: &[(String, String)], state: &mut State) -> Plan {
    let present_ids: HashSet<&str> = present.iter().map(|(id, _)| id.as_str()).collect();
    let wanted_ids: HashMap<&str, &Want> =
        wants.iter().map(|w| (w.thread_id.as_str(), w)).collect();

    // A window the user closed: remember the thread as it was, so it stays closed until the
    // thread changes.
    let gone: Vec<String> = state
        .windows
        .iter()
        .filter(|(_, w)| !present_ids.contains(w.window_id.as_str()))
        .map(|(thread, _)| thread.clone())
        .collect();
    // Every window gone at once is the session going, not the user closing threads.
    let teardown = gone.len() > 1 && gone.len() == state.windows.len();
    let mut settle = Vec::new();
    for thread in gone {
        state.windows.remove(&thread);
        if let Some(want) = wanted_ids.get(thread.as_str()) {
            if want.settles && !teardown {
                settle.push(thread.clone());
            }
            state.dismissed.insert(thread, want.updated_at.clone());
        }
    }
    state.dismissed.retain(|thread, at| {
        wanted_ids
            .get(thread.as_str())
            .is_some_and(|want| &want.updated_at == at)
    });

    let mut plan = Plan {
        settle,
        ..Plan::default()
    };
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
                    plan.relabel
                        .push((window.window_id.clone(), want.label.clone()));
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
    let mine: HashSet<&str> = state
        .windows
        .values()
        .map(|w| w.window_id.as_str())
        .collect();
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

/// The block label that marks a thread's conversation pane: `t3:<thread>`, or for an id too
/// long for Rex's 128-character labels, `t3#` and a hash of it.
pub fn tag(thread: &str) -> String {
    let tag = format!("t3:{thread}");
    if tag.chars().count() <= 128 {
        return tag;
    }
    // FNV-1a: stable across builds, which a std hasher isn't.
    let hash = thread.bytes().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ b as u64).wrapping_mul(0x100000001b3)
    });
    format!("t3#{hash:016x}")
}

/// Each window's thread, from the tag on its conversation pane's block label, `t3:<thread>`.
/// A hashed tag comes back as written; `adopt` matches it to a thread.
fn tagged(windows: &[Value]) -> Vec<(String, String)> {
    windows
        .iter()
        .filter_map(|w| {
            let id = w["window_id"].as_str()?;
            let thread = w["layers"]
                .as_array()?
                .iter()
                .flat_map(|l| l["blocks"].as_array().into_iter().flatten())
                .find_map(|b| {
                    let label = b["label"].as_str()?;
                    label
                        .strip_prefix("t3:")
                        .or(label.starts_with("t3#").then_some(label))
                })?;
            Some((id.to_string(), thread.to_string()))
        })
        .collect()
}

/// Takes back windows t3rex made but lost track of, as after a script that failed partway,
/// and returns the extra windows of a thread that has more than one, to close. A window
/// tagged with a thread T3 hasn't reported yet is left alone: a new-thread window tags itself
/// before it launches, and becomes that thread's window once T3 has it.
fn adopt(windows: &[Value], state: &mut State, known: &HashSet<String>) -> Vec<String> {
    let present: HashSet<&str> = windows
        .iter()
        .filter_map(|w| w["window_id"].as_str())
        .collect();
    let mut extra = Vec::new();
    let hashed: HashMap<String, String> = known
        .iter()
        .chain(state.windows.keys())
        .map(|t| (tag(t), t.clone()))
        .filter(|(tag, _)| tag.starts_with("t3#"))
        .collect();
    for (window, thread) in tagged(windows) {
        let thread = hashed.get(&thread).cloned().unwrap_or(thread);
        if !known.contains(&thread) && !state.windows.contains_key(&thread) {
            continue;
        }
        match state.windows.get(&thread) {
            Some(w) if w.window_id == window => {}
            Some(w) if present.contains(w.window_id.as_str()) => extra.push(window),
            _ => {
                let label = windows
                    .iter()
                    .find(|w| w["window_id"] == window.as_str())
                    .and_then(|w| w["label"].as_str())
                    .unwrap_or_default()
                    .to_string();
                state.windows.insert(
                    thread,
                    Window {
                        window_id: window,
                        label,
                    },
                );
            }
        }
    }
    extra
}

/// The Lua that carries out `plan` in one connection, so the blocks it starts are disowned by
/// the client that made them and size to whoever views them.
/// Each pane runs t3rex under the name of the thread's provider, so Rex's sidebar shows that
/// provider's glyph for the window.
pub fn script(plan: &Plan, exe: &std::path::Path, config: Option<&Value>) -> String {
    let create: Vec<Value> = plan
        .create
        .iter()
        .map(|want| {
            let program =
                crate::icons::as_program(exe, crate::icons::driver_of(config, &want.instance));
            let program = program.to_string_lossy();
            json!({
                "thread": want.thread_id,
                "tag": tag(&want.thread_id),
                "label": want.label,
                "cwd": want.cwd,
                "chat": [program, "chat", want.thread_id],
                "compose": [program, "compose", want.thread_id],
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
-- Labelled as they are made, so the new-window watcher never takes them for a New Tab.
local function pane(argv, cwd, label)
  return {{ block = {{ flavor = term, label = label, options = {{ shell = "none", login = false, command = argv, cwd = cwd, exit = {{ on_completion = false }} }} }} }}
end
for _, id in ipairs(plan.close or {{}}) do
  local _, err = rex.session.close_window{{ window_id = id }}
  if err then table.insert(out.errors, err) end
end
for _, c in ipairs(plan.create or {{}}) do
  local w, err = rex.session.new_window{{
    window_label = c.label,
    focus = false,
    layout = rex.layout.vertical(0.86, pane(c.chat, c.cwd, c.tag), pane(c.compose, c.cwd, "Reply")),
  }}
  if w then
    out.created[c.thread] = w.window_id
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
    /// The `rex do` script that turns a New Tab in the session into a new thread, and the
    /// session it watches.
    pub watcher: Option<(String, tokio::process::Child)>,
    /// T3's server config, for each provider instance's driver.
    pub config: Option<Value>,
    /// Threads whose window the user closed since the last reconcile, for `run` to settle.
    pub closed: Vec<String>,
}

/// Turns each window made in the session by Rex's New Tab, its + button or the palette, which
/// is one unlabelled shell, into a new-thread composer. The shell gives way to it in place, so
/// the window keeps its spot and focus. t3rex's own windows are labelled as they are made.
pub fn watcher_script(exe: &str) -> String {
    let argv = json!([exe, "new"]);
    format!(
        r#"local argv = rex.json.decode({argv})
local term = "com.superlogical.terminal.shell"
rex.on("window_created", function(ctx, ev)
  local view = rex.session.view{{}}
  for _, w in ipairs((view and view.windows) or {{}}) do
    local blocks = w.layers and #w.layers == 1 and w.layers[1].blocks
    if w.window_id == ev.window_id and blocks and #blocks == 1
        and blocks[1].flavor == term and (blocks[1].label or "shell") == "shell" then
      local shell = blocks[1].block_id
      local _, err = rex.session.new_split{{ anchor_id = shell, direction = "vertical", side = "after",
        focus = true, layout = {{ block = {{ flavor = term, label = "New Thread",
          options = {{ shell = "none", login = false, command = argv }} }} }} }}
      if not err then
        rex.call("block.close", {{ session_id = ctx.session_id, block_id = shell }})
        rex.session.set_window_label{{ window_id = w.window_id, label = "New Thread" }}
      end
      rex.session.disown_block{{}}
    end
  end
end)
"#,
        argv = lua_str(&argv.to_string())
    )
}

/// A new-thread window that also shows the keys that matter, for a session with no threads.
pub fn welcome_script(exe: &str) -> String {
    let argv = json!([exe, "new", "--welcome"]);
    format!(
        r#"local argv = rex.json.decode({argv})
local w, err = rex.session.new_window{{ window_label = "New Thread", focus = true,
  layout = {{ block = {{ flavor = "com.superlogical.terminal.shell", label = "New Thread",
    options = {{ shell = "none", login = false, command = argv }} }} }} }}
rex.session.disown_block{{}}
if err then error(err, 0) end
return {{}}
"#,
        argv = lua_str(&argv.to_string())
    )
}

impl Syncer {
    /// The session t3rex keeps, made if it is gone.
    async fn session(&mut self) -> Result<String> {
        let sessions = self.rex.sessions().await?;
        if let Some(id) = &self.state.session_id
            && sessions
                .iter()
                .any(|s| s["session_id"].as_str() == Some(id))
        {
            return Ok(id.clone());
        }
        let id = self.rex.create_session(SESSION_LABEL).await?;
        // Recorded before anything else can fail, so a retry never makes a second session.
        self.state = State {
            session_id: Some(id.clone()),
            ..State::default()
        };
        self.state.save()?;
        if self.first
            && let Err(e) = self.rex.move_session(&id, 1).await
        {
            eprintln!("t3rex: couldn't move the session first: {e:#}");
        }
        Ok(id)
    }

    /// Keeps the New Tab watcher running for `session`.
    fn watch(&mut self, session: &str) {
        let running = match self.watcher.as_mut() {
            Some((watched, child)) => watched == session && matches!(child.try_wait(), Ok(None)),
            None => false,
        };
        if running {
            return;
        }
        let exe = self.exe.to_string_lossy().into_owned();
        let child = tokio::process::Command::new(&self.rex.bin)
            .args(["do", "-s", session, "-"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn();
        match child {
            Ok(mut child) => {
                if let Some(mut stdin) = child.stdin.take() {
                    let script = watcher_script(&exe);
                    tokio::spawn(async move {
                        use tokio::io::AsyncWriteExt;
                        let _ = stdin.write_all(script.as_bytes()).await;
                    });
                }
                self.watcher = Some((session.to_string(), child));
            }
            Err(e) => eprintln!("t3rex: couldn't watch for new windows: {e:#}"),
        }
    }

    /// Brings the session in line with `wants`. Returns whether anything changed.
    pub async fn reconcile(&mut self, wants: &[Want], known: &HashSet<String>) -> Result<bool> {
        let session = self.session().await?;
        self.watch(&session);
        let view = self.rex.inspect(&session).await?;
        // Rex leaves `windows` out of an empty session. An answer that isn't this session at all
        // is not "every window was closed".
        if view["session_id"].as_str() != Some(session.as_str()) {
            anyhow::bail!("Rex described something other than the session");
        }
        let none = Vec::new();
        let windows = view["windows"].as_array().unwrap_or(&none);
        let mut extra = adopt(windows, &mut self.state, known);
        let present: Vec<(String, String)> = windows
            .iter()
            .filter_map(|w| {
                Some((
                    w["window_id"].as_str()?.to_string(),
                    w["label"].as_str().unwrap_or_default().to_string(),
                ))
            })
            .collect();
        // With no threads to show, the session holds a new-thread window to start from.
        if wants.is_empty() && windows.is_empty() {
            let exe = self.exe.to_string_lossy().into_owned();
            self.rex.lua(&session, &welcome_script(&exe)).await?;
            return Ok(true);
        }
        let mut plan = plan(wants, &present, &mut self.state);
        self.closed.append(&mut plan.settle);
        plan.close.append(&mut extra);
        if plan.is_empty() {
            self.state.save()?;
            return Ok(false);
        }
        let result = self
            .rex
            .lua(&session, &script(&plan, &self.exe, self.config.as_ref()))
            .await?;
        for (thread, id) in result["created"].as_object().into_iter().flatten() {
            if let (Some(id), Some(want)) = (
                id.as_str(),
                plan.create.iter().find(|w| &w.thread_id == thread),
            ) {
                self.state.windows.insert(
                    thread.clone(),
                    Window {
                        window_id: id.to_string(),
                        label: want.label.clone(),
                    },
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
    // One sync at a time: two would each make windows for the same threads.
    std::fs::create_dir_all(crate::state::dir())?;
    let lock = std::fs::File::create(crate::state::dir().join("sync.lock"))?;
    if lock.try_lock().is_err() {
        anyhow::bail!("another t3rex sync is already running");
    }
    // Operate, to settle a thread whose window is closed.
    let client = t3::connect(true).await.context("couldn't reach T3")?;
    let caps = t3::capabilities(&client);
    let mut syncer = Syncer {
        rex: Rex::find(),
        exe: crate::icons::real_exe(),
        state: State::load(),
        first,
        watcher: None,
        config: client.server_config().await.ok(),
        closed: Vec::new(),
    };
    let mut events = client.watch_shell(None);
    let mut shell = ShellState::default();
    let mut inbox = Inbox::default();
    let mut ready = false;
    let mut failures = 0;
    let mut dirty = false;
    let mut last: Vec<Want> = Vec::new();
    let mut tick = tokio::time::interval(Duration::from_secs(5));
    loop {
        tokio::select! {
            event = events.recv() => match event {
                Some(WatchEvent::Item(item)) => {
                    failures = 0;
                    shell.apply(&item);
                    ready |= item["kind"] == "synchronized";
                    dirty = true;
                }
                Some(WatchEvent::Reconnecting { reason, .. }) => {
                    eprintln!("t3rex: T3 reconnecting: {reason}");
                    // After a few minutes, start over with a fresh login; launchd restarts us.
                    failures += 1;
                    if failures > 40 {
                        anyhow::bail!("T3 hasn't answered for a while: {reason}");
                    }
                }
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
        let known: HashSet<String> = shell
            .threads
            .iter()
            .filter_map(|t| t["id"].as_str().map(str::to_string))
            .collect();
        match syncer.reconcile(&wants, &known).await {
            Ok(changed) if changed || (changed_wants && wants.len() != last.len()) => {
                let names: Vec<&str> = wants.iter().map(|w| w.label.as_str()).collect();
                eprintln!("t3rex: {} thread(s): {}", wants.len(), names.join(" | "));
            }
            Ok(_) => {}
            Err(e) => eprintln!("t3rex: {e:#}"),
        }
        // A plain settle, never stopping a run: a thread that started again since stays hidden
        // until it changes.
        for thread in syncer.closed.drain(..) {
            let client = client.clone();
            tokio::spawn(async move {
                let command = t3::command(&thread, &t3::Lifecycle::Settle);
                if let Err(e) = client.dispatch(command).await {
                    eprintln!("t3rex: couldn't settle {thread}: {e:#}");
                }
            });
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
            instance: String::new(),
            settles: true,
        }
    }

    fn win(id: &str, label: &str) -> Window {
        Window {
            window_id: id.into(),
            label: label.into(),
        }
    }

    #[test]
    fn creates_closes_and_orders() {
        let mut state = State::default();
        state.windows.insert("a".into(), win("w-a", "A"));
        state.windows.insert("gone".into(), win("w-g", "G"));
        let present = vec![
            ("w-g".into(), "G".into()),
            ("w-a".into(), "A".into()),
            ("user".into(), "mine".into()),
        ];
        let plan = plan(&[want("b", "B"), want("a", "A2")], &present, &mut state);
        assert_eq!(plan.close, vec!["w-g".to_string()]);
        assert_eq!(plan.create.len(), 1);
        assert_eq!(plan.relabel, vec![("w-a".to_string(), "A2".to_string())]);
        assert_eq!(
            plan.order,
            vec![Slot::New("b".into()), Slot::Window("w-a".into())]
        );
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
    fn closing_a_window_settles_its_thread_unless_it_is_running() {
        let mut state = State::default();
        state.windows.insert("a".into(), win("w-a", "A"));
        state.windows.insert("b".into(), win("w-b", "B"));
        state.windows.insert("c".into(), win("w-c", "C"));
        let mut running = want("b", "B");
        running.settles = false;
        let present = vec![("w-c".into(), "C".into())];
        let plan = plan(
            &[want("a", "A"), running, want("c", "C")],
            &present,
            &mut state,
        );
        assert_eq!(plan.settle, vec!["a".to_string()]);
        assert!(plan.create.is_empty());
        assert!(state.dismissed.contains_key("a") && state.dismissed.contains_key("b"));
    }

    #[test]
    fn every_window_going_at_once_settles_nothing() {
        let mut state = State::default();
        state.windows.insert("a".into(), win("w-a", "A"));
        state.windows.insert("b".into(), win("w-b", "B"));
        assert!(
            plan(&[want("a", "A"), want("b", "B")], &[], &mut state)
                .settle
                .is_empty()
        );
        // The last window, closed on its own, is the user's.
        let mut state = State::default();
        state.windows.insert("a".into(), win("w-a", "A"));
        assert_eq!(
            plan(&[want("a", "A")], &[], &mut state).settle,
            vec!["a".to_string()]
        );
    }

    #[test]
    fn nothing_to_do_when_in_order() {
        let mut state = State::default();
        state.windows.insert("a".into(), win("w-a", "A"));
        state.windows.insert("b".into(), win("w-b", "B"));
        let present = vec![("w-a".into(), "A".into()), ("w-b".into(), "B".into())];
        assert!(plan(&[want("a", "A"), want("b", "B")], &present, &mut state).is_empty());
    }

    fn tagged_window(id: &str, thread: &str) -> Value {
        json!({"window_id": id, "label": "L", "layers": [{"blocks": [
            {"block_id": "b1", "label": format!("t3:{thread}")},
            {"block_id": "b2", "label": "Reply"}
        ]}]})
    }

    #[test]
    fn lost_windows_are_adopted_and_duplicates_closed() {
        let mut state = State::default();
        state.windows.insert("a".into(), win("w-a", "A"));
        let windows = vec![
            tagged_window("w-a", "a"),
            tagged_window("w-a2", "a"),
            tagged_window("w-b", "b"),
            json!({"window_id": "user", "label": "mine", "layers": [{"blocks": [{"label": "shell"}]}]}),
        ];
        let known: HashSet<String> = ["a", "b"].iter().map(|s| s.to_string()).collect();
        let extra = adopt(&windows, &mut state, &known);
        assert_eq!(extra, vec!["w-a2".to_string()]);
        assert_eq!(state.windows["b"].window_id, "w-b");
        assert!(!state.windows.values().any(|w| w.window_id == "user"));
    }

    #[test]
    fn a_window_for_a_thread_t3_hasnt_reported_is_left_alone() {
        let mut state = State::default();
        let windows = vec![tagged_window("w-new", "launching")];
        assert!(adopt(&windows, &mut state, &HashSet::new()).is_empty());
        assert!(state.windows.is_empty());
    }

    fn thread(id: &str, extra: Value) -> Value {
        let mut t = json!({
            "id": id, "title": id, "projectId": "p", "archivedAt": null, "settledAt": null,
            "updatedAt": "2026-10-30T00:00:00.000Z",
            "lineage": {"parentThreadId": null, "relationshipToParent": null, "rootThreadId": id},
        });
        for (k, v) in extra.as_object().unwrap() {
            t[k] = v.clone();
        }
        t
    }

    #[test]
    fn subagents_follow_their_parent() {
        let child = |id: &str, parent: &str, extra: Value| {
            let mut t = thread(id, extra);
            t["lineage"] = json!({"parentThreadId": parent, "relationshipToParent": "subagent", "rootThreadId": parent});
            t
        };
        let shell = ShellState {
            threads: vec![
                thread("a", json!({})),
                thread("b", json!({})),
                // A task delegated through T3, not settled: shown under a.
                child("task", "a", json!({"creationSource": "mcp"})),
                // Its own subagent, working: shown under a too.
                child(
                    "deep",
                    "task",
                    json!({"creationSource": "provider", "activeRunId": "r", "latestRunId": "r", "activityRunStatus": "running"}),
                ),
                // A provider's subagent at rest: history, not shown.
                child(
                    "old",
                    "b",
                    json!({"creationSource": "provider", "activityRunStatus": "idle"}),
                ),
                // A settled delegated task: not shown.
                child(
                    "done",
                    "b",
                    json!({"creationSource": "mcp", "settledAt": "2026-10-30T00:00:00.000Z"}),
                ),
            ],
            ..ShellState::default()
        };
        let caps = t3term::tui::Capabilities::of(&json!({}));
        let wants = wanted(&shell, &mut Inbox::default(), caps, 0);
        let ids: Vec<&str> = wants.iter().map(|w| w.thread_id.as_str()).collect();
        let a = ids.iter().position(|i| *i == "a").unwrap();
        assert_eq!(&ids[a + 1..a + 3], &["task", "deep"]);
        assert!(!ids.contains(&"old") && !ids.contains(&"done"));
        assert_eq!(wants[a + 1].label, "↳ task");
    }

    #[test]
    fn long_thread_ids_get_a_short_tag_that_adopts() {
        let long = format!("thread:delegated-task:{}", "x".repeat(140));
        let t = tag(&long);
        assert!(t.starts_with("t3#") && t.len() == 19);
        assert_eq!(tag("abc"), "t3:abc");
        let windows = vec![
            json!({"window_id": "w", "label": "L", "layers": [{"blocks": [
            {"block_id": "b1", "label": t}, {"block_id": "b2", "label": "Reply"}]}]}),
        ];
        let mut state = State::default();
        let known: HashSet<String> = [long.clone()].into_iter().collect();
        adopt(&windows, &mut state, &known);
        assert_eq!(state.windows[&long].window_id, "w");
    }

    #[test]
    fn labels_are_one_clean_line() {
        assert_eq!(label("  fix\tthe\nthing "), "fix the thing");
        assert_eq!(label(""), "Untitled");
        assert_eq!(label(&"x".repeat(300)).chars().count(), 120);
    }
}
