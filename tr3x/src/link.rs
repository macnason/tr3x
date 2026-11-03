//! Where a thread opens. The panes run on the Mac that runs T3, but a click on a link is
//! opened by the device it's made on, which can be a laptop or phone looking at the same Rex
//! session. So a thread's link is T3's web app at the address T3 serves on the tailnet, which
//! every device on it can open, else T3 Code's own `t3code://` link, which opens only where
//! T3 Code is installed. `TR3X_WEB_URL` sets the web address, and `none` keeps the app link.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use pulldown_cmark::{Event, Parser, Tag};
use serde_json::{Value, json};
use t3client::env::Env;

/// The web app's address: `TR3X_WEB_URL`, else the tailnet endpoint T3 advertises in its
/// server config, HTTPS first.
fn web_origin(config: Option<&Value>, set: Option<&str>) -> Option<String> {
    if let Some(set) = set.map(str::trim).filter(|s| !s.is_empty()) {
        return (set != "none").then(|| set.trim_end_matches('/').to_string());
    }
    let endpoints = config?["directEndpoints"].as_array()?;
    let tailnet: Vec<&str> = endpoints
        .iter()
        .filter(|e| e["kind"] == "tailnet")
        .filter_map(|e| e["httpBaseUrl"].as_str())
        .collect();
    tailnet
        .iter()
        .find(|u| u.starts_with("https://"))
        .or(tailnet.first())
        .map(|u| u.trim_end_matches('/').to_string())
}

/// The thread's link, as T3's web app routes it (`/$environmentId/$threadId`) or as T3 Code's
/// `t3code://threads/…` deep link, which the app turns into the same route.
pub fn thread(config: Option<&Value>, environment: &str, thread: &str) -> String {
    let set = std::env::var("TR3X_WEB_URL").ok();
    match web_origin(config, set.as_deref()) {
        Some(origin) => format!("{origin}/{environment}/{thread}"),
        None => format!("t3code://threads/{environment}/{thread}"),
    }
}

/// `text` as an OSC 8 link to `url`.
pub fn osc8(url: &str, text: &str) -> String {
    format!("\x1b]8;;{url}\x1b\\{text}\x1b]8;;\x1b\\")
}

/// URLs minted by the owning T3 server. Unresolved file paths stay plain text until
/// the server has granted access; a path on the host is not a URL on the phone.
#[derive(Default)]
pub struct FileLinks {
    context: String,
    entries: HashMap<String, FileEntry>,
    now: i64,
    parsed: HashMap<String, Vec<String>>,
    parsed_bytes: usize,
}

struct FileEntry {
    url: String,
    refresh_at: i64,
    expires_at: i64,
    pending: bool,
}

pub struct FileRequest {
    context: String,
    destination: String,
    origin: String,
    thread_id: String,
    path: String,
}

pub struct FileResult {
    context: String,
    destination: String,
    result: Option<(String, i64)>,
}

