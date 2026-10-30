//! Setting t3rex up on a Mac: the binary in ~/.local/bin, the Rex actions beside init.lua, a
//! launchd agent that keeps `t3rex sync` running, and a check that Rex and T3 both answer.
//! Nothing needs sudo, and `uninstall` puts everything back.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::rex::Rex;
use crate::state::{self, State};

const LABEL: &str = "com.t3rex.sync";
const LUA: &str = include_str!("../../rex/t3rex.lua");
const REQUIRE: &str = "require(\"t3rex\") -- T3 Code in Rex, added by t3rex install";

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

fn bin_path() -> PathBuf {
    home().join(".local/bin/t3rex")
}

fn config_dir() -> PathBuf {
    match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) => PathBuf::from(dir).join("rex"),
        None => home().join(".config/rex"),
    }
}

fn plist_path() -> PathBuf {
    home()
        .join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist"))
}

fn uid() -> String {
    std::process::Command::new("id")
        .arg("-u")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|_| "501".into())
}

fn step(ok: bool, what: &str, detail: &str) {
    let mark = if ok {
        "\x1b[32m✓\x1b[0m"
    } else {
        "\x1b[31m✗\x1b[0m"
    };
    if detail.is_empty() {
        println!("{mark} {what}");
    } else {
        println!("{mark} {what}  \x1b[2m{detail}\x1b[0m");
    }
}

fn path_var() -> String {
    let bin = home().join(".local/bin");
    format!(
        "{}:/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin",
        bin.display()
    )
}

fn plist(bin: &Path, rex: &Path) -> String {
    let log = state::dir().join("sync.log");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{LABEL}</string>
  <key>ProgramArguments</key><array><string>{bin}</string><string>sync</string></array>
  <key>EnvironmentVariables</key><dict>
    <key>T3REX_REX</key><string>{rex}</string>
    <key>PATH</key><string>{path}</string>
    <key>T3REX_STATE_DIR</key><string>{state}</string>
  </dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ThrottleInterval</key><integer>10</integer>
  <key>ProcessType</key><string>Background</string>
  <key>StandardOutPath</key><string>{log}</string>
  <key>StandardErrorPath</key><string>{log}</string>
</dict>
</plist>
"#,
        bin = bin.display(),
        rex = rex.display(),
        path = path_var(),
        log = log.display(),
        state = state::dir().display(),
    )
}

/// Adds the `require` line to init.lua, after copying the file aside once.
fn add_require(dir: &Path) -> Result<&'static str> {
    let init = dir.join("init.lua");
    // A file that exists but can't be read is left alone, never replaced.
    let text = match std::fs::read_to_string(&init) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => bail!("couldn't read {}: {e}", init.display()),
    };
    if text.lines().any(|l| l.trim() == REQUIRE) {
        return Ok("already loads t3rex");
    }
    if init.exists() {
        let backup = dir.join("init.lua.before-t3rex");
        if !backup.exists() {
            std::fs::copy(&init, &backup)?;
        }
    }
    let mut new = text.clone();
    if !new.is_empty() && !new.ends_with('\n') {
        new.push('\n');
    }
    new.push_str(REQUIRE);
    new.push('\n');
    std::fs::write(&init, new)?;
    Ok(if text.is_empty() {
        "created init.lua"
    } else {
        "added one line to init.lua (backup: init.lua.before-t3rex)"
    })
}

fn remove_require(dir: &Path) -> Result<()> {
    let init = dir.join("init.lua");
    let Ok(text) = std::fs::read_to_string(&init) else {
        return Ok(());
    };
    // Only the line install added goes; anything else is the user's.
    if !text.lines().any(|l| l.trim() == REQUIRE) {
        return Ok(());
    }
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let kept: Vec<&str> = text.lines().filter(|l| l.trim() != REQUIRE).collect();
    if kept.iter().all(|l| l.trim().is_empty()) && !dir.join("init.lua.before-t3rex").exists() {
        std::fs::remove_file(&init)?;
    } else {
        let ends = text.ends_with('\n');
        std::fs::write(&init, kept.join(newline) + if ends { newline } else { "" })?;
    }
    Ok(())
}

async fn launchctl(args: &[&str]) -> bool {
    tokio::process::Command::new("launchctl")
        .args(args)
        .output()
        .await
        .is_ok_and(|o| o.status.success())
}

