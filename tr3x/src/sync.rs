//! Keeps the "T3 Code" Rex session holding one window per open T3 thread, from every machine
//! tr3x is paired with, in T3's order: what needs you, then the rest of the inbox, then
//! Working. A settled, snoozed or archived thread's window closes, and comes back when T3
//! brings the thread back. With the `sessions` setting at `per-machine`, each machine gets a
//! session of its own instead.
//!
//! T3 is the source of truth. tr3x follows each machine's shell subscription and never decides
//! a thread's state itself. It only touches the sessions and windows its state file names, and
//! it creates, closes or settles a machine's windows only once that machine's shell has caught
//! up on its current connection. A machine out of reach keeps its windows as they were.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use serde_json::{Value, json};
use t3client::env::{Env, WatchEvent};
use t3client::hub::{Forwarder, Hub, ThreadRef};
use t3client::inbox::{Capabilities, Inbox, Place};
use t3client::projection::ShellState;

use crate::machines::Sessions;
use crate::rex::{Rex, lua_str};
use crate::state::{Disk, Group, Next, State, Window};
use crate::t3;

pub const SESSION_LABEL: &str = "T3 Code";

/// The session's name: "T3 Code", or `TR3X_SESSION_LABEL`, for a development sync beside an
/// installed one.
fn session_label() -> String {
    std::env::var("TR3X_SESSION_LABEL")
        .ok()
        .filter(|l| !l.trim().is_empty())
        .unwrap_or_else(|| SESSION_LABEL.into())
}

/// How long a machine is out of reach before its windows say so.
const OFFLINE_AFTER: Duration = Duration::from_secs(30);
const OFFLINE: &str = " (offline)";

/// One window tr3x wants, in order.
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
    /// A subagent's window.
    pub child: bool,
    /// For a subagent under a window: the thread it works for.
    pub parent: Option<Parent>,
    /// The subagent is waiting on an approval or an answer: it keeps a window of its own even
    /// when the thread it works for has none, since the agent can't go on without you.
    pub needs_you: bool,
    /// What the label says when the subagent stands alone: `↳ title · for Parent`.
    pub alone: Option<String>,
}

/// The thread a subagent works for, the one at the top of its chain.
#[derive(Debug, Clone, PartialEq)]
pub struct Parent {
    pub key: String,
    pub title: String,
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
            t["tr3xEnv"] = json!(m.env);
            threads.push(t);
        }
    }
    let env_of = |t: &Value| t["tr3xEnv"].as_str().unwrap_or_default().to_string();
    // `under`: the thread a subagent works for, when it shows beneath it. `alone`: the subagent
    // shows by itself, since that thread has no window.
    let want = |thread: &Value, place: Place, under: Option<Parent>, alone: Option<&str>| {
        let env = env_of(thread);
        let m = by_env.get(env.as_str())?;
        let id = thread["id"].as_str()?.to_string();
        // A thread on another machine works in a folder there, not here.
        let cwd = thread["worktreePath"]
            .as_str()
            .or_else(|| roots.get(&(m.env, thread["projectId"].as_str()?)).copied())
            .filter(|dir| m.local && std::path::Path::new(dir).is_dir())
            .map(str::to_string);
        let status = t3client::inbox::status_label(thread, now);
        let title = thread["title"].as_str().unwrap_or("Untitled");
        let title = match glyph(status) {
            Some(glyph) => format!("{glyph} {title}"),
            None => title.to_string(),
        };
        let child = under.is_some() || alone.is_some();
        let needs_you = matches!(status, Some("Approval" | "Input"));
        let tag = |parent: &str| format!("↳ {title} · for {}", clean(parent, 24));
        let (title, alone_label) = match (&under, alone) {
            (_, Some(parent)) => (tag(parent), None),
            (Some(parent), None) => (format!("↳ {title}"), Some(tag(&parent.title))),
            (None, None) => (title, None),
        };
        // A subagent that only works has nothing to say until it changes, so closing its window
        // hides it until it needs you or finishes, not until its next tool call.
        let updated_at = match (child, status) {
            (true, Some("Working" | "Goal")) => "working".to_string(),
            _ => thread["updatedAt"].as_str().unwrap_or_default().to_string(),
        };
        Some(Want {
            thread: ThreadRef::new(env.clone(), id),
            label: decorate(&title, m.badge, m.offline),
            cwd,
            updated_at,
            place,
            instance: thread["modelSelection"]["instanceId"]
                .as_str()
                .or(thread["providerInstanceId"].as_str())
                .unwrap_or_default()
                .to_string(),
            settles: m.caps.settlement
                && !matches!(
                    status,
                    Some("Approval" | "Input" | "Working" | "Goal" | "Waiting")
                ),
            child,
            parent: under,
            needs_you: child && needs_you,
            alone: alone_label.map(|l| decorate(&l, m.badge, m.offline)),
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
    let mut out: Vec<Want> = Vec::new();
    for (thread, place) in inbox.order(&threads, caps, now) {
        out.extend(want(thread, place, None, None));
        let id = thread["id"].as_str().unwrap_or_default().to_string();
        let env = env_of(thread);
        let parent = Parent {
            key: format!("{env}/{id}"),
            title: thread["title"].as_str().unwrap_or("Untitled").to_string(),
        };
        for child in children.remove(&(env, id)).unwrap_or_default() {
            out.extend(want(child, place, Some(parent.clone()), None));
        }
    }
    // A subagent whose thread isn't listed (snoozed, settled, archived) and that waits on you
    // still gets a window, with the attention threads: nothing else shows its request, and the
    // agent can't go on. One that failed is history, as it is under any thread.
    let mut stranded: Vec<(&(String, String), &Vec<&Value>)> = children.iter().collect();
    stranded.sort_by(|a, b| a.0.cmp(b.0));
    let mut alone: Vec<Want> = Vec::new();
    for ((env, root), subagents) in stranded {
        let title = by_id
            .get(&(env.clone(), root.as_str()))
            .and_then(|t| t["title"].as_str())
            .unwrap_or("Untitled");
        for child in subagents {
            if let Some(w) =
                want(child, Place::Attention, None, Some(title)).filter(|w| w.needs_you)
            {
                alone.push(w);
            }
        }
    }
    let after = out
        .iter()
        .rposition(|w| w.place == Place::Attention)
        .map_or(0, |i| i + 1);
    out.splice(after..after, alone);
    out
}

/// The mark before a window's title for T3's status word. Labels are plain text, so it's a
/// shape, not a colour. Work has none: Rex's spinner shows it.
fn glyph(word: Option<&str>) -> Option<char> {
    match word? {
        "Approval" => Some('●'),
        "Input" => Some('?'),
        "Failed" | "Limited" => Some('✕'),
        "Done" | "Woke" => Some('✓'),
        _ => None,
    }
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
    /// Every tr3x window, existing by id or new by `env/thread`, in the order they should end
    /// up. Empty when the order is already right.
    pub order: Vec<Slot>,
    /// Threads whose window the user closed, to settle in T3.
    pub settle: Vec<ThreadRef>,
    /// Windows whose panes don't match the `reply` setting, to reshape in place.
    pub rebuild: Vec<Rebuild>,
}

/// A thread's window with the panes it has, `blocks`, which the shape the setting asks for
/// replaces.
#[derive(Debug, Clone, PartialEq)]
pub struct Rebuild {
    pub window: String,
    pub blocks: Vec<String>,
    pub want: Want,
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
            && self.rebuild.is_empty()
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
    active: Option<&str>,
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

    // A subagent shows under the thread it works for. When the user closed that thread's
    // window, it goes too, unless it needs you: that one stays, named for its thread.
    let shown: Vec<Want> = wants
        .iter()
        .filter_map(|w| match &w.parent {
            Some(p) if group.dismissed.contains_key(&p.key) || !wanted.contains_key(&p.key) => {
                w.needs_you.then(|| Want {
                    label: w.alone.clone().unwrap_or_else(|| w.label.clone()),
                    parent: None,
                    ..w.clone()
                })
            }
            _ => Some(w.clone()),
        })
        .collect();
    let wanted: HashMap<String, &Want> = shown.iter().map(|w| (w.key(), w)).collect();
    let wants = &shown[..];

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
        } else if window.child && active == Some(window.window_id.as_str()) {
            // A subagent that finished while you read it stays until you leave.
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

    // Only tr3x's windows count. Others in the session keep their places at the front.
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

/// A tr3x window's conversation pane: the block labelled with its tag.
fn conversation(window: &Value) -> Option<&Value> {
    window["layers"]
        .as_array()?
        .iter()
        .flat_map(|l| l["blocks"].as_array().into_iter().flatten())
        .find(|b| {
            b["label"]
                .as_str()
                .is_some_and(|l| l.starts_with("t3:") || l.starts_with("t3#"))
        })
}

/// Each window's tag, from its conversation pane's block label: what follows `t3:`, or a
/// whole `t3#` tag.
fn tagged(windows: &[Value]) -> Vec<(String, String)> {
    windows
        .iter()
        .filter_map(|w| {
            let id = w["window_id"].as_str()?;
            let label = conversation(w)?["label"].as_str()?;
            let thread = label.strip_prefix("t3:").unwrap_or(label);
            Some((id.to_string(), thread.to_string()))
        })
        .collect()
}

/// Whether a pane's `process` answer has tr3x running in it, by the program's file name
/// against `exe`'s, or `None` when it doesn't say. Rex can bring a session back after a
/// restart with a shell where the conversation was, under the same label.
fn runs_tr3x(process: &Value, exe: &std::path::Path) -> Option<bool> {
    let path = process["child"]["path"].as_str()?;
    Some(std::path::Path::new(path).file_name() == exe.file_name())
}

/// The `env/thread` a tag names, among `known` and the machines `envs`. A tag from before
/// tr3x knew more than one machine, a bare thread id or its hash, is on `home`.
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

/// Takes back windows tr3x made but lost track of, as after a script that failed partway,
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
                        child: false,
                    },
                );
            }
        }
    }
    extra
}

