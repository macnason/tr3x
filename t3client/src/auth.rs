//! Pairing with a T3 server, the way T3's own apps do: a one-time pairing code, traded at
//! `POST /oauth/token` for a bearer token that lasts 30 days (`AuthTokenExchangeRequest` in
//! T3's packages/contracts/src/auth.ts). The code comes from the machine's own T3 CLI
//! (`t3 auth pairing create --json`), run here or over SSH as T3's desktop app does for SSH
//! machines, or from a pairing link the user pastes. A code is used once and never kept.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::descriptor::{Local, normalize_origin};
use crate::error::{err, err_exit, exit};
use crate::http::{Api, direct_client};
use crate::store::Credential;

/// What t3rex asks a machine for: reading and running threads, listing its linked machines,
/// reading project files for icons, and branches.
pub const SCOPES: &[&str] = &[
    "orchestration:read",
    "orchestration:operate",
    "access:read",
    "filesystem:read",
    "source-control:write",
];

const GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:token-exchange";
const SUBJECT_TOKEN_TYPE: &str = "urn:t3:params:oauth:token-type:environment-bootstrap";
const ACCESS_TOKEN_TYPE: &str = "urn:ietf:params:oauth:token-type:access_token";

/// Where a pairing link points, and its one-time code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingTarget {
    pub origin: String,
    pub code: String,
}

/// Reads every form of pairing link T3 accepts (`resolveRemotePairingTarget` in T3's
/// packages/shared/src/remote.ts):
///
/// - `https://host/pair#token=…`, or `?token=…`
/// - the hosted form, `https://app/pair?host=…&token=…`
/// - `t3code://pair?pairingUrl=…`, on any of T3's app schemes
/// - a host and a bare code, separated by a space
pub fn parse_link(input: &str) -> Result<PairingTarget> {
    let input = input.trim();
    let invalid = |why: &str| err_exit("PAIRING_LINK_INVALID", exit::USAGE, why.to_string());
    if input.is_empty() {
        return Err(invalid(
            "Paste a pairing link from T3 → Settings → Connections.",
        ));
    }
    let mut words = input.split_whitespace();
    let (first, second) = (words.next().unwrap_or_default(), words.next());
    if let Some(code) = second {
        return Ok(PairingTarget {
            origin: normalize_origin(first),
            code: code.to_string(),
        });
    }
    let link = match app_link(first) {
        Some(inner) => inner,
        None => first.to_string(),
    };
    let Some((scheme, _)) = link.split_once("://") else {
        return Err(invalid(
            "That isn't a pairing link. Paste the whole link, or a host and a code.",
        ));
    };
    if !["http", "https", "ws", "wss"].contains(&scheme.to_ascii_lowercase().as_str()) {
        return Err(invalid("A pairing link starts with https:// or http://."));
    }
    let params = |part: &str| -> Vec<(String, String)> {
        part.split('&')
            .filter_map(|pair| {
                let (k, v) = pair.split_once('=')?;
                Some((percent_decode(k), percent_decode(v)))
            })
            .collect()
    };
    let (before_hash, hash) = link.split_once('#').unwrap_or((&link, ""));
    let query = before_hash.split_once('?').map_or("", |(_, q)| q);
    let (hash, query) = (params(hash), params(query));
    let get = |list: &[(String, String)], key: &str| {
        list.iter()
            .find(|(k, v)| k == key && !v.trim().is_empty())
            .map(|(_, v)| v.trim().to_string())
    };
    let token = get(&hash, "token").or_else(|| get(&query, "token"));
    if let (Some(host), Some(token)) = (get(&query, "host"), token.clone()) {
        return Ok(PairingTarget {
            origin: normalize_origin(&host),
            code: token,
        });
    }
    let Some(code) = token else {
        return Err(invalid("That link has no pairing code in it."));
    };
    Ok(PairingTarget {
        origin: normalize_origin(&link),
        code,
    })
}

