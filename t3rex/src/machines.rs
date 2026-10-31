//! The machines t3rex follows: pairing one, listing them, naming them, and whether the
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

/// The `sessions` setting in `~/.config/t3rex/settings.json`.
pub fn sessions() -> Sessions {
    let setting = crate::width::settings().and_then(|s| s.get("sessions").cloned());
    match setting.as_ref().and_then(Value::as_str) {
        Some("per-machine") => Sessions::PerMachine,
        _ => Sessions::Merged,
    }
}

/// `t3rex sessions [merged|per-machine]`.
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

/// How to pair a machine SSH doesn't reach, for someone reading rather than typing.
pub async fn join_hint(name: &str) -> String {
    match t3client::join::command().await {
        Some(join) => format!("run `{join}` on {name}"),
        None => format!(
            "`t3rex pair {} '<link>'` with a link from T3 there",
            quoted(name)
        ),
    }
}

/// Pairing a machine SSH doesn't reach: the join command to run there, or a pasted link.
/// Returns the link to pair with, or `None` once the machine joined or was skipped.
async fn ask(hub: &Hub, name: &str, id: &str, interactive: bool) -> Result<Option<How>> {
    if !interactive {
        bail!("To pair {name}, {}", join_hint(name).await);
    }
    let paired_at = || hub.store.credential(id).map(|c| c.issued_at);
    let before = paired_at();
    let answer = match t3client::join::command().await {
        Some(join) => {
            println!("Run on {name}:  {join}");
            prompt("Then press Enter, or paste a pairing link from T3 there:")
        }
        None => prompt(&format!(
            "Paste a pairing link from T3 on {name} (Settings → Connections), or Enter to skip:"
        )),
    };
    match answer.filter(|a| !a.is_empty()) {
        Some(link) => Ok(Some(How::Link(link))),
        None if paired_at().is_some() && paired_at() != before => {
            println!("✓ Paired with {name}");
            Ok(None)
        }
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

/// `t3rex pair [machine|link] [link]`.
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
            _ => match ask(&hub, machine.name(), &machine.id, interactive).await? {
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
            || (target.len() >= 4 && c.id.starts_with(&target))
    });
    match found {
        Some(c) => {
            let how = match (&c.ssh, &link) {
                (_, Some(link)) => How::Link(link.clone()),
                (Some(host), None) => How::Ssh(host.clone()),
                (None, None) => match ask(&hub, &c.label, &c.id, interactive).await? {
                    Some(how) => how,
                    None => return Ok(()),
                },
            };
            pair_with(&hub, how, Some(&c.id)).await?;
        }
        // Anything else is an SSH host.
        None => {
            let how = match link {
                Some(link) => How::Link(link),
                None => How::Ssh(target),
            };
            pair_with(&hub, how, None).await?;
        }
    }
    Ok(())
}

/// `t3rex pair` alone: the machines on the tailnet that run T3 and aren't paired, each paired
/// over SSH with one confirm where SSH reaches it, or with a pasted link.
async fn pair_all(hub: &Hub, interactive: bool) -> Result<()> {
    println!("Looking for T3 on your other machines…");
    let home = hub.home();
    let candidates = pair::candidates(home.as_deref(), &hub.registry).await;
    if candidates.is_empty() {
        println!("No other machine on your tailnet runs T3 that Rex isn't paired with.");
        println!("To pair one anyway: `t3rex pair '<link>'` with a link from T3 there.");
        return Ok(());
    }
    if !interactive {
        for c in &candidates {
            let how = match &c.ssh {
                Some(host) => format!("`t3rex pair {host}` (over SSH)"),
                None => join_hint(&c.label).await,
            };
            println!("{}  {}  {how}", c.label, c.origin);
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
    let how = match &c.ssh {
        Some(host) if yes(&format!("Pair with {} over SSH ({host})?", c.label)) => {
            How::Ssh(host.clone())
        }
        Some(_) => return,
        None => match ask(hub, &c.label, &c.id, true).await {
            Ok(Some(how)) => how,
            _ => return,
        },
    };
    if let Err(e) = pair_with(hub, how, Some(&c.id)).await {
        println!("✗ {}: {e:#}", c.label);
    }
}

/// `t3rex envs`: every machine, whether it answers, and how it was paired.
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
        };
        let state = match checked {
            Ok(Ok(found)) => format!("✓ T3 {}", found.server_version),
            Ok(Err(e)) if t3client::env::is_pairing_error(&e) => {
                format!("✗ needs pairing: `t3rex pair {}`", quoted(env.name()))
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
        let how = match &c.ssh {
            Some(host) => format!("`t3rex pair {host}`"),
            None => join_hint(&c.label).await,
        };
        println!("{:width$}  not paired        {how}", c.label, width = width);
    }
    Ok(())
}

/// `t3rex env rename <machine> <alias>`; an empty alias goes back to T3's name.
pub async fn rename(target: &str, alias: &str) -> Result<()> {
    let hub = crate::t3::hub();
    let machine = hub.registry.find(target).cloned().with_context(|| {
        format!("No paired machine is called {target:?}. `t3rex envs` lists them.")
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

/// `t3rex env forget <machine>`: unpairs it, revoking t3rex's session there when it can.
pub async fn forget(target: &str) -> Result<()> {
    let hub = crate::t3::hub();
    let machine = hub.registry.find(target).cloned().with_context(|| {
        format!("No paired machine is called {target:?}. `t3rex envs` lists them.")
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