pub async fn install() -> Result<()> {
    let rex = Rex::find();
    if rex.sessions().await.is_err() {
        step(
            false,
            "Rex",
            &format!("couldn't reach the Rex server with {}", rex.bin.display()),
        );
        bail!("open Rex first, then run t3rex install again");
    }
    step(true, "Rex", &rex.bin.display().to_string());
    let client = crate::t3::connect(false)
        .await
        .context("couldn't reach T3 Code; is it running?")?;
    step(
        true,
        "T3 Code",
        &format!(
            "{} at {}",
            client.runtime.server_version, client.runtime.origin
        ),
    );

    let bin = bin_path();
    std::fs::create_dir_all(bin.parent().unwrap())?;
    let exe = std::env::current_exe()?;
    if exe != bin {
        // Copy beside it and rename, so a running sync keeps its old binary.
        let tmp = bin.with_extension("new");
        std::fs::copy(&exe, &tmp)?;
        std::fs::rename(&tmp, &bin)?;
    }
    step(true, "Installed t3rex", &bin.display().to_string());

    let dir = config_dir();
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        dir.join("t3rex.lua"),
        LUA.replace("@T3REX@", &bin.to_string_lossy()),
    )?;
    let what = add_require(&dir)?;
    step(
        true,
        "Rex actions",
        &format!("{}/t3rex.lua, {what}", dir.display()),
    );
    let reloaded = tokio::process::Command::new(&rex.bin)
        .args(["config", "reload"])
        .output()
        .await;
    step(
        reloaded.is_ok_and(|o| o.status.success()),
        "Reloaded Rex config",
        "",
    );

    std::fs::create_dir_all(state::dir())?;
    let plist_file = plist_path();
    std::fs::create_dir_all(plist_file.parent().unwrap())?;
    std::fs::write(&plist_file, plist(&bin, &rex.bin))?;
    let domain = format!("gui/{}", uid());
    let _ = launchctl(&["bootout", &format!("{domain}/{LABEL}")]).await;
    // launchd takes a moment to forget a job it just booted out, and refuses it again until then.
    let mut loaded = false;
    for _ in 0..20 {
        loaded = launchctl(&["bootstrap", &domain, &plist_file.to_string_lossy()]).await;
        if loaded {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    step(loaded, "Background sync", &format!("launchd agent {LABEL}"));

    // Wait for the first sync to make the session.
    for _ in 0..30 {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let state = State::load();
        if let Some(session) = state.session_id.filter(|_| !state.windows.is_empty()) {
            let count = state.windows.len();
            step(
                true,
                "Synced",
                &format!(
                    "{count} thread(s) in the \"{}\" session",
                    crate::sync::SESSION_LABEL
                ),
            );
            let _ = rex
                .client_do("session.select", &[("session_id", &session)])
                .await;
            println!(
                "\nKeys: ⌃⌘I next thread that needs you · ⌃⌘S settle · ⌃⌘Z snooze 1h · ⌃⌘P pin · ⌃⌘O open in T3"
            );
            println!("Every action is also in Rex's command palette under T3 Code.");
            return Ok(());
        }
    }
    step(
        false,
        "Synced",
        &format!(
            "no session yet; see {}",
            state::dir().join("sync.log").display()
        ),
    );
    Ok(())
}

pub async fn uninstall() -> Result<()> {
    let domain = format!("gui/{}", uid());
    let _ = launchctl(&["bootout", &format!("{domain}/{LABEL}")]).await;
    let _ = std::fs::remove_file(plist_path());
    step(true, "Stopped background sync", "");
    let state = State::load();
    if let Some(session) = &state.session_id {
        let rex = Rex::find();
        let gone = tokio::process::Command::new(&rex.bin)
            .args(["kill", session])
            .output()
            .await;
        step(
            gone.is_ok_and(|o| o.status.success()),
            "Closed the T3 Code session",
            "",
        );
    }
    let dir = config_dir();
    remove_require(&dir)?;
    let _ = std::fs::remove_file(dir.join("t3rex.lua"));
    let _ = tokio::process::Command::new(Rex::find().bin)
        .args(["config", "reload"])
        .output()
        .await;
    step(true, "Removed the Rex actions", "");
    let _ = std::fs::remove_dir_all(state::dir());
    let _ = std::fs::remove_file(bin_path());
    step(true, "Removed t3rex", "");
    Ok(())
}

pub async fn doctor() -> Result<()> {
    let rex = Rex::find();
    let sessions = rex.sessions().await;
    step(
        sessions.is_ok(),
        "Rex server",
        &rex.bin.display().to_string(),
    );
    match crate::t3::connect(false).await {
        Ok(client) => {
            step(
                true,
                "T3 server",
                &format!(
                    "{} at {}",
                    client.runtime.server_version, client.runtime.origin
                ),
            );
            let shell = client.shell().await?;
            step(
                true,
                "T3 threads",
                &format!("{} in the shell", shell.threads.len()),
            );
        }
        Err(e) => step(false, "T3 server", &format!("{e:#}")),
    }
    let state = State::load();
    let session_ok = match (&state.session_id, &sessions) {
        (Some(id), Ok(list)) => list.iter().any(|s| s["session_id"].as_str() == Some(id)),
        _ => false,
    };
    step(
        session_ok,
        "T3 Code session",
        &format!("{} window(s)", state.windows.len()),
    );
    let running = tokio::process::Command::new("launchctl")
        .args(["print", &format!("gui/{}/{LABEL}", uid())])
        .output()
        .await
        .is_ok_and(|o| o.status.success());
    step(running, "Background sync", LABEL);
    step(
        config_dir().join("t3rex.lua").exists(),
        "Rex actions",
        &config_dir().join("t3rex.lua").display().to_string(),
    );
    Ok(())
}

/// Opens the thread in the T3 Code desktop app.
pub async fn open_in_t3(thread: &str) -> Result<()> {
    let client = crate::t3::connect(false).await?;
    let runtime = &client.runtime;
    let url = format!("t3code://threads/{}/{}", runtime.environment_id, thread);
    let opened = tokio::process::Command::new("/usr/bin/open")
        .arg(&url)
        .output()
        .await?;
    if !opened.status.success() {
        bail!("couldn't open {url}");
    }
    Ok(())
}

/// Where to go for the thread that most needs you: the first window of the T3 session, which
/// sync keeps in T3's order.
pub async fn inbox() -> Result<()> {
    let state = State::load();
    let session = state.session_id.context("t3rex hasn't synced yet")?;
    let view = Rex::find().inspect(&session).await?;
    let mine: std::collections::HashSet<&str> = state
        .windows
        .values()
        .map(|w| w.window_id.as_str())
        .collect();
    let first = view["windows"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|w| w["window_id"].as_str())
        .find(|id| mine.contains(id));
    let out = match first {
        Some(window) => serde_json::json!({"session_id": session, "window_id": window}),
        None => serde_json::json!({"session_id": session}),
    };
    println!("{out}");
    Ok(())
}
