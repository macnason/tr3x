//! What a T3 server says about itself at `/.well-known/t3/environment`, and what this
//! machine's own T3 records about its server on disk.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use serde::Deserialize;
use serde_json::Value;

use crate::error::{err_exit, exit};
use crate::http::direct_client;

pub const PROTOCOL_VERSION: u64 = 2;
pub const PROTOCOL_HEADER: &str = "x-t3-orchestration-protocol";
pub const PROTOCOL_QUERY_PARAM: &str = "orchestrationProtocol";

/// The largest `server-runtime.json` or `environment-id` read. T3 writes well under 300
/// bytes to each.
const MAX_RECORD_BYTES: u64 = 16 * 1024;

/// The longest descriptor body read. T3 sends about 2 KB: an id, a label, the platform, the
/// version and some 60 capability flags.
const MAX_DESCRIPTOR_BYTES: usize = 256 * 1024;

/// A server's environment descriptor (`ExecutionEnvironmentDescriptor` in T3's
/// packages/contracts/src/environment.ts).
#[derive(Debug, Clone, PartialEq)]
pub struct Descriptor {
    pub origin: String,
    pub environment_id: String,
    pub label: Option<String>,
    pub server_version: String,
    pub protocol_version: Option<u64>,
    /// Feature flags, such as `threadSettlement`. Unknown keys are kept.
    pub capabilities: Value,
    pub platform: Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Wire {
    environment_id: String,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    server_version: String,
    #[serde(default)]
    orchestration_protocol_version: Option<Value>,
    #[serde(default)]
    capabilities: Value,
    #[serde(default)]
    platform: Value,
}

impl Descriptor {
    pub fn protocol_supported(&self) -> bool {
        self.protocol_version == Some(PROTOCOL_VERSION)
    }

    pub fn has_capability(&self, name: &str) -> bool {
        self.capabilities[name] == true
    }

    /// The machine's name as T3 shows it.
    pub fn name(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.environment_id)
    }

    pub fn require_supported_protocol(&self) -> Result<()> {
        if self.protocol_supported() {
            return Ok(());
        }
        let message = match self.protocol_version {
            Some(v) if v > PROTOCOL_VERSION => format!(
                "T3 {} on {} speaks orchestration protocol {v}, which this build doesn't know yet. Update tr3x.",
                self.server_version,
                self.name()
            ),
            _ => format!(
                "T3 {} on {} speaks an older orchestration protocol. Update T3 Code there.",
                self.server_version,
                self.name()
            ),
        };
        Err(err_exit("T3_PROTOCOL_UNSUPPORTED", exit::REJECTED, message))
    }

    /// Stops unless the server is the machine `expected` names, so a token never goes to
    /// another machine that took over the address.
    pub fn require_environment(&self, expected: &str) -> Result<()> {
        if self.environment_id == expected {
            return Ok(());
        }
        Err(err_exit(
            "ENVIRONMENT_CHANGED",
            exit::REJECTED,
            format!(
                "The T3 at {} is {} ({}), not the machine tr3x paired with ({expected}).",
                self.origin,
                self.name(),
                self.environment_id
            ),
        ))
    }
}

/// `origin` without a trailing slash, path, query or fragment, and with `ws` read as `http`.
pub fn normalize_origin(origin: &str) -> String {
    let origin = origin.trim();
    let origin = origin
        .strip_prefix("ws://")
        .map(|rest| format!("http://{rest}"))
        .or_else(|| {
            origin
                .strip_prefix("wss://")
                .map(|rest| format!("https://{rest}"))
        })
        .unwrap_or_else(|| origin.to_string());
    let (scheme, rest) = origin.split_once("://").unwrap_or(("https", &origin));
    let host = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .to_string();
    format!("{}://{host}", scheme.to_ascii_lowercase())
}

/// What the server at `origin` says about itself. Two and a half seconds for the whole answer.
pub async fn fetch(origin: &str) -> Result<Descriptor> {
    fetch_within(origin, Duration::from_millis(2500)).await
}

pub async fn fetch_within(origin: &str, timeout: Duration) -> Result<Descriptor> {
    let origin = normalize_origin(origin);
    let unavailable = |why: String| {
        err_exit(
            "T3_UNREACHABLE",
            exit::UNAVAILABLE,
            format!("T3 at {origin} {why}"),
        )
    };
    let mut response = direct_client()
        .get(format!("{origin}/.well-known/t3/environment"))
        .timeout(timeout)
        .send()
        .await
        .map_err(|e| unavailable(format!("didn't answer: {}", e.without_url())))?;
    let status = response.status();
    if !status.is_success() {
        return Err(unavailable(format!(
            "answered HTTP {status} instead of describing itself"
        )));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_DESCRIPTOR_BYTES as u64)
    {
        return Err(unavailable("sent a descriptor that's too long".into()));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| unavailable("stopped answering partway".into()))?
    {
        if body.len() + chunk.len() > MAX_DESCRIPTOR_BYTES {
            return Err(unavailable("sent a descriptor that's too long".into()));
        }
        body.extend_from_slice(&chunk);
    }
    let wire: Wire = serde_json::from_slice(&body)
        .map_err(|_| unavailable("sent something other than a descriptor".into()))?;
    Ok(Descriptor {
        origin,
        environment_id: wire.environment_id,
        label: wire.label,
        server_version: wire.server_version,
        protocol_version: wire.orchestration_protocol_version.and_then(|v| v.as_u64()),
        capabilities: wire.capabilities,
        platform: wire.platform,
    })
}

