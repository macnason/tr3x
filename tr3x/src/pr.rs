//! A thread's pull request, in a drawer on the right of its window. T3 makes it: Create Pull
//! Request commits, pushes and opens one with the title and body T3 writes, as T3's own Create
//! PR does, and links it to the thread. sup (github.com/superultrainc/sup) shows it: the
//! description, conversation, review threads, checks and files, and merges, approves and
//! comments from the keyboard.
//!
//! The drawer floats over the thread's pane: down its right side, full height, when the window
//! has room for both, else over nearly all of it, as on a phone. A floating layer, not a split;
//! Rex's iPhone app shows it as a tab of its own, which a phone looking is taken to. `q` in sup
//! closes it.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind};
use futures_util::StreamExt;
use serde_json::{Value, json};

use crate::chat::{BOLD, DIM, GREEN, RED, RESET};
use crate::rex::{Rex, lua_str};
use crate::t3;
use crate::term::out;

/// The drawer's block label, which finds it again.
const LABEL: &str = "Pull Request";
/// The window width, in columns, that holds the conversation and the drawer side by side.
const ROOM: u64 = 140;
/// The conversation's share of the window beside the drawer.
const RATIO: f64 = 0.45;

/// Opens the drawer for `thread` in the window holding `block`, or goes to the one already
/// open there. `create` makes the pull request first, replacing an open drawer.
pub async fn open(thread: &str, block: &str, create: bool) -> Result<()> {
    let rex = Rex::find();
    let mut sessions = crate::state::State::load().sessions();
    // The session the action ran in first.
    if let Ok(here) = std::env::var("REX_SESSION") {
        sessions.insert(0, here);
    }
    let mut found = None;
    for session in sessions {
        let Ok(view) = rex.inspect(&session).await else {
            continue;
        };
        let window = view["windows"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|w| blocks(w).any(|b| b["block_id"] == block))
            .cloned();
        if let Some(window) = window {
            found = Some((session, window));
            break;
        }
    }
    let (session, window) = found.context("Go to a T3 thread first")?;
    let window = &window;
    let drawer = blocks(window)
        .find(|b| b["label"] == LABEL)
        .and_then(|b| b["block_id"].as_str());
    if let Some(drawer) = drawer {
        if !create {
            let args = json!({"session_id": session, "block_id": drawer}).to_string();
            rex.call("session.focus_block", &args).await?;
            return rex.show_on_phones(&session).await;
        }
        let args = json!({"session_id": session, "block_id": drawer}).to_string();
        rex.call("block.close", &args).await?;
    }
    // The window's width: the pane's columns over its share of the window, as the layout has it
    // now. A pane's size follows a change of layout a moment later.
    let size = rex.block_method(&session, block, "size").await?;
    let share = window["layers"]
        .as_array()
        .into_iter()
        .flatten()
        .find_map(|l| share_of(&l["layout"], block))
        .unwrap_or(1.0);
    let wide = size["columns"].as_f64().unwrap_or(0.0) / share >= ROOM as f64;
    let exe = crate::icons::real_exe().to_string_lossy().into_owned();
    let mut args = vec!["pr-pane", thread];
    if create {
        args.push("--create");
    }
    let data = json!({
        "argv": crate::rex::pane_argv(&exe, &args),
        "window": window["window_id"],
        "wide": wide,
        "ratio": RATIO,
        "label": LABEL,
    });
    let script = format!(
        r#"local d = rex.json.decode({})
local layout = {{ block = {{ flavor = "com.superlogical.terminal.shell", label = d.label,
  options = {{ shell = "none", login = false, command = d.argv }} }} }}
local bounds = {{ 0.03, 0.03, 0.94, 0.94 }}
if d.wide then bounds = {{ d.ratio, 0, 1 - d.ratio, 1 }} end
local _, err = rex.session.new_layer{{ window_id = d.window, bounds = bounds, focus = true,
  layout = layout }}
rex.session.disown_block{{}}
if err then error(err, 0) end
return {{}}"#,
        lua_str(&data.to_string())
    );
    rex.lua(&session, &script).await?;
    rex.show_on_phones(&session).await
}

/// The fraction of the window's width `block` takes in `node`'s layout.
fn share_of(node: &Value, block: &str) -> Option<f64> {
    if node["block_id"] == block {
        return node["rect"][2].as_f64().filter(|w| *w > 0.0);
    }
    let split = node.get("split")?;
    share_of(&split["before"], block).or_else(|| share_of(&split["after"], block))
}

fn blocks(window: &Value) -> impl Iterator<Item = &Value> {
    window["layers"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|l| l["blocks"].as_array().into_iter().flatten())
}

