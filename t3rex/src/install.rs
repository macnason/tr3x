//! Setting t3rex up on a Mac: the binary, and the Rex config that runs it. `rex/t3rex.lua`
//! holds the actions and keys, and starts `t3rex sync` as Rex loads its config, so sync lives
//! and restarts with Rex. Nothing needs sudo, and `uninstall` puts everything back.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::rex::Rex;
use crate::state::{self, State};
use crate::{fade, sync};

/// The launchd agent earlier versions ran sync as.
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

enum Mark {
    Ok,
    Warn,
    Fail,
}

fn mark(mark: Mark, what: &str, detail: &str) {
    let colour = std::io::stdout().is_terminal();
    let (glyph, code) = match mark {
        Mark::Ok => ("✓", "32"),
        Mark::Warn => ("!", "33"),
        Mark::Fail => ("✗", "31"),
    };
    let glyph = if colour {
        format!("\x1b[{code}m{glyph}\x1b[0m")
    } else {
        glyph.to_string()
    };
    match (detail.is_empty(), colour) {
        (true, _) => println!("{glyph} {what}"),
        (false, true) => println!("{glyph} {what}  \x1b[2m{detail}\x1b[0m"),
        (false, false) => println!("{glyph} {what}  {detail}"),
    }
}

fn step(ok: bool, what: &str, detail: &str) {
    mark(if ok { Mark::Ok } else { Mark::Fail }, what, detail);
}

/// A failed check, with what to do about it on the line below.
fn fail_with(what: &str, detail: &str, fix: &str) {
    mark(Mark::Fail, what, detail);
    println!("  {fix}");
}