/// The pairing URL inside `t3code://pair?pairingUrl=…`, on any `t3code-…` scheme.
fn app_link(text: &str) -> Option<String> {
    let (scheme, rest) = text.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    let app = scheme == "t3code"
        || scheme
            .strip_prefix("t3code-")
            .is_some_and(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_lowercase()));
    if !app {
        return None;
    }
    let (host, query) = rest.split_once('?')?;
    if host.trim_end_matches('/') != "pair" {
        return None;
    }
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(k, _)| *k == "pairingUrl")
        .map(|(_, v)| percent_decode(v).trim().to_string())
        .filter(|v| !v.is_empty())
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 3;
                        continue;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            b'+' => out.push(b' '),
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn form_encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            b' ' => "+".into(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// How t3rex names itself in T3 → Connections: "Rex on Mac mini".
pub fn client_label() -> String {
    let name = std::process::Command::new("/usr/sbin/scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "this Mac".into());
    format!("Rex on {}", device_name(&name))
}

/// "Mackenzie’s Mac mini" as "Mac mini": a computer name without its owner.
fn device_name(name: &str) -> &str {
    ["’s ", "'s "]
        .iter()
        .find_map(|sep| name.split_once(sep).map(|(_, rest)| rest.trim()))
        .filter(|rest| !rest.is_empty())
        .unwrap_or(name)
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Trades a one-time pairing `code` at `origin` for a bearer token.
pub async fn exchange(origin: &str, code: &str, label: &str) -> Result<Credential> {
    let origin = normalize_origin(origin);
    let body = [
        ("grant_type", GRANT_TYPE),
        ("subject_token", code.trim()),
        ("subject_token_type", SUBJECT_TOKEN_TYPE),
        ("requested_token_type", ACCESS_TOKEN_TYPE),
        ("scope", &SCOPES.join(" ")),
        ("client_label", label),
        ("client_device_type", "desktop"),
        ("client_os", "macOS"),
    ]
    .iter()
    .map(|(k, v)| format!("{k}={}", form_encode(v)))
    .collect::<Vec<_>>()
    .join("&");
    let response = direct_client()
        .post(format!("{origin}/oauth/token"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("accept", "application/json")
        .timeout(Duration::from_secs(15))
        .body(body)
        .send()
        .await
        .map_err(|e| {
            err_exit(
                "T3_UNREACHABLE",
                exit::UNAVAILABLE,
                format!(
                    "T3 at {origin} didn't answer the pairing: {}",
                    e.without_url()
                ),
            )
        })?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    let answer: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if !status.is_success() {
        let why = answer["error_description"]
            .as_str()
            .or(answer["error"].as_str())
            .or(answer["message"].as_str())
            .unwrap_or("it refused the code");
        return Err(err_exit(
            "PAIRING_REFUSED",
            exit::REJECTED,
            format!(
                "T3 at {origin} didn't pair ({status}): {why}. Pairing codes work once, for 5 minutes."
            ),
        ));
    }
    let token = answer["access_token"]
        .as_str()
        .filter(|t| !t.is_empty())
        .ok_or_else(|| {
            err(
                "PAIRING_REFUSED",
                "T3 answered the pairing without a token.",
            )
        })?;
    if answer["token_type"]
        .as_str()
        .is_some_and(|t| !t.eq_ignore_ascii_case("bearer"))
    {
        bail!("T3 at {origin} issued a key-bound token, which t3rex can't use");
    }
    let now = now_ms();
    let lasts = answer["expires_in"].as_i64().unwrap_or(30 * 24 * 3600);
    Ok(Credential {
        token: token.to_string(),
        scopes: answer["scope"]
            .as_str()
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_string)
            .collect(),
        issued_at: now,
        expires_at: now + lasts * 1000,
    })
}

/// What `/api/auth/session` says about a token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Checked {
    /// The server no longer accepts it: expired or revoked.
    Rejected,
    Accepted,
}

pub async fn check_session(api: &Api) -> Result<Checked> {
    let state = api.get("/api/auth/session").await?;
    match state.get("authenticated").and_then(Value::as_bool) {
        Some(true) => Ok(Checked::Accepted),
        Some(false) => Ok(Checked::Rejected),
        None => Err(err(
            "T3_API_ERROR",
            format!(
                "T3 at {} answered /api/auth/session without saying whether it accepts t3rex.",
                api.origin
            ),
        )),
    }
}

/// How to run this machine's `t3`.
#[derive(Debug, Clone)]
pub struct T3Command {
    pub program: PathBuf,
    pub args_prefix: Vec<String>,
    /// Electron apps run their bundled server as plain Node only with this set.
    pub electron_as_node: bool,
}

const SERVER_ENTRY_SUFFIX: &str = "apps/server/dist/bin.mjs";

impl T3Command {
    fn command(&self) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(&self.program);
        command.args(&self.args_prefix);
        if self.electron_as_node {
            command.env("ELECTRON_RUN_AS_NODE", "1");
        }
        command
    }

    /// This machine's `t3`: `T3REX_T3_COMMAND`, else the running server's own, else `t3` on
    /// the PATH, else the newest runtime T3 installed, else the T3 Code app's.
    pub fn local(local: &Local) -> Result<T3Command> {
        if let Ok(configured) = std::env::var("T3REX_T3_COMMAND") {
            let mut parts = configured.split_whitespace().map(str::to_string);
            let program = parts.next().context("T3REX_T3_COMMAND is empty")?;
            return Ok(T3Command {
                program: program.into(),
                args_prefix: parts.collect(),
                electron_as_node: false,
            });
        }
        if let Some(found) = local.pid.and_then(server_process_command) {
            return Ok(found);
        }
        let path = std::env::var_os("PATH").unwrap_or_default();
        if let Some(t3) = std::env::split_paths(&path)
            .map(|dir| dir.join("t3"))
            .find(|p| p.is_file())
        {
            return Ok(T3Command {
                program: t3,
                args_prefix: Vec::new(),
                electron_as_node: false,
            });
        }
        let versions = crate::descriptor::t3_home().join("runtime/versions");
        let mut runtimes: Vec<PathBuf> = std::fs::read_dir(&versions)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path().join("t3"))
            .filter(|p| p.is_file())
            .collect();
        runtimes.sort();
        if let Some(t3) = runtimes.pop() {
            return Ok(T3Command {
                program: t3,
                args_prefix: Vec::new(),
                electron_as_node: false,
            });
        }
        let apps = std::fs::read_dir("/Applications")
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("T3 Code") && n.ends_with(".app"))
            });
        for app in apps {
            if let Some(found) = app_command(&app) {
                return Ok(found);
            }
        }
        Err(err_exit(
            "T3_CLI_MISSING",
            exit::UNAVAILABLE,
            "Couldn't find T3 Code's `t3` on this Mac. Open T3 Code once, or set T3REX_T3_COMMAND.",
        ))
    }
}

