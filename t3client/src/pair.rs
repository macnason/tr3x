//! Pairing a machine, and finding the machines on the tailnet that run T3 and aren't paired
//! yet.

use std::collections::HashSet;
use std::time::Duration;

use anyhow::{Result, bail};
use serde_json::Value;

use crate::auth::{self, client_label, exchange, parse_link};
use crate::descriptor::{self, Descriptor, Local, normalize_origin};
use crate::env::Env;
use crate::store::{Machine, Registry, Store, Via};

/// Where a pairing code comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum How {
    /// This machine's own T3 CLI.
    Local,
    /// The T3 CLI on an SSH host.
    Ssh(String),
    /// A pairing link the user pasted, in any form T3 accepts.
    Link(String),
}

/// Pairs with a machine and saves it. Returns the machine as saved.
pub async fn pair(store: &Store, how: How, expect: Option<&str>) -> Result<Machine> {
    let _lock = store.lock().await?;
    pair_locked(store, how, expect).await
}

/// Pairs `machine` again the way it was paired. Callers hold the store's lock.
pub(crate) async fn again_locked(store: &Store, machine: &Machine) -> Result<Machine> {
    let how = match (machine.via, &machine.ssh) {
        (Via::Local, _) => How::Local,
        (Via::Ssh, Some(host)) => How::Ssh(host.clone()),
        _ => return Err(crate::env::needs_pairing(machine)),
    };
    pair_locked(store, how, Some(&machine.id)).await
}

/// The first of `origins` that answers, as `expect` when that's given.
async fn reach(origins: &[String], expect: Option<&str>) -> Result<Descriptor> {
    let mut last = None;
    for origin in origins {
        match descriptor::fetch(origin).await {
            Ok(found) => match expect {
                Some(id) if found.environment_id != id => {
                    last = Some(found.require_environment(id).unwrap_err());
                }
                _ => return Ok(found),
            },
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| anyhow::anyhow!("no address to reach T3 at")))
}

async fn pair_locked(store: &Store, how: How, expect: Option<&str>) -> Result<Machine> {
    let label = client_label();
    let (origins, code, known_id, via, ssh) = match &how {
        How::Local => {
            let local = Local::read();
            let code = auth::local_code(&local, &label).await?;
            (
                vec![local.origin_or_default()],
                code,
                local.environment_id.clone(),
                Via::Local,
                None,
            )
        }
        How::Ssh(host) => {
            let found = auth::ssh_code(host, &label).await?;
            let port = found.port.unwrap_or(3773);
            let mut origins: Vec<String> = found
                .hostname
                .iter()
                .map(|h| format!("http://{}:{port}", h.trim_end_matches('.')))
                .collect();
            for origin in tailnet_origins(port).await {
                if !origins.contains(&origin) {
                    origins.push(origin);
                }
            }
            (
                origins,
                found.code,
                found.environment_id,
                Via::Ssh,
                Some(host.clone()),
            )
        }
        How::Link(text) => {
            let target = parse_link(text)?;
            (vec![target.origin], target.code, None, Via::Link, None)
        }
    };
    if let (Some(expect), Some(known)) = (expect, known_id.as_deref())
        && expect != known
    {
        bail!("that machine is {known}, not {expect}");
    }
    let expect = expect.map(str::to_string).or(known_id);
    // A tailnet with many machines: only the ones that answer as this machine are kept.
    let found = reach(&origins, expect.as_deref()).await?;
    found.require_supported_protocol()?;
    let credential = exchange(&found.origin, &code, &label).await?;
    store.save_credential(&found.environment_id, credential)?;
    let mut registry = store.registry();
    let alias = ssh
        .as_deref()
        .filter(|host| short_name(host) && !host.eq_ignore_ascii_case(found.name()))
        .map(str::to_string);
    let mut origins = vec![found.origin.clone()];
    if via == Via::Local {
        // This machine's server can move ports; Env asks it where it listens each time.
        origins.clear();
    }
    registry.upsert(Machine {
        id: found.environment_id.clone(),
        label: found.label.clone().unwrap_or_default(),
        alias,
        origins,
        ssh,
        via,
        version: found.server_version.clone(),
    });
    if via == Via::Local {
        registry.home = Some(found.environment_id.clone());
    }
    store.save_registry(&registry)?;
    Ok(registry
        .get(&found.environment_id)
        .cloned()
        .expect("just saved"))
}

/// A name a person would type, like `omarchy`, rather than an address.
fn short_name(host: &str) -> bool {
    !host.contains('.') && !host.contains(':') && !host.contains('@')
}

/// A machine on the tailnet that runs T3 and isn't paired yet.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub id: String,
    /// T3's name for it.
    pub label: String,
    pub origin: String,
    /// An SSH host from `~/.ssh/config` that reaches it, for pairing without a link.
    pub ssh: Option<String>,
}

fn tailscale_bin() -> Option<String> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .map(|d| d.join("tailscale"))
        .chain(
            [
                "/opt/homebrew/bin/tailscale",
                "/usr/local/bin/tailscale",
                "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
            ]
            .map(Into::into),
        )
        .find(|p| p.is_file())
        .map(|p| p.to_string_lossy().into_owned())
}

