//! A dev server an agent starts on another machine says `http://localhost:5173`, which on this
//! Mac is nothing. So while a thread on a machine tr3x reaches over SSH is open, each localhost
//! port its latest run names in a reply is forwarded to the same port here (`ssh -L`), to
//! whichever of `127.0.0.1` and `::1` answers there, once it answers as a web server. The link
//! then opens as written on this Mac, and Rex's own ⌘-click forwarding carries it on to a
//! laptop or phone looking at the session. A port already taken here is left alone, and so are
//! the ports databases and Docker listen on. The forwards close with the pane.

use std::collections::HashMap;
use std::net::{Ipv4Addr, TcpListener};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::Value;
use t3client::env::Env;
use t3client::projection::ThreadState;
use t3client::store::Via;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;

/// Services that listen on localhost and aren't for a browser: Postgres, MySQL, Redis,
/// MongoDB, Elasticsearch, memcached, RabbitMQ, Consul, etcd, Docker's API, the kubelet.
const NOT_WEB: [u16; 15] = [
    5432, 3306, 6379, 27017, 9200, 9300, 11211, 5672, 15672, 8500, 2379, 2375, 2376, 4243, 10250,
];

struct Hold {
    task: JoinHandle<()>,
    /// It stopped trying: the port is taken here, or nothing there answered in time.
    gave_up: Arc<AtomicBool>,
}

pub struct Forwards {
    /// The SSH host the thread's machine answers on. None for this machine and machines
    /// reached another way, which get no forwards.
    ssh: Option<String>,
    held: HashMap<u16, Hold>,
}

impl Drop for Forwards {
    fn drop(&mut self) {
        for hold in self.held.values() {
            hold.task.abort();
        }
    }
}

impl Forwards {
    /// Whether any port is forwarded, or being tried: starting the pane again would stop it.
    pub fn holding(&self) -> bool {
        !self.held.is_empty()
    }

    pub fn of(env: &Env) -> Forwards {
        let machine = env.machine();
        let ssh = (machine.via == Via::Ssh)
            .then(|| machine.ssh.clone())
            .flatten()
            .filter(|h| !h.is_empty());
        Forwards {
            ssh,
            held: HashMap::new(),
        }
    }

    /// Forwards the ports the thread's latest run names that aren't yet. One whose `ssh` ended,
    /// such as when the connection dropped, is tried again; one it gave up on isn't.
    pub fn follow(&mut self, state: &ThreadState) {
        let Some(host) = self.ssh.clone() else {
            return;
        };
        for port in ports_in_latest_run(state) {
            let again = match self.held.get(&port) {
                None => true,
                Some(h) => h.task.is_finished() && !h.gave_up.load(Ordering::Relaxed),
            };
            if again {
                let gave_up = Arc::new(AtomicBool::new(false));
                let task = tokio::spawn(hold(host.clone(), port, gave_up.clone()));
                self.held.insert(port, Hold { task, gave_up });
            }
        }
    }
}

/// Where a dev server on the other machine listens: `localhost` can be either.
const TARGETS: [&str; 2] = ["127.0.0.1", "[::1]"];
/// A server the agent names before it's up gets this many tries, `RETRY` apart.
const TRIES: u32 = 12;
const RETRY: Duration = Duration::from_secs(10);

/// `ssh "$@"`, which also ends when tr3x does, however it ends: the shell holds tr3x's pipe
/// and stops `ssh` once the pipe closes. A background job's stdin is `/dev/null`, so the pipe
/// goes to it as fd 3.
const WATCH: &str =
    "exec 3<&0; ssh \"$@\" </dev/null & p=$!; (read _ <&3; kill $p 2>/dev/null) & wait $p";

/// Holds `127.0.0.1:PORT` here on the other machine's `PORT`, for as long as `ssh` runs, while
/// the port is free here. Each try forwards to one address there and checks that it answers.
async fn hold(host: String, port: u16, gave_up: Arc<AtomicBool>) {
    for _ in 0..TRIES {
        for target in TARGETS {
            if TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_err() {
                gave_up.store(true, Ordering::Relaxed);
                return;
            }
            let Ok(mut ssh) = Command::new("/bin/sh")
                .args(["-c", WATCH, "sh", "-N"])
                .args(["-o", "BatchMode=yes", "-o", "ExitOnForwardFailure=yes"])
                .args(["-o", "ServerAliveInterval=30", "-o", "ConnectTimeout=8"])
                .arg("-L")
                .arg(format!("127.0.0.1:{port}:{target}:{port}"))
                .args(["--", &host])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()
            else {
                gave_up.store(true, Ordering::Relaxed);
                return;
            };
            // `wait` closes the child's stdin, which is the pipe that keeps `ssh` going.
            let pipe = ssh.stdin.take();
            if answers(&mut ssh, port).await {
                let _ = ssh.wait().await;
                return;
            }
            // Closing the pipe stops `ssh`, which lets the port go a moment later.
            drop(pipe);
            let _ = ssh.kill().await;
            for _ in 0..20 {
                if TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_ok() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        tokio::time::sleep(RETRY).await;
    }
    gave_up.store(true, Ordering::Relaxed);
}

/// Whether the forward reaches a web server: an HTTP request through it gets an HTTP answer,
/// or a TLS one from a server on https. Through a forward with nothing behind it, ssh accepts
/// the connection here and then drops it.
async fn answers(ssh: &mut Child, port: u16) -> bool {
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(250)).await;
        if ssh.try_wait().ok().flatten().is_some() {
            return false;
        }
        let Ok(mut stream) = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).await else {
            continue;
        };
        let request = format!("HEAD / HTTP/1.0\r\nHost: localhost:{port}\r\n\r\n");
        if stream.write_all(request.as_bytes()).await.is_err() {
            return false;
        }
        let mut head = [0u8; 5];
        let read = tokio::time::timeout(Duration::from_secs(3), stream.read(&mut head)).await;
        return matches!(read, Ok(Ok(n)) if n > 0 && is_web(&head[..n]));
    }
    false
}

