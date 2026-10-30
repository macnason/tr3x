//! A new thread from Rex: the window Rex's New Tab makes in the T3 Code session. It is T3's
//! composer in a terminal: the first message, and under it the project, model, effort,
//! access, workspace and device chips. Enter launches the thread through T3's
//! `orchestration.launchThread`, and the window becomes that thread's: this pane turns into the
//! conversation and the Reply pane opens under it.
//!
//! The pane labels itself `t3:<thread>` before it launches, with the id the launch will use, so
//! sync takes the window as the thread's as soon as T3 reports the thread.

use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;

use anyhow::{Context, Result};
use crossterm::event::{
    Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use futures_util::StreamExt;
use serde_json::{Value, json};
use t3term::client::Client;
use t3term::launch::{LaunchRequest, Workspace};
use t3term::models;
use tokio::sync::mpsc;

use crate::chat::{BLUE, BOLD, DIM, RED, RESET, fit};
use crate::compose::{Editor, edit, wrap};
use crate::icons::{self, Icon, Painter};
use crate::picker::{self, Chip, Item, Menu, MenuKey, Settings};
use crate::rex::{Rex, lua_str};
use crate::t3;

fn out(text: &str) {
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(text.as_bytes());
    let _ = stdout.flush();
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Field {
    Project,
    Model,
    Effort,
    Access,
    Workspace,
    Branch,
    Device,
}

/// What arrives from work started in the background.
enum Loaded {
    Icons(HashMap<String, Icon>),
    Logos(HashMap<String, Icon>),
    Branches(String, Vec<Value>, bool),
    Peers(Vec<Value>),
    Launched(Result<String, String>),
}

struct NewThread {
    client: Arc<Client>,
    config: Value,
    projects: Vec<Value>,
    /// The most recent thread of each project, for its model.
    recent: HashMap<String, Value>,
    project: String,
    settings: Settings,
    worktree: bool,
    /// The project's branches, by project id, and whether its folder is a git repository.
    branches: HashMap<String, (Vec<Value>, bool)>,
    base: Option<String>,
    /// Linked environments T3 knows, from `peerLinks.list`.
    peers: Vec<Value>,
    /// None for this Mac, else a peer's environment id.
    device: Option<String>,
    icons: HashMap<String, Icon>,
    logos: HashMap<String, Icon>,
    editor: Editor,
    focus: Option<usize>,
    menu: Option<(Field, Menu)>,
    note: Option<(String, bool)>,
    launching: bool,
    welcome: bool,
    cols: usize,
    rows: usize,
    painter: Painter,
    chip_spans: Vec<(usize, usize)>,
    chip_row: u16,
    tx: mpsc::UnboundedSender<Loaded>,
}

/// The settings T3 starts a new thread in this project with: the project's default model,
/// else the environment's, else the model of the project's latest thread, else the first
/// enabled model. The runtime mode is the project's or environment's default.
fn defaults(config: &Value, project: &Value, recent: Option<&Value>) -> Settings {
    let selection = [
        &project["defaultModelSelection"],
        &config["settings"]["defaultModelSelection"],
        recent.map_or(&Value::Null, |t| &t["modelSelection"]),
    ]
    .into_iter()
    .find(|s| s["instanceId"].is_string() && s["model"].is_string())
    .cloned();
    let project_id = project["id"].as_str().unwrap_or_default();
    let mode = models::default_runtime_mode(config, project_id)
        .map(|(m, _)| m.to_string())
        .unwrap_or_else(|_| "full-access".into());
    let thread = match selection {
        Some(selection) => json!({"modelSelection": selection, "runtimeMode": mode}),
        None => {
            let first = models::providers(config)
                .iter()
                .filter(|p| models::enabled(p))
                .find_map(|p| models::models(p).first().map(|m| (p, m)));
            match first {
                Some((p, m)) => json!({"modelSelection": {
                    "instanceId": p["instanceId"], "model": m["slug"]}, "runtimeMode": mode}),
                None => json!({"runtimeMode": mode}),
            }
        }
    };
    Settings::of_thread(config, &thread)
}

/// Whether new threads in this project start in a worktree: the project's override, else the
/// environment's setting, else the project folder, as T3 resolves `defaultThreadEnvMode`.
fn starts_in_worktree(config: &Value, project: &Value) -> bool {
    let id = project["id"].as_str().unwrap_or_default();
    let mode = project["defaultThreadEnvMode"]
        .as_str()
        .or(config["settings"]["projectSettingsOverrides"][id]["defaultThreadEnvMode"].as_str())
        .or(config["settings"]["defaultThreadEnvMode"].as_str())
        .unwrap_or("local");
    mode == "worktree"
}

impl NewThread {
    fn project_value(&self) -> Option<&Value> {
        self.projects
            .iter()
            .find(|p| p["id"].as_str() == Some(self.project.as_str()))
    }

    fn is_repo(&self) -> bool {
        self.branches
            .get(&self.project)
            .is_none_or(|(_, repo)| *repo)
    }

    fn fields(&self) -> Vec<Field> {
        let mut out = vec![Field::Project, Field::Model];
        if !picker::effort_items(&self.config, &self.settings).is_empty() {
            out.push(Field::Effort);
        }
        out.push(Field::Access);
        if self.is_repo() {
            out.push(Field::Workspace);
            if self.worktree {
                out.push(Field::Branch);
            }
        }
        if !self.peers.is_empty() {
            out.push(Field::Device);
        }
        out
    }

    fn chip(&self, field: Field) -> Chip {
        let (icon, label) = match field {
            Field::Project => {
                let p = self.project_value();
                (
                    Some(
                        self.icons
                            .get(&self.project)
                            .cloned()
                            .or_else(|| p.map(icons::fallback))
                            .unwrap_or(Icon::Text("PR".into(), 4)),
                    ),
                    p.and_then(|p| p["title"].as_str())
                        .unwrap_or("Project")
                        .to_string(),
                )
            }
            Field::Model => (
                self.logos.get(&self.settings.instance).cloned(),
                self.settings.model_label(&self.config),
            ),
            Field::Effort => (
                None,
                self.settings.effort_label(&self.config).unwrap_or_default(),
            ),
            Field::Access => (None, self.settings.mode_label()),
            Field::Workspace => (
                None,
                if self.worktree {
                    "New worktree"
                } else {
                    "Local"
                }
                .to_string(),
            ),
            Field::Branch => (
                None,
                format!("from {}", self.base.as_deref().unwrap_or("…")),
            ),
            Field::Device => (
                None,
                match &self.device {
                    None => "This Mac".to_string(),
                    Some(id) => self
                        .peers
                        .iter()
                        .find(|p| p["environmentId"].as_str() == Some(id.as_str()))
                        .and_then(|p| p["label"].as_str())
                        .unwrap_or("Device")
                        .to_string(),
                },
            ),
        };
        Chip { icon, label }
    }

    fn open(&mut self, field: Field) {
        let (items, current): (Vec<Item>, Option<String>) = match field {
            Field::Project => (
                self.projects
                    .iter()
                    .filter_map(|p| {
                        let id = p["id"].as_str()?;
                        let mut item = Item::new(id, p["title"].as_str().unwrap_or("Project"));
                        item.icon = Some(
                            self.icons
                                .get(id)
                                .cloned()
                                .unwrap_or_else(|| icons::fallback(p)),
                        );
                        item.detail = p["workspaceRoot"]
                            .as_str()
                            .map(crate::chat::short_home)
                            .unwrap_or_default();
                        Some(item)
                    })
                    .collect(),
                Some(self.project.clone()),
            ),
            Field::Model => (
                picker::model_items(&self.config, &self.logos),
                Some(format!(
                    "{}/{}",
                    self.settings.instance, self.settings.model
                )),
            ),
            Field::Effort => (
                picker::effort_items(&self.config, &self.settings),
                picker::effort_now(&self.config, &self.settings),
            ),
            Field::Access => (
                picker::mode_items(&self.config, &self.settings),
                Some(self.settings.mode.clone()),
            ),
            Field::Workspace => (
                vec![
                    Item::new("local", "Local"),
                    Item::new("worktree", "New worktree"),
                ],
                Some(if self.worktree { "worktree" } else { "local" }.into()),
            ),
            Field::Branch => (
                self.branches
                    .get(&self.project)
                    .map(|(refs, _)| refs.as_slice())
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|r| {
                        let name = r["name"].as_str()?;
                        let mut item = Item::new(name, name);
                        if r["isDefault"] == true {
                            item.detail = "default".into();
                        } else if r["current"] == true {
                            item.detail = "current".into();
                        }
                        Some(item)
                    })
                    .collect(),
                self.base.clone(),
            ),
            Field::Device => {
                let mut items = vec![Item::new("", "This Mac")];
                for p in &self.peers {
                    let (Some(id), Some(label)) =
                        (p["environmentId"].as_str(), p["label"].as_str())
                    else {
                        continue;
                    };
                    let mut item = Item::new(id, label);
                    item.detail = if p["status"] == "reachable" {
                        "in T3 Code".into()
                    } else {
                        p["status"].as_str().unwrap_or_default().to_string()
                    };
                    items.push(item);
                }
                (items, Some(self.device.clone().unwrap_or_default()))
            }
        };
        self.menu = Some((field, Menu::new(items, current.as_deref())));
    }

    fn choose(&mut self, field: Field, id: String) {
        match field {
            Field::Project => self.set_project(id),
            Field::Model => self.settings.set_model(&self.config, &id),
            Field::Effort => self.settings.effort = Some(id),
            Field::Access => self.settings.mode = id,
            Field::Workspace => self.worktree = id == "worktree",
            Field::Branch => self.base = Some(id),
            Field::Device => self.device = (!id.is_empty()).then_some(id),
        }
    }

    fn set_project(&mut self, id: String) {
        self.project = id;
        if let Some(p) = self.project_value().cloned() {
            self.settings = defaults(&self.config, &p, self.recent.get(&self.project));
            self.worktree = starts_in_worktree(&self.config, &p);
        }
        self.base = None;
        self.pick_base();
        self.load_branches();
    }

    /// The base branch T3 would pick: the default branch, else the current one.
    fn pick_base(&mut self) {
        if self.base.is_some() {
            return;
        }
        let Some((refs, _)) = self.branches.get(&self.project) else {
            return;
        };
        self.base = refs
            .iter()
            .find(|r| r["isDefault"] == true)
            .or_else(|| refs.iter().find(|r| r["current"] == true))
            .or(refs.first())
            .and_then(|r| r["name"].as_str())
            .map(str::to_string);
    }

    fn load_branches(&self) {
        if self.branches.contains_key(&self.project) {
            return;
        }
        let Some(root) = self
            .project_value()
            .and_then(|p| p["workspaceRoot"].as_str())
            .map(str::to_string)
        else {
            return;
        };
        let (client, tx, project) = (self.client.clone(), self.tx.clone(), self.project.clone());
        tokio::spawn(async move {
            let Ok(rpc) = client.rpc().await else { return };
            let result = rpc
                .call(
                    "vcs.listRefs",
                    json!({"cwd": root, "limit": 100, "refKind": "local"}),
                    std::time::Duration::from_secs(15),
                )
                .await;
            let (refs, repo) = match result {
                Ok(r) => (
                    r["refs"].as_array().cloned().unwrap_or_default(),
                    r["isRepo"].as_bool().unwrap_or(true),
                ),
                Err(_) => (Vec::new(), true),
            };
            let _ = tx.send(Loaded::Branches(project, refs, repo));
        });
    }

    fn draw(&mut self) {
        let width = self.cols.saturating_sub(4).clamp(20, 100);
        let left = 2;
        let pad = " ".repeat(left);
        let mut lines: Vec<String> = vec![String::new()];
        // The message, in a rounded box like T3's composer.
        let inner = width.saturating_sub(4);
        let (rows, cursor) = wrap(&self.editor.text, self.editor.cursor, inner);
        let mut body: Vec<String> = rows;
        let empty = self.editor.text.is_empty();
        if empty {
            body = vec![format!("{DIM}Ask anything{RESET}")];
        }
        while body.len() < 3 {
            body.push(String::new());
        }
        lines.push(format!(
            "{pad}{DIM}╭{}╮{RESET}",
            "─".repeat(width.saturating_sub(2))
        ));
        let first_body = lines.len();
        for row in &body {
            let w = crate::render::visible_width(row);
            lines.push(format!(
                "{pad}{DIM}│{RESET} {row}{} {DIM}│{RESET}",
                " ".repeat(inner.saturating_sub(w))
            ));
        }
        lines.push(format!(
            "{pad}{DIM}╰{}╯{RESET}",
            "─".repeat(width.saturating_sub(2))
        ));
        let fields = self.fields();
        let chips: Vec<Chip> = fields.iter().map(|f| self.chip(*f)).collect();
        let open = self
            .menu
            .as_ref()
            .and_then(|(f, _)| fields.iter().position(|x| x == f));
        let (row, spans) = picker::chips(&mut self.painter, &chips, self.focus, open, left);
        self.chip_row = lines.len() as u16;
        lines.push(format!("{pad}{row}"));
        self.chip_spans = spans;
        if let Some((_, menu)) = self.menu.as_mut() {
            lines.push(String::new());
            let top = lines.len() as u16;
            let room = self.rows.saturating_sub(lines.len() + 2).max(3);
            for line in menu.lines(&mut self.painter, top, width, room) {
                lines.push(format!("{pad}{line}"));
            }
        } else if self.welcome {
            lines.push(String::new());
            lines.push(String::new());
            let keys = [
                ("⌘T", "New thread"),
                ("⌃⌘I", "Next that needs you"),
                ("⌃⌘M", "Model"),
                ("⌃⌘S", "Settle"),
            ];
            let text: Vec<String> = keys
                .iter()
                .map(|(k, what)| format!("{BOLD}{k}{RESET} {DIM}{what}{RESET}"))
                .collect();
            lines.push(format!("{pad}{}", text.join("    ")));
        }
        if let Some((note, error)) = &self.note {
            let color = if *error { RED } else { DIM };
            lines.push(String::new());
            lines.push(format!("{pad}{color}{}{RESET}", fit(note, width)));
        } else if self.launching {
            lines.push(String::new());
            lines.push(format!("{pad}{BLUE}●{RESET} {DIM}Starting…{RESET}"));
        }
        let mut text = String::from("\x1b[?2026h\x1b[H\x1b[2J");
        text.push_str(Painter::clear());
        for (i, line) in lines.iter().take(self.rows).enumerate() {
            text.push_str(&format!("\x1b[{};1H{line}", i + 1));
        }
        if self.menu.is_none() && self.focus.is_none() {
            let (r, c) = (first_body + cursor.0 + 1, left + 3 + cursor.1);
            text.push_str(&format!("\x1b[{r};{c}H\x1b[?25h"));
        } else {
            text.push_str("\x1b[?25l");
        }
        text.push_str("\x1b[?2026l");
        out(&text);
    }

    fn key(&mut self, key: KeyEvent) -> bool {
        self.note = None;
        let fields = self.fields();
        if let Some((field, menu)) = self.menu.as_mut() {
            let field = *field;
            match menu.key(key) {
                MenuKey::Chose(id) => {
                    self.menu = None;
                    self.choose(field, id);
                }
                MenuKey::Close => self.menu = None,
                MenuKey::Moved => {}
            }
            return false;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match (key.code, self.focus) {
            (KeyCode::Tab, f) => {
                self.focus = match f {
                    None => Some(0),
                    Some(i) if i + 1 < fields.len() => Some(i + 1),
                    Some(_) => None,
                }
            }
            (KeyCode::BackTab, f) => {
                self.focus = match f {
                    None => Some(fields.len() - 1),
                    Some(0) => None,
                    Some(i) => Some(i - 1),
                }
            }
            (KeyCode::Esc, Some(_)) => self.focus = None,
            (KeyCode::Left, Some(i)) => self.focus = Some(i.saturating_sub(1)),
            (KeyCode::Right, Some(i)) => self.focus = Some((i + 1).min(fields.len() - 1)),
            (KeyCode::Enter | KeyCode::Down | KeyCode::Char(' '), Some(i)) => {
                if let Some(f) = fields.get(i) {
                    self.open(*f);
                }
            }
            (KeyCode::Up, Some(_)) => self.focus = None,
            (KeyCode::Char('c'), _) if ctrl => {
                if self.editor.text.is_empty() {
                    return true;
                }
                self.editor.take();
            }
            (KeyCode::Enter, None)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT) =>
            {
                self.send()
            }
            (_, Some(_)) => {
                // Typing on a chip goes to the message.
                self.focus = None;
                edit(&mut self.editor, key);
            }
            (_, None) => {
                edit(&mut self.editor, key);
            }
        }
        false
    }

    fn click(&mut self, col: u16, row: u16) {
        self.note = None;
        if let Some((field, menu)) = self.menu.as_ref() {
            let field = *field;
            if let Some(id) = menu.click(row) {
                self.menu = None;
                self.choose(field, id);
                return;
            }
        }
        let fields = self.fields();
        let col = col as usize + 1;
        if row == self.chip_row
            && let Some(i) = self
                .chip_spans
                .iter()
                .position(|(a, b)| (*a..=*b).contains(&col))
        {
            let reopen = self.menu.as_ref().map(|(f, _)| *f) == fields.get(i).copied();
            self.menu = None;
            self.focus = Some(i);
            if !reopen {
                self.open(fields[i]);
            }
            return;
        }
        self.menu = None;
        self.focus = None;
    }

    fn send(&mut self) {
        if self.launching {
            return;
        }
        let prompt = self.editor.string();
        if prompt.trim().is_empty() {
            return;
        }
        if let Some(device) = self.device.clone() {
            // A linked environment takes launches only from a client paired with it, which T3
            // Code is. Hand the message over.
            let link = format!("t3code://new?environmentId={device}");
            let copied = std::process::Command::new("/usr/bin/pbcopy")
                .stdin(std::process::Stdio::piped())
                .spawn()
                .and_then(|mut child| {
                    child
                        .stdin
                        .take()
                        .context("no stdin")
                        .map_err(std::io::Error::other)?
                        .write_all(prompt.as_bytes())?;
                    child.wait()
                })
                .is_ok_and(|s| s.success());
            let _ = std::process::Command::new("/usr/bin/open")
                .arg(&link)
                .status();
            self.note = Some((
                if copied {
                    "Opened T3 Code. The message is on the clipboard.".into()
                } else {
                    "Opened T3 Code.".into()
                },
                false,
            ));
            return;
        }
        let plan =
            match models::plan_new_thread(&self.config, &self.project, &self.settings.choice()) {
                Ok((plan, _)) => plan,
                Err(e) => {
                    self.note = Some((format!("{e:#}"), true));
                    return;
                }
            };
        let workspace = if self.worktree && self.is_repo() {
            match &self.base {
                Some(base) => Workspace::Worktree {
                    base_ref: base.clone(),
                    branch: None,
                    start_from_origin: false,
                },
                None => {
                    self.note = Some(("Choose a branch to start the worktree from".into(), true));
                    return;
                }
            }
        } else {
            Workspace::Root
        };
        let request = match LaunchRequest::new(
            &self.client,
            &self.project,
            &prompt,
            None,
            &plan,
            workspace,
        ) {
            Ok(r) => r,
            Err(e) => {
                self.note = Some((format!("{e:#}"), true));
                return;
            }
        };
        self.launching = true;
        let (client, tx) = (self.client.clone(), self.tx.clone());
        tokio::spawn(async move {
            let thread = request.thread_id().to_string();
            // Named first, so sync finds this window when T3 reports the thread.
            let _ = set_label(&crate::sync::tag(&thread)).await;
            let result = match client.launch_thread(&request).await {
                Ok(receipt) => Ok(receipt.thread_id),
                Err(e) => {
                    let _ = set_label("New Thread").await;
                    Err(format!("{e:#}"))
                }
            };
            let _ = tx.send(Loaded::Launched(result));
        });
    }
}

/// Labels this pane's block.
async fn set_label(label: &str) -> Result<()> {
    let (Ok(session), Ok(block)) = (std::env::var("REX_SESSION"), std::env::var("REX_BLOCK"))
    else {
        return Ok(());
    };
    let script = format!(
        "rex.session.set_block_label{{ block_id = {}, label = {} }}\nreturn {{}}",
        lua_str(&block),
        lua_str(label)
    );
    Rex::find().lua(&session, &script).await.map(drop)
}

/// The Reply pane under this one, for `thread`.
async fn open_reply(thread: &str, cwd: Option<&str>) -> Result<()> {
    let (Ok(session), Ok(block)) = (std::env::var("REX_SESSION"), std::env::var("REX_BLOCK"))
    else {
        return Ok(());
    };
    let exe = std::env::current_exe()?.to_string_lossy().into_owned();
    let data = json!({"block": block, "argv": [exe, "compose", thread], "cwd": cwd});
    let script = format!(
        r#"local d = rex.json.decode({})
local r, err = rex.session.new_split{{ anchor_id = d.block, direction = "vertical", side = "after",
  ratio = 0.86, focus = true,
  layout = {{ block = {{ flavor = "com.superlogical.terminal.shell", label = "Reply",
    options = {{ shell = "none", command = d.argv, cwd = d.cwd, exit = {{ on_completion = false }} }} }} }} }}
rex.session.disown_block{{}}
if err then error(err, 0) end
-- The app evens out a new split when its Autobalance setting is on; set the ratio again after.
rex.sleep(0.5)
rex.session.resize_pane{{ before_block_id = d.block, after_block_id = r.block_ids[1],
  direction = "vertical", ratio = 0.86 }}
return {{}}"#,
        lua_str(&data.to_string())
    );
    Rex::find().lua(&session, &script).await.map(drop)
}

pub async fn run(welcome: bool) -> Result<()> {
    let client = t3::connect(true).await?;
    let (config, shell) = tokio::try_join!(client.server_config(), client.shell())?;
    // Projects with the most recent work first, as T3's picker lists them.
    let mut recent: HashMap<String, Value> = HashMap::new();
    for t in &shell.threads {
        if t["lineage"]["relationshipToParent"] == "subagent" {
            continue;
        }
        let Some(p) = t["projectId"].as_str() else {
            continue;
        };
        let newer = recent
            .get(p)
            .is_none_or(|r| r["updatedAt"].as_str() < t["updatedAt"].as_str());
        if newer {
            recent.insert(p.to_string(), t.clone());
        }
    }
    let mut projects = shell.projects.clone();
    projects.sort_by(|a, b| {
        let at = |p: &Value| {
            p["id"]
                .as_str()
                .and_then(|id| recent.get(id))
                .and_then(|t| t["updatedAt"].as_str())
                .unwrap_or("")
                .to_string()
        };
        at(b).cmp(&at(a))
    });
    let first = projects
        .first()
        .and_then(|p| p["id"].as_str())
        .context("T3 has no projects yet. Add one in T3 Code.")?
        .to_string();
    let (tx, mut loaded) = mpsc::unbounded_channel();
    let (cols, rows) = crossterm::terminal::size().unwrap_or((100, 30));
    let mut ui = NewThread {
        client: client.clone(),
        config,
        projects,
        recent,
        project: String::new(),
        settings: Settings::default(),
        worktree: false,
        branches: HashMap::new(),
        base: None,
        peers: Vec::new(),
        device: None,
        icons: HashMap::new(),
        logos: HashMap::new(),
        editor: Editor::default(),
        focus: None,
        menu: None,
        note: None,
        launching: false,
        welcome,
        cols: cols as usize,
        rows: rows as usize,
        painter: Painter,
        chip_spans: Vec::new(),
        chip_row: 0,
        tx: tx.clone(),
    };
    ui.set_project(first);
    {
        let (c, t, projects) = (client.clone(), tx.clone(), ui.projects.clone());
        tokio::spawn(async move {
            let _ = t.send(Loaded::Icons(icons::projects(&c, &projects).await));
        });
        let (t, providers) = (tx.clone(), models::providers(&ui.config).to_vec());
        tokio::spawn(async move {
            let _ = t.send(Loaded::Logos(logos(&providers).await));
        });
        let tx = tx.clone();
        tokio::spawn(async move {
            // Linked environments need the access:read scope, which only this call asks for.
            let Ok(access) = t3::connect_access().await else {
                return;
            };
            let Ok(rpc) = access.rpc().await else { return };
            if let Ok(r) = rpc
                .call(
                    "peerLinks.list",
                    json!({}),
                    std::time::Duration::from_secs(10),
                )
                .await
            {
                let _ = tx.send(Loaded::Peers(
                    r["links"].as_array().cloned().unwrap_or_default(),
                ));
            }
        });
    }
    crossterm::terminal::enable_raw_mode()?;
    // The alternate screen, bracketed paste, and clicks (SGR mouse reports).
    out("\x1b]2;New Thread\x07\x1b[?1049h\x1b[?2004h\x1b[?1000h\x1b[?1006h");
    let mut input = EventStream::new();
    ui.draw();
    let mut launched: Option<String> = None;
    let result: Result<()> = async {
        loop {
            tokio::select! {
                event = input.next() => match event {
                    Some(Ok(Event::Key(key))) if key.kind != KeyEventKind::Release => {
                        if ui.launching { continue; }
                        if ui.key(key) { return Ok(()); }
                    }
                    Some(Ok(Event::Paste(text))) if !ui.launching => {
                        ui.menu = None;
                        ui.focus = None;
                        ui.editor.insert(&text.replace("\r\n", "\n").replace('\r', "\n"));
                    }
                    Some(Ok(Event::Mouse(m))) if !ui.launching => {
                        if let MouseEventKind::Down(MouseButton::Left) = m.kind {
                            ui.click(m.column, m.row);
                        } else {
                            continue;
                        }
                    }
                    Some(Ok(Event::Resize(c, r))) => {
                        ui.cols = c as usize;
                        ui.rows = r as usize;
                    }
                    Some(Err(e)) => return Err(e.into()),
                    None => return Ok(()),
                    _ => continue,
                },
                Some(event) = loaded.recv() => match event {
                    Loaded::Icons(icons) => ui.icons = icons,
                    Loaded::Logos(logos) => ui.logos = logos,
                    Loaded::Peers(peers) => ui.peers = peers,
                    Loaded::Branches(project, refs, repo) => {
                        ui.branches.insert(project, (refs, repo));
                        ui.pick_base();
                    }
                    Loaded::Launched(Ok(thread)) => {
                        launched = Some(thread);
                        return Ok(());
                    }
                    Loaded::Launched(Err(e)) => {
                        ui.launching = false;
                        ui.note = Some((e, true));
                    }
                },
            }
            ui.draw();
        }
    }
    .await;
    out("\x1b[?1006l\x1b[?1000l\x1b[?2004l\x1b[?1049l");
    let _ = crossterm::terminal::disable_raw_mode();
    result?;
    let Some(thread) = launched else {
        return Ok(());
    };
    let cwd = ui
        .project_value()
        .and_then(|p| p["workspaceRoot"].as_str())
        .map(str::to_string);
    if let Err(e) = open_reply(&thread, cwd.as_deref()).await {
        eprintln!("t3rex: couldn't open the Reply pane: {e:#}");
    }
    if let Some(dir) = &cwd {
        let _ = std::env::set_current_dir(dir);
    }
    // This pane becomes the conversation. The login goes first; exec skips the exit that
    // would revoke it.
    drop(ui);
    drop(client);
    t3term::auth::revoke_all_sessions();
    use std::os::unix::process::CommandExt;
    let error = std::process::Command::new(std::env::current_exe()?)
        .args(["chat", &thread])
        .exec();
    Err(error.into())
}

/// Every provider's logo, by instance id.
pub async fn logos(providers: &[Value]) -> HashMap<String, Icon> {
    let dark = crate::media::mac_is_dark();
    let mut out = HashMap::new();
    for p in providers {
        if let Some(id) = p["instanceId"].as_str() {
            out.insert(id.to_string(), icons::provider(p, dark).await);
        }
    }
    out
}
