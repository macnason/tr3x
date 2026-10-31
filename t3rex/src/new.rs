//! A new thread from Rex: the window Rex's New Tab makes in the T3 Code session. It is T3's
//! composer in a terminal: the first message, and under it the project, model, effort,
//! access and workspace chips. The project chip also adds a folder as a project, and with no
//! projects yet the composer opens on that. Enter launches the thread through T3's
//! `orchestration.launchThread`, and the window becomes that thread's: this pane turns into the
//! conversation and the Reply pane opens under it.
//!
//! The pane labels itself `t3:<thread>` before it launches, with the id the launch will use, so
//! sync takes the window as the thread's as soon as T3 reports the thread.

use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;

use anyhow::Result;
use crossterm::event::{
    Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use futures_util::StreamExt;
use serde_json::{Value, json};
use t3client::env::Env;
use t3client::launch::{LaunchRequest, Workspace};
use t3client::models;
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
}

/// What arrives from work started in the background.
enum Loaded {
    Icons(HashMap<String, Icon>),
    Logos(HashMap<String, Icon>),
    Branches(String, Vec<Value>, bool),
    Launched(Result<String, String>),
    /// A folder T3 took as a project, or why it didn't.
    Added(Result<Value, String>),
}

struct NewThread {
    client: Arc<Env>,
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
    icons: HashMap<String, Icon>,
    logos: HashMap<String, Icon>,
    editor: Editor,
    focus: Option<usize>,
    menu: Option<(Field, Menu)>,
    /// The folder being added as a project, from the project chip's Add folder….
    folder: Option<Folder>,
    note: Option<(String, bool)>,
    launching: bool,
    welcome: bool,
    cols: usize,
    rows: usize,
    painter: Painter,
    chip_spans: Vec<(usize, usize)>,
    chip_row: u16,
    chat_width: crate::width::ChatWidth,
    tx: mpsc::UnboundedSender<Loaded>,
}

/// The project menu's item that adds a folder instead.
const ADD_FOLDER: &str = "\u{0}add-folder";

/// A folder being typed in to add as a project: its path, and the folders under the part
/// typed so far that start with the rest.
struct Folder {
    path: Editor,
    menu: Menu,
}

impl Folder {
    fn new(path: &str) -> Folder {
        let mut folder = Folder {
            path: Editor::default(),
            menu: Menu::default(),
        };
        folder.path.insert(path);
        folder.list();
        folder
    }

    fn list(&mut self) {
        let typed = self.path.string();
        let (dir, start) = split_path(&typed);
        let mut names = subfolders(&expand(&dir), &start);
        names.truncate(200);
        self.menu = Menu::new(
            names
                .into_iter()
                .map(|n| Item::new(format!("{dir}{n}/"), n))
                .collect(),
            None,
        );
    }

    /// Completes the path to `to`, a folder from the list.
    fn complete(&mut self, to: &str) {
        self.path.take();
        self.path.insert(to);
        self.list();
    }
}

/// `~` and `~/…` as the home folder.
fn expand(path: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    match path.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("{home}{rest}"),
        _ => path.to_string(),
    }
}

/// A typed path as the folder it names so far, with its `/`, and the start of the next name.
fn split_path(typed: &str) -> (String, String) {
    match typed.rfind('/') {
        Some(i) => (typed[..=i].to_string(), typed[i + 1..].to_string()),
        None => (String::new(), typed.to_string()),
    }
}

/// The folders in `dir` whose names start with `start`, ignoring case, sorted. Hidden ones
/// only once a `.` is typed.
fn subfolders(dir: &str, start: &str) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(if dir.is_empty() { "." } else { dir }) else {
        return Vec::new();
    };
    let start = start.to_lowercase();
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|e| std::fs::metadata(e.path()).is_ok_and(|m| m.is_dir()))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.to_lowercase().starts_with(&start))
        .filter(|n| !n.starts_with('.') || start.starts_with('.'))
        .collect();
    names.sort_by_key(|n| n.to_lowercase());
    names
}