impl FileLinks {
    /// At most four requests run per pane. Call again after results or a periodic tick
    /// to replenish the batch and renew links before their signed URLs expire.
    pub fn requests<'a>(
        &mut self,
        texts: impl IntoIterator<Item = &'a str>,
        config: Option<&Value>,
        thread_id: &str,
        workspace_root: &str,
        now: i64,
    ) -> Vec<FileRequest> {
        self.now = now;
        let origin = web_origin(config, None).filter(|origin| remote_origin(origin));
        let context = format!(
            "{}\n{thread_id}\n{workspace_root}",
            origin.as_deref().unwrap_or_default()
        );
        if self.context != context {
            self.entries.clear();
            self.context = context;
        }
        let Some(origin) = origin else {
            return Vec::new();
        };
        let mut destinations = Vec::new();
        for text in texts {
            destinations.extend(self.destinations(text));
        }
        let active: std::collections::HashSet<&str> =
            destinations.iter().map(String::as_str).collect();
        self.entries
            .retain(|destination, _| active.contains(destination.as_str()));
        let mut slots = 4usize.saturating_sub(self.entries.values().filter(|e| e.pending).count());
        let mut requests = Vec::new();
        for dest_url in destinations {
            let Some(path) = file_path(&dest_url, workspace_root) else {
                continue;
            };
            if slots == 0 {
                return requests;
            }
            let entry = self
                .entries
                .entry(dest_url.to_string())
                .or_insert(FileEntry {
                    url: String::new(),
                    refresh_at: 0,
                    expires_at: 0,
                    pending: false,
                });
            if entry.pending || now < entry.refresh_at {
                continue;
            }
            entry.pending = true;
            slots -= 1;
            requests.push(FileRequest {
                context: self.context.clone(),
                destination: dest_url.to_string(),
                origin: origin.clone(),
                thread_id: thread_id.to_string(),
                path,
            });
        }
        requests
    }

    fn destinations(&mut self, text: &str) -> Vec<String> {
        if let Some(destinations) = self.parsed.get(text) {
            return destinations.clone();
        }
        let destinations: Vec<String> = Parser::new(text)
            .filter_map(|event| match event {
                Event::Start(Tag::Link { dest_url, .. }) if is_file_destination(&dest_url) => {
                    Some(dest_url.to_string())
                }
                _ => None,
            })
            .collect();
        if text.len() <= 256 * 1024 {
            while self.parsed.len() >= 4096 || self.parsed_bytes + text.len() > 1024 * 1024 {
                let Some(key) = self.parsed.keys().next().cloned() else {
                    break;
                };
                self.parsed_bytes -= key.len();
                self.parsed.remove(&key);
            }
            self.parsed_bytes += text.len();
            self.parsed.insert(text.to_string(), destinations.clone());
        }
        destinations
    }

    pub fn finish(&mut self, done: FileResult, now: i64) -> bool {
        self.now = now;
        if done.context != self.context {
            return false;
        }
        let Some(entry) = self.entries.get_mut(&done.destination) else {
            return false;
        };
        entry.pending = false;
        match done.result {
            Some((url, expires_at)) if expires_at > now => {
                // A fresh signature for an already-live link needs no reprint.
                let changed = entry.url.is_empty() || entry.expires_at <= now;
                entry.url = url;
                entry.expires_at = expires_at;
                entry.refresh_at = (expires_at - 60_000).max(now + 1000);
                changed
            }
            _ => {
                entry.refresh_at = now + 60_000;
                if entry.expires_at <= now {
                    let changed = !entry.url.is_empty();
                    entry.url.clear();
                    changed
                } else {
                    false
                }
            }
        }
    }

    pub fn resolve(&self, destination: &str) -> String {
        if is_file_destination(destination) {
            self.entries
                .get(destination)
                .filter(|e| e.expires_at > self.now)
                .map(|e| e.url.clone())
                .unwrap_or_default()
        } else {
            destination.to_string()
        }
    }
}

impl FileRequest {
    pub async fn run(self, client: Arc<Env>) -> FileResult {
        let result = client
            .call(
                "assets.createUrl",
                json!({"resource": {
                    "_tag": "media-file", "disposition": "attachment", "threadId": self.thread_id, "path": self.path,
                }}),
            )
            .await
            .ok()
            .and_then(|value| {
                let relative = value["relativeUrl"].as_str()?;
                // The owning server supplies a path, never a new authority or protocol.
                if !relative.starts_with('/')
                    || relative.starts_with("//")
                    || relative.chars().any(char::is_control)
                {
                    return None;
                }
                Some((
                    format!("{}{relative}", self.origin.trim_end_matches('/')),
                    value["expiresAt"].as_i64()?,
                ))
            });
        FileResult {
            context: self.context,
            destination: self.destination,
            result,
        }
    }
}

fn remote_origin(origin: &str) -> bool {
    let Some(authority) = origin
        .strip_prefix("https://")
        .or_else(|| origin.strip_prefix("http://"))
    else {
        return false;
    };
    let host = authority
        .split('/')
        .next()
        .unwrap_or_default()
        .split(':')
        .next()
        .unwrap_or_default();
    !host.is_empty()
        && !host.contains('@')
        && host != "localhost"
        && host != "0.0.0.0"
        && !host.starts_with("127.")
        && !host.starts_with('[')
}