fn app_command(app: &Path) -> Option<T3Command> {
    let name = app.file_stem()?.to_str()?;
    let program = app.join("Contents/MacOS").join(name);
    let entry = app
        .join("Contents/Resources/app.asar")
        .join(SERVER_ENTRY_SUFFIX);
    program.is_file().then(|| T3Command {
        program,
        args_prefix: vec![entry.to_string_lossy().into_owned()],
        electron_as_node: true,
    })
}

fn server_process_command(pid: u32) -> Option<T3Command> {
    let output = std::process::Command::new("/bin/ps")
        .args(["-o", "command=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    parse_ps_command(String::from_utf8_lossy(&output.stdout).trim())
}

/// `ps` joins argv with spaces, and app paths contain spaces, so anchor on the `.app` bundle.
fn parse_ps_command(line: &str) -> Option<T3Command> {
    if let Some(index) = line.find(".app/Contents/MacOS/") {
        return app_command(Path::new(&line[..index + 4]));
    }
    let mut tokens = line.split_whitespace();
    let program = tokens.next()?;
    let entry = tokens.find(|token| token.ends_with("bin.mjs"))?;
    Some(T3Command {
        program: program.into(),
        args_prefix: vec![entry.to_string()],
        electron_as_node: false,
    })
}

/// The flags of `t3 auth pairing create` for a code t3rex trades at once.
fn pairing_args(label: &str) -> Vec<String> {
    let mut args: Vec<String> = ["auth", "pairing", "create", "--json", "--ttl", "5m"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    args.push("--label".into());
    args.push(label.into());
    for scope in SCOPES {
        args.push("--scope".into());
        args.push(scope.to_string());
    }
    args
}

/// The code in `t3 auth pairing create --json`'s answer.
fn read_pairing(text: &str) -> Result<String> {
    let start = text.find('{').context("T3's CLI printed no pairing code")?;
    let end = text
        .rfind('}')
        .context("T3's CLI printed no pairing code")?;
    let value: Value = serde_json::from_str(&text[start..=end])
        .context("T3's CLI printed something other than a pairing code")?;
    value["credential"]
        .as_str()
        .filter(|c| !c.is_empty())
        .map(str::to_string)
        .context("T3's CLI printed no pairing code")
}

/// A pairing code from this machine's own T3.
pub async fn local_code(local: &Local, label: &str) -> Result<String> {
    let t3 = T3Command::local(local)?;
    let mut command = t3.command();
    command
        .args(pairing_args(label))
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(60), command.output())
        .await
        .context("T3's CLI took over a minute to make a pairing code")?
        .with_context(|| format!("couldn't run {}", t3.program.display()))?;
    if !output.status.success() {
        bail!(
            "T3's CLI couldn't make a pairing code: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    read_pairing(&String::from_utf8_lossy(&output.stdout))
}

/// What pairing over SSH learns about the machine.
#[derive(Debug, Clone)]
pub struct SshPairing {
    pub environment_id: Option<String>,
    pub port: Option<u16>,
    pub code: String,
    /// Where `ssh` reaches the host, from `ssh -G`.
    pub hostname: Option<String>,
}

/// The script run on an SSH machine: its environment id, its server's port, and a pairing
/// code from its `t3`, found as T3's desktop app looks for it.
fn ssh_script(label: &str) -> String {
    let args: Vec<String> = pairing_args(label)
        .iter()
        .map(|a| format!("'{}'", a.replace('\'', r"'\''")))
        .collect();
    format!(
        r#"set -u
H="${{T3CODE_HOME:-$HOME/.t3}}"
T3="$(command -v t3 2>/dev/null || true)"
[ -n "$T3" ] || [ ! -x "$HOME/.local/bin/t3" ] || T3="$HOME/.local/bin/t3"
[ -n "$T3" ] || T3="$(ls -1d "$H"/runtime/versions/*/t3 2>/dev/null | sort | tail -n 1)"
[ -n "$T3" ] || {{ echo "t3rex: no t3 on this machine" >&2; exit 3; }}
printf 'ENV %s\n' "$(cat "$H/userdata/environment-id" 2>/dev/null)"
printf 'RUNTIME %s\n' "$(tr -d '\n' < "$H/userdata/server-runtime.json" 2>/dev/null)"
printf 'PAIRING %s\n' "$("$T3" {args} | tr -d '\n')"
"#,
        args = args.join(" ")
    )
}

/// A pairing code from `host`'s T3, over SSH. Batch mode: it never prompts, so a host that
/// wants a password or a new host key fails at once with SSH's own words.
pub async fn ssh_code(host: &str, label: &str) -> Result<SshPairing> {
    if host.starts_with('-') || host.chars().any(|c| c.is_whitespace() || c.is_control()) {
        bail!("{host:?} isn't an SSH host");
    }
    let output = tokio::time::timeout(
        Duration::from_secs(60),
        tokio::process::Command::new("ssh")
            .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=8", "--", host])
            .arg(ssh_script(label))
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("SSH took over a minute")?
    .context("couldn't run ssh")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "couldn't pair over SSH with {host}: {}",
            stderr.trim().lines().last().unwrap_or("ssh failed")
        );
    }
    let line = |tag: &str| {
        stdout
            .lines()
            .find_map(|l| l.strip_prefix(tag))
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
    };
    let port = line("RUNTIME ")
        .and_then(|r| serde_json::from_str::<Value>(&r).ok())
        .and_then(|r| r["port"].as_u64())
        .and_then(|p| u16::try_from(p).ok());
    Ok(SshPairing {
        environment_id: line("ENV "),
        port,
        code: read_pairing(&line("PAIRING ").unwrap_or_default())?,
        hostname: ssh_hostname(host).await,
    })
}

/// The host name `ssh` connects to for `host`, after its config.
pub async fn ssh_hostname(host: &str) -> Option<String> {
    let output = tokio::process::Command::new("ssh")
        .args(["-G", "--", host])
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .ok()?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("hostname "))
        .map(|h| h.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_link_form_t3_accepts() {
        let want = |origin: &str, code: &str| PairingTarget {
            origin: origin.into(),
            code: code.into(),
        };
        assert_eq!(
            parse_link("https://mini.example.ts.net/pair#token=ABC123").unwrap(),
            want("https://mini.example.ts.net", "ABC123")
        );
        assert_eq!(
            parse_link("http://macbook-air-2:3773/pair?token=ABC").unwrap(),
            want("http://macbook-air-2:3773", "ABC")
        );
        assert_eq!(
            parse_link("https://app.t3.codes/pair?host=http%3A%2F%2Fair%3A3773&token=T&label=Air")
                .unwrap(),
            want("http://air:3773", "T")
        );
        let inner = "https%3A%2F%2Fair.example%2Fpair%23token%3DXYZ";
        assert_eq!(
            parse_link(&format!("t3code://pair?pairingUrl={inner}")).unwrap(),
            want("https://air.example", "XYZ")
        );
        assert_eq!(
            parse_link(&format!("t3code-nightly://pair?pairingUrl={inner}")).unwrap(),
            want("https://air.example", "XYZ")
        );
        assert_eq!(
            parse_link("  air.example:3773   CODE9 ").unwrap(),
            want("https://air.example:3773", "CODE9")
        );
        assert!(parse_link("https://air.example/pair").is_err());
        assert!(parse_link("ftp://air/pair#token=x").is_err());
        assert!(parse_link("").is_err());
    }

    #[test]
    fn the_client_names_the_device_not_its_owner() {
        assert_eq!(device_name("Mackenzie’s Mac mini"), "Mac mini");
        assert_eq!(device_name("Kim's MacBook Air"), "MacBook Air");
        assert_eq!(device_name("studio"), "studio");
    }

    #[test]
    fn pairing_answers_are_read_past_log_lines() {
        let text = "warn: something\n{\n  \"id\": \"p1\",\n  \"credential\": \"CODE\",\n  \"expiresAt\": \"x\"\n}\n";
        assert_eq!(read_pairing(text).unwrap(), "CODE");
        assert!(read_pairing("nothing").is_err());
    }

    #[test]
    fn app_paths_with_spaces_come_back_whole() {
        let line = "/Applications/T3 Code (Nightly).app/Contents/MacOS/T3 Code (Nightly) --flag";
        let found = parse_ps_command(line);
        // The test machine may not have the app; when it does, the entry is inside it.
        if let Some(t3) = found {
            assert!(t3.electron_as_node);
            assert!(t3.args_prefix[0].ends_with("app.asar/apps/server/dist/bin.mjs"));
        }
        let node = parse_ps_command("/usr/bin/node /opt/t3/bin.mjs serve").unwrap();
        assert_eq!(node.args_prefix, vec!["/opt/t3/bin.mjs".to_string()]);
    }

    #[test]
    fn the_ssh_script_quotes_its_arguments() {
        let script = ssh_script("Rex on Kim's Mac");
        assert!(script.contains(r"'Rex on Kim'\''s Mac'"));
        assert!(script.contains("'--scope' 'orchestration:read'"));
    }

    #[tokio::test]
    async fn the_exchange_sends_t3s_form_and_reads_the_token() {
        let server = crate::http::fake::Server::start(|path| {
            assert_eq!(path, "/oauth/token");
            crate::http::fake::response(
                "200 OK",
                &["content-type: application/json"],
                r#"{"access_token":"tok","issued_token_type":"urn:ietf:params:oauth:token-type:access_token","token_type":"Bearer","expires_in":2592000,"scope":"orchestration:read orchestration:operate"}"#,
            )
        });
        let credential = exchange(&server.origin, "CODE", "Rex on Mac mini")
            .await
            .unwrap();
        assert_eq!(credential.token, "tok");
        assert_eq!(credential.scopes.len(), 2);
        assert!(credential.expires_at - credential.issued_at >= 2_592_000_000);
        let body = &server.bodies()[0];
        assert!(
            body.contains("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Atoken-exchange")
        );
        assert!(body.contains("subject_token=CODE"));
        assert!(body.contains("client_label=Rex+on+Mac+mini"));
        assert!(body.contains("scope=orchestration%3Aread+orchestration%3Aoperate"));
    }

    #[tokio::test]
    async fn a_refused_code_says_so() {
        let server = crate::http::fake::Server::start(|_| {
            crate::http::fake::response(
                "400 Bad Request",
                &["content-type: application/json"],
                r#"{"error":"invalid_grant"}"#,
            )
        });
        let error = exchange(&server.origin, "OLD", "Rex").await.unwrap_err();
        assert_eq!(crate::error::code_of(&error), Some("PAIRING_REFUSED"));
        assert!(format!("{error}").contains("invalid_grant"));
    }
}
