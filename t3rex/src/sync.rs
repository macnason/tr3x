//! Keeps the "T3 Code" Rex session holding one window per open T3 thread, from every machine
//! t3rex is paired with, in T3's order: what needs you, then the rest of the inbox, then
//! Working. A settled, snoozed or archived thread's window closes, and comes back when T3
//! brings the thread back. With the `sessions` setting at `per-machine`, each machine gets a
//! session of its own instead.
//!
//! T3 is the source of truth. t3rex follows each machine's shell subscription and never decides
//! a thread's state itself. It only touches the sessions and windows its state file names, and
//! it creates, closes or settles a machine's windows only once that machine's shell has caught
//! up on its current connection. A machine out of reach keeps its windows as they were.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;
use serde_json::{Value, json};
use t3client::env::WatchEvent;
use t3client::hub::{Hub, ThreadRef};
use t3client::inbox::{Capabilities, Inbox, Place};
use t3client::projection::ShellState;

use crate::machines::Sessions;
use crate::rex::{Rex, lua_str};
use crate::state::{Group, Next, State, Window};
use crate::t3;

pub const SESSION_LABEL: &str = "T3 Code";

/// The session's name: "T3 Code", or `T3REX_SESSION_LABEL`, for a development sync beside an
/// installed one.
fn session_label() -> String {
    std::env::var("T3REX_SESSION_LABEL")
        .ok()
        .filter(|l| !l.trim().is_empty())
        .unwrap_or_else(|| SESSION_LABEL.into())
}

/// How long a machine is out of reach before its windows say so.
const OFFLINE_AFTER: Duration = Duration::from_secs(30);
const OFFLINE: &str = " (offline)";

/// One window t3rex wants, in order.
#[derive(Debug, Clone, PartialEq)]
pub struct Want {
    pub thread: ThreadRef,
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

impl Want {
    pub fn key(&self) -> String {
        self.thread.to_string()
    }
}

/// One machine's shell, as sync last saw it.
pub struct MachineShell<'a> {
    pub env: &'a str,
    /// What its windows say after the title, when more than one machine is paired.
    pub badge: Option<&'a str>,
    /// This Mac: its threads' folders are folders here.
    pub local: bool,
    pub shell: &'a ShellState,
    pub caps: Capabilities,
    /// Out of reach for a while: its windows say so.
    pub offline: bool,
}

/// The windows the machines' shells ask for at `now`: T3's sidebar order over every machine's
/// threads, with each subagent thread worth showing right after the window of the thread it
/// works for.
pub fn wanted(shells: &[MachineShell], inbox: &mut Inbox, now: i64) -> Vec<Want> {
    let mut threads = Vec::new();
    let mut roots: HashMap<(&str, &str), &str> = HashMap::new();
    let mut caps = Capabilities::default();
    let mut by_env: HashMap<&str, &MachineShell> = HashMap::new();
    for m in shells {
        by_env.insert(m.env, m);
        caps.settlement |= m.caps.settlement;
        caps.snooze |= m.caps.snooze;
        for p in &m.shell.projects {
            if let (Some(id), Some(root)) = (p["id"].as_str(), p["workspaceRoot"].as_str()) {
                roots.insert((m.env, id), root);
            }
        }
        for t in &m.shell.threads {
            let mut t = t.clone();
            t["t3rexEnv"] = json!(m.env);
            threads.push(t);
        }
    }
    let env_of = |t: &Value| t["t3rexEnv"].as_str().unwrap_or_default().to_string();
    let want = |thread: &Value, place: Place, child: bool| {
        let env = env_of(thread);
        let m = by_env.get(env.as_str())?;
        let id = thread["id"].as_str()?.to_string();
        // A thread on another machine works in a folder there, not here.
        let cwd = thread["worktreePath"]
            .as_str()
            .or_else(|| roots.get(&(m.env, thread["projectId"].as_str()?)).copied())
            .filter(|dir| m.local && std::path::Path::new(dir).is_dir())
            .map(str::to_string);
        let title = thread["title"].as_str().unwrap_or("Untitled");
        let title = if child {
            format!("↳ {title}")
        } else {
            title.to_string()
        };
        Some(Want {
            thread: ThreadRef::new(env.clone(), id),
            label: decorate(&title, m.badge, m.offline),
            cwd,
            updated_at: thread["updatedAt"].as_str().unwrap_or_default().to_string(),
            place,
            instance: thread["modelSelection"]["instanceId"]
                .as_str()
                .or(thread["providerInstanceId"].as_str())
                .unwrap_or_default()
                .to_string(),
            settles: m.caps.settlement
                && !matches!(
                    t3client::inbox::status_label(thread, now),
                    Some("Approval" | "Input" | "Working" | "Goal" | "Waiting")
                ),
        })
    };
    let by_id: HashMap<(String, &str), &Value> = threads
        .iter()
        .filter_map(|t| Some(((env_of(t), t["id"].as_str()?), t)))
        .collect();
    // Each shown subagent under the nearest ancestor that isn't one, on its own machine.
    let mut children: HashMap<(String, String), Vec<&Value>> = HashMap::new();
    for thread in threads.iter().filter(|t| shown_subagent(t, now)) {
        let env = env_of(thread);
        if let Some(root) = top_ancestor(thread, &env, &by_id) {
            children.entry((env, root)).or_default().push(thread);
        }
    }
    let mut out = Vec::new();
    for (thread, place) in inbox.order(&threads, caps, now) {
        out.extend(want(thread, place, false));
        let id = thread["id"].as_str().unwrap_or_default().to_string();
        for child in children.remove(&(env_of(thread), id)).unwrap_or_default() {
            out.extend(want(child, place, true));
        }
    }
    out
}

/// A subagent gets a window only while it works or needs you. Once it finishes, its result
/// belongs to the parent thread, which is where T3 shows it.
fn shown_subagent(thread: &Value, now: i64) -> bool {
    thread["lineage"]["relationshipToParent"] == "subagent"
        && thread["archivedAt"].is_null()
        && matches!(
            t3client::inbox::status_label(thread, now),
            Some("Approval" | "Input" | "Working" | "Goal" | "Failed" | "Limited")
        )
}