/// Where t3rex runs from once installed: where it already is, unless that's a build or a
/// download, which go to ~/.local/bin.
fn install_target(exe: &Path) -> PathBuf {
    let real = std::fs::canonicalize(exe).unwrap_or_else(|_| exe.to_path_buf());
    let temp = [std::env::temp_dir(), "/tmp".into(), "/var/folders".into()];
    // /tmp and /var are links into /private, so each is checked both ways.
    let passing = real.components().any(|c| c.as_os_str() == "target")
        || temp.iter().any(|dir| {
            let resolved = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.clone());
            [exe, real.as_path()]
                .iter()
                .any(|p| p.starts_with(dir) || p.starts_with(&resolved))
        });
    if passing {
        bin_path()
    } else {
        exe.to_path_buf()
    }
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
    if has_require(&text) {
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

fn has_require(init: &str) -> bool {
    init.lines().any(|l| l.trim() == REQUIRE)
}

fn remove_require(dir: &Path) -> Result<()> {
    let init = dir.join("init.lua");
    let Ok(text) = std::fs::read_to_string(&init) else {
        return Ok(());
    };
    // Only the line install added goes; anything else is the user's.
    if !has_require(&text) {
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

/// The Rex config t3rex adds, pointing at this binary, this Rex, this T3 and this state
/// directory.
fn lua(bin: &Path, rex: &Path) -> String {
    LUA.replace("@T3REX@", &bin.to_string_lossy())
        .replace("@REX@", &rex.to_string_lossy())
        .replace(
            "@T3HOME@",
            &t3client::descriptor::t3_home().to_string_lossy(),
        )
        .replace("@STATE@", &state::dir().to_string_lossy())
}

/// Boots out and deletes the launchd agent an earlier install made. Returns whether there was
/// one.
async fn retire_launchd() -> bool {
    let plist = plist_path();
    if !plist.exists() {
        return false;
    }
    // SAFETY: getuid(2) can't fail.
    let uid = unsafe { libc::getuid() };
    let _ = tokio::process::Command::new("launchctl")
        .args(["bootout", &format!("gui/{uid}/{LABEL}")])
        .output()
        .await;
    let _ = std::fs::remove_file(plist);
    true
}

/// `rex config reload`, and what it printed when the config has errors.
async fn reload(rex: &Rex) -> Result<(), String> {
    let out = tokio::process::Command::new(&rex.bin)
        .args(crate::rex::NO_AUTOSTART)
        .args(["config", "reload"])
        .output()
        .await
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        return Ok(());
    }
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    Err(text.trim().to_string())
}

/// The last `n` lines of sync's log.
fn log_tail(n: usize) -> Vec<String> {
    let text = std::fs::read_to_string(sync::log_path()).unwrap_or_default();
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(n)..]
        .iter()
        .map(|l| l.to_string())
        .collect()
}

/// A line of sync's log that says something went wrong, rather than what it synced.
fn is_error(line: &str) -> bool {
    !(line.contains(" thread(s)")
        || line.ends_with(" is back")
        || line.ends_with(" joined")
        || line.contains(": following "))
}

/// Waits for a sync that started after `since` to save the session.
async fn first_sync(since: SystemTime) -> Option<State> {
    let saved = || {
        std::fs::metadata(state::dir().join("state.json"))
            .and_then(|m| m.modified())
            .is_ok_and(|t| t >= since)
    };
    for i in 0..40 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if sync::running().is_none() || !saved() {
            continue;
        }
        let state = State::load();
        // The session comes first and its windows a moment later; with no threads, it stays
        // empty but for the new-thread window.
        if state.session().is_some() && (state.window_count() > 0 || i >= 12) {
            return Some(state);
        }
    }
    None
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
    let client = crate::t3::connect()
        .await
        .context("couldn't reach T3 Code; is it running?")?;
    let found = client.descriptor().expect("a connected machine");
    step(
        true,
        "T3 Code",
        &format!("{} at {}", found.server_version, found.origin),
    );

    let exe = std::env::current_exe()?;
    let bin = install_target(&exe);
    if bin != exe {
        std::fs::create_dir_all(bin.parent().unwrap())?;
        // Copy beside it and rename, so a running sync keeps its old binary.
        let tmp = bin.with_extension("new");
        std::fs::copy(&exe, &tmp)?;
        std::fs::rename(&tmp, &bin)?;
    }
    step(true, "Installed t3rex", &bin.display().to_string());

    if retire_launchd().await {
        step(true, "Removed the launchd agent", LABEL);
    }
    // The one Rex starts next runs this binary.
    if sync::stop().await {
        step(true, "Stopped the old sync", "");
    }

    let dir = config_dir();
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("t3rex.lua"), lua(&bin, &rex.bin))?;
    let what = add_require(&dir)?;
    step(
        true,
        "Rex config",
        &format!("{}/t3rex.lua, {what}", dir.display()),
    );
    // Earlier versions turned Rex's pane fading off for every session.
    if !fade::enabled() && fade::restore() {
        step(true, "Put Rex's pane fading back", "");
    }

    std::fs::create_dir_all(state::dir())?;
    let since = SystemTime::now();
    match reload(&rex).await {
        Ok(()) => step(true, "Reloaded Rex config", "Rex starts sync with it"),
        Err(text) => {
            mark(Mark::Warn, "Rex config has errors", "");
            for line in text.lines() {
                println!("  {line}");
            }
        }
    }

    let Some(state) = first_sync(since).await else {
        step(false, "Synced", "no sync after 20 seconds");
        let tail = log_tail(15);
        if !tail.is_empty() {
            println!("\n{}:", sync::log_path().display());
            for line in tail {
                println!("  {line}");
            }
        }
        bail!("sync didn't start; `t3rex doctor` checks each part");
    };
    step(
        true,
        "Synced",
        &format!(
            "{} thread(s) in the \"{}\" session",
            state.window_count(),
            crate::sync::SESSION_LABEL
        ),
    );
    if let Some(session) = &state.session() {
        let _ = rex
            .client_do("session.select", &[("session_id", session)])
            .await;
    }
    other_machines().await;
    println!("\nKeys: ⌘T new thread (in T3 Code) · ⌃⌘N new thread · ⌃⌘I next that needs you");
    println!("Everything else is in Rex's command palette (⌘⇧P) under T3 Code.");
    Ok(())
}

/// Offers to pair the machines on the tailnet that run T3: over SSH with one confirm where an
/// SSH host reaches them, else with a pasted link. Without a terminal to ask in, says how.
async fn other_machines() {
    let hub = crate::t3::hub();
    let home = hub.home();
    let found = t3client::pair::candidates(home.as_deref(), &hub.registry).await;
    if found.is_empty() {
        return;
    }
    println!();
    if !std::io::stdin().is_terminal() {
        for c in &found {
            mark(
                Mark::Warn,
                &format!("{} runs T3 too", c.label),
                &pair_hint(c).await,
            );
        }
        return;
    }
    for c in &found {
        crate::machines::offer(&hub, c).await;
    }
}

