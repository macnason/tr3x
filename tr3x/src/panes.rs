//! Sync and the panes, over one local socket, `panes.sock` in the state directory.
//!
//! Sync already follows each machine's whole shell. Each pane used to follow it too, on a
//! connection of its own, to read one thread's row: every change to any thread went to every
//! pane, from T3's server, to be parsed there. Now a pane joins sync instead, and sync sends it
//! only its own thread's slice of the shell, and only when that slice changes. It also tells
//! the pane what it would otherwise have to poll for: whether its window is the one on screen,
//! that settings changed, that the picker held a model, that tr3x was installed again.
//!
//! A pane that can't join, because sync isn't running, follows the shell itself as before, and
//! joins once sync is back.
//!
//! Each line on the socket is one JSON object. A pane's first line is `{"watch": "env/thread"}`
//! and it stays connected; any tr3x process can send `{"notify": ...}` alone and leave. A pane
//! sends `{"holding": true}` while closing it would lose something, and `false` once it wouldn't.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use t3client::projection::ShellState;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, watch};

pub fn socket_path() -> PathBuf {
    crate::state::dir().join("panes.sock")
}

/// What sync tells a pane.
#[derive(Debug, Clone)]
pub enum Note {
    /// The pane's thread as sync's shell has it: the thread, the threads above it and its
    /// project. No threads when the shell doesn't list it.
    Shell(ShellState),
    /// Whether the thread's window is the one its session shows. None when sync knows no window
    /// for the thread, as for one `tr3x new` just started: the pane takes itself to be seen.
    Visible(Option<bool>),
    /// tr3x's settings or T3 Code's changed.
    Settings,
    /// The picker held a model for the pane's thread, or let one go.
    Draft,
    /// tr3x was installed again, so the pane's binary may have been replaced.
    Check,
    /// Sync went away. The pane looks after itself until it can join again.
    Lost,
}

impl Note {
    fn line(&self) -> String {
        let value = match self {
            Note::Shell(shell) => {
                json!({"shell": {"threads": shell.threads, "projects": shell.projects}})
            }
            Note::Visible(on) => json!({"visible": on}),
            Note::Settings => json!({"settings": true}),
            Note::Draft => json!({"draft": true}),
            Note::Check => json!({"check": true}),
            Note::Lost => json!({"lost": true}),
        };
        value.to_string() + "\n"
    }

    fn read(line: &str) -> Option<Note> {
        let value: Value = serde_json::from_str(line).ok()?;
        let list = |v: &Value| v.as_array().cloned().unwrap_or_default();
        Some(if let Some(shell) = value.get("shell") {
            Note::Shell(ShellState {
                sequence: 0,
                threads: list(&shell["threads"]),
                projects: list(&shell["projects"]),
                synchronized: true,
            })
        } else if let Some(on) = value.get("visible") {
            Note::Visible(on.as_bool())
        } else if value.get("settings").is_some() {
            Note::Settings
        } else if value.get("draft").is_some() {
            Note::Draft
        } else if value.get("check").is_some() {
            Note::Check
        } else {
            return None;
        })
    }
}

/// The slice of `shell` a pane for `thread_id` reads: the thread, each thread above it, for
/// its parent's title, and its project, for the project's name and folder.
pub fn slice(shell: &ShellState, thread_id: &str) -> ShellState {
    let mut threads = Vec::new();
    let mut at = t3client::inbox::find_thread(shell, thread_id);
    while let Some(thread) = at {
        threads.push(thread.clone());
        if threads.len() > 16 {
            break;
        }
        at = thread["lineage"]["parentThreadId"]
            .as_str()
            .and_then(|parent| t3client::inbox::find_thread(shell, parent));
    }
    let projects = threads
        .first()
        .and_then(|t| t["projectId"].as_str())
        .and_then(|id| shell.projects.iter().find(|p| p["id"] == id))
        .cloned()
        .into_iter()
        .collect();
    ShellState {
        sequence: 0,
        threads,
        projects,
        synchronized: true,
    }
}

// ---- the pane's side ----

/// Whether this pane holds something closing it would lose, as it last said.
static HOLDING: LazyLock<watch::Sender<bool>> = LazyLock::new(|| watch::channel(false).0);

/// Says whether closing the pane would lose something: text typed in Reply, or a forwarded
/// port. Sync keeps the tab of a pane that does past the tab budget. Only a change goes to
/// sync, so the pane can say it as often as it likes.
pub fn hold(on: bool) {
    HOLDING.send_if_modified(|held| std::mem::replace(held, on) != on);
}