/// The windows in `group` whose shape isn't the one the setting asks for, Reply in a pane of
/// its own or not, or whose conversation pane is `dead`, with what to run in their place.
fn reshape(
    windows: &[Value],
    group: &Group,
    wants: &[Want],
    own_pane: bool,
    dead: &HashSet<String>,
) -> Vec<Rebuild> {
    group
        .windows
        .iter()
        .filter_map(|(key, w)| {
            let window = windows
                .iter()
                .find(|x| x["window_id"] == w.window_id.as_str())?;
            let blocks: Vec<&Value> = window["layers"]
                .as_array()?
                .iter()
                .filter(|l| l["kind"] != "floating")
                .flat_map(|l| l["blocks"].as_array().into_iter().flatten())
                .collect();
            if !dead.contains(&w.window_id) && crate::reply::shaped(window, own_pane) {
                return None;
            }
            Some(Rebuild {
                window: w.window_id.clone(),
                blocks: blocks
                    .iter()
                    .filter_map(|b| b["block_id"].as_str().map(str::to_string))
                    .collect(),
                want: wants.iter().find(|want| &want.key() == key)?.clone(),
            })
        })
        .collect()
}

/// The Lua that carries out `plan` in one connection, so the blocks it starts are disowned by
/// the client that made them and size to whoever views them.
/// Each pane runs tr3x under the name of the thread's provider, so Rex's sidebar shows that
/// provider's glyph for the window.
/// With `own_pane`, each window has Reply in a pane of its own under the conversation.
pub fn script(
    plan: &Plan,
    exe: &std::path::Path,
    configs: &HashMap<String, Value>,
    own_pane: bool,
) -> String {
    let pane = |want: &Want| {
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
            "chat": chat_argv(&program, &["chat", &key]),
            "compose": chat_argv(&program, &["compose", &key]),
        })
    };
    let create: Vec<Value> = plan.create.iter().map(pane).collect();
    let rebuild: Vec<Value> = plan
        .rebuild
        .iter()
        .map(|r| {
            let mut pane = pane(&r.want);
            pane["blocks"] = json!(r.blocks);
            pane
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
        "rebuild": rebuild,
        "relabel": plan.relabel.iter().map(|(w, l)| json!({"window": w, "label": l})).collect::<Vec<_>>(),
        "order": order,
        "split": own_pane.then_some(crate::reply::SPLIT),
    });
    format!(
        r#"local plan = rex.json.decode({data})
local term = "com.superlogical.terminal.shell"
local out = {{ created = {{}}, errors = {{}} }}
-- Labelled as they are made, so the new-window watcher never takes them for a New Tab.
local function pane(argv, cwd, label)
  return {{ block = {{ flavor = term, label = label, options = {{ shell = "none", login = false, command = argv, cwd = cwd, exit = {{ on_completion = false }} }} }} }}
end
-- A thread's window: the conversation, and Reply under it when it has a pane of its own.
local function shape(c)
  if plan.split then
    return rex.layout.vertical(plan.split, pane(c.chat, c.cwd, c.tag), pane(c.compose, c.cwd, "Reply"))
  end
  return pane(c.chat, c.cwd, c.tag)
end
for _, id in ipairs(plan.close or {{}}) do
  local _, err = rex.session.close_window{{ window_id = id }}
  if err then table.insert(out.errors, err) end
end
for _, c in ipairs(plan.create or {{}}) do
  local w, err = rex.session.new_window{{
    window_label = c.label,
    focus = false,
    layout = shape(c),
  }}
  if w then
    out.created[c.thread] = w.window_id
  else
    table.insert(out.errors, err)
  end
end
-- The new panes go in beside the old ones, which then close, so the window keeps its place.
for _, r in ipairs(plan.rebuild or {{}}) do
  local _, err = rex.session.new_split{{ anchor_id = r.blocks[1], direction = "vertical",
    side = "after", ratio = 0.5, focus = false, layout = shape(r) }}
  if err then
    table.insert(out.errors, err)
  else
    for _, b in ipairs(r.blocks) do
      rex.call("block.close", {{ block_id = b, session_id = rex.session_id }})
    end
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

/// A thread pane's command line, `args` after the program. A development sync
/// (`TR3X_PASSIVE`) starts panes that only read, beside an installed one.
fn chat_argv(program: &str, args: &[&str]) -> Value {
    let mut argv = crate::rex::pane_argv(program, args);
    if std::env::var_os("TR3X_PASSIVE").is_some() {
        argv.push("--passive".into());
    }
    json!(argv)
}

/// The `tr3x new` command line for a session: on its machine, in a session per machine.
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
/// the window keeps its spot and focus. tr3x's own windows are labelled as they are made.
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
    /// The state file, written only when `state` differs from what it holds.
    pub disk: Disk,
    /// Put the merged session, or this Mac's, first in Rex's list when it's made.
    pub first: bool,
    /// The `rex do` scripts that turn a New Tab in each session into a new thread, by session.
    pub watchers: HashMap<String, tokio::process::Child>,
    /// Each machine's server config, for each provider instance's driver.
    pub configs: HashMap<String, Value>,
    /// Threads whose window the user closed since the last reconcile, for `follow` to settle.
    pub closed: Vec<ThreadRef>,
    /// Conversation panes seen running tr3x, by session, so each is asked only once.
    pub verified: HashMap<String, HashSet<String>>,
}