/// How to pair a machine found on the tailnet: over SSH where it reaches, else joining.
async fn pair_hint(c: &t3client::pair::Candidate) -> String {
    match &c.ssh {
        Some(host) => format!("`t3rex pair {host}`"),
        None => crate::machines::join_hint(&c.label).await,
    }
}

pub async fn uninstall() -> Result<()> {
    let rex = Rex::find();
    // The config goes first, so nothing in Rex starts sync again.
    let dir = config_dir();
    remove_require(&dir)?;
    let _ = std::fs::remove_file(dir.join("t3rex.lua"));
    let _ = reload(&rex).await;
    step(true, "Removed the Rex config", "");
    retire_launchd().await;
    sync::stop().await;
    step(true, "Stopped sync", "");
    let state = State::load();
    for session in state.sessions() {
        let gone = tokio::process::Command::new(&rex.bin)
            .args(crate::rex::NO_AUTOSTART)
            .args(["kill", &session])
            .output()
            .await;
        step(
            gone.is_ok_and(|o| o.status.success()),
            "Closed the T3 Code session",
            "",
        );
    }
    if fade::restore() {
        step(true, "Put Rex's pane fading back", "");
    }
    // Rex leaves T3 → Connections on each machine it can reach.
    let hub = crate::t3::hub();
    for machine in hub.registry.machines.clone() {
        if hub.store.credential(&machine.id).is_none() {
            continue;
        }
        match t3client::pair::forget(&hub.store, &machine).await {
            Ok(t3client::pair::Forgot::Revoked) => {
                step(true, &format!("Unpaired {}", machine.name()), "")
            }
            _ => mark(
                Mark::Warn,
                &format!(
                    "Remove Rex from T3 → Settings → Connections on {}",
                    machine.name()
                ),
                "",
            ),
        }
    }
    let _ = std::fs::remove_dir_all(state::dir());
    let _ = std::fs::remove_dir_all(home().join("Library/Caches/t3rex"));
    let _ = std::fs::remove_file(bin_path());
    step(true, "Removed t3rex", "");
    Ok(())
}

/// A headless Chromium for HTML snapshots, where `media` looks for one.
fn has_chromium() -> bool {
    if std::env::var_os("T3REX_CHROME").is_some() {
        return true;
    }
    let playwright = std::fs::read_dir(home().join("Library/Caches/ms-playwright"))
        .into_iter()
        .flatten()
        .flatten()
        .any(|d| {
            d.file_name()
                .to_string_lossy()
                .starts_with("chromium_headless_shell")
        });
    playwright
        || [
            "/Applications/Google Chrome.app",
            "/Applications/Chromium.app",
            "/Applications/Google Chrome Canary.app",
        ]
        .iter()
        .any(|app| Path::new(app).exists())
}