/// The drawer: makes the pull request if asked, or if there's none and Enter says to, then
/// becomes sup on it.
pub async fn pane(key: String, create: bool) -> Result<()> {
    let result = run(&key, create).await;
    if let Err(e) = &result {
        out(&format!("\r\n {RED}✗{RESET} {e:#}\r\n"));
        wait_key().await;
    }
    result
}

async fn run(key: &str, create: bool) -> Result<()> {
    let (client, thread) = t3::thread(key)?;
    let shell = client.shell().await?;
    let found = t3client::inbox::find_thread(&shell, &thread.thread)
        .context("T3 doesn't have that thread")?;
    let project = found["projectId"].as_str().unwrap_or_default().to_string();
    let cwd = found["worktreePath"]
        .as_str()
        .filter(|p| !p.is_empty())
        .or_else(|| {
            shell
                .projects
                .iter()
                .find(|p| p["id"] == project.as_str())
                .and_then(|p| p["workspaceRoot"].as_str())
        })
        .context("that thread has no folder")?
        .to_string();
    let status = client
        .call("vcs.refreshStatus", json!({"cwd": cwd}))
        .await?;
    let branch = status["refName"].as_str().unwrap_or("detached");
    out(&format!(
        "\r\n {BOLD}{LABEL}{RESET}\r\n {DIM}{branch}{RESET}\r\n\r\n"
    ));
    // The PR linked to the thread, open or not; else the branch's.
    let link = linked(found);
    let known = link
        .and_then(|pr| pr["url"].as_str())
        .or(status["pr"]["url"].as_str());
    let url = match (known, create) {
        (Some(url), false) => url.to_string(),
        (_, false) => {
            out(&format!(
                " No pull request\r\n\r\n {DIM}Enter{RESET} Create   {DIM}Esc{RESET} Close\r\n"
            ));
            if !confirm().await {
                return Ok(());
            }
            out("\x1b[3A\r\x1b[J");
            make(&client, &cwd, &project, &thread.thread, &status).await?
        }
        (_, true) => make(&client, &cwd, &project, &thread.thread, &status).await?,
    };
    let reference = reference(&project, link.filter(|l| l["url"] == url.as_str()), &url);
    show(&url, Path::new(&cwd), &client, reference).await
}

/// The pull request as T3's `pullRequests` methods name it: the thread's link, else read from
/// its URL (`https://host/owner/repo/pull/N`).
fn reference(project: &str, link: Option<&Value>, url: &str) -> Option<Value> {
    if let Some(link) = link
        && let (Some(repository), Some(number)) =
            (link["repository"].as_str(), link["number"].as_u64())
    {
        let mut reference =
            json!({"projectId": project, "repository": repository, "number": number});
        if let Some(host) = link["host"].as_str() {
            reference["host"] = json!(host);
        }
        return Some(reference);
    }
    let path = url.split_once("://")?.1.split_once('/')?.1;
    let mut parts = path.split('/');
    let (owner, repo, pull, number) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    let number: u64 = number.parse().ok()?;
    (pull == "pull" && !owner.is_empty() && !repo.is_empty()).then(
        || json!({"projectId": project, "repository": format!("{owner}/{repo}"), "number": number}),
    )
}

/// How often the drawer asks T3 to look at the pull request again while sup is open.
const RECHECK: Duration = Duration::from_secs(5);

/// Asks T3 to read the pull request fresh, and whether it's merged. A merge in sup goes
/// straight to GitHub, so T3 would only see it on its own sweep, a minute or two later; this
/// settles the thread as soon as T3 reads it merged.
/// None when T3 couldn't say.
async fn recheck(client: &t3client::env::Env, reference: &Value) -> Option<bool> {
    client
        .call("pullRequests.invalidate", json!({"reference": reference}))
        .await
        .ok()?;
    let summary = client
        .call("pullRequests.summary", reference.clone())
        .await
        .ok()?;
    Some(summary["state"] == "merged")
}