/// Joins sync as a pane for `key`, `env/thread`. None when sync isn't there to join. The
/// receiver ends with `Note::Lost` when sync goes.
pub async fn join(key: &str) -> Option<mpsc::UnboundedReceiver<Note>> {
    join_at(&socket_path(), key, HOLDING.subscribe()).await
}

async fn join_at(
    path: &std::path::Path,
    key: &str,
    mut holding: watch::Receiver<bool>,
) -> Option<mpsc::UnboundedReceiver<Note>> {
    let stream = UnixStream::connect(path).await.ok()?;
    let (read, mut write) = stream.into_split();
    let hello = json!({"watch": key}).to_string() + "\n";
    write.write_all(hello.as_bytes()).await.ok()?;
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        // `write` is held open for as long as the pane listens: sync sees the pane go when it
        // closes. Each sync joined starts out knowing nothing held, so it hears at once if
        // something is.
        let mut lines = BufReader::new(read).lines();
        let mut held = false;
        loop {
            let on = *holding.borrow_and_update();
            if on != held {
                held = on;
                let line = json!({"holding": on}).to_string() + "\n";
                if write.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
            }
            tokio::select! {
                line = lines.next_line() => match line {
                    Ok(Some(line)) => {
                        if let Some(note) = Note::read(&line)
                            && tx.send(note).is_err()
                        {
                            return;
                        }
                    }
                    _ => break,
                },
                Ok(()) = holding.changed() => {}
            }
        }
        let _ = tx.send(Note::Lost);
    });
    Some(rx)
}

/// Joins sync again once it's back, trying less often the longer it's gone, up to every 30
/// seconds. Only a pane that lost sync waits on this; the receiver comes on the channel.
pub fn rejoin(key: String) -> tokio::sync::oneshot::Receiver<mpsc::UnboundedReceiver<Note>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let mut wait = Duration::from_millis(500);
        loop {
            tokio::time::sleep(wait).await;
            if tx.is_closed() {
                return;
            }
            if let Some(notes) = join(&key).await {
                let _ = tx.send(notes);
                return;
            }
            wait = (wait * 2).min(Duration::from_secs(30));
        }
    });
    rx
}

/// Tells sync, and through it the panes, that something changed: `settings`, `check`, or
/// `draft` for the thread `key`. Nothing happens when sync isn't running; panes that follow
/// T3 themselves then find out the old way.
pub fn notify(what: &str, key: Option<&str>) {
    notify_at(&socket_path(), what, key);
}

fn notify_at(path: &std::path::Path, what: &str, key: Option<&str>) {
    use std::io::Write;
    let Ok(mut stream) = std::os::unix::net::UnixStream::connect(path) else {
        return;
    };
    let _ = stream.set_write_timeout(Some(Duration::from_millis(200)));
    let line = json!({"notify": what, "key": key}).to_string() + "\n";
    let _ = stream.write_all(line.as_bytes());
}

// ---- sync's side ----

/// What sync hears on the socket.
#[derive(Debug, PartialEq, Eq)]
pub enum Heard {
    /// A pane joined: it wants its slice and whether it's on screen.
    Joined,
    Settings,
    Check,
    Draft(String),
    /// A pane began or stopped holding something closing it would lose, or left holding it.
    Holding,
}

/// What's still to go to a pane. A pane that stops reading costs sync one slice, not every
/// slice since: a new slice or visibility replaces the one not sent yet, and a note already
/// waiting isn't added again.
#[derive(Default)]
struct Outbox {
    shell: Option<String>,
    visible: Option<String>,
    notes: Vec<String>,
}

impl Outbox {
    fn note(&mut self, line: &str) {
        if !self.notes.iter().any(|l| l == line) {
            self.notes.push(line.to_string());
        }
    }

    fn take(&mut self) -> Vec<String> {
        let mut lines: Vec<String> = self.shell.take().into_iter().collect();
        lines.extend(self.visible.take());
        lines.append(&mut self.notes);
        lines
    }
}

#[derive(Default)]
struct Out {
    lines: Mutex<Outbox>,
    ready: tokio::sync::Notify,
}

impl Out {
    fn put(&self, put: impl FnOnce(&mut Outbox)) {
        put(&mut self.lines.lock().unwrap());
        self.ready.notify_one();
    }
}

struct Pane {
    key: String,
    out: Arc<Out>,
    /// The last slice and visibility sent, so each goes again only when it changes.
    shell: Option<String>,
    visible: Option<Option<bool>>,
    /// Closing the pane would lose something, as it last said.
    holding: bool,
}