pub async fn doctor() -> Result<()> {
    let mut failed = 0;
    let mut fail = |what: &str, detail: &str, fix: &str| {
        failed += 1;
        fail_with(what, detail, fix);
    };
    let reinstall = "Run `t3rex install`.";

    let rex = Rex::find();
    let sessions = rex.sessions().await;
    match &sessions {
        Ok(_) => step(true, "Rex server", &rex.bin.display().to_string()),
        Err(e) => fail("Rex server", &format!("{e:#}"), "Open Rex Beta."),
    }

    let dir = config_dir();
    let init = std::fs::read_to_string(dir.join("init.lua")).unwrap_or_default();
    if has_require(&init) {
        step(
            true,
            "init.lua loads t3rex",
            &dir.join("init.lua").display().to_string(),
        );
    } else {
        fail("init.lua doesn't load t3rex", "", reinstall);
    }
    if dir.join("t3rex.lua").exists() {
        step(
            true,
            "Rex actions",
            &dir.join("t3rex.lua").display().to_string(),
        );
    } else {
        fail(
            "Rex actions are missing",
            &dir.join("t3rex.lua").display().to_string(),
            reinstall,
        );
    }
    if sessions.is_ok() {
        let keymap = tokio::process::Command::new(&rex.bin)
            .args(crate::rex::NO_AUTOSTART)
            .args(["keymap", "--json"])
            .output()
            .await
            .ok()
            .and_then(|o| serde_json::from_slice::<Value>(&o.stdout).ok())
            .unwrap_or_default();
        let errors: Vec<String> = keymap["diagnostics"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|d| d["severity"] == "error")
            .map(|d| {
                format!(
                    "{} {}",
                    d["source"].as_str().unwrap_or("init.lua"),
                    d["message"].as_str().unwrap_or_default()
                )
            })
            .collect();
        let loaded = keymap["actions"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|a| a["name"] == "t3_inbox");
        let fix = "Run `rex config check`, fix what it reports, then `rex config reload`.";
        match (errors.first(), loaded) {
            (Some(error), _) => fail("Rex config has errors", error, fix),
            (None, false) => fail("Rex hasn't loaded t3rex", "", "Run `rex config reload`."),
            (None, true) => step(true, "Rex config loads", ""),
        }
    }
    if plist_path().exists() {
        fail(
            "An old launchd agent runs sync",
            LABEL,
            "Run `t3rex install` to move sync into Rex.",
        );
    }

    // Each machine: whether it answers, as which T3, and whether Rex is still paired there.
    let hub = crate::t3::hub();
    if hub.envs().is_empty() {
        fail(
            "T3 server",
            "T3 Code isn't set up on this Mac",
            "Open T3 Code once, or `t3rex pair` a machine that runs it.",
        );
    }
    let checks = hub.envs().iter().map(|env| {
        let env = env.clone();
        async move {
            let checked = tokio::time::timeout(Duration::from_secs(8), env.check()).await;
            (env, checked)
        }
    });
    for (env, checked) in futures_util::future::join_all(checks).await {
        let name = env.name().to_string();
        let what = if env.is_local() {
            "T3 server".to_string()
        } else {
            format!("T3 on {name}")
        };
        match checked {
            Ok(Ok(found)) => {
                step(
                    true,
                    &what,
                    &format!("{} at {}", found.server_version, found.origin),
                );
                let missing: Vec<&str> = ["threadSettlement", "threadSnooze"]
                    .into_iter()
                    .filter(|c| !found.has_capability(c))
                    .collect();
                if !missing.is_empty() {
                    fail(
                        &format!("T3 on {name} is missing capabilities"),
                        &missing.join(", "),
                        "Update T3 Code there.",
                    );
                }
                let machine = env.machine();
                if let Some(c) = hub.store.credential(env.id())
                    && matches!(
                        machine.via,
                        t3client::store::Via::Link | t3client::store::Via::Join
                    )
                    && !c.renews_itself()
                    && c.expires_at - crate::t3::now_ms() < 7 * 86_400_000
                {
                    mark(
                        Mark::Warn,
                        &format!("Rex's pairing with {name} ends within a week"),
                        &format!(
                            "To pair it again, {}",
                            crate::machines::join_hint(&name).await
                        ),
                    );
                }
            }
            Ok(Err(e)) if t3client::env::is_pairing_error(&e) => fail(
                &format!("{name} needs pairing again"),
                "",
                &format!("Run `t3rex pair \"{name}\"`."),
            ),
            Ok(Err(e)) if env.is_local() => fail(&what, &format!("{e:#}"), "Open T3 Code."),
            // Another machine can be asleep or away; its windows wait for it.
            Ok(Err(e)) => mark(Mark::Warn, &format!("{name} is offline"), &format!("{e:#}")),
            Err(_) => mark(Mark::Warn, &format!("{name} didn't answer"), ""),
        }
    }
    for c in t3client::pair::candidates(hub.home().as_deref(), &hub.registry).await {
        mark(
            Mark::Warn,
            &format!("{} runs T3 and isn't paired", c.label),
            &pair_hint(&c).await,
        );
    }

    let log = sync::log_path();
    match sync::running() {
        Some(pid) => step(true, "Sync", &format!("pid {pid}")),
        None => fail(
            "Sync isn't running",
            "",
            "Run `rex config reload`; Rex starts sync as its config loads.",
        ),
    }
    let tail = log_tail(200);
    let last_error = tail.iter().rposition(|l| is_error(l));
    match last_error {
        // Still failing: nothing has synced since.
        Some(at) if at + 1 == tail.len() => fail(
            "Sync's last word is an error",
            &tail[at],
            &format!("See {}.", log.display()),
        ),
        Some(at) => mark(Mark::Warn, "Sync's last error, since recovered", &tail[at]),
        None => {}
    }
    let state = State::load();
    let session_ok = match (&state.session(), &sessions) {
        (Some(id), Ok(list)) => list.iter().any(|s| s["session_id"].as_str() == Some(id)),
        _ => false,
    };
    if session_ok {
        step(
            true,
            "T3 Code session",
            &format!("{} window(s)", state.window_count()),
        );
    } else {
        fail(
            "No T3 Code session",
            "",
            "Sync makes it once T3 answers; check the lines above.",
        );
    }

    if has_chromium() {
        step(true, "Headless Chromium", "for HTML snapshots");
    } else {
        mark(
            Mark::Warn,
            "No headless Chromium",
            "HTML embeds show as links",
        );
        println!("  For snapshots: `npx playwright install chromium-headless-shell`.");
    }

    if failed > 0 {
        bail!("{failed} check(s) failed");
    }
    Ok(())
}

