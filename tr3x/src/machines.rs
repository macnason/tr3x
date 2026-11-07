//! The machines tr3x follows: pairing one, listing them, naming them, and whether the
//! T3 Code session holds them all or there's a session for each.

use std::io::{BufRead, IsTerminal, Write};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use t3client::hub::Hub;
use t3client::pair::{self, Candidate, Forgot, How};
use t3client::store::{Machine, Via};

/// How the T3 Code session is split across machines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sessions {
    /// One session, every machine's threads in T3's order.
    Merged,
    /// A session for each machine.
    PerMachine,
}

/// The `sessions` setting in `~/.config/tr3x/settings.json`.
pub fn sessions() -> Sessions {
    let setting = crate::width::settings().and_then(|s| s.get("sessions").cloned());
    match setting.as_ref().and_then(Value::as_str) {
        Some("per-machine") => Sessions::PerMachine,
        _ => Sessions::Merged,
    }
}

/// `tr3x sessions [merged|per-machine]`.
pub fn set_sessions(arg: Option<&str>) -> Result<String> {
    let name = |s: Sessions| match s {
        Sessions::Merged => "merged: one T3 Code session for every machine",
        Sessions::PerMachine => "per machine: a T3 Code session for each",
    };
    let Some(arg) = arg.map(str::trim).filter(|a| !a.is_empty()) else {
        return Ok(format!("Sessions: {}", name(sessions())));
    };
    let chosen = match arg {
        "merged" | "one" | "all" => Sessions::Merged,
        "per-machine" | "machine" | "machines" | "each" => Sessions::PerMachine,
        other => bail!("Can't read {other:?}: merged or per-machine"),
    };
    crate::width::set_setting(
        "sessions",
        (chosen == Sessions::PerMachine).then(|| json!("per-machine")),
    )?;
    Ok(format!("Sessions: {}", name(chosen)))
}