/// A session tr3x keeps: the merged one, or one machine's.
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
    /// Makes the state file hold `state`, writing only if it doesn't already.
    pub fn persist(&mut self) -> Result<()> {
        self.disk.save(&self.state).map(drop)
    }

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
        self.persist()?;
        if self.first
            && spec.first
            && let Err(e) = self.rex.move_session(&id, 1).await
        {
            eprintln!("tr3x: couldn't move the session first: {e:#}");
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
            Err(e) => eprintln!("tr3x: couldn't watch for new windows: {e:#}"),
        }
    }

    /// Closes a session tr3x made that the `sessions` setting no longer asks for. Its threads'
    /// windows open again in the sessions that hold them now, and nothing is settled.
    pub async fn retire(&mut self, key: &str) -> Result<()> {
        let Some(group) = self.state.groups.remove(key) else {
            return Ok(());
        };
        self.persist()?;
        if let Some(id) = group.session_id {
            self.watchers.remove(&id);
            let args = json!({ "session_id": id }).to_string();
            self.rex.call("session.destroy", &args).await?;
        }
        Ok(())
    }

    /// The windows in `ours` whose conversation pane runs something other than tr3x. Each
    /// pane is asked once: one seen running tr3x is remembered, so a pass with nothing new
    /// asks Rex nothing more. A pane that can't say is asked again next time.
    async fn dead(
        &mut self,
        session: &str,
        windows: &[Value],
        ours: &HashSet<String>,
    ) -> HashSet<String> {
        let mut dead = HashSet::new();
        let mut seen = HashSet::new();
        let mut verified = self.verified.remove(session).unwrap_or_default();
        for w in windows {
            let (Some(id), Some(block)) = (
                w["window_id"].as_str(),
                conversation(w).and_then(|b| b["block_id"].as_str()),
            ) else {
                continue;
            };
            seen.insert(block.to_string());
            if !ours.contains(id) || verified.contains(block) {
                continue;
            }
            let Ok(process) = self.rex.block_method(session, block, "process").await else {
                continue;
            };
            match runs_tr3x(&process, &self.exe) {
                Some(true) => {
                    verified.insert(block.to_string());
                }
                Some(false) => {
                    dead.insert(id.to_string());
                }
                None => {}
            }
        }
        verified.retain(|b| seen.contains(b));
        self.verified.insert(session.to_string(), verified);
        dead
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
        let ours: HashSet<String> = group
            .windows
            .values()
            .map(|w| w.window_id.clone())
            .collect();
        let dead = self.dead(&session, windows, &ours).await;
        if !dead.is_empty() {
            eprintln!(
                "tr3x: {} conversation pane(s) had something else running in them; starting them again",
                dead.len()
            );
        }
        let group = self.state.groups.entry(spec.key.clone()).or_default();
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
        let mut plan = plan(
            wants,
            &present,
            view["active_window_id"].as_str(),
            group,
            health,
        );
        self.closed.append(&mut plan.settle);
        plan.close.append(&mut extra);
        let closing = plan.close.clone();
        let own_pane = crate::reply::own_pane();
        plan.rebuild = reshape(windows, group, wants, own_pane, &dead);
        plan.rebuild.retain(|r| !closing.contains(&r.window));
        if plan.is_empty() {
            self.persist()?;
            return Ok(false);
        }
        let result = self
            .rex
            .lua(&session, &script(&plan, &self.exe, &self.configs, own_pane))
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
                        child: want.child,
                    },
                );
            }
        }
        for error in result["errors"].as_array().into_iter().flatten() {
            eprintln!("tr3x: rex: {error}");
        }
        self.persist()?;
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
    /// The `Env` the subscription connects through, which holds the machine's descriptor.
    env: Option<Arc<Env>>,
    /// The machine's shell subscription. The track owns it: forgetting the machine, or sync
    /// ending, drops the track and with it the subscription.
    forwarder: Option<Forwarder>,
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

/// Where Find Thread asks sync to show threads it hid when their window closed, one
/// `env/thread` a line.
fn restore_path() -> PathBuf {
    crate::state::dir().join("restore")
}

/// Asks the running sync to show `key` again, even if its window was closed while it worked,
/// and wakes it to do so now.
pub fn restore(key: &str) -> Result<()> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(restore_path())?;
    writeln!(file, "{key}")?;
    if let Some(pid) = running() {
        // SAFETY: kill(2) with a pid the lock holder wrote; sync handles SIGUSR1.
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGUSR1) };
    }
    Ok(())
}