/// Commits, pushes and opens the pull request through T3, with each step as it goes. On the
/// default branch, T3 makes a feature branch first. Returns the pull request's URL.
async fn make(
    client: &t3client::env::Env,
    cwd: &str,
    project: &str,
    thread: &str,
    status: &Value,
) -> Result<String> {
    let mut payload = json!({
        "actionId": uuid::Uuid::new_v4().to_string(),
        "cwd": cwd,
        "action": "commit_push_pr",
        "threadId": thread,
        "projectId": project,
    });
    if status["isDefaultRef"] == true {
        payload["featureBranch"] = json!(true);
    }
    let rpc = client.rpc().await?;
    let mut events = rpc.stream("git.runStackedAction", payload).await?;
    let mut step: Option<String> = None;
    let mut pr = None;
    while let Some(event) = events.next().await {
        let event = event?;
        // A stream chunk can carry several events.
        for event in event.as_array().cloned().unwrap_or_else(|| vec![event]) {
            match event["kind"].as_str() {
                Some("phase_started") => {
                    if let Some(done) = step.take() {
                        out(&format!("\r\x1b[2K {GREEN}✓{RESET} {done}\r\n"));
                    }
                    let label = event["label"].as_str().unwrap_or("Working").to_string();
                    out(&format!(" {DIM}·{RESET} {label}"));
                    step = Some(label);
                }
                Some("hook_output") => {
                    if let Some(text) = event["text"].as_str() {
                        out(&format!("\r\x1b[2K   {DIM}{}{RESET}\r\n", one_line(text)));
                        if let Some(label) = &step {
                            out(&format!(" {DIM}·{RESET} {label}"));
                        }
                    }
                }
                Some("action_failed") => {
                    out("\r\x1b[2K");
                    bail!(
                        "{}",
                        event["message"].as_str().unwrap_or("T3 couldn't do that")
                    );
                }
                Some("action_finished") => {
                    if let Some(done) = step.take() {
                        out(&format!("\r\x1b[2K {GREEN}✓{RESET} {done}\r\n"));
                    }
                    pr = Some(event["result"]["pr"].clone());
                }
                _ => {}
            }
        }
    }
    let pr = pr.context("T3 stopped before it finished")?;
    let url = pr["url"]
        .as_str()
        .context("T3 didn't open a pull request")?;
    if let Some(title) = pr["title"].as_str() {
        out(&format!("\r\n {BOLD}{}{RESET}\r\n", one_line(title)));
    }
    Ok(url.to_string())
}

/// Runs sup on `url`, asking T3 to look at the pull request again while it's open and once it
/// closes. Once the pull request is merged, the drawer closes. Without sup, `gh pr view` and an
/// offer to install sup, which then shows it.
async fn show(
    url: &str,
    cwd: &Path,
    client: &t3client::env::Env,
    reference: Option<Value>,
) -> Result<()> {
    // Rex starts panes with its own PATH; sup runs gh for its token.
    let path = search_path();
    let find = |name: &str| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(name))
            .find(|p| p.is_file())
    };
    let cwd = if cwd.is_dir() { cwd } else { Path::new("/") };
    // Without sup, the drawer shows what gh can and offers to install sup, then shows it.
    let mut offered = false;
    loop {
        if let Some(sup) = find("sup") {
            let mut child = tokio::process::Command::new(sup)
                .arg(url)
                .env("PATH", &path)
                .current_dir(cwd)
                .kill_on_drop(true)
                .spawn()
                .context("couldn't start sup")?;
            // The first tick reads the pull request as the drawer opens.
            let mut tick = tokio::time::interval(RECHECK);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            // Merged already, as when the drawer opens on an old pull request: stays open.
            let mut was_merged = None;
            loop {
                tokio::select! {
                    _ = child.wait() => break,
                    _ = tick.tick() => {
                        let Some(reference) = &reference else { continue };
                        let Some(merged) = recheck(client, reference).await else { continue };
                        if merged && was_merged == Some(false) {
                            let _ = child.kill().await;
                            return Ok(());
                        }
                        was_merged.get_or_insert(merged);
                    }
                }
            }
            if let Some(reference) = &reference {
                recheck(client, reference).await;
            }
            return Ok(());
        }
        if offered {
            // Installed, yet not where the drawer looks.
            wait_key().await;
            return Ok(());
        }
        if let Some(gh) = find("gh") {
            out("\r\n");
            let _ = std::process::Command::new(gh)
                .args(["pr", "view", url, "--comments"])
                .env("PATH", &path)
                .env("GH_PAGER", "cat")
                .current_dir(cwd)
                .status();
        }
        out(&format!(
            "\r\n \x1b]8;;{url}\x1b\\{url}\x1b]8;;\x1b\\\r\n\r\n {BOLD}Enter{RESET} installs sup for reviews and merging {DIM}· q closes{RESET}\r\n"
        ));
        if !confirm().await {
            return Ok(());
        }
        offered = true;
        out(&format!("\r\n {DIM}Installing sup…{RESET}\r\n"));
        match crate::sup::install().await {
            Ok(at) => out(&format!(
                " {GREEN}✓{RESET} sup {DIM}{}{RESET}\r\n",
                at.display()
            )),
            Err(e) => {
                out(&format!(
                    " {RED}✗{RESET} {e:#}\r\n {DIM}github.com/superultrainc/sup{RESET}\r\n"
                ));
                wait_key().await;
                return Ok(());
            }
        }
    }
}