/// The thread a subagent works for at the top of its chain: the first ancestor that isn't a
/// subagent itself. None for a broken chain.
fn top_ancestor(
    thread: &Value,
    env: &str,
    by_id: &HashMap<(String, &str), &Value>,
) -> Option<String> {
    let mut parent = thread["lineage"]["parentThreadId"].as_str()?;
    for _ in 0..16 {
        let t = by_id.get(&(env.to_string(), parent))?;
        if t["lineage"]["relationshipToParent"] != "subagent" {
            return Some(parent.to_string());
        }
        parent = t["lineage"]["parentThreadId"].as_str()?;
    }
    None
}

/// A window label Rex accepts: one line, no control characters, at most 128 characters.
pub fn label(title: &str) -> String {
    clean(title, 120)
}

fn clean(title: &str, max: usize) -> String {
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
    if clean.chars().count() <= max {
        clean
    } else {
        clean
            .chars()
            .take(max.saturating_sub(1))
            .collect::<String>()
            + "…"
    }
}

/// A thread's window label: its title, then the machine it runs on, then whether that
/// machine is out of reach. The title gives way first.
pub fn decorate(title: &str, badge: Option<&str>, offline: bool) -> String {
    let mut suffix = String::new();
    if let Some(badge) = badge {
        suffix.push_str(" · ");
        suffix.push_str(&clean(badge, 40));
    }
    if offline {
        suffix.push_str(OFFLINE);
    }
    clean(title, 120 - suffix.chars().count()) + &suffix
}

/// `label` with the offline mark, or without it.
fn marked(label: &str, offline: bool) -> String {
    let base = label.strip_suffix(OFFLINE).unwrap_or(label);
    if offline {
        format!("{base}{OFFLINE}")
    } else {
        base.to_string()
    }
}

/// What sync may do to a machine's windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Health {
    /// Its shell caught up on the current connection: its windows can be made, closed and
    /// settled.
    pub ready: bool,
    /// Out of reach for a while: its windows say so.
    pub offline: bool,
}

impl Health {
    pub const READY: Health = Health {
        ready: true,
        offline: false,
    };
}

fn env_of(key: &str) -> &str {
    key.split_once('/').map_or("", |(env, _)| env)
}

/// What one reconcile does to a session.
#[derive(Debug, Default, PartialEq)]
pub struct Plan {
    pub close: Vec<String>,
    pub create: Vec<Want>,
    pub relabel: Vec<(String, String)>,
    /// Every t3rex window, existing by id or new by `env/thread`, in the order they should end
    /// up. Empty when the order is already right.
    pub order: Vec<Slot>,
    /// Threads whose window the user closed, to settle in T3.
    pub settle: Vec<ThreadRef>,
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

/// Works out the plan for one session from what T3 wants and the session's windows as Rex
/// lists them, and updates `group` for windows the user closed. Closing a window settles its
/// thread; the window stays closed until T3 reports it settled, or until the thread changes if
/// T3 won't. A machine whose `health` isn't ready gets no new window, and keeps the ones it has
/// where they are, whatever its last known shell says.
pub fn plan(
    wants: &[Want],
    present: &[(String, String)],
    group: &mut Group,
    health: &dyn Fn(&str) -> Health,
) -> Plan {
    let present_ids: HashSet<&str> = present.iter().map(|(id, _)| id.as_str()).collect();
    let wanted: HashMap<String, &Want> = wants.iter().map(|w| (w.key(), w)).collect();

    // A window the user closed: remember the thread as it was, so it stays closed until the
    // thread changes.
    let gone: Vec<String> = group
        .windows
        .iter()
        .filter(|(_, w)| !present_ids.contains(w.window_id.as_str()))
        .map(|(key, _)| key.clone())
        .collect();
    // Every window gone at once is the session going, not the user closing threads.
    let teardown = gone.len() > 1 && gone.len() == group.windows.len();
    let mut settle = Vec::new();
    for key in gone {
        group.windows.remove(&key);
        if let Some(want) = wanted.get(&key) {
            if want.settles && !teardown && health(env_of(&key)).ready {
                settle.push(want.thread.clone());
            }
            group.dismissed.insert(key, want.updated_at.clone());
        }
    }
    group.dismissed.retain(|key, at| match wanted.get(key) {
        Some(want) => &want.updated_at == at,
        // A machine that hasn't caught up can't say the thread is gone.
        None => !health(env_of(key)).ready,
    });

    let mut plan = Plan {
        settle,
        ..Plan::default()
    };
    let mut kept: BTreeMap<String, Window> = BTreeMap::new();
    // Windows of machines that haven't caught up, with nothing to say about them.
    let mut held: Vec<String> = Vec::new();
    for (key, window) in std::mem::take(&mut group.windows) {
        if wanted.contains_key(&key) {
            kept.insert(key, window);
        } else if !health(env_of(&key)).ready {
            held.push(key.clone());
            kept.insert(key, window);
        } else {
            plan.close.push(window.window_id);
        }
    }
    let shown = |window: &Window| {
        present
            .iter()
            .find(|(id, _)| id == &window.window_id)
            .map(|(_, label)| label.clone())
    };
    let mut order = Vec::new();
    for want in wants {
        let key = want.key();
        if group.dismissed.contains_key(&key) {
            continue;
        }
        match kept.get_mut(&key) {
            Some(window) => {
                if shown(window).as_deref() != Some(want.label.as_str()) {
                    plan.relabel
                        .push((window.window_id.clone(), want.label.clone()));
                    window.label = want.label.clone();
                }
                order.push(Slot::Window(window.window_id.clone()));
            }
            None if health(&want.thread.env).ready => {
                plan.create.push(want.clone());
                order.push(Slot::New(key));
            }
            None => {}
        }
    }
    // A held window keeps its place after the window before it, and says when its machine is
    // out of reach.
    for key in held {
        let window = kept.get_mut(&key).expect("held windows are kept");
        let offline = health(env_of(&key)).offline;
        let label = marked(&window.label, offline);
        if shown(window).as_deref() != Some(label.as_str()) {
            plan.relabel.push((window.window_id.clone(), label.clone()));
            window.label = label;
        }
        let id = window.window_id.clone();
        let before = present.iter().position(|(w, _)| *w == id).and_then(|at| {
            present[..at]
                .iter()
                .rev()
                .find_map(|(w, _)| order.iter().position(|s| *s == Slot::Window(w.clone())))
        });
        let at = before.map_or(0, |i| i + 1);
        order.insert(at, Slot::Window(id));
    }
    group.windows = kept;

    // Only t3rex's windows count. Others in the session keep their places at the front.
    let mine: HashSet<&str> = group
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

/// The block label that marks a thread's conversation pane: `t3:<env8>/<thread>`, with the
/// first eight characters of the machine's id, or for one too long for Rex's 128-character
/// labels, `t3#` and a hash of `env/thread`.
pub fn tag(thread: &ThreadRef) -> String {
    let env8: String = thread.env.chars().take(8).collect();
    let tag = format!("t3:{env8}/{}", thread.thread);
    if tag.chars().count() <= 128 {
        return tag;
    }
    hashed(&thread.to_string())
}

/// FNV-1a: stable across builds, which a std hasher isn't.
fn hashed(text: &str) -> String {
    let hash = text.bytes().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ b as u64).wrapping_mul(0x100000001b3)
    });
    format!("t3#{hash:016x}")
}