/// The threads Find Thread asked for since the last pass.
fn take_restores() -> Vec<String> {
    let taken = restore_path().with_extension("taken");
    if std::fs::rename(restore_path(), &taken).is_err() {
        return Vec::new();
    }
    let text = std::fs::read_to_string(&taken).unwrap_or_default();
    let _ = std::fs::remove_file(taken);
    text.lines().map(str::to_string).collect()
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
            anyhow::bail!("another tr3x sync is already running");
        }
        return Ok(());
    }
    // Find Thread wakes sync with SIGUSR1 once the pid is written, so it's handled first.
    let mut wake = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::user_defined1())?;
    lock.set_len(0)?;
    write!(lock, "{}", std::process::id())?;
    if log {
        log_to_file()?;
    }
    tokio::spawn(crate::fade::follow());
    // Once a day: tr3x's and each machine's T3's updates. On `auto`, tr3x installs its own.
    tokio::spawn(crate::update::follow());
    // Pictures sent to this machine with Taildrop go to the thread in front.
    tokio::spawn(crate::taildrop::follow());
    // Other machines join with `curl …/join | sh`; off a tailnet there's nothing to listen on.
    tokio::spawn(async {
        if let Err(e) = t3client::join::serve(t3::hub().store).await {
            eprintln!("tr3x: not taking joins: {e:#}");
        }
    });
    let mut pause = 2;
    loop {
        let started = Instant::now();
        let Err(error) = follow(first, &mut wake).await;
        if started.elapsed() > Duration::from_secs(120) {
            pause = 2;
        }
        eprintln!("tr3x: {error:#}; trying again in {pause}s");
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

type Shells = tokio::sync::mpsc::Sender<(String, WatchEvent)>;

/// Gives each of `envs` not followed yet a track and a shell subscription, and lets go of the
/// tracks of machines no longer among them, subscriptions with them. A machine that's only out
/// of reach stays among `envs` and keeps reconnecting on its own. A machine whose record changed
/// (a new address, say) comes as a new `Env`: its subscription starts again through it, since
/// readiness is read from the `Env` that connects. Returns the names of the machines newly followed.
fn follow_machines(
    tracks: &mut BTreeMap<String, Track>,
    envs: &[Arc<Env>],
    tx: &Shells,
    now: Instant,
) -> Vec<String> {
    let mut followed = Vec::new();
    for env in envs {
        match tracks.get_mut(env.id()) {
            Some(track) => {
                if !track.env.as_ref().is_some_and(|e| Arc::ptr_eq(e, env)) {
                    track.env = Some(env.clone());
                    // Replacing the forwarder ends the old subscription.
                    track.forwarder = Some(Hub::forward(env.clone(), tx.clone()));
                    track.configured = false;
                    track.retry_at = None;
                }
            }
            None => {
                followed.push(env.name().to_string());
                tracks.insert(
                    env.id().to_string(),
                    Track {
                        down_since: Some(now),
                        env: Some(env.clone()),
                        forwarder: Some(Hub::forward(env.clone(), tx.clone())),
                        ..Track::default()
                    },
                );
            }
        }
    }
    tracks.retain(|id, _| envs.iter().any(|e| e.id() == id));
    followed
}

/// Every machine's shell, reconciling the session after each burst of changes and every few
/// seconds, which picks up snoozes that end, windows closed in Rex and machines that go quiet.
/// Each machine connects and reconnects on its own. Returns only when it fails.
async fn follow(
    first: bool,
    wake: &mut tokio::signal::unix::Signal,
) -> Result<std::convert::Infallible> {
    let mut hub = t3::hub();
    // A Mac without T3 of its own can still follow the machines it's paired with.
    let home = hub.home_id().unwrap_or_default().to_string();
    if hub.envs().is_empty() {
        anyhow::bail!(
            "T3 Code isn't set up on this Mac, and Rex isn't paired with another machine"
        );
    }
    let found = State::load();
    let mut state = found.clone();
    if !home.is_empty() {
        state.migrate(&home);
    }
    let mut syncer = Syncer {
        rex: Rex::find(),
        exe: crate::icons::real_exe(),
        state,
        disk: Disk::new(found),
        first,
        watchers: HashMap::new(),
        configs: HashMap::new(),
        closed: Vec::new(),
        verified: HashMap::new(),
    };
    if !home.is_empty() {
        syncer.persist()?;
    }
    let (tx, mut events) = tokio::sync::mpsc::channel(32);
    let (config_tx, mut configs) = tokio::sync::mpsc::unbounded_channel::<(String, Value)>();
    let mut tracks: BTreeMap<String, Track> = BTreeMap::new();
    let started = Instant::now();
    follow_machines(&mut tracks, hub.envs(), &tx, started);
    let mut stamp = machines_stamp(&hub);
    let mut inbox = Inbox::default();
    let mut dirty = false;
    let mut last: Vec<Want> = Vec::new();
    let mut tick = tokio::time::interval(Duration::from_secs(5));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut next_reconcile = tokio::time::Instant::now();
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
                            eprintln!("tr3x: {} is back", name_of(&hub, &env));
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
                        eprintln!("tr3x: {}: {reason}", name_of(&hub, &env));
                        track.reason = reason;
                    }
                }
                WatchEvent::Failed(message) => {
                    eprintln!(
                        "tr3x: {} refused the subscription: {message}",
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
            _ = wake.recv() => dirty = true,
            _ = tokio::time::sleep_until(next_reconcile), if dirty => {},
        }
        // Drain a burst before acting, so a snapshot's items reconcile once.
        for _ in 0..32 {
            let Ok((env, event)) = events.try_recv() else {
                break;
            };
            take(env, event, &mut tracks);
        }
        if !dirty || tokio::time::Instant::now() < next_reconcile {
            continue;
        }
        dirty = false;
        next_reconcile = tokio::time::Instant::now() + Duration::from_millis(100);
        let now = Instant::now();
        // Find Thread brought these back: a window closed while they worked no longer hides them.
        for key in take_restores() {
            for group in syncer.state.groups.values_mut() {
                group.dismissed.remove(&key);
            }
        }

        // A machine paired, renamed or forgotten while sync runs.
        if machines_stamp(&hub) != stamp {
            stamp = machines_stamp(&hub);
            // Reloaded in place: a fresh `Env` has no descriptor until it connects, and only
            // the subscriptions connect, so replacing every machine's left them all unready.
            hub = hub.reload();
            for name in follow_machines(&mut tracks, hub.envs(), &tx, now) {
                eprintln!("tr3x: following {name}");
            }
        }
        for env in hub.envs() {
            let Some(track) = tracks.get_mut(env.id()) else {
                continue;
            };
            if track.retry_at.is_some_and(|at| at <= now) {
                track.retry_at = None;
                // The refused subscription is over; this replaces it.
                track.forwarder = Some(Hub::forward(env.clone(), tx.clone()));
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

        // A machine's capabilities come from its descriptor. Until that has loaded, T3's rules
        // can't be read (without settlement every settled thread looks open), so the machine
        // counts as not ready: it adds nothing, and keeps the windows it has.
        let described: HashSet<String> = hub
            .envs()
            .iter()
            .filter(|e| e.descriptor().is_some())
            .map(|e| e.id().to_string())
            .collect();
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
                (track.shell.synchronized && described.contains(e.id())).then(|| MachineShell {
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
            Some(track) => {
                let mut health = track.health(now);
                health.ready &= described.contains(env);
                health
            }
            // A machine tr3x no longer follows: its windows go.
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
                eprintln!("tr3x: couldn't close an old session: {e:#}");
            }
        }
        let mut changed = false;
        for (spec, group_wants) in &specs {
            match syncer
                .reconcile(spec, group_wants, &known, &envs, &home, &health)
                .await
            {
                Ok(c) => changed |= c,
                Err(e) => eprintln!("tr3x: {e:#}"),
            }
        }
        if changed || wants.len() != last.len() {
            let names: Vec<&str> = wants.iter().map(|w| w.label.as_str()).collect();
            eprintln!("tr3x: {} thread(s): {}", wants.len(), names.join(" | "));
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
            syncer.persist()?;
        }
        // Which subagent works for which thread, for the actions that go between them. A
        // subagent that finished but still has its window keeps its entry.
        let mut parents: BTreeMap<String, String> = syncer
            .state
            .parents
            .iter()
            .filter(|(child, _)| {
                syncer
                    .state
                    .groups
                    .values()
                    .any(|g| g.windows.contains_key(*child))
            })
            .map(|(c, p)| (c.clone(), p.clone()))
            .collect();
        for w in &wants {
            if let Some(parent) = &w.parent {
                parents.insert(w.key(), parent.key.clone());
            }
        }
        if parents != syncer.state.parents {
            syncer.state.parents = parents;
            syncer.persist()?;
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
                    eprintln!("tr3x: couldn't settle {thread}: {e:#}");
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

    /// Pairing a machine while sync runs reloads the registry. The machines already followed
    /// keep the `Env` their subscription connects through, so they stay described and ready;
    /// a machine whose record changed is followed through its new `Env`.
    #[tokio::test]
    async fn pairing_a_machine_keeps_the_others_followed() {
        use t3client::store::{Machine, Registry, Store, Via};

        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        let machine = |id: &str| Machine {
            id: id.into(),
            label: id.into(),
            alias: None,
            // Nothing answers here: the subscriptions only retry.
            origins: vec!["http://127.0.0.1:9".into()],
            ssh: None,
            via: Via::Link,
            version: String::new(),
        };
        let mut registry = Registry::default();
        registry.upsert(machine("a"));
        registry.upsert(machine("b"));
        store.save_registry(&registry).unwrap();
        let hub = Hub::load(dir.path());
        let (tx, _events) = tokio::sync::mpsc::channel(32);
        let mut tracks = BTreeMap::new();
        follow_machines(&mut tracks, hub.envs(), &tx, Instant::now());
        tracks.get_mut("a").unwrap().configured = true;
        tracks.get_mut("b").unwrap().configured = true;

        registry.upsert(machine("c"));
        registry
            .machines
            .iter_mut()
            .find(|m| m.id == "b")
            .unwrap()
            .origins = vec!["http://127.0.0.1:10".into()];
        store.save_registry(&registry).unwrap();
        let hub2 = hub.reload();
        let followed = follow_machines(&mut tracks, hub2.envs(), &tx, Instant::now());

        assert_eq!(followed, vec!["c".to_string()]);
        let env_of = |id: &str| tracks[id].env.clone().unwrap();
        assert!(Arc::ptr_eq(&env_of("a"), &hub.get("a").unwrap()));
        assert!(tracks["a"].configured);
        assert!(Arc::ptr_eq(&env_of("b"), &hub2.get("b").unwrap()));
        assert!(!tracks["b"].configured);
        assert!(hub2.envs().iter().all(|e| Arc::ptr_eq(e, &env_of(e.id()))));
    }

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
            child: false,
            parent: None,
            needs_you: false,
            alone: None,
        }
    }

    fn win(id: &str, label: &str) -> Window {
        Window {
            window_id: id.into(),
            label: label.into(),
            child: false,
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
            None,
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
        let plan1 = plan(&[want("a", "A")], &[], None, &mut group, &ready);
        assert!(plan1.create.is_empty());
        assert_eq!(group.dismissed.get("h/a").map(String::as_str), Some("u1"));
        let mut changed = want("a", "A");
        changed.updated_at = "u2".into();
        let plan2 = plan(&[changed], &[], None, &mut group, &ready);
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
            None,
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
        assert!(
            plan(&wants, &[], None, &mut group, &ready)
                .settle
                .is_empty()
        );
        // The last window, closed on its own, is the user's.
        let mut group = Group::default();
        group.windows.insert("h/a".into(), win("w-a", "A"));
        assert_eq!(
            plan(&[want("a", "A")], &[], None, &mut group, &ready).settle,
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
        assert!(plan(&wants, &present, None, &mut group, &ready).is_empty());
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
        let plan = plan(&wants, &present, None, &mut group, &b_down);
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
        let plan = super::plan(&back, &present, None, &mut group, &ready);
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
    fn windows_take_the_shape_the_setting_asks_for() {
        let mut group = Group::default();
        group.windows.insert("home-env/a".into(), win("w-a", "A"));
        group.windows.insert("home-env/b".into(), win("w-b", "B"));
        let windows = vec![
            tagged_window("w-a", "home-env/a"),
            json!({"window_id": "w-b", "label": "B", "layers": [{"blocks": [
                {"block_id": "b9", "label": "t3:home-env/b"}]}]}),
        ];
        let wants: Vec<Want> = ["a", "b"]
            .iter()
            .map(|t| Want {
                thread: ThreadRef::new("home-env", *t),
                label: t.to_string(),
                cwd: None,
                updated_at: String::new(),
                place: Place::Open,
                instance: String::new(),
                settles: true,
                child: false,
                parent: None,
                needs_you: false,
                alone: None,
            })
            .collect();
        let rebuild = reshape(&windows, &group, &wants, false, &HashSet::new());
        assert_eq!(rebuild.len(), 1);
        assert_eq!(rebuild[0].window, "w-a");
        assert_eq!(rebuild[0].blocks, vec!["b1".to_string(), "b2".to_string()]);
        assert_eq!(rebuild[0].want.key(), "home-env/a");
        let rebuild = reshape(&windows, &group, &wants, true, &HashSet::new());
        assert_eq!(rebuild.len(), 1);
        assert_eq!(rebuild[0].window, "w-b");
        assert_eq!(rebuild[0].blocks, vec!["b9".to_string()]);
    }

    #[test]
    fn a_pane_runs_tr3x_by_its_program_not_its_label() {
        let exe = std::path::Path::new("/Users/me/.local/bin/tr3x");
        let running = |path: &str| json!({"last_exit": null, "child": {"name": "x", "path": path}});
        // Started under a provider's name, the program is still tr3x.
        assert_eq!(
            runs_tr3x(&running("/Users/me/.local/bin/tr3x"), exe),
            Some(true)
        );
        // What Rex leaves after a restart: a shell under the conversation's label.
        assert_eq!(runs_tr3x(&running("/bin/zsh"), exe), Some(false));
        assert_eq!(
            runs_tr3x(&json!({"last_exit": null, "child": null}), exe),
            None
        );
        assert_eq!(runs_tr3x(&Value::Null, exe), None);
    }

    #[test]
    fn a_window_with_a_dead_conversation_is_rebuilt_in_place() {
        let mut group = Group::default();
        group.windows.insert("home-env/a".into(), win("w-a", "A"));
        let windows = vec![tagged_window("w-a", "home-env/a")];
        let wants = vec![want("home-env/a", "A")];
        // Already the shape the setting asks for: nothing to do while tr3x runs in it.
        assert!(reshape(&windows, &group, &wants, true, &HashSet::new()).is_empty());
        let dead: HashSet<String> = ["w-a".to_string()].into();
        let rebuild = reshape(&windows, &group, &wants, true, &dead);
        assert_eq!(rebuild.len(), 1);
        assert_eq!(rebuild[0].window, "w-a");
        assert_eq!(rebuild[0].blocks, vec!["b1".to_string(), "b2".to_string()]);
    }

    #[test]
    fn lost_windows_are_adopted_and_duplicates_closed() {
        let mut group = Group::default();
        group.windows.insert("home-env/a".into(), win("w-a", "A"));
        let windows = vec![
            tagged_window("w-a", "home-env/a"),
            tagged_window("w-a2", "home-env/a"),
            tagged_window("w-b", "home-env/b"),
            // A window from before tr3x knew more than one machine.
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

    fn child_of(parent: &str, id: &str, extra: Value) -> Value {
        let mut t = thread(id, extra);
        t["lineage"] = json!({"parentThreadId": parent, "relationshipToParent": "subagent", "rootThreadId": parent});
        t
    }

    /// The thread a subagent works for is archived, so it has no window. A subagent of it that
    /// needs you still does, named for it; one that only works doesn't.
    #[test]
    fn a_subagents_request_shows_without_its_thread() {
        let shell = ShellState {
            threads: vec![
                thread("open", json!({})),
                thread(
                    "old",
                    json!({"archivedAt": "2026-10-30T00:00:00.000Z", "title": "Refactor sync"}),
                ),
                child_of(
                    "old",
                    "asks",
                    json!({"creationSource": "mcp", "title": "Audit tests", "pendingRuntimeRequest": {"kind": "command"}}),
                ),
                child_of(
                    "old",
                    "failed",
                    json!({"creationSource": "mcp", "status": "error", "latestRunId": "r", "lastErrorClass": "provider_error"}),
                ),
                child_of(
                    "old",
                    "busy",
                    json!({"creationSource": "mcp", "activeRunId": "r", "latestRunId": "r", "activityRunStatus": "running"}),
                ),
            ],
            ..ShellState::default()
        };
        let wants = wanted(&[machine("h", &shell, None)], &mut Inbox::default(), 0);
        let ids: Vec<&str> = wants.iter().map(|w| w.thread.thread.as_str()).collect();
        assert!(ids.contains(&"asks") && !ids.contains(&"busy") && !ids.contains(&"old"));
        assert!(
            !ids.contains(&"failed"),
            "a failure under a closed thread is history"
        );
        let asks = wants.iter().find(|w| w.thread.thread == "asks").unwrap();
        assert_eq!(asks.label, "↳ ● Audit tests · for Refactor sync");
        assert_eq!(asks.place, Place::Attention);
        assert!(asks.parent.is_none() && asks.child && asks.needs_you);
    }

    /// A subagent that only works says nothing new until it changes, so closing its window
    /// hides it until it needs you.
    #[test]
    fn a_working_subagent_stays_closed_until_it_needs_you() {
        let working = json!({"creationSource": "mcp", "activeRunId": "r", "latestRunId": "r", "activityRunStatus": "running"});
        let at = |updated: &str, extra: Value| {
            let mut t = child_of("a", "task", working.clone());
            t["updatedAt"] = json!(updated);
            if !extra.is_null() {
                for (k, v) in extra.as_object().unwrap() {
                    t[k] = v.clone();
                }
            }
            ShellState {
                threads: vec![thread("a", json!({})), t],
                ..ShellState::default()
            }
        };
        let first = at("2026-11-04T10:00:00.000Z", Value::Null);
        let later = at("2026-11-04T10:00:09.000Z", Value::Null);
        let asks = at(
            "2026-11-04T10:00:20.000Z",
            json!({"pendingRuntimeRequest": {"kind": "command"}}),
        );
        let stamp = |shell: &ShellState| {
            wanted(&[machine("h", shell, None)], &mut Inbox::default(), 0)
                .into_iter()
                .find(|w| w.thread.thread == "task")
                .unwrap()
                .updated_at
        };
        assert_eq!(stamp(&first), stamp(&later));
        assert_ne!(stamp(&first), stamp(&asks));
    }

    fn sub(key: &str, label: &str, parent: &str, needs_you: bool) -> Want {
        Want {
            child: true,
            needs_you,
            parent: Some(Parent {
                key: parent.into(),
                title: "Parent".into(),
            }),
            alone: Some(format!("{label} · for Parent")),
            ..want(key, label)
        }
    }

    /// Closing a thread's window takes its subagents' windows with it, except one that needs
    /// you, which stays and says whose it is.
    #[test]
    fn closing_a_thread_closes_its_busy_subagents_but_not_one_that_asks() {
        let mut group = Group::default();
        group.windows.insert("h/busy".into(), win("w-b", "↳ busy"));
        group.windows.insert("h/asks".into(), win("w-k", "↳ asks"));
        group.dismissed.insert("h/a".into(), "u1".into());
        let present = vec![
            ("w-b".into(), "↳ busy".into()),
            ("w-k".into(), "↳ asks".into()),
        ];
        let wants = [
            want("a", "A"),
            sub("h/busy", "↳ busy", "h/a", false),
            sub("h/asks", "↳ asks", "h/a", true),
        ];
        let plan = plan(&wants, &present, None, &mut group, &ready);
        assert_eq!(plan.close, vec!["w-b".to_string()]);
        assert_eq!(
            plan.relabel,
            vec![("w-k".to_string(), "↳ asks · for Parent".to_string())]
        );
        assert!(group.windows.contains_key("h/asks") && !group.windows.contains_key("h/busy"));
    }

    /// A subagent that finished while it had focus keeps its window until you leave it.
    #[test]
    fn a_finished_subagent_stays_while_it_has_focus() {
        let mut open = Group::default();
        open.windows.insert("h/a".into(), win("w-a", "A"));
        let mut child = win("w-c", "↳ done");
        child.child = true;
        open.windows.insert("h/task".into(), child);
        let present = vec![("w-a".into(), "A".into()), ("w-c".into(), "↳ done".into())];
        let wants = [want("a", "A")];

        let mut group = open.clone();
        let made = super::plan(&wants, &present, Some("w-c"), &mut group, &ready);
        assert!(made.close.is_empty() && group.windows.contains_key("h/task"));

        // Gone from view, it closes.
        let mut group = open.clone();
        let made = super::plan(&wants, &present, Some("w-a"), &mut group, &ready);
        assert_eq!(made.close, vec!["w-c".to_string()]);

        // Only a subagent lingers: an ordinary thread's window closes under you as before.
        let mut group = open;
        group.windows.get_mut("h/task").unwrap().child = false;
        let made = super::plan(&wants, &present, Some("w-c"), &mut group, &ready);
        assert_eq!(made.close, vec!["w-c".to_string()]);
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
        assert_eq!(
            label("o/x"),
            "● x · studio",
            "an approval's mark leads the label"
        );
    }

    #[test]
    fn status_marks() {
        let marks: Vec<Option<char>> = [
            "Approval", "Input", "Failed", "Limited", "Done", "Woke", "Working", "Goal", "Waiting",
        ]
        .into_iter()
        .map(|w| glyph(Some(w)))
        .collect();
        let want = [
            Some('●'),
            Some('?'),
            Some('✕'),
            Some('✕'),
            Some('✓'),
            Some('✓'),
        ];
        assert_eq!(&marks[..6], &want);
        assert!(marks[6..].iter().all(Option::is_none) && glyph(None).is_none());
    }

    #[test]
    fn labels_are_one_clean_line() {
        assert_eq!(label("  fix\tthe\nthing "), "fix the thing");
        assert_eq!(label(""), "Untitled");
        assert_eq!(label(&"x".repeat(300)).chars().count(), 120);
    }

    /// A fake T3 that holds every connection open and counts them, whole and still open.
    struct Fake {
        origin: String,
        total: Arc<std::sync::atomic::AtomicUsize>,
        live: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl Fake {
        /// Answers nothing: a machine that is subscribing but hasn't been heard from. With
        /// `hang_up`, closes each connection as soon as it opens: a machine that's out of reach.
        async fn start(hang_up: bool) -> Fake {
            use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
            use tokio::io::AsyncReadExt;
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let origin = format!("http://{}", listener.local_addr().unwrap());
            let (total, live) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
            let (t, l) = (total.clone(), live.clone());
            tokio::spawn(async move {
                loop {
                    let Ok((mut stream, _)) = listener.accept().await else {
                        return;
                    };
                    t.fetch_add(1, SeqCst);
                    if hang_up {
                        continue;
                    }
                    l.fetch_add(1, SeqCst);
                    let l = l.clone();
                    tokio::spawn(async move {
                        let mut buf = [0u8; 1024];
                        while stream.read(&mut buf).await.is_ok_and(|n| n > 0) {}
                        l.fetch_sub(1, SeqCst);
                    });
                }
            });
            Fake {
                origin,
                total,
                live,
            }
        }

        fn total(&self) -> usize {
            self.total.load(std::sync::atomic::Ordering::SeqCst)
        }

        /// Waits until `count` connections are open, and fails if that doesn't happen.
        async fn live(&self, count: usize) {
            for _ in 0..100 {
                if self.live.load(std::sync::atomic::Ordering::SeqCst) == count {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            panic!(
                "{} open connections, wanted {count}",
                self.live.load(std::sync::atomic::Ordering::SeqCst)
            );
        }
    }

    fn env_at(id: &str, origin: &str) -> Arc<Env> {
        use t3client::store::{Machine, Store, Via};
        let machine = Machine {
            id: id.into(),
            label: id.into(),
            alias: None,
            origins: vec![origin.into()],
            ssh: None,
            via: Via::Link,
            version: String::new(),
        };
        Arc::new(Env::new(Store::new(std::env::temp_dir()), machine))
    }

    /// Each machine gets one subscription for as long as it's followed. Forgetting it ends that
    /// one alone, pairing it again makes one new one rather than a second beside the old, and a
    /// machine that is merely out of reach keeps reconnecting on its own.
    #[tokio::test]
    async fn a_followed_machine_has_one_subscription_until_it_is_forgotten() {
        let (a, b, c) = (
            Fake::start(false).await,
            Fake::start(false).await,
            Fake::start(true).await,
        );
        let (tx, _events) = tokio::sync::mpsc::channel(32);
        let mut tracks = BTreeMap::new();
        let (env_a, env_b, env_c) = (
            env_at("a", &a.origin),
            env_at("b", &b.origin),
            env_at("c", &c.origin),
        );

        let followed = follow_machines(
            &mut tracks,
            &[env_a.clone(), env_b.clone(), env_c.clone()],
            &tx,
            Instant::now(),
        );
        assert_eq!(followed, ["a", "b", "c"]);
        a.live(1).await;
        b.live(1).await;
        // Out of reach: it hangs up on each try, and sync tries again.
        for _ in 0..100 {
            if c.total() >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(c.total() >= 2, "{} tries", c.total());

        // Nothing changed: nobody subscribes twice.
        let envs = [env_a.clone(), env_b, env_c.clone()];
        assert!(follow_machines(&mut tracks, &envs, &tx, Instant::now()).is_empty());
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!((a.total(), b.total()), (1, 1));

        // b is forgotten: its subscription goes, the others stay.
        let envs = [env_a.clone(), env_c.clone()];
        assert!(follow_machines(&mut tracks, &envs, &tx, Instant::now()).is_empty());
        assert_eq!(tracks.keys().collect::<Vec<_>>(), ["a", "c"]);
        b.live(0).await;
        a.live(1).await;

        // b is paired again: one subscription, though the old one's machine had the same id.
        let envs = [env_a, env_at("b", &b.origin), env_c];
        assert_eq!(
            follow_machines(&mut tracks, &envs, &tx, Instant::now()),
            ["b"]
        );
        b.live(1).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            (b.total(), b.live.load(std::sync::atomic::Ordering::SeqCst)),
            (2, 1)
        );
        assert_eq!(a.total(), 1);

        // Sync ends: every subscription with it.
        drop(tracks);
        a.live(0).await;
        b.live(0).await;
    }

    /// A Rex that answers `ls` and `session inspect` from files and records what it was asked.
    struct FakeRex {
        dir: tempfile::TempDir,
    }

    impl FakeRex {
        fn new(windows: Value) -> FakeRex {
            use std::os::unix::fs::PermissionsExt;
            let dir = tempfile::tempdir().unwrap();
            let bin = dir.path().join("rex");
            std::fs::write(
                &bin,
                "#!/bin/sh\nd=$(dirname \"$0\")\necho \"$*\" >> \"$d/calls\"\n\
                 case \"$2\" in\n ls) cat \"$d/ls.json\";;\n session) cat \"$d/inspect.json\";;\n \
                 do) cat > /dev/null;;\n block) cat \"$d/process.json\" 2>/dev/null;;\nesac\n",
            )
            .unwrap();
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
            let rex = FakeRex { dir };
            std::fs::write(
                rex.dir.path().join("ls.json"),
                json!({"sessions": [{"session_id": "s1", "label": "T3 Code"}]}).to_string(),
            )
            .unwrap();
            rex.show(windows);
            rex
        }

        /// What `session inspect` shows from now on.
        fn show(&self, windows: Value) {
            let view = json!({"session_id": "s1", "windows": windows});
            std::fs::write(self.dir.path().join("inspect.json"), view.to_string()).unwrap();
        }

        fn bin(&self) -> Rex {
            Rex {
                bin: self.dir.path().join("rex"),
            }
        }

        /// The window of `tag`: its conversation, and Reply in a pane of its own if that's the
        /// setting, so tr3x has nothing to reshape.
        fn window(id: &str, tag: &str) -> Value {
            let mut blocks = vec![json!({"block_id": "b1", "label": format!("t3:{tag}")})];
            if crate::reply::own_pane() {
                blocks.push(json!({"block_id": "b2", "label": "Reply"}));
            }
            json!({"window_id": id, "label": "A", "layers": [{"blocks": blocks}]})
        }
    }

    /// Reconciles session `s1` once, wanting thread `e/a`.
    async fn reconcile_once(syncer: &mut Syncer) -> bool {
        let a = want("e/a", "A");
        let known: HashSet<String> = [a.key()].into();
        let spec = SessionSpec {
            key: String::new(),
            label: "T3 Code".into(),
            env: None,
            first: false,
        };
        syncer
            .reconcile(&spec, &[a], &known, &["e".to_string()], "e", &|_| {
                Health::READY
            })
            .await
            .unwrap()
    }

    /// Rex brought the session back after a restart with a shell where the conversation was.
    /// The window is rebuilt where it is, not settled as closed, and a pane seen running tr3x
    /// isn't asked about again.
    #[tokio::test]
    async fn a_shell_left_under_a_conversation_label_is_replaced() {
        let rex = FakeRex::new(json!([FakeRex::window("w9", "e/a")]));
        let mut state = State {
            version: 2,
            ..State::default()
        };
        let mut group = Group {
            session_id: Some("s1".into()),
            ..Group::default()
        };
        group.windows.insert("e/a".into(), win("w9", "A"));
        state.groups.insert(String::new(), group);
        let mut syncer = Syncer {
            rex: rex.bin(),
            exe: PathBuf::from("/opt/tr3x/tr3x"),
            disk: Disk::at(rex.dir.path().join("state.json"), State::default()),
            state,
            first: false,
            watchers: HashMap::new(),
            configs: HashMap::new(),
            closed: Vec::new(),
            verified: HashMap::new(),
        };
        let process = rex.dir.path().join("process.json");
        let calls = || std::fs::read_to_string(rex.dir.path().join("calls")).unwrap_or_default();
        let count = |what: &str| calls().lines().filter(|l| l.contains(what)).count();

        std::fs::write(&process, json!({"child": {"path": "/bin/zsh"}}).to_string()).unwrap();
        // Rebuilt: a pass with something to ask Rex for.
        assert!(reconcile_once(&mut syncer).await);
        assert!(syncer.closed.is_empty());
        assert_eq!(syncer.state.groups[""].windows["e/a"].window_id, "w9");

        std::fs::write(
            &process,
            json!({"child": {"path": "/opt/tr3x/tr3x"}}).to_string(),
        )
        .unwrap();
        assert!(!reconcile_once(&mut syncer).await);
        let asked = count("block call");
        assert!(!reconcile_once(&mut syncer).await);
        assert_eq!(count("block call"), asked);
    }

    fn saved_windows(file: &std::path::Path) -> Vec<String> {
        let on_disk: Value = serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap();
        on_disk["groups"][""]["windows"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect()
    }

    /// A pass that has nothing to ask Rex for leaves the file alone, but not when it still
    /// changed what tr3x knows: a window it takes back, or one the user closed.
    #[tokio::test]
    async fn a_pass_with_nothing_to_do_writes_only_what_it_learned() {
        let rex = FakeRex::new(json!([]));
        let file = rex.dir.path().join("state.json");
        let mut state = State {
            version: 2,
            ..State::default()
        };
        state.groups.insert(
            String::new(),
            Group {
                session_id: Some("s1".into()),
                ..Group::default()
            },
        );
        let mut syncer = Syncer {
            rex: rex.bin(),
            exe: PathBuf::from("tr3x"),
            disk: Disk::at(file.clone(), State::default()),
            state,
            first: false,
            watchers: HashMap::new(),
            configs: HashMap::new(),
            closed: Vec::new(),
            verified: HashMap::new(),
        };
        syncer.persist().unwrap();
        assert!(saved_windows(&file).is_empty());

        // Rex shows a window for the thread that the state doesn't have. The plan is empty, and
        // the window is taken back and saved all the same.
        rex.show(json!([FakeRex::window("w9", "e/a")]));
        assert!(!reconcile_once(&mut syncer).await);
        assert_eq!(saved_windows(&file), ["e/a"]);

        // The same again: no change, no write.
        std::fs::write(&file, "untouched").unwrap();
        assert!(!reconcile_once(&mut syncer).await);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "untouched");

        // The user closes the window: nothing to ask Rex for, and the thread is remembered as
        // dismissed.
        rex.show(json!([FakeRex::window("w1", "e/other")]));
        assert!(!reconcile_once(&mut syncer).await);
        let on_disk: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!(on_disk["groups"][""]["dismissed"]["e/a"], "u1");
        assert_eq!(syncer.closed.len(), 1);
    }
}