fn prompt(question: &str) -> Option<String> {
    print!("{question} ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).ok()?;
    Some(line.trim().to_string())
}

fn yes(question: &str) -> bool {
    prompt(&format!("{question} [Y/n]")).is_some_and(|a| {
        a.is_empty() || a.eq_ignore_ascii_case("y") || a.eq_ignore_ascii_case("yes")
    })
}

fn looks_like_link(text: &str) -> bool {
    text.contains("://") || text.split_whitespace().count() == 2
}

fn quoted(name: &str) -> String {
    if name.contains(char::is_whitespace) {
        format!("\"{name}\"")
    } else {
        name.to_string()
    }
}

/// How to pair a machine neither Rex nor SSH reaches, for someone reading rather than typing.
pub fn link_hint(name: &str) -> String {
    format!(
        "`tr3x pair {} '<link>'` with a link from T3 there (Settings → Connections), or add it as a Rex host",
        quoted(name)
    )
}

/// Pairing a machine neither Rex nor SSH reaches: a pasted link. Returns how to pair, or `None`
/// once it was skipped.
async fn ask(name: &str, interactive: bool) -> Result<Option<How>> {
    if !interactive {
        bail!("To pair {name}, {}", link_hint(name));
    }
    let answer = prompt(&format!(
        "Paste a pairing link from T3 on {name} (Settings → Connections), or Enter to skip:"
    ));
    match answer.filter(|a| !a.is_empty()) {
        Some(link) => Ok(Some(How::Link(link))),
        None => {
            println!("Skipped {name}");
            Ok(None)
        }
    }
}

async fn pair_with(hub: &Hub, how: How, expect: Option<&str>) -> Result<Machine> {
    let machine = pair::pair(&hub.store, how, expect).await?;
    println!(
        "✓ Paired with {} (T3 {})",
        machine.name(),
        if machine.version.is_empty() {
            "?"
        } else {
            &machine.version
        }
    );
    Ok(machine)
}

/// `tr3x pair [machine|link] [link]`.
pub async fn pair_command(target: Option<String>, link: Option<String>) -> Result<()> {
    let hub = crate::t3::hub();
    let interactive = std::io::stdin().is_terminal();
    let Some(target) = target.filter(|t| !t.trim().is_empty()) else {
        return pair_all(&hub, interactive).await;
    };
    if link.is_none() && looks_like_link(&target) {
        pair_with(&hub, How::Link(target), None).await?;
        return Ok(());
    }
    if let Some(machine) = hub.registry.find(&target).cloned() {
        let how = match (machine.via, &machine.ssh, &link) {
            (_, _, Some(link)) => How::Link(link.clone()),
            (Via::Local, _, _) => How::Local,
            (Via::Ssh, Some(host), _) => How::Ssh(host.clone()),
            (Via::Rex, _, _) if machine.rex.is_some() => {
                How::Rex(machine.rex.clone().unwrap_or_default())
            }
            _ => match ask(machine.name(), interactive).await? {
                Some(how) => how,
                None => return Ok(()),
            },
        };
        pair_with(&hub, how, Some(&machine.id)).await?;
        return Ok(());
    }
    let home = hub.home();
    let candidates = pair::candidates(home.as_deref(), &hub.registry).await;
    let found = candidates.iter().find(|c| {
        c.label.eq_ignore_ascii_case(&target)
            || c.ssh.as_deref() == Some(target.as_str())
            || c.rex
                .as_deref()
                .is_some_and(|r| r.eq_ignore_ascii_case(&target))
            || (target.len() >= 4 && c.id.starts_with(&target))
    });
    match found {
        Some(c) => {
            let how = match (way(c), &link) {
                (_, Some(link)) => How::Link(link.clone()),
                (Some(how), None) => how,
                (None, None) => match ask(&c.label, interactive).await? {
                    Some(how) => how,
                    None => return Ok(()),
                },
            };
            pair_with(&hub, how, Some(&c.id)).await?;
        }
        // Anything else is a Rex host when Rex knows it by that label, else an SSH host.
        None => {
            let how = match link {
                Some(link) => How::Link(link),
                None if t3client::auth::rex_reaches(&target).await => How::Rex(target),
                None => How::Ssh(target),
            };
            pair_with(&hub, how, None).await?;
        }
    }
    Ok(())
}

/// The way to pair a machine found on the tailnet without asking for a link: through its Rex
/// host, else over SSH.
fn way(c: &Candidate) -> Option<How> {
    c.rex
        .clone()
        .map(How::Rex)
        .or_else(|| c.ssh.clone().map(How::Ssh))
}

/// How pairing `c` goes, in a few words.
pub fn way_hint(c: &Candidate) -> String {
    match (&c.rex, &c.ssh) {
        (Some(host), _) => format!("`tr3x pair {}` (through Rex)", quoted(host)),
        (None, Some(host)) => format!("`tr3x pair {host}` (over SSH)"),
        (None, None) => link_hint(&c.label),
    }
}

/// `tr3x pair` alone: the machines on the tailnet that run T3 and aren't paired, each paired
/// over SSH with one confirm where SSH reaches it, or with a pasted link.
async fn pair_all(hub: &Hub, interactive: bool) -> Result<()> {
    println!("Looking for T3 on your other machines…");
    let home = hub.home();
    let candidates = pair::candidates(home.as_deref(), &hub.registry).await;
    if candidates.is_empty() {
        println!("No other machine on your tailnet runs T3 that Rex isn't paired with.");
        println!("To pair one anyway: `tr3x pair '<link>'` with a link from T3 there.");
        return Ok(());
    }
    if !interactive {
        for c in &candidates {
            println!("{}  {}  {}", c.label, c.origin, way_hint(c));
        }
        return Ok(());
    }
    for c in candidates {
        offer(hub, &c).await;
    }
    Ok(())
}

/// Offers to pair one machine found on the tailnet.
pub async fn offer(hub: &Hub, c: &Candidate) {
    let how = match way(c) {
        Some(how) => {
            let through = match &how {
                How::Rex(host) => format!("through Rex ({host})"),
                How::Ssh(host) => format!("over SSH ({host})"),
                _ => String::new(),
            };
            if !yes(&format!("Pair with {} {through}?", c.label)) {
                return;
            }
            how
        }
        None => match ask(&c.label, true).await {
            Ok(Some(how)) => how,
            _ => return,
        },
    };
    if let Err(e) = pair_with(hub, how, Some(&c.id)).await {
        println!("✗ {}: {e:#}", c.label);
    }
}

/// `tr3x envs`: every machine, whether it answers, and how it was paired.
pub async fn list() -> Result<()> {
    let hub = crate::t3::hub();
    let checks = hub.envs().iter().map(|env| {
        let env = env.clone();
        async move {
            let checked = tokio::time::timeout(Duration::from_secs(8), env.check()).await;
            (env, checked)
        }
    });
    let results = futures_util::future::join_all(checks).await;
    let width = results
        .iter()
        .map(|(env, _)| env.name().chars().count())
        .max()
        .unwrap_or(0);
    for (env, checked) in results {
        let m = env.machine();
        let how = match m.via {
            Via::Local => "this Mac".to_string(),
            Via::Ssh => format!("over SSH ({})", m.ssh.as_deref().unwrap_or("?")),
            Via::Link => "by link".to_string(),
            Via::Join => "joined".to_string(),
            Via::Rex => format!("through Rex ({})", m.rex.as_deref().unwrap_or("?")),
        };
        let state = match checked {
            Ok(Ok(found)) => format!("✓ T3 {}", found.server_version),
            Ok(Err(e)) if t3client::env::is_pairing_error(&e) => {
                format!("✗ needs pairing: `tr3x pair {}`", quoted(env.name()))
            }
            Ok(Err(e)) => format!("✗ {e:#}"),
            Err(_) => "✗ didn't answer".to_string(),
        };
        let label = if m.alias.is_some() && !m.label.is_empty() {
            format!("  ({})", m.label)
        } else {
            String::new()
        };
        println!(
            "{:width$}  {how:16}  {state}{label}",
            env.name(),
            width = width
        );
    }
    let home = hub.home();
    let candidates = pair::candidates(home.as_deref(), &hub.registry).await;
    for c in candidates {
        println!(
            "{:width$}  not paired        {}",
            c.label,
            way_hint(&c),
            width = width
        );
    }
    Ok(())
}

/// `tr3x env rename <machine> <alias>`; an empty alias goes back to T3's name.
pub async fn rename(target: &str, alias: &str) -> Result<()> {
    let hub = crate::t3::hub();
    let machine = hub.registry.find(target).cloned().with_context(|| {
        format!("No paired machine is called {target:?}. `tr3x envs` lists them.")
    })?;
    let alias = alias.trim();
    let alias = (!alias.is_empty()).then(|| crate::sync::label(alias));
    hub.store
        .update_registry(|r| {
            if let Some(m) = r.machines.iter_mut().find(|m| m.id == machine.id) {
                m.alias = alias.clone();
            } else {
                // This Mac before its first pairing.
                let mut m = machine.clone();
                m.alias = alias.clone();
                r.machines.push(m);
                r.home = r.home.clone().or(Some(machine.id.clone()));
            }
        })
        .await?;
    match alias {
        Some(a) => println!("{} is now {a} in Rex", machine.name()),
        None => println!(
            "{} goes by T3's name again",
            if machine.label.is_empty() {
                &machine.id
            } else {
                &machine.label
            }
        ),
    }
    Ok(())
}

/// `tr3x env forget <machine>`: unpairs it, revoking tr3x's session there when it can.
pub async fn forget(target: &str) -> Result<()> {
    let hub = crate::t3::hub();
    let machine = hub.registry.find(target).cloned().with_context(|| {
        format!("No paired machine is called {target:?}. `tr3x envs` lists them.")
    })?;
    match pair::forget(&hub.store, &machine).await? {
        Forgot::Revoked => println!("Forgot {}, and removed Rex from its T3", machine.name()),
        Forgot::Kept => println!(
            "Forgot {}. Remove Rex from T3 → Settings → Connections there too.",
            machine.name()
        ),
    }
    Ok(())
}
