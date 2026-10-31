//! Joining Rex from another machine: `curl -fsSL http://<this Mac>:3779/join | sh` there.
//!
//! Sync listens on this Mac's tailnet addresses alone. A request counts only from a machine
//! Tailscale says belongs to the same person (`tailscale whois`). `GET /join` answers with a
//! script that asks the machine's own T3 for a pairing code, the way pairing over SSH does,
//! and posts it back. `POST /join` trades that code with T3 at the machine's tailnet
//! address, never one the request names, so a code can only pair the machine that sent it.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

use crate::auth::{self, client_label};
use crate::pair::{self, How};
use crate::store::Store;

pub const DEFAULT_PORT: u16 = 3779;

/// The port sync listens on: `T3REX_JOIN_PORT`, else 3779.
pub fn port() -> u16 {
    std::env::var("T3REX_JOIN_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(DEFAULT_PORT)
}

/// This Mac on the tailnet.
#[derive(Debug, Clone)]
struct Me {
    dns: String,
    ips: Vec<IpAddr>,
    user: Value,
}

async fn tailscale(args: &[&str]) -> Option<Value> {
    let bin = pair::tailscale_bin()?;
    let output = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::process::Command::new(bin)
            .args(args)
            .stdin(std::process::Stdio::null())
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    serde_json::from_slice(&output.stdout).ok()
}

async fn me() -> Option<Me> {
    let status = tailscale(&["status", "--json"]).await?;
    let me = &status["Self"];
    let dns = me["DNSName"].as_str()?.trim_end_matches('.').to_string();
    let ips = me["TailscaleIPs"]
        .as_array()?
        .iter()
        .filter_map(|ip| ip.as_str()?.parse().ok())
        .collect();
    let user = me["UserID"].clone();
    (!dns.is_empty() && !user.is_null()).then_some(Me { dns, ips, user })
}

/// The command another machine runs to join Rex, or `None` off a tailnet.
pub async fn command() -> Option<String> {
    let me = me().await?;
    Some(format!("curl -fsSL http://{}:{}/join | sh", me.dns, port()))
}

/// The machine at `ip` and its owner, by `tailscale whois`.
async fn whois(ip: IpAddr) -> Option<(String, Value)> {
    let who = tailscale(&["whois", "--json", &ip.to_string()]).await?;
    let name = who["Node"]["Name"]
        .as_str()?
        .trim_end_matches('.')
        .to_string();
    Some((name, who["UserProfile"]["ID"].clone()))
}

/// Listens for machines joining until the process ends. Returns at once off a tailnet, or
/// when another process has the port.
pub async fn serve(store: Store) -> Result<()> {
    let me = me().await.context("not on a tailnet")?;
    let mut bound = Vec::new();
    for ip in &me.ips {
        match TcpListener::bind(SocketAddr::new(*ip, port())).await {
            Ok(listener) => bound.push(listener),
            Err(e) => eprintln!("t3rex: can't take joins on {ip}:{}: {e}", port()),
        }
    }
    if bound.is_empty() {
        bail!("no tailnet address to take joins on");
    }
    let tasks = bound.into_iter().map(|listener| {
        let (store, me) = (store.clone(), me.clone());
        async move {
            loop {
                let Ok((stream, peer)) = listener.accept().await else {
                    continue;
                };
                let (store, me) = (store.clone(), me.clone());
                tokio::spawn(async move {
                    let _ = tokio::time::timeout(
                        Duration::from_secs(120),
                        handle(stream, peer, &store, &me),
                    )
                    .await;
                });
            }
        }
    });
    futures_util::future::join_all(tasks).await;
    Ok(())
}

struct Request {
    method: String,
    path: String,
    host: Option<String>,
    body: String,
}

const MAX_BODY: usize = 64 * 1024;

async fn read_request(stream: &mut TcpStream) -> Result<Request> {
    // Nothing is checked until the request is read, so it can't be larger than a join's.
    let mut reader = BufReader::new(stream.take((MAX_BODY + 16 * 1024) as u64));
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let mut words = line.split_whitespace();
    let (method, path) = (
        words.next().unwrap_or_default().to_string(),
        words.next().unwrap_or_default().to_string(),
    );
    let (mut host, mut length) = (None, 0usize);
    loop {
        line.clear();
        if reader.read_line(&mut line).await? == 0 || line.trim().is_empty() {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            let value = value.trim();
            if key.eq_ignore_ascii_case("host") {
                host = Some(value.to_string());
            } else if key.eq_ignore_ascii_case("content-length") {
                length = value.parse().unwrap_or(0);
            }
        }
    }
    if length > MAX_BODY {
        bail!("request too large");
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).await?;
    Ok(Request {
        method,
        path,
        host,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

async fn handle(mut stream: TcpStream, peer: SocketAddr, store: &Store, me: &Me) -> Result<()> {
    let request = read_request(&mut stream).await?;
    let (status, body) = respond(&request, peer.ip(), store, me).await;
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body.as_bytes()).await?;
    stream.shutdown().await?;
    Ok(())
}

/// A `Host` header fit to put back in a script: a name or address and a port.
fn safe_host(host: &str) -> bool {
    !host.is_empty()
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || ".-:[]".contains(c))
}

async fn respond(request: &Request, ip: IpAddr, store: &Store, me: &Me) -> (&'static str, String) {
    if request.path.split('?').next() != Some("/join") {
        return ("404 Not Found", "Nothing here.\n".into());
    }
    let Some((name, _)) = whois(ip).await.filter(|(_, owner)| *owner == me.user) else {
        return (
            "403 Forbidden",
            "Only your own machines on your tailnet can join Rex.\n".into(),
        );
    };
    let label = client_label();
    match request.method.as_str() {
        "GET" => {
            let host = request
                .host
                .clone()
                .filter(|h| safe_host(h))
                .unwrap_or_else(|| format!("{}:{}", me.dns, port()));
            (
                "200 OK",
                auth::join_script(&label, &format!("http://{host}/join")),
            )
        }
        "POST" => match join(request, ip, &name, store).await {
            Ok(joined) => ("200 OK", format!("✓ {label} follows {joined} now\n")),
            Err(e) => ("400 Bad Request", format!("✗ {e:#}\n")),
        },
        _ => ("405 Method Not Allowed", "GET or POST /join.\n".into()),
    }
}

async fn join(request: &Request, ip: IpAddr, name: &str, store: &Store) -> Result<String> {
    let minted = auth::read_minted(&request.body)?;
    if minted.environment_id.is_some() && minted.environment_id == store.registry().home {
        bail!("this is the Mac Rex runs on, which pairs on its own");
    }
    let port = minted.port.unwrap_or(3773);
    let ip = match ip {
        IpAddr::V6(v6) => format!("[{v6}]"),
        IpAddr::V4(v4) => v4.to_string(),
    };
    let origins = vec![
        format!("http://{name}:{port}"),
        format!("http://{ip}:{port}"),
    ];
    let machine = pair::pair(store, How::Joined { origins, minted }, None).await?;
    eprintln!("t3rex: {} joined", machine.name());
    Ok(machine.name().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_hosts_go_back_into_the_script() {
        assert!(safe_host("mini.example-tailnet.ts.net:3779"));
        assert!(safe_host("[fd7a:115c::1]:3779"));
        assert!(!safe_host("mini; rm -rf ~"));
        assert!(!safe_host("mini$(id)"));
        assert!(!safe_host(""));
    }

    #[test]
    fn the_join_script_posts_the_pairing_back() {
        let script = auth::join_script("Rex on Mac mini", "http://mini:3779/join");
        assert!(script.contains("'auth' 'pairing' 'create'"));
        assert!(script.contains("--data-binary @- 'http://mini:3779/join'"));
        assert!(script.contains("'--scope' 'access:write'"));
    }
}