fn is_file_destination(destination: &str) -> bool {
    if destination.starts_with('#') || destination.starts_with('?') || destination.starts_with("//")
    {
        return false;
    }
    if destination.starts_with("file://") {
        return true;
    }
    // Colons in a trailing line/column reference are not URL schemes. `host:3000/x` is a
    // URL: a line reference ends the destination.
    let first = destination.split('/').next().unwrap_or_default();
    let bare = first.len() == destination.len();
    !first.split_once(':').is_some_and(|(_, tail)| {
        tail.is_empty() || !(bare && tail.chars().all(|c| c.is_ascii_digit() || c == ':'))
    })
}

fn file_path(destination: &str, root: &str) -> Option<String> {
    if !is_file_destination(destination) {
        return None;
    }
    let raw = destination.strip_prefix("file://").unwrap_or(destination);
    if destination.starts_with("file://") && !raw.starts_with('/') {
        return None;
    }
    let raw = raw.split(['#', '?']).next()?;
    let mut raw = raw;
    for _ in 0..2 {
        if let Some((path, line)) = raw.rsplit_once(':')
            && !line.is_empty()
            && line.chars().all(|c| c.is_ascii_digit())
        {
            raw = path;
        } else {
            break;
        }
    }
    let mut decoded = Vec::new();
    let mut bytes = raw.bytes();
    while let Some(byte) = bytes.next() {
        decoded.push(if byte == b'%' {
            let hi = (bytes.next()? as char).to_digit(16)?;
            let lo = (bytes.next()? as char).to_digit(16)?;
            (hi * 16 + lo) as u8
        } else {
            byte
        });
    }
    let decoded = String::from_utf8(decoded).ok()?;
    if decoded.is_empty() || decoded.chars().any(char::is_control) {
        return None;
    }
    let path = Path::new(&decoded);
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        if !Path::new(root).is_absolute() {
            return None;
        }
        Path::new(root).join(path)
    };
    let mut normalized = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir => {}
            other => normalized.push(other.as_os_str()),
        }
    }
    Some(normalized.to_string_lossy().into_owned())
}

/// Whether every Rex app attached to the server is on this Mac, so whoever typed a command
/// is here too. A laptop or phone attached over the tailnet makes that unknown.
pub async fn only_here() -> bool {
    let rex = crate::rex::Rex::find();
    let Ok(out) = tokio::process::Command::new(&rex.bin)
        .args(["api", "call", "client.list", "{}"])
        .stdin(std::process::Stdio::null())
        .output()
        .await
    else {
        return false;
    };
    let Ok(list) = serde_json::from_slice::<Value>(&out.stdout) else {
        return false;
    };
    all_local(&list)
}