/// The pane's PATH, then where sup and gh install.
fn search_path() -> std::ffi::OsString {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    for dir in [
        home.join(".local/bin"),
        home.join("go/bin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ] {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    std::env::join_paths(dirs).unwrap_or_default()
}

/// Whether sup is installed where the drawer looks for it.
pub fn has_sup() -> bool {
    std::env::split_paths(&search_path()).any(|dir| dir.join("sup").is_file())
}

fn one_line(text: &str) -> String {
    t3client::transcript::plain(text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Enter says yes; Esc, `q` or the pane closing says no.
async fn confirm() -> bool {
    let _ = crossterm::terminal::enable_raw_mode();
    let mut input = EventStream::new();
    let yes = loop {
        match input.next().await {
            Some(Ok(Event::Key(k))) if k.kind != KeyEventKind::Release => match k.code {
                KeyCode::Enter => break true,
                KeyCode::Esc | KeyCode::Char('q') => break false,
                _ => {}
            },
            Some(Ok(_)) => {}
            _ => break false,
        }
    };
    let _ = crossterm::terminal::disable_raw_mode();
    yes
}

/// The pull request a thread is linked to: an open one, else the one linked last.
fn linked(thread: &Value) -> Option<&Value> {
    let prs = thread["pullRequests"].as_array()?;
    prs.iter()
        .filter(|pr| pr["snapshot"]["state"] == "open")
        .chain(prs.iter())
        .max_by_key(|pr| {
            (
                pr["snapshot"]["state"] == "open",
                pr["linkedAt"].as_str().unwrap_or_default(),
            )
        })
}

/// Holds the pane open on what it says until a key, or a minute.
async fn wait_key() {
    let _ = crossterm::terminal::enable_raw_mode();
    let mut input = EventStream::new();
    let _ = tokio::time::timeout(Duration::from_secs(60), async {
        while let Some(Ok(event)) = input.next().await {
            if matches!(event, Event::Key(k) if k.kind != KeyEventKind::Release) {
                break;
            }
        }
    })
    .await;
    let _ = crossterm::terminal::disable_raw_mode();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_open_pull_request_else_the_last_linked() {
        let pr = |n: u64, state: &str, at: &str| json!({"number": n, "linkedAt": at, "snapshot": {"state": state}});
        let thread = json!({"pullRequests": [
            pr(1, "merged", "2026-11-01T00:00:00Z"),
            pr(2, "open", "2026-10-01T00:00:00Z"),
            pr(3, "closed", "2026-11-02T00:00:00Z"),
        ]});
        assert_eq!(
            linked(&thread).map(|pr| pr["number"].clone()),
            Some(json!(2))
        );
        let thread = json!({"pullRequests": [
            pr(1, "merged", "2026-11-01T00:00:00Z"),
            pr(3, "merged", "2026-11-02T00:00:00Z"),
        ]});
        assert_eq!(
            linked(&thread).map(|pr| pr["number"].clone()),
            Some(json!(3))
        );
        assert!(linked(&json!({})).is_none());
    }

    #[test]
    fn a_reference_from_the_link_else_the_url() {
        let link = json!({"host": "github.com", "repository": "o/r", "number": 7, "url": "x"});
        assert_eq!(
            reference("p", Some(&link), "x"),
            Some(json!({"projectId": "p", "host": "github.com", "repository": "o/r", "number": 7}))
        );
        assert_eq!(
            reference("p", None, "https://github.com/o/r/pull/12"),
            Some(json!({"projectId": "p", "repository": "o/r", "number": 12}))
        );
        assert_eq!(
            reference("p", None, "https://github.com/o/r/issues/12"),
            None
        );
        assert_eq!(reference("p", None, "nope"), None);
    }

    #[test]
    fn share_of_a_pane_in_a_split() {
        let layout = json!({"rect": [0, 0, 1, 1], "split": {
            "before": {"rect": [0, 0, 0.55, 1], "split": {
                "before": {"rect": [0, 0, 0.55, 0.86], "block_id": "chat"},
                "after": {"rect": [0, 0.86, 0.55, 0.14], "block_id": "reply"}}},
            "after": {"rect": [0.55, 0, 0.45, 1], "block_id": "drawer"}}});
        assert_eq!(share_of(&layout, "reply"), Some(0.55));
        assert_eq!(share_of(&layout, "drawer"), Some(0.45));
        assert_eq!(share_of(&layout, "gone"), None);
    }
}