/// The start of an HTTP response, or a TLS record (an alert or a handshake).
fn is_web(head: &[u8]) -> bool {
    b"HTTP/".starts_with(head) && head.len() >= 4 || matches!(head.first(), Some(0x15 | 0x16))
}

/// The localhost ports the agent's replies in the latest run link to.
fn ports_in_latest_run(state: &ThreadState) -> Vec<u16> {
    let Some(run) = state
        .latest_run()
        .and_then(|r| r.get("id"))
        .and_then(Value::as_str)
    else {
        return Vec::new();
    };
    let mut ports = Vec::new();
    for item in state.items() {
        if item.get("runId").and_then(Value::as_str) != Some(run)
            || item.get("type").and_then(Value::as_str) != Some("assistant_message")
        {
            continue;
        }
        for port in ports_in(item.get("text").and_then(Value::as_str).unwrap_or("")) {
            if !ports.contains(&port) {
                ports.push(port);
            }
        }
    }
    ports
}

/// Ports of `http(s)://localhost:PORT` links in `text`, and the same for `127.0.0.1`,
/// `0.0.0.0` and `[::1]`. Below 1024 is a system's, not a dev server's.
fn ports_in(text: &str) -> Vec<u16> {
    const HOSTS: [&str; 4] = ["localhost:", "127.0.0.1:", "0.0.0.0:", "[::1]:"];
    let mut ports = Vec::new();
    for scheme in ["http://", "https://"] {
        for (at, _) in text.match_indices(scheme) {
            let rest = &text[at + scheme.len()..];
            let Some(after) = HOSTS.iter().find_map(|h| rest.strip_prefix(h)) else {
                continue;
            };
            let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
            let next = after[digits.len()..].chars().next();
            if next.is_some_and(|c| c.is_ascii_alphanumeric()) {
                continue;
            }
            if let Ok(port) = digits.parse::<u16>()
                && port >= 1024
                && !NOT_WEB.contains(&port)
                && !ports.contains(&port)
            {
                ports.push(port);
            }
        }
    }
    ports
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn finds_localhost_ports_in_links() {
        let text = "Running at http://localhost:5173/ and https://127.0.0.1:8443/app, \
                    API on http://0.0.0.0:3000. Not http://localhost:80, http://localhost:5432, \
                    http://localhost:5173x, http://example.com:9000 or http://[::1]:abc";
        assert_eq!(ports_in(text), vec![5173, 3000, 8443]);
        assert_eq!(ports_in("http://[::1]:4000"), vec![4000]);
        assert!(ports_in("localhost:5173 without a scheme").is_empty());
    }

    #[test]
    fn only_the_latest_runs_replies_count() {
        let state = ThreadState::from_snapshot(&json!({"projection": {
            "thread": {"id": "t"},
            "runs": [{"id": "r1", "ordinal": 1}, {"id": "r2", "ordinal": 2}],
            "turnItems": [
                {"id": "a", "runId": "r1", "ordinal": 1, "type": "assistant_message",
                 "text": "http://localhost:3000"},
                {"id": "b", "runId": "r2", "ordinal": 2, "type": "command_execution",
                 "input": "curl http://localhost:4000"},
                {"id": "c", "runId": "r2", "ordinal": 3, "type": "assistant_message",
                 "text": "Up on http://localhost:5173/"},
            ],
        }}))
        .unwrap();
        assert_eq!(ports_in_latest_run(&state), vec![5173]);
    }

    #[test]
    fn a_web_server_answers_in_http_or_tls() {
        assert!(is_web(b"HTTP/"));
        assert!(is_web(b"HTTP"));
        assert!(is_web(&[0x15, 3, 3, 0, 2]));
        assert!(!is_web(b"E\0\0\0n"));
        assert!(!is_web(b"-ERR "));
        assert!(!is_web(b"H"));
    }

    /// `TR3X_FORWARD_HOST=devbox TR3X_FORWARD_PORT=5799 cargo test -- --ignored forward`, with
    /// a server on that port there.
    #[tokio::test]
    #[ignore]
    async fn holds_a_port_on_a_real_host() {
        let host = std::env::var("TR3X_FORWARD_HOST").unwrap();
        let port: u16 = std::env::var("TR3X_FORWARD_PORT").unwrap().parse().unwrap();
        let gave_up = Arc::new(AtomicBool::new(false));
        let task = tokio::spawn(hold(host, port, gave_up.clone()));
        tokio::time::sleep(Duration::from_secs(5)).await;
        assert!(
            !task.is_finished(),
            "gave up: {}",
            gave_up.load(Ordering::Relaxed)
        );
        assert!(
            TcpStream::connect((Ipv4Addr::LOCALHOST, port))
                .await
                .is_ok()
        );
        task.abort();
        let _ = task.await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(
            TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_ok(),
            "freed"
        );
    }
}
