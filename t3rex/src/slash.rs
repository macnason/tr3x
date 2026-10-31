//! The menu `/` opens in Reply: t3rex's commands, then the thread's provider commands and its
//! skills, as T3's composer lists them. Typing after the `/` filters, ranked the way T3 ranks
//! (exact, then prefix, word boundary, substring, fuzzy). Tab or Enter completes, Esc closes.
//!
//! What a pick puts in the input is what T3 puts there, and the message goes out as plain text
//! the same way: a provider command as `/name `, a skill as `$name `, which T3's server turns
//! into the provider's own skill call (ClaudeSkillDispatch.ts for Claude).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use serde_json::Value;
use t3term::client::Client;
use t3term::models;

use crate::compose::Editor;
use crate::icons::{Icon, Painter};
use crate::picker::{Item, Menu};

/// Rows the menu shows at most; more scroll.
const ROWS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    /// One of t3rex's own commands, run in Reply.
    Rex,
    /// The provider's slash command, sent to the agent.
    Command,
    Skill,
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub kind: Kind,
    pub name: String,
    pub description: String,
    /// Shown dim after the cursor once completed, until you type.
    pub hint: Option<String>,
    /// A skill's other fields, which T3 also matches on.
    display: String,
    short: String,
    long: String,
    scope: String,
}

impl Entry {
    fn new(kind: Kind, name: &str, description: &str, hint: Option<&str>) -> Entry {
        Entry {
            kind,
            name: clean(name),
            description: clean(description),
            hint: hint.map(clean),
            display: String::new(),
            short: String::new(),
            long: String::new(),
            scope: String::new(),
        }
    }

    /// What a pick puts in the input.
    fn insert(&self) -> String {
        match self.kind {
            Kind::Skill => format!("${} ", self.name),
            _ => format!("/{} ", self.name),
        }
    }

    fn mark(&self) -> Icon {
        match self.kind {
            Kind::Rex => Icon::Text("›".into(), 4),
            Kind::Command => Icon::Text("/".into(), 8),
            Kind::Skill => Icon::Text("$".into(), 8),
        }
    }
}

/// t3rex's commands, as compose runs them.
const REX: &[(&str, &str, Option<&str>)] = &[
    ("steer", "Send into the running turn", Some("message")),
    ("settle", "Settle this thread", None),
    ("snooze", "Snooze this thread", Some("2h, tomorrow")),
    ("wake", "End the snooze", None),
    ("pin", "Pin this thread", None),
    ("unpin", "Unpin this thread", None),
    ("archive", "Archive this thread", None),
    ("stop", "Stop the running turn", None),
    ("model", "Choose the model", None),
    ("effort", "Choose the effort", None),
    ("mode", "Choose the access", None),
    ("open", "Open in T3 Code", None),
    (
        "width",
        "Set the chat width",
        Some("comfortable, wide, full, t3"),
    ),
];

/// The folder T3 lists a thread's commands and skills for: its worktree, else its project's
/// root (T3's composer passes the same as `gitCwd`).
fn cwd<'a>(thread: &'a Value, roots: &'a HashMap<String, String>) -> Option<&'a str> {
    thread["worktreePath"]
        .as_str()
        .filter(|p| !p.is_empty())
        .or_else(|| roots.get(thread["projectId"].as_str()?).map(String::as_str))
}