/// Each window's tag, from its conversation pane's block label: what follows `t3:`, or a
/// whole `t3#` tag.
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

/// The `env/thread` a tag names, among `known` and the machines `envs`. A tag from before
/// t3rex knew more than one machine, a bare thread id or its hash, is on `home`.
fn resolve(tag: &str, known: &HashSet<String>, envs: &[String], home: &str) -> Option<String> {
    if tag.starts_with("t3#") {
        return known
            .iter()
            .find(|key| {
                hashed(key) == tag || (env_of(key) == home && hashed(&key[home.len() + 1..]) == tag)
            })
            .cloned();
    }
    match tag.split_once('/') {
        Some((env8, thread)) => {
            let mut found = envs.iter().filter(|e| e.starts_with(env8));
            let env = found.next()?;
            found.next().is_none().then(|| format!("{env}/{thread}"))
        }
        None => Some(format!("{home}/{tag}")),
    }
}

/// Takes back windows t3rex made but lost track of, as after a script that failed partway,
/// and returns the extra windows of a thread that has more than one, to close. A window
/// tagged with a thread T3 hasn't reported yet is left alone: a new-thread window tags itself
/// before it launches, and becomes that thread's window once T3 has it.
fn adopt(
    windows: &[Value],
    group: &mut Group,
    known: &HashSet<String>,
    envs: &[String],
    home: &str,
) -> Vec<String> {
    let present: HashSet<&str> = windows
        .iter()
        .filter_map(|w| w["window_id"].as_str())
        .collect();
    let mut every: HashSet<String> = known.clone();
    every.extend(group.windows.keys().cloned());
    let mut extra = Vec::new();
    for (window, tag) in tagged(windows) {
        let Some(key) = resolve(&tag, &every, envs, home) else {
            continue;
        };
        if !known.contains(&key) && !group.windows.contains_key(&key) {
            continue;
        }
        match group.windows.get(&key) {
            Some(w) if w.window_id == window => {}
            Some(w) if present.contains(w.window_id.as_str()) => extra.push(window),
            _ => {
                let label = windows
                    .iter()
                    .find(|w| w["window_id"] == window.as_str())
                    .and_then(|w| w["label"].as_str())
                    .unwrap_or_default()
                    .to_string();
                group.windows.insert(
                    key,
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
pub fn script(plan: &Plan, exe: &std::path::Path, configs: &HashMap<String, Value>) -> String {
    let create: Vec<Value> = plan
        .create
        .iter()
        .map(|want| {
            let config = configs.get(&want.thread.env);
            let program =
                crate::icons::as_program(exe, crate::icons::driver_of(config, &want.instance));
            let program = program.to_string_lossy();
            let key = want.key();
            json!({
                "thread": key,
                "tag": tag(&want.thread),
                "label": want.label,
                "cwd": want.cwd,
                "chat": chat_argv(&program, &key),
                "compose": crate::rex::pane_argv(&program, &["compose", &key]),
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

/// A conversation pane's command line. A development sync (`T3REX_PASSIVE`) starts panes that
/// only read, beside an installed one.
fn chat_argv(program: &str, key: &str) -> Value {
    let mut argv = crate::rex::pane_argv(program, &["chat", key]);
    if std::env::var_os("T3REX_PASSIVE").is_some() {
        argv.push("--passive".into());
    }
    json!(argv)
}

/// The `t3rex new` command line for a session: on its machine, in a session per machine.
fn new_argv(exe: &str, env: Option<&str>, welcome: bool) -> Value {
    let mut argv = crate::rex::pane_argv(exe, &["new"]);
    if let Some(env) = env {
        argv.push("--env".into());
        argv.push(env.into());
    }
    if welcome {
        argv.push("--welcome".into());
    }
    json!(argv)
}

/// Turns each window made in the session by Rex's New Tab, its + button or the palette, which
/// is one unlabelled shell, into a new-thread composer. The shell gives way to it in place, so
/// the window keeps its spot and focus. t3rex's own windows are labelled as they are made.
pub fn watcher_script(exe: &str, env: Option<&str>) -> String {
    let argv = new_argv(exe, env, false);
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
pub fn welcome_script(exe: &str, env: Option<&str>) -> String {
    let argv = new_argv(exe, env, true);
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

pub struct Syncer {
    pub rex: Rex,
    pub exe: PathBuf,
    pub state: State,
    /// Put the merged session, or this Mac's, first in Rex's list when it's made.
    pub first: bool,
    /// The `rex do` scripts that turn a New Tab in each session into a new thread, by session.
    pub watchers: HashMap<String, tokio::process::Child>,
    /// Each machine's server config, for each provider instance's driver.
    pub configs: HashMap<String, Value>,
    /// Threads whose window the user closed since the last reconcile, for `follow` to settle.
    pub closed: Vec<ThreadRef>,
}

/// A session t3rex keeps: the merged one, or one machine's.
pub struct SessionSpec {
    /// The key of its group in the state file: "" for the merged session, else the machine's id.
    pub key: String,
    pub label: String,
    /// The machine new threads in it start on, in a session per machine.
    pub env: Option<String>,
    /// Put first in Rex's list when made.
    pub first: bool,
}

impl Syncer {
    /// The group's session, made if it is gone.
    async fn session(&mut self, spec: &SessionSpec) -> Result<String> {
        let sessions = self.rex.sessions().await?;
        let group = self.state.groups.entry(spec.key.clone()).or_default();
        if let Some(id) = &group.session_id
            && sessions
                .iter()
                .any(|s| s["session_id"].as_str() == Some(id))
        {
            return Ok(id.clone());
        }
        let id = self.rex.create_session(&spec.label).await?;
        // Recorded before anything else can fail, so a retry never makes a second session.
        *group = Group {
            session_id: Some(id.clone()),
            ..Group::default()
        };
        self.state.save()?;
        if self.first
            && spec.first
            && let Err(e) = self.rex.move_session(&id, 1).await
        {
            eprintln!("t3rex: couldn't move the session first: {e:#}");
        }
        Ok(id)
    }

    /// Keeps the New Tab watcher running for `session`.
    fn watch(&mut self, session: &str, env: Option<&str>) {
        if let Some(child) = self.watchers.get_mut(session)
            && matches!(child.try_wait(), Ok(None))
        {
            return;
        }
        let exe = self.exe.to_string_lossy().into_owned();
        let child = tokio::process::Command::new(&self.rex.bin)
            .args(crate::rex::NO_AUTOSTART)
            .args(["do", "-s", session, "-"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn();
        match child {
            Ok(mut child) => {
                if let Some(mut stdin) = child.stdin.take() {
                    let script = watcher_script(&exe, env);
                    tokio::spawn(async move {
                        use tokio::io::AsyncWriteExt;
                        let _ = stdin.write_all(script.as_bytes()).await;
                    });
                }
                self.watchers.insert(session.to_string(), child);
            }
            Err(e) => eprintln!("t3rex: couldn't watch for new windows: {e:#}"),
        }
    }

    /// Closes a session t3rex made that the `sessions` setting no longer asks for. Its threads'
    /// windows open again in the sessions that hold them now, and nothing is settled.
    pub async fn retire(&mut self, key: &str) -> Result<()> {
        let Some(group) = self.state.groups.remove(key) else {
            return Ok(());
        };
        self.state.save()?;
        if let Some(id) = group.session_id {
            self.watchers.remove(&id);
            let args = json!({ "session_id": id }).to_string();
            self.rex.call("session.destroy", &args).await?;
        }
        Ok(())
    }

    /// Brings one session in line with `wants`. Returns whether anything changed.
    pub async fn reconcile(
        &mut self,
        spec: &SessionSpec,
        wants: &[Want],
        known: &HashSet<String>,
        envs: &[String],
        home: &str,
        health: &dyn Fn(&str) -> Health,
    ) -> Result<bool> {
        let session = self.session(spec).await?;
        self.watch(&session, spec.env.as_deref());
        let view = self.rex.inspect(&session).await?;
        // Rex leaves `windows` out of an empty session. An answer that isn't this session at all
        // is not "every window was closed".
        if view["session_id"].as_str() != Some(session.as_str()) {
            anyhow::bail!("Rex described something other than the session");
        }
        let none = Vec::new();
        let windows = view["windows"].as_array().unwrap_or(&none);
        let group = self.state.groups.entry(spec.key.clone()).or_default();
        let mut extra = adopt(windows, group, known, envs, home);
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
            self.rex
                .lua(&session, &welcome_script(&exe, spec.env.as_deref()))
                .await?;
            return Ok(true);
        }
        let mut plan = plan(wants, &present, group, health);
        self.closed.append(&mut plan.settle);
        plan.close.append(&mut extra);
        if plan.is_empty() {
            self.state.save()?;
            return Ok(false);
        }
        let result = self
            .rex
            .lua(&session, &script(&plan, &self.exe, &self.configs))
            .await?;
        let group = self.state.groups.entry(spec.key.clone()).or_default();
        for (key, id) in result["created"].as_object().into_iter().flatten() {
            if let (Some(id), Some(want)) =
                (id.as_str(), plan.create.iter().find(|w| &w.key() == key))
            {
                group.windows.insert(
                    key.clone(),
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

/// What sync knows about one machine's connection.
#[derive(Default)]
struct Track {
    shell: ShellState,
    /// Counts the connections: each drop starts the next.
    generation: u64,
    /// The connection whose shell last caught up.
    synced: Option<u64>,
    /// Since when it's been out of reach, or never reached since sync started.
    down_since: Option<Instant>,
    /// Why, as last said, so the log says it once.
    reason: String,
    /// The server config was asked for.
    configured: bool,
    /// When to subscribe again after T3 refused the subscription.
    retry_at: Option<Instant>,
}

impl Track {
    fn health(&self, now: Instant) -> Health {
        Health {
            ready: self.synced == Some(self.generation),
            offline: self
                .down_since
                .is_some_and(|since| now.duration_since(since) >= OFFLINE_AFTER),
        }
    }
}

/// Where a running sync writes its process id, and holds a lock while it runs.
pub fn lock_path() -> PathBuf {
    crate::state::dir().join("sync.lock")
}

/// The log a sync started by Rex writes to.
pub fn log_path() -> PathBuf {
    crate::state::dir().join("sync.log")
}

/// The process id of the sync that's running, if one is.
pub fn running() -> Option<u32> {
    let file = std::fs::File::open(lock_path()).ok()?;
    if file.try_lock_shared().is_ok() {
        return None;
    }
    std::fs::read_to_string(lock_path())
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Stops the running sync, waiting a few seconds for it to let go of its lock.
pub async fn stop() -> bool {
    let Some(pid) = running() else {
        return false;
    };
    // SAFETY: kill(2) with a pid the lock holder wrote.
    unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    for _ in 0..30 {
        if running().is_none() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    true
}

/// Sends this process's output to `sync.log`, starting it afresh once it passes a megabyte.
fn log_to_file() -> Result<()> {
    use std::os::fd::AsRawFd;
    let path = log_path();
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 1 << 20) {
        let _ = std::fs::remove_file(&path);
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    // SAFETY: dup2(2) onto stdout and stderr, before any other thread writes to them.
    unsafe {
        libc::dup2(file.as_raw_fd(), 1);
        libc::dup2(file.as_raw_fd(), 2);
    }
    Ok(())
}

/// Follows T3 until stopped. Rex starts sync when its config loads, and every load starts
/// another, so the one that holds the lock carries on and the rest leave at once. Nothing
/// restarts it, so whatever goes wrong it starts over after a pause.
pub async fn run(first: bool, log: bool) -> Result<()> {
    use std::io::{IsTerminal, Write};
    std::fs::create_dir_all(crate::state::dir())?;
    // One sync at a time: two would each make windows for the same threads.
    let mut lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(lock_path())?;
    if lock.try_lock().is_err() {
        if std::io::stderr().is_terminal() {
            anyhow::bail!("another t3rex sync is already running");
        }
        return Ok(());
    }
    lock.set_len(0)?;
    write!(lock, "{}", std::process::id())?;
    if log {
        log_to_file()?;
    }
    tokio::spawn(crate::fade::follow());
    let mut pause = 2;
    loop {
        let started = Instant::now();
        let Err(error) = follow(first).await;
        if started.elapsed() > Duration::from_secs(120) {
            pause = 2;
        }
        eprintln!("t3rex: {error:#}; trying again in {pause}s");
        tokio::time::sleep(Duration::from_secs(pause)).await;
        pause = (pause * 2).min(60);
    }
}

/// When the machine files last changed, so a machine paired while sync runs joins at once.
fn machines_stamp(hub: &Hub) -> Option<std::time::SystemTime> {
    std::fs::metadata(hub.store.dir.join("envs.json"))
        .and_then(|m| m.modified())
        .ok()
}

/// Every machine's shell, reconciling the session after each burst of changes and every few
/// seconds, which picks up snoozes that end, windows closed in Rex and machines that go quiet.
/// Each machine connects and reconnects on its own. Returns only when it fails.
async fn follow(first: bool) -> Result<std::convert::Infallible> {
    let mut hub = t3::hub();
    // A Mac without T3 of its own can still follow the machines it's paired with.
    let home = hub.home_id().unwrap_or_default().to_string();
    if hub.envs().is_empty() {
        anyhow::bail!(
            "T3 Code isn't set up on this Mac, and Rex isn't paired with another machine"
        );
    }
    let mut state = State::load();
    if !home.is_empty() {
        state.migrate(&home);
        state.save()?;
    }
    let mut syncer = Syncer {
        rex: Rex::find(),
        exe: crate::icons::real_exe(),
        state,
        first,
        watchers: HashMap::new(),
        configs: HashMap::new(),
        closed: Vec::new(),
    };
    let (tx, mut events) = tokio::sync::mpsc::unbounded_channel();
    let (config_tx, mut configs) = tokio::sync::mpsc::unbounded_channel::<(String, Value)>();
    let mut tracks: BTreeMap<String, Track> = BTreeMap::new();
    let started = Instant::now();
    for env in hub.envs() {
        Hub::forward(env.clone(), tx.clone());
        tracks.insert(
            env.id().to_string(),
            Track {
                down_since: Some(started),
                ..Track::default()
            },
        );
    }
    let mut stamp = machines_stamp(&hub);
    let mut inbox = Inbox::default();
    let mut dirty = false;
    let mut last: Vec<Want> = Vec::new();
    let mut tick = tokio::time::interval(Duration::from_secs(5));
    loop {
        let take = |env: String, event: WatchEvent, tracks: &mut BTreeMap<String, Track>| {
            let Some(track) = tracks.get_mut(&env) else {
                return;
            };
            match event {
                WatchEvent::Item(item) => {
                    track.shell.apply(&item);
                    if item["kind"] == "synchronized" {
                        if track.synced != Some(track.generation) && !track.reason.is_empty() {
                            eprintln!("t3rex: {} is back", name_of(&hub, &env));
                        }
                        track.synced = Some(track.generation);
                        track.down_since = None;
                        track.reason.clear();
                    }
                }
                WatchEvent::Reconnecting { reason, .. } => {
                    if track.synced == Some(track.generation) {
                        track.generation += 1;
                    }
                    track.down_since.get_or_insert_with(Instant::now);
                    if reason != track.reason {
                        eprintln!("t3rex: {}: {reason}", name_of(&hub, &env));
                        track.reason = reason;
                    }
                }
                WatchEvent::Failed(message) => {
                    eprintln!(
                        "t3rex: {} refused the subscription: {message}",
                        name_of(&hub, &env)
                    );
                    track.generation += 1;
                    track.down_since.get_or_insert_with(Instant::now);
                    track.retry_at = Some(Instant::now() + Duration::from_secs(60));
                }
            }
        };
        tokio::select! {
            event = events.recv() => {
                let Some((env, event)) = event else {
                    anyhow::bail!("every machine's subscription ended");
                };
                take(env, event, &mut tracks);
                dirty = true;
            }
            Some((env, config)) = configs.recv() => {
                syncer.configs.insert(env, config);
            }
            _ = tick.tick() => dirty = true,
        }
        // Drain a burst before acting, so a snapshot's items reconcile once.
        while let Ok((env, event)) = events.try_recv() {
            take(env, event, &mut tracks);
        }
        if !dirty {
            continue;
        }
        dirty = false;
        let now = Instant::now();

        // A machine paired, renamed or forgotten while sync runs.
        if machines_stamp(&hub) != stamp {
            stamp = machines_stamp(&hub);
            hub = t3::hub();
            for env in hub.envs() {
                if !tracks.contains_key(env.id()) {
                    eprintln!("t3rex: following {}", env.name());
                    Hub::forward(env.clone(), tx.clone());
                    tracks.insert(
                        env.id().to_string(),
                        Track {
                            down_since: Some(now),
                            ..Track::default()
                        },
                    );
                }
            }
            tracks.retain(|id, _| hub.envs().iter().any(|e| e.id() == id));
        }
        for env in hub.envs() {
            let Some(track) = tracks.get_mut(env.id()) else {
                continue;
            };
            if track.retry_at.is_some_and(|at| at <= now) {
                track.retry_at = None;
                Hub::forward(env.clone(), tx.clone());
            }
            if track.shell.synchronized && !track.configured {
                track.configured = true;
                let (env, config_tx) = (env.clone(), config_tx.clone());
                tokio::spawn(async move {
                    if let Ok(config) = env.server_config().await {
                        let _ = config_tx.send((env.id().to_string(), config));
                    }
                });
            }
        }
        // Nothing to say until some machine has caught up once.
        if !tracks.values().any(|t| t.synced.is_some()) {
            continue;
        }

        let badges = hub.paired() >= 2;
        let names: HashMap<String, String> = hub
            .envs()
            .iter()
            .map(|e| (e.id().to_string(), e.name().to_string()))
            .collect();
        let shells: Vec<MachineShell> = hub
            .envs()
            .iter()
            .filter_map(|e| {
                let track = tracks.get(e.id())?;
                track.shell.synchronized.then(|| MachineShell {
                    env: e.id(),
                    badge: (badges && e.id() != home).then(|| names[e.id()].as_str()),
                    local: e.is_local(),
                    shell: &track.shell,
                    caps: Capabilities::of(&e.capabilities()),
                    offline: track.health(now).offline,
                })
            })
            .collect();
        let wants = wanted(&shells, &mut inbox, t3::now_ms());
        let known: HashSet<String> = wants
            .iter()
            .map(Want::key)
            .chain(shells.iter().flat_map(|m| {
                m.shell
                    .threads
                    .iter()
                    .filter_map(|t| Some(format!("{}/{}", m.env, t["id"].as_str()?)))
            }))
            .collect();
        let envs: Vec<String> = hub.envs().iter().map(|e| e.id().to_string()).collect();
        let health = |env: &str| match tracks.get(env) {
            Some(track) => track.health(now),
            // A machine t3rex no longer follows: its windows go.
            None => Health::READY,
        };

        // The sessions the setting asks for, and the threads each holds.
        let mode = crate::machines::sessions();
        let mut specs: Vec<(SessionSpec, Vec<Want>)> = Vec::new();
        match mode {
            Sessions::Merged => specs.push((
                SessionSpec {
                    key: String::new(),
                    label: session_label(),
                    env: None,
                    first: true,
                },
                wants.clone(),
            )),
            Sessions::PerMachine => {
                for env in hub.envs() {
                    let id = env.id().to_string();
                    let ready = health(&id).ready;
                    // A machine never reached gets a session once it answers.
                    if !ready && !syncer.state.groups.contains_key(&id) {
                        continue;
                    }
                    specs.push((
                        SessionSpec {
                            key: id.clone(),
                            label: format!("{} · {}", session_label(), env.name()),
                            env: (id != home).then(|| id.clone()),
                            first: id == home,
                        },
                        wants
                            .iter()
                            .filter(|w| w.thread.env == id)
                            .cloned()
                            .collect(),
                    ));
                }
            }
        }
        let keep: BTreeSet<String> = specs.iter().map(|(s, _)| s.key.clone()).collect();
        let retired: Vec<String> = syncer
            .state
            .groups
            .keys()
            .filter(|k| {
                !keep.contains(*k)
                    && match mode {
                        Sessions::Merged => true,
                        // A machine that's paired but hasn't answered keeps its session.
                        Sessions::PerMachine => k.is_empty() || !envs.contains(k),
                    }
            })
            .cloned()
            .collect();
        for key in retired {
            if let Err(e) = syncer.retire(&key).await {
                eprintln!("t3rex: couldn't close an old session: {e:#}");
            }
        }
        let mut changed = false;
        for (spec, group_wants) in &specs {
            match syncer
                .reconcile(spec, group_wants, &known, &envs, &home, &health)
                .await
            {
                Ok(c) => changed |= c,
                Err(e) => eprintln!("t3rex: {e:#}"),
            }
        }
        if changed || wants.len() != last.len() {
            let names: Vec<&str> = wants.iter().map(|w| w.label.as_str()).collect();
            eprintln!("t3rex: {} thread(s): {}", wants.len(), names.join(" | "));
        }
        // Where ⌃⌘I goes: the first window in T3's order, on any machine.
        let next = wants.iter().find_map(|w| {
            let key = w.key();
            syncer.state.groups.values().find_map(|g| {
                let window = g.windows.get(&key)?;
                Some(Next {
                    session_id: g.session_id.clone()?,
                    window_id: Some(window.window_id.clone()),
                })
            })
        });
        let next = next.or_else(|| {
            syncer.state.session().map(|session_id| Next {
                session_id,
                window_id: None,
            })
        });
        if next != syncer.state.next {
            syncer.state.next = next;
            syncer.state.save()?;
        }
        // A plain settle, never stopping a run: a thread that started again since stays hidden
        // until it changes.
        for thread in syncer.closed.drain(..) {
            let Some(env) = hub.envs().iter().find(|e| e.id() == thread.env).cloned() else {
                continue;
            };
            tokio::spawn(async move {
                let command = t3::command(&thread.thread, &t3::Lifecycle::Settle);
                if let Err(e) = env.dispatch(command).await {
                    eprintln!("t3rex: couldn't settle {thread}: {e:#}");
                }
            });
        }
        last = wants;
    }
}

fn name_of(hub: &Hub, env: &str) -> String {
    hub.envs()
        .iter()
        .find(|e| e.id() == env)
        .map_or_else(|| env.to_string(), |e| e.name().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn want(key: &str, label: &str) -> Want {
        let (env, thread) = key.split_once('/').unwrap_or(("h", key));
        Want {
            thread: ThreadRef::new(env, thread),
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

    fn ready(_: &str) -> Health {
        Health::READY
    }

    /// Machine `b` is out of reach, and has been for a while; the rest are ready.
    fn b_down(env: &str) -> Health {
        if env == "b" {
            Health {
                ready: false,
                offline: true,
            }
        } else {
            Health::READY
        }
    }

    #[test]
    fn creates_closes_and_orders() {
        let mut group = Group::default();
        group.windows.insert("h/a".into(), win("w-a", "A"));
        group.windows.insert("h/gone".into(), win("w-g", "G"));
        let present = vec![
            ("w-g".into(), "G".into()),
            ("w-a".into(), "A".into()),
            ("user".into(), "mine".into()),
        ];
        let plan = plan(
            &[want("b", "B"), want("a", "A2")],
            &present,
            &mut group,
            &ready,
        );
        assert_eq!(plan.close, vec!["w-g".to_string()]);
        assert_eq!(plan.create.len(), 1);
        assert_eq!(plan.relabel, vec![("w-a".to_string(), "A2".to_string())]);
        assert_eq!(
            plan.order,
            vec![Slot::New("h/b".into()), Slot::Window("w-a".into())]
        );
        assert!(!group.windows.contains_key("h/gone"));
    }

    #[test]
    fn a_closed_window_stays_closed_until_the_thread_changes() {
        let mut group = Group::default();
        group.windows.insert("h/a".into(), win("w-a", "A"));
        let plan1 = plan(&[want("a", "A")], &[], &mut group, &ready);
        assert!(plan1.create.is_empty());
        assert_eq!(group.dismissed.get("h/a").map(String::as_str), Some("u1"));
        let mut changed = want("a", "A");
        changed.updated_at = "u2".into();
        let plan2 = plan(&[changed], &[], &mut group, &ready);
        assert_eq!(plan2.create.len(), 1);
        assert!(group.dismissed.is_empty());
    }

    #[test]
    fn closing_a_window_settles_its_thread_unless_it_is_running() {
        let mut group = Group::default();
        group.windows.insert("h/a".into(), win("w-a", "A"));
        group.windows.insert("h/b".into(), win("w-b", "B"));
        group.windows.insert("h/c".into(), win("w-c", "C"));
        let mut running = want("b", "B");
        running.settles = false;
        let present = vec![("w-c".into(), "C".into())];
        let plan = plan(
            &[want("a", "A"), running, want("c", "C")],
            &present,
            &mut group,
            &ready,
        );
        assert_eq!(plan.settle, vec![ThreadRef::new("h", "a")]);
        assert!(plan.create.is_empty());
        assert!(group.dismissed.contains_key("h/a") && group.dismissed.contains_key("h/b"));
    }

    #[test]
    fn every_window_going_at_once_settles_nothing() {
        let mut group = Group::default();
        group.windows.insert("h/a".into(), win("w-a", "A"));
        group.windows.insert("h/b".into(), win("w-b", "B"));
        let wants = [want("a", "A"), want("b", "B")];
        assert!(plan(&wants, &[], &mut group, &ready).settle.is_empty());
        // The last window, closed on its own, is the user's.
        let mut group = Group::default();
        group.windows.insert("h/a".into(), win("w-a", "A"));
        assert_eq!(
            plan(&[want("a", "A")], &[], &mut group, &ready).settle,
            vec![ThreadRef::new("h", "a")]
        );
    }

    #[test]
    fn nothing_to_do_when_in_order() {
        let mut group = Group::default();
        group.windows.insert("h/a".into(), win("w-a", "A"));
        group.windows.insert("h/b".into(), win("w-b", "B"));
        let present = vec![("w-a".into(), "A".into()), ("w-b".into(), "B".into())];
        let wants = [want("a", "A"), want("b", "B")];
        assert!(plan(&wants, &present, &mut group, &ready).is_empty());
    }

    /// A machine out of reach: its windows stay, in place, marked offline. None closes, none is
    /// made and none settles, whatever its last known shell said.
    #[test]
    fn a_machine_out_of_reach_keeps_its_windows() {
        let mut group = Group::default();
        group.windows.insert("h/a".into(), win("w-a", "A"));
        group.windows.insert("b/x".into(), win("w-x", "X · b"));
        group.windows.insert("b/y".into(), win("w-y", "Y · b"));
        group.windows.insert("b/z".into(), win("w-z", "Z · b"));
        let present = vec![
            ("w-x".into(), "X · b".into()),
            ("w-a".into(), "A".into()),
            ("w-y".into(), "Y · b".into()),
        ];
        // `b` is known only from before: x is still in its last shell, y isn't, and a new
        // thread n showed up. The user closed z's window while `b` was down.
        let wants = [
            want("b/x", "X · b (offline)"),
            want("h/a", "A"),
            want("b/n", "N · b (offline)"),
        ];
        let plan = plan(&wants, &present, &mut group, &b_down);
        assert!(plan.close.is_empty() && plan.create.is_empty() && plan.settle.is_empty());
        assert!(group.windows.contains_key("b/y"));
        assert!(group.dismissed.contains_key("b/z") || !group.windows.contains_key("b/z"));
        assert!(
            plan.relabel
                .contains(&("w-y".into(), "Y · b (offline)".into()))
        );
        assert!(
            plan.relabel
                .contains(&("w-x".into(), "X · b (offline)".into()))
        );
        // y stays after a, where it was.
        assert!(plan.order.is_empty() || plan.order.last() == Some(&Slot::Window("w-y".into())));
        // Back, and caught up: y goes, n comes, and the marks come off.
        let back = [want("b/x", "X · b"), want("h/a", "A"), want("b/n", "N · b")];
        let present = vec![
            ("w-x".into(), "X · b (offline)".into()),
            ("w-a".into(), "A".into()),
            ("w-y".into(), "Y · b (offline)".into()),
        ];
        let plan = super::plan(&back, &present, &mut group, &ready);
        assert_eq!(plan.close, vec!["w-y".to_string()]);
        assert_eq!(plan.create.len(), 1);
        assert!(plan.relabel.contains(&("w-x".into(), "X · b".into())));
    }

    #[test]
    fn badges_and_offline_marks_keep_the_label_in_bounds() {
        assert_eq!(decorate("Fix it", None, false), "Fix it");
        assert_eq!(decorate("Fix it", Some("studio"), false), "Fix it · studio");
        assert_eq!(
            decorate("Fix it", Some("studio"), true),
            "Fix it · studio (offline)"
        );
        let long = decorate(&"x".repeat(300), Some("studio"), true);
        assert!(long.chars().count() <= 120 && long.ends_with("· studio (offline)"));
        assert_eq!(marked("A (offline)", false), "A");
        assert_eq!(marked("A (offline)", true), "A (offline)");
    }

    fn tagged_window(id: &str, tag: &str) -> Value {
        json!({"window_id": id, "label": "L", "layers": [{"blocks": [
            {"block_id": "b1", "label": format!("t3:{tag}")},
            {"block_id": "b2", "label": "Reply"}
        ]}]})
    }

    #[test]
    fn lost_windows_are_adopted_and_duplicates_closed() {
        let mut group = Group::default();
        group.windows.insert("home-env/a".into(), win("w-a", "A"));
        let windows = vec![
            tagged_window("w-a", "home-env/a"),
            tagged_window("w-a2", "home-env/a"),
            tagged_window("w-b", "home-env/b"),
            // A window from before t3rex knew more than one machine.
            tagged_window("w-old", "c"),
            tagged_window("w-r", "remote-e/r"),
            json!({"window_id": "user", "label": "mine", "layers": [{"blocks": [{"label": "shell"}]}]}),
        ];
        let known: HashSet<String> = ["home-env/a", "home-env/b", "home-env/c", "remote-env/r"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let envs = vec!["home-env".to_string(), "remote-env".to_string()];
        let extra = adopt(&windows, &mut group, &known, &envs, "home-env");
        assert_eq!(extra, vec!["w-a2".to_string()]);
        assert_eq!(group.windows["home-env/b"].window_id, "w-b");
        assert_eq!(group.windows["home-env/c"].window_id, "w-old");
        assert_eq!(group.windows["remote-env/r"].window_id, "w-r");
        assert!(!group.windows.values().any(|w| w.window_id == "user"));
    }

    #[test]
    fn a_window_for_a_thread_t3_hasnt_reported_is_left_alone() {
        let mut group = Group::default();
        let windows = vec![tagged_window("w-new", "home-env/launching")];
        let envs = vec!["home-env".to_string()];
        assert!(adopt(&windows, &mut group, &HashSet::new(), &envs, "home-env").is_empty());
        assert!(group.windows.is_empty());
    }

    #[test]
    fn long_thread_ids_get_a_short_tag_that_adopts() {
        let long = ThreadRef::new(
            "env-1",
            format!("thread:delegated-task:{}", "x".repeat(140)),
        );
        let t = tag(&long);
        assert!(t.starts_with("t3#") && t.len() == 19);
        assert_eq!(tag(&ThreadRef::new("0123456789", "abc")), "t3:01234567/abc");
        let windows = vec![
            json!({"window_id": "w", "label": "L", "layers": [{"blocks": [
            {"block_id": "b1", "label": t}, {"block_id": "b2", "label": "Reply"}]}]}),
            // A hashed tag from before, of the bare thread id, on this Mac.
            json!({"window_id": "w-old", "label": "L", "layers": [{"blocks": [
            {"block_id": "b1", "label": hashed(&format!("thread:old:{}", "y".repeat(140)))}]}]}),
        ];
        let old = format!("env-1/thread:old:{}", "y".repeat(140));
        let mut group = Group::default();
        let known: HashSet<String> = [long.to_string(), old.clone()].into_iter().collect();
        adopt(&windows, &mut group, &known, &["env-1".into()], "env-1");
        assert_eq!(group.windows[&long.to_string()].window_id, "w");
        assert_eq!(group.windows[&old].window_id, "w-old");
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

    fn machine<'a>(
        env: &'a str,
        shell: &'a ShellState,
        badge: Option<&'a str>,
    ) -> MachineShell<'a> {
        MachineShell {
            env,
            badge,
            local: badge.is_none(),
            shell,
            caps: Capabilities::of(&json!({})),
            offline: false,
        }
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
                // A task delegated through T3, working: shown under a.
                child(
                    "task",
                    "a",
                    json!({"creationSource": "mcp", "activeRunId": "r1", "latestRunId": "r1", "activityRunStatus": "running"}),
                ),
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
                // A delegated task that finished: its result is the parent's, not shown.
                child(
                    "finished",
                    "b",
                    json!({"creationSource": "mcp", "status": "idle", "latestRunId": "r", "lastVisitedAt": "2026-10-31T00:00:00.000Z"}),
                ),
                // A settled delegated task: not shown.
                child(
                    "done",
                    "b",
                    json!({"creationSource": "mcp", "settledAt": "2026-10-30T00:00:00.000Z"}),
                ),
                // A cancelled delegated task: never settles, not shown.
                child(
                    "cancelled",
                    "b",
                    json!({"creationSource": "mcp", "status": "interrupted", "latestRunId": "r"}),
                ),
            ],
            ..ShellState::default()
        };
        let wants = wanted(&[machine("h", &shell, None)], &mut Inbox::default(), 0);
        let ids: Vec<&str> = wants.iter().map(|w| w.thread.thread.as_str()).collect();
        let a = ids.iter().position(|i| *i == "a").unwrap();
        assert_eq!(&ids[a + 1..a + 3], &["task", "deep"]);
        assert!(
            !ids.contains(&"old")
                && !ids.contains(&"done")
                && !ids.contains(&"cancelled")
                && !ids.contains(&"finished")
        );
        assert_eq!(wants[a + 1].label, "↳ task");
    }

    /// Two machines' threads merge in one order, each knowing its machine, and only the other
    /// machine's threads carry its name.
    #[test]
    fn machines_merge_into_one_order() {
        let here = ShellState {
            threads: vec![thread(
                "a",
                json!({"createdAt": "2026-10-01T00:00:00.000Z"}),
            )],
            ..ShellState::default()
        };
        let there = ShellState {
            threads: vec![
                thread("a", json!({"createdAt": "2026-10-02T00:00:00.000Z"})),
                thread("x", json!({"pendingRuntimeRequest": {"kind": "command"}})),
            ],
            ..ShellState::default()
        };
        let wants = wanted(
            &[
                machine("h", &here, None),
                machine("o", &there, Some("studio")),
            ],
            &mut Inbox::default(),
            0,
        );
        let keys: Vec<String> = wants.iter().map(Want::key).collect();
        assert_eq!(keys[0], "o/x", "an approval comes first, from any machine");
        assert!(keys.contains(&"h/a".to_string()) && keys.contains(&"o/a".to_string()));
        let label = |key: &str| wants.iter().find(|w| w.key() == key).unwrap().label.clone();
        assert_eq!(label("h/a"), "a");
        assert_eq!(label("o/a"), "a · studio");
    }

    #[test]
    fn labels_are_one_clean_line() {
        assert_eq!(label("  fix\tthe\nthing "), "fix the thing");
        assert_eq!(label(""), "Untitled");
        assert_eq!(label(&"x".repeat(300)).chars().count(), 120);
    }
}