fn all_local(list: &Value) -> bool {
    list["clients"].as_array().is_some_and(|clients| {
        clients
            .iter()
            .filter(|c| c["info"]["kind"] == "app")
            .all(|c| c["principal"]["transport"] == "unix")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_web_app_is_on_the_tailnet_https_first() {
        let config = json!({"directEndpoints": [
            {"kind": "lan", "httpBaseUrl": "http://192.168.1.2:3773/"},
            {"kind": "tailnet", "httpBaseUrl": "http://100.1.2.3:3773/"},
            {"kind": "tailnet", "httpBaseUrl": "https://mini.example.ts.net/"},
        ]});
        assert_eq!(
            web_origin(Some(&config), None).as_deref(),
            Some("https://mini.example.ts.net")
        );
        assert_eq!(
            web_origin(Some(&config), Some("https://t3.example/")).as_deref(),
            Some("https://t3.example")
        );
        assert_eq!(web_origin(Some(&config), Some("none")), None);
        // No tailnet: only T3 Code's own link can open it.
        let lan = json!({"directEndpoints": [{"kind": "lan", "httpBaseUrl": "http://192.168.1.2:3773/"}]});
        assert_eq!(web_origin(Some(&lan), None), None);
        assert_eq!(web_origin(None, None), None);
    }

    #[test]
    fn remote_apps_are_not_here() {
        let mini = json!({"info": {"kind": "app"}, "principal": {"transport": "unix"}});
        let cli = json!({"info": {"kind": "cli"}, "principal": {"transport": "tailscale"}});
        let air = json!({"info": {"kind": "app"}, "principal": {"transport": "tailscale"}});
        assert!(all_local(&json!({"clients": [mini, cli]})));
        assert!(!all_local(&json!({"clients": [mini, air]})));
    }

    #[test]
    fn files_resolve_relative_paths_and_remove_line_locations_from_asset_requests() {
        assert_eq!(
            file_path("docs/Report%20One.md:12:3", "/repo/worktree"),
            Some("/repo/worktree/docs/Report One.md".into())
        );
        assert_eq!(
            file_path("../README.md#L12-L15", "/repo/worktree"),
            Some("/repo/README.md".into())
        );
        assert_eq!(
            file_path("file:///tmp/a+b.rs:8", "/repo"),
            Some("/tmp/a+b.rs".into())
        );
        assert_eq!(file_path("https://example.com/a.md", "/repo"), None);
        assert_eq!(file_path("#section", "/repo"), None);
        assert_eq!(file_path("file://another-host/repo/a", "/repo"), None);
        assert_eq!(file_path("bad%0Aname.md", "/repo"), None);
    }

    #[test]
    fn host_and_port_is_a_web_link_not_a_file() {
        assert!(!is_file_destination("localhost:3000/x"));
        assert!(is_file_destination("src/main.rs:12"));
        assert!(is_file_destination("main.rs:12:3"));
    }

    #[test]
    fn asset_links_refresh_and_ignore_results_from_an_old_worktree() {
        let config = json!({"directEndpoints": [{"kind":"tailnet","httpBaseUrl":"https://mini.example.ts.net"}]});
        let mut links = FileLinks::default();
        let requests = links.requests(
            ["[report](docs/a.md:12)"],
            Some(&config),
            "thread",
            "/repo",
            1000,
        );
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(request.path, "/repo/docs/a.md");
        assert_eq!(links.resolve("docs/a.md:12"), "");
        let result = FileResult {
            context: request.context.clone(),
            destination: request.destination.clone(),
            result: Some((
                "https://mini.example.ts.net/assets/a?token=signed".into(),
                120000,
            )),
        };
        assert!(links.finish(result, 1000));
        assert_eq!(
            links.resolve("docs/a.md:12"),
            "https://mini.example.ts.net/assets/a?token=signed"
        );
        assert!(
            links
                .requests(
                    ["[report](docs/a.md:12)"],
                    Some(&config),
                    "thread",
                    "/repo",
                    59000
                )
                .is_empty()
        );
        let refresh = links.requests(
            ["[report](docs/a.md:12)"],
            Some(&config),
            "thread",
            "/repo",
            61000,
        );
        assert_eq!(refresh.len(), 1);
        // A new signature for a link that is already live needs no reprint.
        assert!(!links.finish(
            FileResult {
                context: refresh[0].context.clone(),
                destination: refresh[0].destination.clone(),
                result: Some((
                    "https://mini.example.ts.net/assets/b?token=new".into(),
                    180000
                ))
            },
            61000
        ));
        let refresh = links.requests(
            ["[report](docs/a.md:12)"],
            Some(&config),
            "thread",
            "/repo",
            121000,
        );
        links.requests(
            ["[report](docs/a.md:12)"],
            Some(&config),
            "thread",
            "/other",
            61000,
        );
        assert!(!links.finish(
            FileResult {
                context: refresh[0].context.clone(),
                destination: refresh[0].destination.clone(),
                result: Some(("https://mini.example.ts.net/old".into(), 180000))
            },
            61000
        ));
        assert_eq!(links.resolve("docs/a.md:12"), "");
        assert_eq!(links.resolve("https://example.com"), "https://example.com");
    }

    #[test]
    fn file_links_do_not_mint_against_loopback_or_without_a_tailnet_endpoint() {
        let config =
            json!({"directEndpoints": [{"kind":"tailnet","httpBaseUrl":"http://127.0.0.1:3773"}]});
        let mut links = FileLinks::default();
        assert!(
            links
                .requests(["[x](a.md)"], Some(&config), "thread", "/repo", 1000)
                .is_empty()
        );
        assert_eq!(links.resolve("a.md"), "");
    }
}
