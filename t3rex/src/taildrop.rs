//! Pictures sent to the host with Taildrop. Rex hands a pane on another machine no picture: a
//! paste of one arrives as a bare ⌘V key, and a drop as nothing. Your devices can still send
//! the host a file over the tailnet (Share → Tailscale on a Mac or a phone). Sync takes what
//! arrives and puts each picture in the inbox of the thread whose window is active, and that
//! thread's Reply attaches it as if it had been pasted.
//!
//! `tailscale file get --wait --loop` receives into a staging folder; only your own devices
//! can send to it. Anything that isn't a picture is kept aside, never sent.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::state::{self, State};

/// Where Taildrop puts what arrives, before it goes to a thread.
fn staging(base: &Path) -> PathBuf {
    base.join("taildrop")
}

/// What arrived that isn't a picture.
fn kept(base: &Path) -> PathBuf {
    base.join("taildrop-kept")
}

/// The inbox of `env/thread`: what Reply there attaches as if it had been pasted.
pub fn inbox(base: &Path, key: &str) -> PathBuf {
    base.join("inbox").join(key)
}

/// Receives files sent to this machine and gives each picture to the thread in front. Runs
/// for as long as sync does; without Tailscale it leaves at once.
pub async fn follow() {
    let Some(bin) = t3client::pair::tailscale_bin() else {
        return;
    };
    let base = state::dir();
    let dir = staging(&base);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    tokio::spawn(receive(bin, dir.clone()));
    let rex = crate::rex::Rex::find();
    let mut said_waiting = false;
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let files = arrived(&dir);
        if files.is_empty() {
            said_waiting = false;
            continue;
        }
        let Some(key) = looking_at(&rex).await else {
            if !said_waiting {
                println!(
                    "t3rex: {} file(s) from Taildrop wait for a thread's window",
                    files.len()
                );
                said_waiting = true;
            }
            continue;
        };
        for file in files {
            match deliver(&base, &file, &key) {
                Ok(Delivered::Picture) => println!(
                    "t3rex: {} from Taildrop to {key}",
                    file.file_name().unwrap_or_default().to_string_lossy()
                ),
                Ok(Delivered::Kept(to)) => println!(
                    "t3rex: {} from Taildrop isn't a picture; kept in {}",
                    file.file_name().unwrap_or_default().to_string_lossy(),
                    to.display()
                ),
                Err(e) => eprintln!("t3rex: Taildrop: {e:#}"),
            }
        }
    }
}

/// `tailscale file get` into `dir`, started again whenever it stops. A tailnet without
/// Taildrop says so once, and is asked again an hour later.
async fn receive(bin: String, dir: PathBuf) {
    let mut said = false;
    loop {
        let started = std::time::Instant::now();
        let out = tokio::process::Command::new(&bin)
            .args(["file", "get", "--wait", "--loop", "--conflict=rename"])
            .arg(&dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .output()
            .await;
        let quick = started.elapsed() < Duration::from_secs(10);
        if quick && !said {
            let why = match &out {
                Ok(o) => String::from_utf8_lossy(&o.stderr).trim().to_string(),
                Err(e) => e.to_string(),
            };
            eprintln!("t3rex: not taking pictures over Taildrop: {why}");
            said = true;
        }
        let pause = if quick { 3600 } else { 5 };
        tokio::time::sleep(Duration::from_secs(pause)).await;
    }
}

/// The finished files in `dir`, oldest first. Hidden and partial files are left alone.
fn arrived(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .filter(|p| {
            let name = p.file_name().unwrap_or_default().to_string_lossy();
            !name.starts_with('.') && !name.ends_with(".partial")
        })
        .map(|p| {
            let when = p
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            (when, p)
        })
        .collect();
    files.sort();
    files.into_iter().map(|(_, p)| p).collect()
}

/// The `env/thread` whose window is active in a t3rex session: the thread you're looking at.
async fn looking_at(rex: &crate::rex::Rex) -> Option<String> {
    let state = State::load();
    let mut sessions = state.session().into_iter().collect::<Vec<_>>();
    sessions.extend(state.sessions());
    sessions.dedup();
    for session in sessions {
        let Ok(view) = rex.inspect(&session).await else {
            continue;
        };
        if let Some(key) = view["active_window_id"]
            .as_str()
            .and_then(|w| state.thread_for_window(w))
        {
            return Some(key.to_string());
        }
    }
    None
}

#[derive(Debug, PartialEq)]
enum Delivered {
    Picture,
    Kept(PathBuf),
}

/// Moves `file` into the inbox of `key` when it's a picture, else aside.
fn deliver(base: &Path, file: &Path, key: &str) -> std::io::Result<Delivered> {
    let name = file.file_name().unwrap_or_default();
    let (to, delivered) = if crate::paste::mime_of(file).is_some() {
        (inbox(base, key), Delivered::Picture)
    } else {
        let to = kept(base);
        (to.clone(), Delivered::Kept(to))
    };
    std::fs::create_dir_all(&to)?;
    std::fs::rename(file, to.join(name))?;
    Ok(delivered)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pictures_go_to_the_threads_inbox_and_the_rest_aside() {
        let base = tempfile::tempdir().unwrap();
        let dir = staging(base.path());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("shot.png"), b"png").unwrap();
        std::fs::write(dir.join("notes.txt"), b"txt").unwrap();
        std::fs::write(dir.join(".hidden.png"), b"x").unwrap();
        std::fs::write(dir.join("big.png.partial"), b"x").unwrap();

        let files = arrived(&dir);
        let names: Vec<_> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 2, "{names:?}");

        let key = "env1/mcp:launch:abc";
        for file in &files {
            deliver(base.path(), file, key).unwrap();
        }
        assert!(inbox(base.path(), key).join("shot.png").is_file());
        assert!(kept(base.path()).join("notes.txt").is_file());
        assert!(arrived(&dir).is_empty());
    }
}