pub fn t3_home() -> PathBuf {
    if let Some(home) = std::env::var_os("T3CODE_HOME") {
        return PathBuf::from(home);
    }
    home_dir().join(".t3")
}

pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// The bytes of a small regular file, or `None` when there is none or it isn't one.
fn read_small_file(path: &Path) -> Option<Vec<u8>> {
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_RECORD_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(MAX_RECORD_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= MAX_RECORD_BYTES).then_some(bytes)
}

/// What this machine's T3 keeps about its server: where it serves and as which machine.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Local {
    /// The `userdata` directory, where T3 keeps attachments.
    pub state_dir: PathBuf,
    pub environment_id: Option<String>,
    /// The loopback origin the running server listens on, while one runs.
    pub origin: Option<String>,
    pub pid: Option<u32>,
}

#[derive(Deserialize)]
struct RuntimeRecord {
    version: u64,
    origin: String,
    #[serde(default)]
    pid: Option<u32>,
}

impl Local {
    /// Reads `server-runtime.json` and `environment-id` from T3's `userdata`.
    pub fn read() -> Local {
        Local::read_at(&t3_home().join("userdata"))
    }

    pub fn read_at(state_dir: &Path) -> Local {
        let record = read_small_file(&state_dir.join("server-runtime.json"))
            .and_then(|bytes| serde_json::from_slice::<RuntimeRecord>(&bytes).ok())
            .filter(|r| r.version == 1);
        let environment_id = read_small_file(&state_dir.join("environment-id"))
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .map(|id| id.trim().to_string())
            .filter(|id| !id.is_empty());
        Local {
            state_dir: state_dir.to_path_buf(),
            environment_id,
            origin: record.as_ref().map(|r| normalize_origin(&r.origin)),
            pid: record.and_then(|r| r.pid),
        }
    }

    /// Where to reach the local server: what it recorded, else T3's default port.
    pub fn origin_or_default(&self) -> String {
        self.origin
            .clone()
            .unwrap_or_else(|| "http://127.0.0.1:3773".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins_lose_their_path_and_ws_scheme() {
        assert_eq!(
            normalize_origin("ws://mac.example.ts.net:3773/ws?x=1"),
            "http://mac.example.ts.net:3773"
        );
        assert_eq!(
            normalize_origin("https://mini.example/pair#token=abc"),
            "https://mini.example"
        );
        assert_eq!(normalize_origin("HTTP://a:1/"), "http://a:1");
        assert_eq!(normalize_origin("host.example"), "https://host.example");
    }

    #[test]
    fn the_local_record_says_where_and_who() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("environment-id"), "env-1\n").unwrap();
        std::fs::write(
            dir.path().join("server-runtime.json"),
            r#"{"version":1,"pid":42,"origin":"http://127.0.0.1:3999","host":"0.0.0.0"}"#,
        )
        .unwrap();
        let local = Local::read_at(dir.path());
        assert_eq!(local.environment_id.as_deref(), Some("env-1"));
        assert_eq!(local.origin.as_deref(), Some("http://127.0.0.1:3999"));
        assert_eq!(local.pid, Some(42));
        let empty = Local::read_at(&dir.path().join("missing"));
        assert_eq!(empty.origin_or_default(), "http://127.0.0.1:3773");
    }

    #[tokio::test]
    async fn a_descriptor_is_read_and_checked() {
        let server = crate::http::fake::Server::start(|path| {
            assert_eq!(path, "/.well-known/t3/environment");
            crate::http::fake::response(
                "200 OK",
                &["content-type: application/json"],
                r#"{"environmentId":"env-a","label":"mac","serverVersion":"1.0","orchestrationProtocolVersion":2,"capabilities":{"threadSnooze":true}}"#,
            )
        });
        let d = fetch(&server.origin).await.unwrap();
        assert_eq!(d.environment_id, "env-a");
        assert!(d.has_capability("threadSnooze") && d.protocol_supported());
        assert!(d.require_environment("env-a").is_ok());
        let error = d.require_environment("env-b").unwrap_err();
        assert!(format!("{error}").contains("not the machine"));
    }
}