/// Opens the thread in the T3 Code desktop app.
pub async fn open_in_t3(thread: &str) -> Result<()> {
    let (_, thread) = crate::t3::thread(thread)?;
    let url = format!("t3code://threads/{}/{}", thread.env, thread.thread);
    let opened = tokio::process::Command::new("/usr/bin/open")
        .arg(&url)
        .output()
        .await?;
    if !opened.status.success() {
        bail!("couldn't open {url}");
    }
    Ok(())
}

/// Where to go for the thread that most needs you, on any machine, as sync last worked it out
/// in T3's order: `session_id` and `window_id`, as Rex's `session.select` takes them. Also
/// `home`, the session new threads open in, and `sessions`, every session t3rex keeps. Fails
/// when sync isn't running, so Rex's actions start it.
pub async fn inbox() -> Result<()> {
    if sync::running().is_none() {
        bail!("t3rex isn't syncing");
    }
    let state = State::load();
    let home = crate::t3::hub()
        .home_id()
        .and_then(|id| state.groups.get(id))
        .and_then(|g| g.session_id.clone())
        .or_else(|| state.session())
        .context("t3rex hasn't synced yet")?;
    let mut out = match &state.next {
        Some(next) => {
            serde_json::json!({"session_id": next.session_id, "window_id": next.window_id})
        }
        None => serde_json::json!({"session_id": home}),
    };
    out["home"] = serde_json::json!(home);
    out["sessions"] = serde_json::json!(state.sessions());
    println!("{out}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_and_downloads_install_to_local_bin_and_the_rest_stay_put() {
        let home = bin_path();
        assert_eq!(
            install_target(Path::new("/src/t3rex/target/release/t3rex")),
            home
        );
        let download = std::env::temp_dir().join("x/t3rex");
        assert_eq!(install_target(&download), home);
        let brew = Path::new("/opt/homebrew/bin/t3rex");
        assert_eq!(install_target(brew), brew);
    }

    #[test]
    fn require_line_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("init.lua"),
            "rex.bind(\"cmd+d\", \"pane.split\")",
        )
        .unwrap();
        add_require(dir.path()).unwrap();
        let init = std::fs::read_to_string(dir.path().join("init.lua")).unwrap();
        assert!(has_require(&init));
        assert_eq!(add_require(dir.path()).unwrap(), "already loads t3rex");
        remove_require(dir.path()).unwrap();
        let init = std::fs::read_to_string(dir.path().join("init.lua")).unwrap();
        assert_eq!(init, "rex.bind(\"cmd+d\", \"pane.split\")\n");
    }

    #[test]
    fn the_lua_names_every_path() {
        let text = lua(Path::new("/b/t3rex"), Path::new("/r/rex"));
        assert!(!text.contains('@'), "an unfilled placeholder");
        assert!(text.contains("\"/b/t3rex\"") && text.contains("\"/r/rex\""));
    }
}