/// A folder's absolute path for T3, without a trailing `/`, or why it can't be a project.
fn folder_root(typed: &str) -> Result<String, String> {
    let path = expand(typed.trim());
    let path = if path.len() > 1 {
        path.trim_end_matches('/').to_string()
    } else {
        path
    };
    if !path.starts_with('/') {
        return Err("Type a full path, such as ~/Developer/app".into());
    }
    if !std::path::Path::new(&path).is_dir() {
        return Err(format!("No folder at {}", crate::chat::short_home(&path)));
    }
    Ok(path)
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
                    .chain(std::iter::once(Item::new(ADD_FOLDER, "Add folder…")))
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
        };
        self.menu = Some((field, Menu::new(items, current.as_deref())));
    }

    fn choose(&mut self, field: Field, id: String) {
        match field {
            Field::Project if id == ADD_FOLDER => self.folder = Some(Folder::new("~/")),
            Field::Project => self.set_project(id),
            Field::Model => self.settings.set_model(&self.config, &id),
            Field::Effort => self.settings.effort = Some(id),
            Field::Access => self.settings.mode = id,
            Field::Workspace => self.worktree = id == "worktree",
            Field::Branch => self.base = Some(id),
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

    /// Adds the folder typed in as a project, through the same call `t3 project add` makes, or
    /// picks the project that already has it.
    fn add_folder(&mut self) {
        let Some(folder) = self.folder.as_ref() else {
            return;
        };
        let root = match folder_root(&folder.path.string()) {
            Ok(root) => root,
            Err(e) => {
                self.note = Some((e, true));
                return;
            }
        };
        let known = self
            .projects
            .iter()
            .find(|p| p["workspaceRoot"].as_str() == Some(root.as_str()))
            .and_then(|p| p["id"].as_str())
            .map(str::to_string);
        if let Some(id) = known {
            self.folder = None;
            return self.set_project(id);
        }
        let title = std::path::Path::new(&root)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| "project".into());
        let (client, tx) = (self.client.clone(), self.tx.clone());
        tokio::spawn(async move {
            let id = uuid::Uuid::new_v4().to_string();
            let mutation = json!({
                "type": "project.create",
                "commandId": format!("t3rex:{}", uuid::Uuid::new_v4()),
                "projectId": id,
                "title": title,
                "workspaceRoot": root,
            });
            let result = async {
                let rpc = client.rpc().await?;
                let made = rpc
                    .call(
                        "projects.mutate",
                        mutation,
                        std::time::Duration::from_secs(15),
                    )
                    .await?;
                anyhow::Ok(made)
            }
            .await
            .map(|made| {
                if made["id"].is_string() {
                    made
                } else {
                    json!({"id": id, "title": title, "workspaceRoot": root})
                }
            })
            .map_err(|e| format!("{e:#}"));
            let _ = tx.send(Loaded::Added(result));
        });
    }

    fn folder_key(&mut self, key: KeyEvent) {
        let alone = self.projects.is_empty();
        let Some(folder) = self.folder.as_mut() else {
            return;
        };
        let at_end = folder.path.cursor == folder.path.text.len();
        match key.code {
            // With no project yet, there is nothing to go back to.
            KeyCode::Esc if !alone => self.folder = None,
            KeyCode::Enter => self.add_folder(),
            KeyCode::Tab => {
                let shown = folder.menu.shown();
                if let Some(&i) = shown.get(folder.menu.selected) {
                    let to = folder.menu.items[i].id.clone();
                    folder.complete(&to);
                }
            }
            KeyCode::Right if at_end => {
                let shown = folder.menu.shown();
                if let Some(&i) = shown.get(folder.menu.selected) {
                    let to = folder.menu.items[i].id.clone();
                    folder.complete(&to);
                }
            }
            KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown => {
                folder.menu.key(key);
            }
            _ => {
                if edit(&mut folder.path, key) {
                    folder.list();
                }
            }
        }
    }

    fn draw(&mut self) {
        // The column a thread's conversation reads in, as T3 lines its composer up with it.
        let (left, width) = self.chat_width.column(self.cols);
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
        let open = if self.folder.is_some() {
            fields.iter().position(|f| *f == Field::Project)
        } else {
            open
        };
        let (row, spans) = picker::chips(&mut self.painter, &chips, self.focus, open, left);
        self.chip_row = lines.len() as u16;
        lines.push(format!("{pad}{row}"));
        self.chip_spans = spans;
        let mut folder_cursor = None;
        if let Some(folder) = self.folder.as_mut() {
            lines.push(String::new());
            let (shown, cursor) = wrap(
                &folder.path.text,
                folder.path.cursor,
                width.saturating_sub(10),
            );
            let field = format!("{DIM}Folder{RESET}  ");
            folder_cursor = Some((lines.len() + cursor.0 + 1, left + 9 + cursor.1));
            for (i, line) in shown.iter().enumerate() {
                let lead = if i == 0 { field.clone() } else { " ".repeat(8) };
                lines.push(format!("{pad}{lead}{line}"));
            }
            let top = lines.len() as u16;
            let room = self.rows.saturating_sub(lines.len() + 2).max(3);
            if !folder.menu.items.is_empty() {
                for line in folder.menu.lines(&mut self.painter, top, width, room) {
                    lines.push(format!("{pad}{line}"));
                }
            }
        } else if let Some((_, menu)) = self.menu.as_mut() {
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
        if let Some((r, c)) = folder_cursor {
            text.push_str(&format!("\x1b[{r};{c}H\x1b[?25h"));
        } else if self.menu.is_none() && self.focus.is_none() {
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
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if self.folder.is_some() {
            if ctrl && key.code == KeyCode::Char('c') {
                return true;
            }
            self.folder_key(key);
            return false;
        }
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
        if let Some(folder) = self.folder.as_mut()
            && let Some(to) = folder.menu.click(row)
        {
            return folder.complete(&to);
        }
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
            let reopen = self.menu.as_ref().map(|(f, _)| *f) == fields.get(i).copied()
                || self.folder.is_some() && fields[i] == Field::Project;
            self.menu = None;
            if !self.projects.is_empty() {
                self.folder = None;
            }
            self.focus = Some(i);
            if !reopen {
                self.open(fields[i]);
            }
            return;
        }
        self.menu = None;
        self.focus = None;
        if !self.projects.is_empty() {
            self.folder = None;
        }
    }

    fn send(&mut self) {
        if self.launching {
            return;
        }
        let prompt = self.editor.string();
        if prompt.trim().is_empty() {
            return;
        }
        if self.project.is_empty() {
            self.folder.get_or_insert_with(|| Folder::new("~/"));
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

/// The Reply pane under this one, for `thread`, run as `program`.
async fn open_reply(thread: &str, cwd: Option<&str>, program: &std::path::Path) -> Result<()> {
    let (Ok(session), Ok(block)) = (std::env::var("REX_SESSION"), std::env::var("REX_BLOCK"))
    else {
        return Ok(());
    };
    let exe = program.to_string_lossy().into_owned();
    let data = json!({"block": block, "argv": [exe, "compose", thread], "cwd": cwd});
    let script = format!(
        r#"local d = rex.json.decode({})
local r, err = rex.session.new_split{{ anchor_id = d.block, direction = "vertical", side = "after",
  ratio = 0.86, focus = true,
  layout = {{ block = {{ flavor = "com.superlogical.terminal.shell", label = "Reply",
    options = {{ shell = "none", login = false, command = d.argv, cwd = d.cwd, exit = {{ on_completion = false }} }} }} }} }}
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

/// Whether T3's server isn't there to answer, rather than refusing.
fn unavailable(error: &anyhow::Error) -> bool {
    error
        .chain()
        .filter_map(|e| e.downcast_ref::<t3client::error::T3Error>())
        .any(|e| e.exit_code == t3client::error::exit::UNAVAILABLE)
}

/// T3, once it answers. Until then the pane says what's wrong, in place of the composer, and
/// tries again every few seconds, so it opens as soon as T3 Code does.
async fn connect() -> Result<(Arc<Env>, Value, t3client::projection::ShellState)> {
    let mut shown = false;
    loop {
        let attempt = async {
            let client = t3::connect().await?;
            let (config, shell) = tokio::try_join!(client.server_config(), client.shell())?;
            anyhow::Ok((client, config, shell))
        }
        .await;
        let error = match attempt {
            Ok(loaded) => {
                if shown {
                    out("\x1b[H\x1b[2J\x1b[?25h");
                }
                return Ok(loaded);
            }
            Err(e) if unavailable(&e) => "T3 Code isn't running. Open T3 Code.".to_string(),
            Err(e) => format!("{e:#}"),
        };
        let (cols, _) = crossterm::terminal::size().unwrap_or((100, 30));
        let (left, width) = crate::width::current().0.column(cols as usize);
        out(&format!(
            "\x1b]2;New Thread\x07\x1b[H\x1b[2J\x1b[?25l\x1b[3;{}H{RED}{}{RESET}",
            left + 1,
            fit(&error, width)
        ));
        shown = true;
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }
}

pub async fn run(welcome: bool) -> Result<()> {
    let (client, config, shell) = connect().await?;
    // The scheme first, so the logos loaded next are for the screen they're seen on.
    let mut scheme = crate::scheme::listen();
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
        .map(str::to_string);
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
        icons: HashMap::new(),
        logos: HashMap::new(),
        editor: Editor::default(),
        focus: None,
        menu: None,
        folder: None,
        note: None,
        launching: false,
        welcome,
        cols: cols as usize,
        rows: rows as usize,
        painter: Painter,
        chip_spans: Vec::new(),
        chip_row: 0,
        chat_width: crate::width::current().0,
        tx: tx.clone(),
    };
    match first {
        Some(first) => ui.set_project(first),
        // No projects yet: a folder first, and the model T3 would start any thread with.
        None => {
            ui.settings = defaults(&ui.config, &Value::Null, None);
            ui.folder = Some(Folder::new("~/"));
        }
    }
    {
        let (c, t, projects) = (client.clone(), tx.clone(), ui.projects.clone());
        tokio::spawn(async move {
            let _ = t.send(Loaded::Icons(icons::projects(&c, &projects).await));
        });
        let (t, providers) = (tx.clone(), models::providers(&ui.config).to_vec());
        tokio::spawn(async move {
            let _ = t.send(Loaded::Logos(logos(&providers).await));
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
                Ok(()) = scheme.changed() => {
                    let (t, providers) = (ui.tx.clone(), models::providers(&ui.config).to_vec());
                    tokio::spawn(async move {
                        let _ = t.send(Loaded::Logos(logos(&providers).await));
                    });
                }
                Some(event) = loaded.recv() => match event {
                    Loaded::Icons(icons) => ui.icons = icons,
                    Loaded::Logos(logos) => ui.logos = logos,
                    Loaded::Added(Ok(project)) => {
                        let id = project["id"].as_str().unwrap_or_default().to_string();
                        ui.projects.insert(0, project);
                        ui.folder = None;
                        ui.set_project(id);
                        let (c, t, projects) = (ui.client.clone(), ui.tx.clone(), ui.projects.clone());
                        tokio::spawn(async move {
                            let _ = t.send(Loaded::Icons(icons::projects(&c, &projects).await));
                        });
                    }
                    Loaded::Added(Err(e)) => ui.note = Some((e, true)),
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
    // Both panes run under the provider's name, so Rex's sidebar shows its glyph.
    let driver = icons::driver_of(Some(&ui.config), &ui.settings.instance).to_string();
    let program = icons::as_program(&icons::real_exe(), &driver);
    if let Err(e) = open_reply(&thread, cwd.as_deref(), &program).await {
        eprintln!("t3rex: couldn't open the Reply pane: {e:#}");
    }
    if let Some(dir) = &cwd {
        let _ = std::env::set_current_dir(dir);
    }
    // This pane becomes the conversation.
    drop(ui);
    drop(client);
    crate::scheme::stop();
    use std::os::unix::process::CommandExt;
    let error = std::process::Command::new(&program)
        .arg0(&program)
        .args(["chat", &thread])
        .exec();
    Err(error.into())
}

/// Every provider's logo, by instance id, for the screen's scheme.
pub async fn logos(providers: &[Value]) -> HashMap<String, Icon> {
    let dark = crate::scheme::dark_soon().await;
    let mut out = HashMap::new();
    for p in providers {
        if let Some(id) = p["instanceId"].as_str() {
            out.insert(id.to_string(), icons::provider(p, dark).await);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_typed_path_lists_the_folders_it_could_be() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["Apps", "api", ".hidden", "zeta"] {
            std::fs::create_dir(dir.path().join(name)).unwrap();
        }
        std::fs::write(dir.path().join("app.txt"), "").unwrap();
        let root = dir.path().to_string_lossy().into_owned();
        assert_eq!(subfolders(&root, "a"), vec!["api", "Apps"]);
        assert_eq!(subfolders(&root, "."), vec![".hidden"]);
        assert_eq!(subfolders(&root, ""), vec!["api", "Apps", "zeta"]);
        assert_eq!(
            split_path("~/Developer/t3"),
            ("~/Developer/".to_string(), "t3".to_string())
        );
        let mut folder = Folder::new(&format!("{root}/A"));
        assert_eq!(folder.menu.items.len(), 2);
        let first = folder.menu.items[0].id.clone();
        folder.complete(&first);
        assert_eq!(folder.path.string(), format!("{root}/api/"));
    }

    #[test]
    fn only_a_full_path_to_a_folder_is_a_project() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_string_lossy().into_owned();
        assert_eq!(folder_root(&format!("{root}/")), Ok(root.clone()));
        assert!(folder_root("relative/path").is_err());
        assert!(folder_root(&format!("{root}/missing")).is_err());
        assert_eq!(folder_root("/"), Ok("/".to_string()));
    }
}