/// The tailnet's other online computers, as `tailscale status --json` lists them: each one's
/// DNS name and addresses.
async fn tailnet_peers() -> Vec<(String, Vec<String>)> {
    let Some(bin) = tailscale_bin() else {
        return Vec::new();
    };
    let Ok(Ok(output)) = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::process::Command::new(bin)
            .args(["status", "--json"])
            .stdin(std::process::Stdio::null())
            .output(),
    )
    .await
    else {
        return Vec::new();
    };
    let Ok(status) = serde_json::from_slice::<Value>(&output.stdout) else {
        return Vec::new();
    };
    status["Peer"]
        .as_object()
        .into_iter()
        .flat_map(|peers| peers.values())
        .filter(|p| p["Online"] == true)
        .filter(|p| !matches!(p["OS"].as_str(), Some("iOS" | "android")))
        .filter_map(|p| {
            let name = p["DNSName"].as_str()?.trim_end_matches('.').to_string();
            let ips = p["TailscaleIPs"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|ip| ip.as_str().map(str::to_string))
                .collect();
            Some((name, ips))
        })
        .collect()
}

async fn tailnet_origins(port: u16) -> Vec<String> {
    tailnet_peers()
        .await
        .into_iter()
        .map(|(name, _)| format!("http://{name}:{port}"))
        .collect()
}

/// The hosts in `~/.ssh/config` and where each reaches, by `ssh -G`.
async fn ssh_hosts() -> Vec<(String, String)> {
    let config = descriptor::home_dir().join(".ssh/config");
    let text = std::fs::read_to_string(config).unwrap_or_default();
    let aliases: Vec<String> = text
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            let rest = l
                .strip_prefix("Host ")
                .or_else(|| l.strip_prefix("Host\t"))?;
            Some(
                rest.split_whitespace()
                    .map(str::to_string)
                    .collect::<Vec<_>>(),
            )
        })
        .flatten()
        .filter(|a| !a.contains(['*', '?', '!']))
        .take(40)
        .collect();
    let mut out = Vec::new();
    for alias in aliases {
        if let Some(host) = auth::ssh_hostname(&alias).await {
            out.push((alias, host.trim_end_matches('.').to_ascii_lowercase()));
        }
    }
    out
}

/// The machines on the tailnet and among the home machine's linked environments that run T3
/// and aren't in `registry` yet, each with an SSH host that reaches it when there is one.
pub async fn candidates(home: Option<&Env>, registry: &Registry) -> Vec<Candidate> {
    let mut origins: Vec<String> = tailnet_peers()
        .await
        .into_iter()
        .map(|(name, _)| format!("http://{name}:3773"))
        .collect();
    if let Some(home) = home
        && let Ok(links) = home.call("peerLinks.list", serde_json::json!({})).await
    {
        let list = links
            .as_array()
            .or(links["links"].as_array())
            .or(links["peerLinks"].as_array())
            .cloned()
            .unwrap_or_default();
        for link in list {
            for url in link["urls"].as_array().into_iter().flatten() {
                if let Some(url) = url.as_str() {
                    let origin = normalize_origin(url);
                    if !origins.contains(&origin) {
                        origins.push(origin);
                    }
                }
            }
        }
    }
    let probes = origins.into_iter().map(|origin| async move {
        descriptor::fetch_within(&origin, Duration::from_millis(1500))
            .await
            .ok()
    });
    let found: Vec<Descriptor> = futures_util::future::join_all(probes)
        .await
        .into_iter()
        .flatten()
        .collect();
    let mut seen: HashSet<String> = registry.machines.iter().map(|m| m.id.clone()).collect();
    seen.extend(registry.home.clone());
    let hosts = ssh_hosts().await;
    let mut out = Vec::new();
    for d in found {
        if !seen.insert(d.environment_id.clone()) {
            continue;
        }
        let host = d
            .origin
            .split("://")
            .nth(1)
            .and_then(|h| h.split(':').next())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let ssh = hosts
            .iter()
            .find(|(_, reaches)| *reaches == host)
            .map(|(alias, _)| alias.clone());
        out.push(Candidate {
            id: d.environment_id.clone(),
            label: d.name().to_string(),
            origin: d.origin.clone(),
            ssh,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_names_become_aliases() {
        assert!(short_name("omarchy"));
        assert!(!short_name("mac.example-tailnet.ts.net"));
        assert!(!short_name("mac@host"));
        assert!(!short_name("100.97.217.14"));
    }

    /// A pasted link pairs with the machine its address answers as, and saves the token
    /// where every t3rex process finds it.
    #[tokio::test]
    async fn a_link_pairs_and_saves_the_machine() {
        let server = crate::http::fake::Server::start(|path| match path {
            "/.well-known/t3/environment" => crate::http::fake::response(
                "200 OK",
                &[],
                r#"{"environmentId":"env-air","label":"MacBook Air (2)","serverVersion":"1","orchestrationProtocolVersion":2,"capabilities":{}}"#,
            ),
            "/oauth/token" => crate::http::fake::response(
                "200 OK",
                &[],
                r#"{"access_token":"tok","token_type":"Bearer","expires_in":60,"scope":"orchestration:read"}"#,
            ),
            other => panic!("unexpected {other}"),
        });
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        let link = format!("{}/pair#token=CODE", server.origin);
        let machine = pair(&store, How::Link(link.clone()), None).await.unwrap();
        assert_eq!(machine.id, "env-air");
        assert_eq!(machine.via, Via::Link);
        assert_eq!(machine.origins, vec![server.origin.clone()]);
        assert_eq!(store.credential("env-air").unwrap().token, "tok");
        assert_eq!(
            store.registry().get("env-air").unwrap().label,
            "MacBook Air (2)"
        );
        // A link for one machine doesn't pair another.
        let error = pair(&store, How::Link(link), Some("env-other"))
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("not the machine"),
            "{error:#}"
        );
    }
}