#[derive(Default)]
struct Inner {
    next: u64,
    panes: HashMap<u64, Pane>,
}

/// The panes that joined sync.
#[derive(Clone, Default)]
pub struct Panes {
    inner: Arc<Mutex<Inner>>,
}

impl Panes {
    /// Listens on the socket. Sync holds `sync.lock`, so a socket left there is an old sync's.
    pub fn serve() -> std::io::Result<(Panes, mpsc::UnboundedReceiver<Heard>)> {
        Panes::serve_at(&socket_path())
    }

    fn serve_at(
        path: &std::path::Path,
    ) -> std::io::Result<(Panes, mpsc::UnboundedReceiver<Heard>)> {
        let _ = std::fs::remove_file(path);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let listener = UnixListener::bind(path)?;
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        let panes = Panes::default();
        let (heard, rx) = mpsc::unbounded_channel();
        let accepting = panes.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(accepting.clone().take(stream, heard.clone()));
            }
        });
        Ok((panes, rx))
    }

    async fn take(self, stream: UnixStream, heard: mpsc::UnboundedSender<Heard>) {
        let (read, mut write) = stream.into_split();
        let mut lines = BufReader::new(read).lines();
        let Ok(Some(first)) = lines.next_line().await else {
            return;
        };
        let Ok(hello) = serde_json::from_str::<Value>(&first) else {
            return;
        };
        if let Some(what) = hello["notify"].as_str() {
            let _ = heard.send(match what {
                "settings" => Heard::Settings,
                "check" => Heard::Check,
                "draft" => match hello["key"].as_str() {
                    Some(key) => Heard::Draft(key.to_string()),
                    None => return,
                },
                _ => return,
            });
            return;
        }
        let Some(key) = hello["watch"].as_str() else {
            return;
        };
        let out = Arc::new(Out::default());
        let id = {
            let mut inner = self.inner.lock().unwrap();
            inner.next += 1;
            let id = inner.next;
            inner.panes.insert(
                id,
                Pane {
                    key: key.to_string(),
                    out: out.clone(),
                    shell: None,
                    visible: None,
                    holding: false,
                },
            );
            id
        };
        let _ = heard.send(Heard::Joined);
        let writing = async {
            loop {
                out.ready.notified().await;
                let lines = out.lines.lock().unwrap().take();
                for line in lines {
                    if write.write_all(line.as_bytes()).await.is_err() {
                        return;
                    }
                }
            }
        };
        // After joining, a pane only says whether it holds something; its line closing is it
        // leaving.
        let reading = async {
            while let Ok(Some(line)) = lines.next_line().await {
                let Some(on) = serde_json::from_str::<Value>(&line)
                    .ok()
                    .and_then(|v| v["holding"].as_bool())
                else {
                    continue;
                };
                let mut inner = self.inner.lock().unwrap();
                if let Some(pane) = inner.panes.get_mut(&id)
                    && pane.holding != on
                {
                    pane.holding = on;
                    let _ = heard.send(Heard::Holding);
                }
            }
        };
        tokio::select! {
            _ = writing => {},
            _ = reading => {},
        }
        let gone = self.inner.lock().unwrap().panes.remove(&id);
        if gone.is_some_and(|p| p.holding) {
            let _ = heard.send(Heard::Holding);
        }
    }

    /// The threads whose pane holds something closing it would lose, `env/thread`.
    pub fn holding(&self) -> HashSet<String> {
        let inner = self.inner.lock().unwrap();
        inner
            .panes
            .values()
            .filter(|p| p.holding)
            .map(|p| p.key.clone())
            .collect()
    }

    /// The threads that have a pane joined, `env/thread`.
    pub fn keys(&self) -> Vec<String> {
        let inner = self.inner.lock().unwrap();
        let mut keys: Vec<String> = inner.panes.values().map(|p| p.key.clone()).collect();
        keys.sort();
        keys.dedup();
        keys
    }

    /// Sends each of `key`'s panes its slice, and whether it's on screen, where either changed.
    /// `visible` is None while sync can't say yet, and `Some(None)` when it knows no window for
    /// the thread.
    pub fn publish(&self, key: &str, shell: Option<&ShellState>, visible: Option<Option<bool>>) {
        let line = shell.map(|s| Note::Shell(s.clone()).line());
        let mut inner = self.inner.lock().unwrap();
        for pane in inner.panes.values_mut().filter(|p| p.key == key) {
            if let Some(line) = &line
                && pane.shell.as_ref() != Some(line)
            {
                pane.out.put(|o| o.shell = Some(line.clone()));
                pane.shell = Some(line.clone());
            }
            if let Some(on) = visible
                && pane.visible != Some(on)
            {
                pane.out.put(|o| o.visible = Some(Note::Visible(on).line()));
                pane.visible = Some(on);
            }
        }
    }

    /// Tells every pane, or only `key`'s.
    pub fn tell(&self, key: Option<&str>, note: &Note) {
        let line = note.line();
        let inner = self.inner.lock().unwrap();
        for pane in inner
            .panes
            .values()
            .filter(|p| key.is_none_or(|k| p.key == k))
        {
            pane.out.put(|o| o.note(&line));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slice_holds_the_thread_its_ancestors_and_its_project() {
        let shell = ShellState {
            sequence: 3,
            projects: vec![json!({"id": "p", "title": "P"}), json!({"id": "q"})],
            threads: vec![
                json!({"id": "root", "projectId": "p", "lineage": {}}),
                json!({"id": "kid", "projectId": "p", "lineage": {"parentThreadId": "root"}}),
                json!({"id": "other", "projectId": "q"}),
            ],
            synchronized: true,
        };
        let s = slice(&shell, "kid");
        let ids: Vec<&str> = s
            .threads
            .iter()
            .map(|t| t["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["kid", "root"]);
        assert_eq!(s.projects, vec![json!({"id": "p", "title": "P"})]);
        assert!(slice(&shell, "missing").threads.is_empty());
    }

    #[test]
    fn notes_read_back_as_they_were_written() {
        let shell = ShellState {
            threads: vec![json!({"id": "t"})],
            ..ShellState::default()
        };
        match Note::read(Note::Shell(shell).line().trim()) {
            Some(Note::Shell(s)) => {
                assert_eq!(s.threads, vec![json!({"id": "t"})]);
                assert!(s.synchronized);
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            Note::read(Note::Visible(Some(false)).line().trim()),
            Some(Note::Visible(Some(false)))
        ));
        assert!(matches!(
            Note::read(Note::Visible(None).line().trim()),
            Some(Note::Visible(None))
        ));
        assert!(matches!(Note::read(&Note::Draft.line()), Some(Note::Draft)));
        assert!(Note::read("{}").is_none());
    }

    /// Over a real socket: a pane joins, gets its slice once however often sync publishes the
    /// same one, hears what's for it alone, and a notifier's line reaches sync.
    #[tokio::test]
    async fn panes_join_sync_and_hear_only_what_changed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("panes.sock");
        let (panes, mut heard) = Panes::serve_at(&path).unwrap();
        let (hold, holding) = watch::channel(false);
        let mut a = join_at(&path, "e/a", holding).await.unwrap();
        assert_eq!(heard.recv().await, Some(Heard::Joined));
        assert_eq!(panes.keys(), vec!["e/a".to_string()]);
        let shell = ShellState {
            threads: vec![json!({"id": "a"})],
            ..ShellState::default()
        };
        panes.publish("e/a", Some(&shell), Some(Some(true)));
        panes.publish("e/a", Some(&shell), Some(Some(true)));
        panes.publish("e/b", Some(&shell), Some(Some(false)));
        panes.tell(Some("e/a"), &Note::Draft);
        assert!(matches!(a.recv().await, Some(Note::Shell(_))));
        assert!(matches!(a.recv().await, Some(Note::Visible(Some(true)))));
        assert!(matches!(a.recv().await, Some(Note::Draft)));
        tokio::task::spawn_blocking(move || notify_at(&path, "draft", Some("e/a")))
            .await
            .unwrap();
        assert_eq!(heard.recv().await, Some(Heard::Draft("e/a".into())));
        hold.send_replace(true);
        assert_eq!(heard.recv().await, Some(Heard::Holding));
        assert_eq!(panes.holding(), HashSet::from(["e/a".to_string()]));
        hold.send_replace(false);
        assert_eq!(heard.recv().await, Some(Heard::Holding));
        assert!(panes.holding().is_empty());
        drop(panes);
    }

    /// A pane that isn't reading is owed only the latest of each: one slice, one visibility,
    /// each note once.
    #[test]
    fn a_pane_that_stops_reading_is_owed_only_the_latest() {
        let out = Out::default();
        for n in 0..100 {
            out.put(|o| o.shell = Some(format!("shell {n}")));
            out.put(|o| o.visible = Some(format!("visible {n}")));
            out.put(|o| o.note("draft"));
        }
        let lines = out.lines.lock().unwrap().take();
        assert_eq!(lines, vec!["shell 99", "visible 99", "draft"]);
        assert!(out.lines.lock().unwrap().take().is_empty());
    }
}