/// Provider-written text as one plain line, without controls that could redraw the pane.
fn clean(text: &str) -> String {
    t3term::transcript::plain(text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn key(name: &str) -> String {
    name.trim().to_lowercase()
}

/// Everything `/` can invoke on `thread`, in T3's order: t3rex's commands, the provider's
/// commands, its skills. The workspace snapshot for the thread's folder wins over the
/// provider's own lists. Skills are the enabled, user-invocable ones, once each; a provider
/// command with a skill's name gives way to the skill, as in T3.
pub fn entries(config: &Value, thread: &Value, roots: &HashMap<String, String>) -> Vec<Entry> {
    let mut out: Vec<Entry> = REX
        .iter()
        .map(|(n, d, h)| Entry::new(Kind::Rex, n, d, *h))
        .collect();
    let instance = thread["modelSelection"]["instanceId"]
        .as_str()
        .or(thread["providerInstanceId"].as_str())
        .unwrap_or_default();
    let Some(provider) = models::providers(config)
        .iter()
        .find(|p| p["instanceId"].as_str() == Some(instance))
    else {
        return out;
    };
    let snapshot = cwd(thread, roots).and_then(|cwd| {
        provider["workspaceSnapshots"]
            .as_array()?
            .iter()
            .find(|s| s["cwd"].as_str() == Some(cwd))
    });
    let list = |field: &str| -> Vec<Value> {
        snapshot
            .and_then(|s| s[field].as_array())
            .or(provider[field].as_array())
            .cloned()
            .unwrap_or_default()
    };
    let mut seen = std::collections::HashSet::new();
    let skills: Vec<Entry> = list("skills")
        .iter()
        .filter(|s| s["enabled"] == true && s["userInvocable"] != false)
        .filter_map(|s| {
            let name = s["name"].as_str()?;
            seen.insert(key(name)).then(|| {
                let short = s["shortDescription"].as_str().unwrap_or_default();
                let long = s["description"].as_str().unwrap_or_default();
                let scope = s["scope"].as_str().unwrap_or_default();
                let description = if !short.is_empty() {
                    short.to_string()
                } else if !long.is_empty() {
                    long.to_string()
                } else if !scope.is_empty() {
                    format!("{scope} skill")
                } else {
                    String::new()
                };
                let mut e = Entry::new(Kind::Skill, name, &description, None);
                e.display = s["displayName"].as_str().unwrap_or(name).to_lowercase();
                e.short = short.to_lowercase();
                e.long = long.to_lowercase();
                e.scope = scope.to_lowercase();
                e
            })
        })
        .collect();
    let mut commands = std::collections::HashSet::new();
    for c in list("slashCommands") {
        let Some(name) = c["name"].as_str() else {
            continue;
        };
        if seen.contains(&key(name)) || !commands.insert(key(name)) {
            continue;
        }
        let hint = c["input"]["hint"].as_str();
        let description = c["description"].as_str().or(hint).unwrap_or_default();
        out.push(Entry::new(Kind::Command, name, description, hint));
    }
    out.extend(skills);
    out
}

/// T3's `scoreQueryMatch`: lower is better, `None` is no match. Shorter values rank higher among
/// the same kind of match.
#[derive(Default, Clone, Copy)]
struct Bases {
    exact: usize,
    prefix: Option<usize>,
    boundary: Option<usize>,
    includes: Option<usize>,
    fuzzy: Option<usize>,
}

fn score(value: &str, query: &str, b: Bases, markers: &[char]) -> Option<usize> {
    if value.is_empty() || query.is_empty() {
        return None;
    }
    let (v, q): (Vec<char>, Vec<char>) = (value.chars().collect(), query.chars().collect());
    let extra = v.len().saturating_sub(q.len()).min(64);
    if v == q {
        return Some(b.exact);
    }
    if let Some(base) = b.prefix
        && v.starts_with(&q)
    {
        return Some(base + extra);
    }
    let find =
        |from: usize| (from..=v.len().saturating_sub(q.len())).find(|&i| v[i..].starts_with(&q));
    if let Some(base) = b.boundary
        && let Some(at) = (0..v.len())
            .filter(|&i| markers.contains(&v[i]) && v[i + 1..].starts_with(&q))
            .map(|i| i + 1)
            .next()
    {
        return Some(base + at * 2 + extra);
    }
    if let Some(base) = b.includes
        && v.len() >= q.len()
        && let Some(at) = find(0)
    {
        return Some(base + at * 2 + extra);
    }
    let base = b.fuzzy?;
    // In order, not necessarily together: earlier, tighter and shorter is better.
    let (mut n, mut first, mut last, mut gaps) = (0, None, 0, 0);
    for (i, c) in v.iter().enumerate() {
        if *c != q[n] {
            continue;
        }
        if first.is_some() {
            gaps += i - last - 1;
        }
        first.get_or_insert(i);
        last = i;
        n += 1;
        if n == q.len() {
            let first = first.unwrap_or(0);
            let span = i - first + 1 - q.len();
            return Some(base + first * 2 + gaps * 3 + span + extra);
        }
    }
    None
}

const NAME: [char; 3] = ['-', '_', '/'];
const WORDS: [char; 4] = [' ', '-', '_', '/'];

fn name_score(name: &str, query: &str) -> Option<usize> {
    let b = Bases {
        exact: 0,
        prefix: Some(2),
        boundary: Some(4),
        includes: Some(6),
        fuzzy: Some(100),
    };
    score(name, query, b, &NAME)
}

fn text_score(text: &str, query: &str, base: usize) -> Option<usize> {
    let b = Bases {
        exact: base,
        prefix: Some(base + 2),
        boundary: Some(base + 4),
        includes: Some(base + 6),
        fuzzy: None,
    };
    score(text, query, b, &WORDS)
}

/// How well `entry` matches `query` (lowercase, without the `/`), as T3 scores it.
fn rank_of(entry: &Entry, query: &str) -> Option<usize> {
    if entry.kind != Kind::Skill {
        return [
            name_score(&entry.name.to_lowercase(), query),
            text_score(&entry.description.to_lowercase(), query, 20),
        ]
        .into_iter()
        .flatten()
        .min();
    }
    if query == "skill" {
        return Some(0);
    }
    let q = query.strip_prefix("skill:").unwrap_or(query);
    if q.is_empty() {
        return Some(0);
    }
    let display = Bases {
        exact: 1,
        prefix: Some(3),
        boundary: Some(5),
        includes: Some(7),
        fuzzy: Some(110),
    };
    let scope = Bases {
        exact: 40,
        prefix: Some(42),
        includes: Some(44),
        ..Bases::default()
    };
    [
        name_score(&entry.name.to_lowercase(), q),
        score(&entry.display, q, display, &WORDS),
        text_score(&entry.short, q, 20),
        text_score(&entry.long, q, 30),
        score(&entry.scope, q, scope, &WORDS),
    ]
    .into_iter()
    .flatten()
    .min()
    .or_else(|| "skill".starts_with(query).then_some(usize::MAX))
}

/// The entries matching `query`, best first; all of them in order when it's empty. Ties go to
/// t3rex's commands, then provider commands, then skills, then by name.
pub fn rank(entries: &[Entry], query: &str) -> Vec<usize> {
    let query = query.trim().trim_start_matches('/').to_lowercase();
    if query.is_empty() {
        return (0..entries.len()).collect();
    }
    let mut hits: Vec<(usize, u8, String, usize)> = entries
        .iter()
        .enumerate()
        .filter_map(|(i, e)| {
            let kind = match e.kind {
                Kind::Rex => 0,
                Kind::Command => 1,
                Kind::Skill => 2,
            };
            Some((rank_of(e, &query)?, kind, e.name.to_lowercase(), i))
        })
        .collect();
    hits.sort();
    hits.into_iter().map(|(.., i)| i).collect()
}

/// The `/word` being typed, while the input starts with it and the cursor is in it.
fn query(editor: &Editor) -> Option<String> {
    if editor.text.first() != Some(&'/') {
        return None;
    }
    let end = editor
        .text
        .iter()
        .position(|c| c.is_whitespace())
        .unwrap_or(editor.text.len());
    (editor.cursor >= 1 && editor.cursor <= end).then(|| editor.text[1..end].iter().collect())
}

/// What a key did with the menu open.
pub enum Key {
    /// Not the menu's: the input takes it.
    Pass,
    Done,
    /// Enter on what's already typed: send it.
    Send,
}

#[derive(Default)]
pub struct Slash {
    entries: Vec<Entry>,
    menu: Menu,
    /// The query the menu was ranked for; `None` while closed.
    open: Option<String>,
    /// The input Esc closed the menu on. It stays closed until the input changes.
    closed_on: Option<String>,
    /// The input right after a completion, and the hint shown while it's unchanged.
    hint: Option<(String, String)>,
    /// A provider command picked that shares a t3rex command's name: it goes to the agent.
    theirs: Option<String>,
    /// Project roots by id, for threads without a worktree. Filled in the background.
    roots: Arc<Mutex<HashMap<String, String>>>,
}

impl Slash {
    pub fn new(client: Arc<Client>) -> Slash {
        let slash = Slash::default();
        let roots = slash.roots.clone();
        tokio::spawn(async move {
            if let Ok(shell) = client.shell().await
                && let Ok(mut roots) = roots.lock()
            {
                roots.extend(shell.projects.iter().filter_map(|p| {
                    Some((
                        p["id"].as_str()?.to_string(),
                        p["workspaceRoot"].as_str()?.to_string(),
                    ))
                }));
            }
        });
        slash
    }

    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// Follows the input: opens on `/`, re-ranks as the word changes, closes past it.
    pub fn sync(&mut self, editor: &Editor, config: Option<&Value>, thread: Option<&Value>) {
        let text = editor.string();
        if self.closed_on.as_ref().is_some_and(|c| *c != text) {
            self.closed_on = None;
        }
        if self.hint.as_ref().is_some_and(|(t, _)| *t != text) {
            self.hint = None;
        }
        if let Some(name) = &self.theirs
            && !text.starts_with(&format!("/{name}"))
        {
            self.theirs = None;
        }
        let q = query(editor).filter(|_| self.closed_on.is_none());
        let Some(q) = q else {
            self.open = None;
            return;
        };
        if self.open.as_ref() == Some(&q) {
            return;
        }
        if self.open.is_none() {
            let roots = self.roots.lock().map(|r| r.clone()).unwrap_or_default();
            self.entries = match (config, thread) {
                (Some(c), Some(t)) => entries(c, t, &roots),
                _ => entries(&Value::Null, &Value::Null, &roots),
            };
        }
        let items = rank(&self.entries, &q)
            .into_iter()
            .map(|i| {
                let e = &self.entries[i];
                let mut item = Item::new(i.to_string(), e.name.clone());
                item.detail = e.description.clone();
                item.icon = Some(e.mark());
                item
            })
            .collect();
        self.menu = Menu::new(items, None);
        self.open = Some(q);
    }

    fn selected(&self) -> Option<&Entry> {
        let item = self.menu.items.get(self.menu.selected)?;
        self.entries.get(item.id.parse::<usize>().ok()?)
    }

    /// Replaces the `/word` with the entry's text.
    fn complete(&mut self, editor: &mut Editor, index: usize) {
        let Some(entry) = self.entries.get(index).cloned() else {
            return;
        };
        let end = editor
            .text
            .iter()
            .position(|c| c.is_whitespace())
            .unwrap_or(editor.text.len());
        let mut insert: Vec<char> = entry.insert().chars().collect();
        // Don't double the space that's already there.
        if editor.text.get(end) == Some(&' ') {
            insert.pop();
        }
        let at = insert.len() + usize::from(editor.text.get(end) == Some(&' '));
        editor.text.splice(0..end, insert);
        editor.cursor = at;
        self.theirs = (entry.kind == Kind::Command && REX.iter().any(|(n, ..)| *n == entry.name))
            .then(|| entry.name.clone());
        self.hint = entry.hint.map(|h| (editor.string(), h));
        self.open = None;
    }

    pub fn key(&mut self, editor: &mut Editor, key: KeyEvent) -> Key {
        if !self.is_open() || self.menu.items.is_empty() {
            return Key::Pass;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => {
                self.closed_on = Some(editor.string());
                self.open = None;
            }
            KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown => {
                self.menu.key(key);
            }
            KeyCode::Char('p' | 'n') if ctrl => {
                self.menu.key(key);
            }
            KeyCode::Tab | KeyCode::BackTab => {
                if let Some(Ok(i)) = self
                    .menu
                    .items
                    .get(self.menu.selected)
                    .map(|i| i.id.parse())
                {
                    self.complete(editor, i);
                }
            }
            KeyCode::Enter
                if !key
                    .modifiers
                    .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT) =>
            {
                let Some(entry) = self.selected().cloned() else {
                    return Key::Pass;
                };
                let typed = editor.string();
                let index = self.menu.items[self.menu.selected].id.parse().unwrap_or(0);
                if entry.insert().trim_end() == typed.trim_end() {
                    self.complete(editor, index);
                    return Key::Send;
                }
                self.complete(editor, index);
            }
            _ => return Key::Pass,
        }
        Key::Done
    }

    /// Whether a mouse event lands on the menu.
    pub fn hits(&self, m: &MouseEvent) -> bool {
        self.is_open()
            && match m.kind {
                MouseEventKind::Down(MouseButton::Left) => self.menu.click(m.row).is_some(),
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => true,
                _ => false,
            }
    }

    /// A click picks the row; the wheel moves the selection.
    pub fn click(&mut self, editor: &mut Editor, m: &MouseEvent) {
        let last = self.menu.items.len().saturating_sub(1);
        match m.kind {
            MouseEventKind::ScrollUp => self.menu.selected = self.menu.selected.saturating_sub(1),
            MouseEventKind::ScrollDown => self.menu.selected = (self.menu.selected + 1).min(last),
            _ => {
                if let Some(Ok(i)) = self.menu.click(m.row).map(|id| id.parse()) {
                    self.complete(editor, i);
                }
            }
        }
    }

    /// Whether a line that starts with a t3rex command's name is the provider's command of that
    /// name, picked from the menu, so it goes to the agent.
    pub fn is_theirs(&self, line: &str) -> bool {
        self.theirs.as_ref().is_some_and(|name| {
            line.strip_prefix('/')
                .and_then(|l| l.split_whitespace().next())
                == Some(name.as_str())
        })
    }

    /// The hint to show after the cursor, while the input is as the completion left it.
    pub fn hint(&self) -> Option<&str> {
        self.hint.as_ref().map(|(_, h)| h.as_str())
    }

    /// Draws the menu next to the input's rows `top..=bottom` (1-based) in a pane `rows` tall:
    /// above when it fits there, else below, else wherever there's more room. `left` and
    /// `width` are the column.
    pub fn draw(
        &mut self,
        painter: &mut Painter,
        (top, bottom): (usize, usize),
        rows: usize,
        left: usize,
        width: usize,
    ) -> String {
        if !self.is_open() || self.menu.items.is_empty() {
            return String::new();
        }
        let want = self.menu.items.len().min(ROWS);
        let (above, below) = (top.saturating_sub(1), rows.saturating_sub(bottom));
        let (first, height) = if above >= want {
            (top - want, want)
        } else if below >= want {
            (bottom + 1, want)
        } else if above > below {
            (1, above)
        } else {
            (bottom + 1, below)
        };
        if height == 0 {
            return String::new();
        }
        let lines = self.menu.lines(painter, (first - 1) as u16, width, height);
        lines
            .iter()
            .enumerate()
            .map(|(i, line)| format!("\x1b[{};1H\x1b[2K\x1b[{}G{line}", first + i, left + 1))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn names(entries: &[Entry], query: &str) -> Vec<String> {
        rank(entries, query)
            .into_iter()
            .map(|i| entries[i].name.clone())
            .collect()
    }

    fn config() -> Value {
        json!({"providers": [{
            "instanceId": "claude",
            "slashCommands": [
                {"name": "compact", "description": "Summarize the conversation", "input": {"hint": "<instructions>"}},
                {"name": "model", "description": "Set the model"},
                {"name": "review", "description": "Review code"},
                {"name": "review", "description": "Again"},
                {"name": "pdf", "description": "Skill as a command"}
            ],
            "skills": [
                {"name": "pdf", "description": "Read PDFs", "enabled": true},
                {"name": "pdf", "description": "Twice", "enabled": true},
                {"name": "off", "description": "Disabled", "enabled": false},
                {"name": "hidden", "description": "Not for users", "enabled": true, "userInvocable": false}
            ],
            "workspaceSnapshots": [{
                "cwd": "/repo",
                "slashCommands": [{"name": "deploy", "description": "Ship it"}],
                "skills": [{"name": "repo-skill", "description": "This repo's", "enabled": true}]
            }]
        }]})
    }

    fn thread(worktree: Option<&str>) -> Value {
        json!({"projectId": "p", "worktreePath": worktree, "modelSelection": {"instanceId": "claude"}})
    }

    #[test]
    fn lists_rex_then_commands_then_skills_once_each() {
        let e = entries(&config(), &thread(None), &HashMap::new());
        let kinds: Vec<(Kind, &str)> = e[REX.len()..]
            .iter()
            .map(|e| (e.kind, e.name.as_str()))
            .collect();
        assert_eq!(
            kinds,
            vec![
                (Kind::Command, "compact"),
                (Kind::Command, "model"),
                (Kind::Command, "review"),
                (Kind::Skill, "pdf"),
            ]
        );
        assert_eq!(e[REX.len()].hint.as_deref(), Some("<instructions>"));
    }

    #[test]
    fn the_threads_workspace_wins() {
        let roots = HashMap::from([("p".to_string(), "/repo".to_string())]);
        for (t, roots) in [
            (thread(Some("/repo")), HashMap::new()),
            (thread(None), roots),
        ] {
            let e = entries(&config(), &t, &roots);
            let theirs: Vec<&str> = e[REX.len()..].iter().map(|e| e.name.as_str()).collect();
            assert_eq!(theirs, vec!["deploy", "repo-skill"]);
        }
        // Another folder falls back to the provider's lists.
        let e = entries(&config(), &thread(Some("/elsewhere")), &HashMap::new());
        assert!(e.iter().any(|e| e.name == "compact"));
        // An unknown provider leaves t3rex's own.
        let e = entries(
            &config(),
            &json!({"modelSelection": {"instanceId": "x"}}),
            &HashMap::new(),
        );
        assert_eq!(e.len(), REX.len());
    }

    #[test]
    fn ranks_exact_then_prefix_then_the_rest() {
        let e = entries(&config(), &thread(None), &HashMap::new());
        // Both models: t3rex's first.
        let m = rank(&e, "model");
        assert_eq!((e[m[0]].kind, e[m[1]].kind), (Kind::Rex, Kind::Command));
        // Shorter prefix matches first, then by kind and name.
        assert_eq!(names(&e, "s")[..4], ["stop", "steer", "settle", "snooze"]);
        assert_eq!(names(&e, "/sett")[0], "settle");
        // Fuzzy: in order, not together.
        assert_eq!(names(&e, "cpt"), ["compact"]);
        assert!(names(&e, "zzz").is_empty());
        // A skill matches on its description, after any name match.
        assert_eq!(names(&e, "pdfs"), ["pdf"]);
        assert_eq!(names(&e, "skill:pd"), ["pdf"]);
        assert_eq!(names(&e, "").len(), e.len());
    }

    #[test]
    fn three_hundred_rank_fast() {
        let mut e = entries(&config(), &thread(None), &HashMap::new());
        for i in 0..300 {
            let mut skill = Entry::new(
                Kind::Skill,
                &format!("skill-number-{i}"),
                "A long description of what this skill does for the agent when asked",
                None,
            );
            skill.long = skill.description.to_lowercase();
            e.push(skill);
        }
        let start = std::time::Instant::now();
        for q in ["s", "sk", "skn", "number-29", "agent"] {
            assert!(!rank(&e, q).is_empty());
        }
        assert!(start.elapsed() < std::time::Duration::from_millis(50));
    }

    fn typed(text: &str) -> Editor {
        let mut e = Editor::default();
        e.insert(text);
        e
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn opens_on_slash_completes_and_closes() {
        let (config, thread) = (config(), thread(None));
        let mut s = Slash::default();
        let mut ed = typed("/comp");
        s.sync(&ed, Some(&config), Some(&thread));
        assert!(s.is_open());
        assert!(matches!(s.key(&mut ed, press(KeyCode::Tab)), Key::Done));
        assert_eq!(ed.string(), "/compact ");
        assert_eq!(ed.cursor, 9);
        s.sync(&ed, Some(&config), Some(&thread));
        assert!(!s.is_open());
        assert_eq!(s.hint(), Some("<instructions>"));
        ed.insert("x");
        s.sync(&ed, Some(&config), Some(&thread));
        assert_eq!(s.hint(), None);
        // A skill goes in as T3 puts it.
        let mut ed = typed("/pdf");
        s.sync(&ed, Some(&config), Some(&thread));
        s.key(&mut ed, press(KeyCode::Down));
        s.key(&mut ed, press(KeyCode::Enter));
        assert_eq!(ed.string(), "$pdf ");
        // Esc leaves the text, and typing opens it again. Past the `/`, it's closed.
        let mut ed = typed("/se");
        s.sync(&ed, Some(&config), Some(&thread));
        s.key(&mut ed, press(KeyCode::Esc));
        s.sync(&ed, Some(&config), Some(&thread));
        assert!(!s.is_open());
        assert_eq!(ed.string(), "/se");
        ed.insert("t");
        s.sync(&ed, Some(&config), Some(&thread));
        assert!(s.is_open());
        ed.text.clear();
        ed.cursor = 0;
        s.sync(&ed, Some(&config), Some(&thread));
        assert!(!s.is_open());
        // Not at the start, or a path with a space after it: no menu.
        let ed = typed("see /settle");
        s.sync(&ed, Some(&config), Some(&thread));
        assert!(!s.is_open());
    }

    #[test]
    fn enter_on_whats_typed_sends_and_theirs_goes_to_the_agent() {
        let (config, thread) = (config(), thread(None));
        let mut s = Slash::default();
        let mut ed = typed("/settle");
        s.sync(&ed, Some(&config), Some(&thread));
        assert!(matches!(s.key(&mut ed, press(KeyCode::Enter)), Key::Send));
        assert!(!s.is_theirs(ed.string().trim()));
        // The provider's /model, picked, is theirs; t3rex's is the default.
        let mut ed = typed("/model");
        s.sync(&ed, Some(&config), Some(&thread));
        assert!(matches!(s.key(&mut ed, press(KeyCode::Enter)), Key::Send));
        assert!(!s.is_theirs("/model"));
        let mut ed = typed("/model");
        s.sync(&ed, Some(&config), Some(&thread));
        s.key(&mut ed, press(KeyCode::Down));
        assert!(matches!(s.key(&mut ed, press(KeyCode::Enter)), Key::Send));
        assert!(s.is_theirs("/model"));
        // Typed over, it's t3rex's again.
        let ed = typed("/effort");
        s.sync(&ed, Some(&config), Some(&thread));
        assert!(!s.is_theirs("/model"));
    }

    #[test]
    fn draws_above_when_it_fits_else_below() {
        let (config, thread) = (config(), thread(None));
        let mut s = Slash::default();
        let ed = typed("/");
        s.sync(&ed, Some(&config), Some(&thread));
        let mut p = Painter;
        // Input on row 1 of 6: below, five rows.
        let out = s.draw(&mut p, (1, 1), 6, 0, 40);
        assert!(out.contains("\x1b[2;1H") && out.contains("\x1b[6;1H"));
        assert!(!out.contains("\x1b[7;1H"));
        // Input on row 10 of 12: above, eight rows.
        let out = s.draw(&mut p, (10, 10), 12, 0, 40);
        assert!(out.contains("\x1b[2;1H") && out.contains("\x1b[9;1H"));
        assert!(!out.contains("\x1b[10;1H"));
    }
}
