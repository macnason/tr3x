//! Talks to Rex through its CLI, the interface Rex documents and keeps stable. Every change to
//! the T3 session runs as one Lua script per `rex do`, so blocks are created and disowned on
//! the same connection. A block the creating client still owns reads "Sized by another client"
//! in the app and keeps its first size.

use std::path::PathBuf;
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use serde_json::Value;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

#[cfg(target_os = "macos")]
const BUNDLED: &str =
    "/Applications/Rex Beta.app/Contents/Helpers/Rex Server.rex/Contents/MacOS/rex";
/// On Linux, Rex's package puts `rex` on the `PATH`.
#[cfg(not(target_os = "macos"))]
const BUNDLED: &str = "/usr/bin/rex";

/// Never start a Rex server of our own: one that has stopped is Rex quitting, and sync waits
/// for the app's to come back.
pub const NO_AUTOSTART: [&str; 1] = ["--autostart=false"];

#[derive(Clone, Debug)]
pub struct Rex {
    pub bin: PathBuf,
}

impl Rex {
    /// `$TR3X_REX`, else Rex's own copy (`$REX_BIN_DIR`, set in Rex terminals), else `rex` on
    /// `PATH`, else the copy in the app bundle.
    pub fn find() -> Rex {
        let from_env = std::env::var_os("TR3X_REX").map(PathBuf::from);
        let from_rex = std::env::var_os("REX_BIN_DIR").map(|dir| PathBuf::from(dir).join("rex"));
        let on_path = std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|dir| dir.join("rex"))
                .find(|p| p.is_file())
        });
        let bin = from_env
            .or(from_rex.filter(|p| p.is_file()))
            .or(on_path)
            .unwrap_or_else(|| PathBuf::from(BUNDLED));
        Rex { bin }
    }

    async fn run(&self, args: &[&str], stdin: Option<&str>) -> Result<String> {
        let mut command = Command::new(&self.bin);
        command
            .args(NO_AUTOSTART)
            .args(args)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .with_context(|| format!("couldn't run {}", self.bin.display()))?;
        let out = tokio::time::timeout(std::time::Duration::from_secs(15), async {
            if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
                pipe.write_all(text.as_bytes()).await?;
            }
            child.wait_with_output().await
        })
        .await
        .with_context(|| {
            format!(
                "rex {} timed out after 15 seconds",
                args.first().unwrap_or(&"")
            )
        })??;
        if !out.status.success() {
            bail!(
                "rex {} failed: {}",
                args.first().unwrap_or(&""),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    async fn json(&self, args: &[&str], stdin: Option<&str>) -> Result<Value> {
        let text = self.run(args, stdin).await?;
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text).with_context(|| format!("rex printed {text:?}"))
    }

    /// The version of this `rex` and of the server it reaches, as `{rex_version, api_revision}`.
    /// They differ when Rex updated the app but its server kept running.
    pub async fn versions(&self) -> Result<(Value, Value)> {
        let cli = self.json(&["version", "--json"], None).await?;
        let server = self.call("server.version", "{}").await?;
        Ok((cli, server))
    }

    /// Every session, in Rex's order, as `{session_id, label}`.
    pub async fn sessions(&self) -> Result<Vec<Value>> {
        let list = self.json(&["ls", "--json"], None).await?;
        Ok(list["sessions"].as_array().cloned().unwrap_or_default())
    }

    /// The session's windows and blocks, as `session inspect --json` prints them.
    pub async fn inspect(&self, session: &str) -> Result<Value> {
        self.json(&["session", "inspect", session, "--json"], None)
            .await
    }

    pub async fn create_session(&self, label: &str) -> Result<String> {
        let args = serde_json::json!({ "label": label }).to_string();
        let made = self
            .json(&["api", "call", "session.create", &args], None)
            .await?;
        made["session_id"]
            .as_str()
            .map(str::to_string)
            .context("session.create returned no id")
    }

    pub async fn move_session(&self, session: &str, index: usize) -> Result<()> {
        self.run(
            &["session", "move", session, "--index", &index.to_string()],
            None,
        )
        .await
        .map(drop)
    }

    /// Calls one of the server's API methods with `args`, a JSON object.
    pub async fn call(&self, method: &str, args: &str) -> Result<Value> {
        self.json(&["api", "call", method, args], None).await
    }

    /// Calls one of a block's own methods, such as a terminal's `size`.
    pub async fn block_method(&self, session: &str, block: &str, method: &str) -> Result<Value> {
        self.json(&["block", "call", "-s", session, "-b", block, method], None)
            .await
    }

    /// Runs a Lua script against `session` and returns what it returned.
    pub async fn lua(&self, session: &str, script: &str) -> Result<Value> {
        self.json(&["do", "-s", session, "-"], Some(script)).await
    }

    /// Shows a layer just opened with `focus = true` on the phones looking at `session`. A Mac
    /// follows the session's focus; Rex's iPhone app shows each pane as a tab of its own and
    /// keeps the tab it's on, so a layer opened there would land in a tab nobody goes to.
    /// Selecting the window takes the phone to its focused block, the layer's.
    pub async fn show_on_phones(&self, session: &str) -> Result<()> {
        let view = self.inspect(session).await?;
        let Some(window) = view["active_window_id"].as_str() else {
            return Ok(());
        };
        let phones = view["clients"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|c| {
                c["info"]["kind"] == "app"
                    && c["info"]["platform"] == "ios"
                    && c["connection_state"] != "lost"
            });
        for phone in phones {
            let Some(id) = phone["client_id"].as_str() else {
                continue;
            };
            let session = format!("session_id={session}");
            let window = format!("window_id={window}");
            let _ = self
                .run(&["-C", id, "do", "session.select", &session, &window], None)
                .await;
        }
        Ok(())
    }

    /// Asks the app clients to do one of their actions, such as `session.select`. A missing
    /// client is not an error: nothing is looking.
    pub async fn client_do(&self, action: &str, pairs: &[(&str, &str)]) -> Result<()> {
        let clients = self.json(&["client", "ls", "--json"], None).await?;
        let apps = clients["clients"].as_array().cloned().unwrap_or_default();
        for app in apps.iter().filter(|c| c["info"]["kind"] == "app") {
            let Some(id) = app["client_id"].as_str() else {
                continue;
            };
            let mut args = vec!["-C", id, "do", action];
            let kv: Vec<String> = pairs.iter().map(|(k, v)| format!("{k}={v}")).collect();
            args.extend(kv.iter().map(String::as_str));
            let _ = self.run(&args, None).await;
        }
        Ok(())
    }

    /// The Rex apps for Mac attached to this server, which update themselves through
    /// `client.update.*`: each one's client id and its label, else its version.
    pub async fn mac_apps(&self) -> Result<Vec<MacApp>> {
        let clients = self.json(&["client", "ls", "--json"], None).await?;
        Ok(mac_apps_in(&clients))
    }

    /// The client that has `block`, and whether it's connected from this machine.
    pub async fn owner(&self, block: &str) -> Result<Option<(String, bool)>> {
        let inspect = self
            .json(&["block", "inspect", block, "--json"], None)
            .await?;
        let Some(owner) = inspect["block"]["owner"].as_str() else {
            return Ok(None);
        };
        let clients = self.json(&["client", "ls", "--json"], None).await?;
        Ok(owner_in(&clients, owner).map(|here| (owner.to_string(), here)))
    }

    /// One of a client's own actions, such as `client.update.status`, and what it returns.
    pub async fn app_action(&self, client: &str, action: &str) -> Result<Value> {
        self.json(&["-C", client, "do", action, "--json"], None)
            .await
            .map_err(|e| {
                let said = format!("{e:#}");
                if said.to_lowercase().contains("remote control") {
                    anyhow::anyhow!("turn on Remote Control in Rex's settings there")
                } else {
                    e
                }
            })
    }
}

/// Whether `owner`, among `clients`, is connected from this machine.
fn owner_in(clients: &Value, owner: &str) -> Option<bool> {
    clients["clients"]
        .as_array()?
        .iter()
        .find(|c| c["client_id"] == owner)
        .map(|c| c["principal"]["transport"] == "unix")
}

#[derive(Clone, Debug, PartialEq)]
pub struct MacApp {
    pub id: String,
    /// The app's label, such as `Mini`, else its version.
    pub name: String,
}

fn mac_apps_in(clients: &Value) -> Vec<MacApp> {
    clients["clients"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| {
            c["info"]["kind"] == "app"
                && c["info"]["platform"] == "macos"
                && c["connection_state"] != "lost"
        })
        .filter_map(|c| {
            let id = c["client_id"].as_str()?.to_string();
            let name = c["info"]["label"]
                .as_str()
                .or(c["info"]["version"].as_str())
                .unwrap_or("Rex")
                .to_string();
            Some(MacApp { id, name })
        })
        .collect()
}

/// What `client.update.check` or `.status` found, in a few words: `None` when it's up to date.
pub fn rex_update(found: &Value) -> Option<String> {
    let build = |v: &Value| {
        let version = v["version"].as_str()?;
        Some(match v["build"].as_str() {
            Some(b) => format!("{version} ({b})"),
            None => version.to_string(),
        })
    };
    if found["update_downloaded"] == true {
        return Some("downloaded".into());
    }
    match found["result"].as_str() {
        None | Some("up_to_date") => None,
        Some(other) => Some(
            ["update", "available", "latest"]
                .iter()
                .find_map(|k| build(&found[*k]))
                .unwrap_or_else(|| other.replace('_', " ")),
        ),
    }
}

/// The running build a `client.update` answer names, such as `0.1.0 (1853)`.
pub fn rex_build(found: &Value) -> String {
    let current = &found["current"];
    match (current["version"].as_str(), current["build"].as_str()) {
        (Some(v), Some(b)) => format!("{v} ({b})"),
        (Some(v), None) => v.to_string(),
        _ => "?".into(),
    }
}

#[cfg(test)]
mod app_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_connected_mac_apps_update() {
        let clients = json!({"clients": [
            {"client_id": "c1", "info": {"kind": "cli", "platform": "macos"}},
            {"client_id": "c2", "info": {"kind": "app", "platform": "macos", "label": "Mini", "version": "0.1.0 (1853)"}, "connection_state": "connected"},
            {"client_id": "c3", "info": {"kind": "app", "platform": "ios", "label": "Phone"}},
            {"client_id": "c4", "info": {"kind": "app", "platform": "macos", "version": "0.1.0 (1850)"}, "connection_state": "lost"},
            {"client_id": "c5", "info": {"kind": "app", "platform": "macos", "version": "0.1.0 (1856)"}},
        ]});
        let apps = mac_apps_in(&clients);
        assert_eq!(
            apps,
            vec![
                MacApp {
                    id: "c2".into(),
                    name: "Mini".into()
                },
                MacApp {
                    id: "c5".into(),
                    name: "0.1.0 (1856)".into()
                },
            ]
        );
    }

    #[test]
    fn reads_what_the_updater_found() {
        let up = json!({"current": {"build": "1853", "version": "0.1.0"}, "result": "up_to_date"});
        assert_eq!(rex_update(&up), None);
        assert_eq!(rex_build(&up), "0.1.0 (1853)");
        let found =
            json!({"result": "update_available", "update": {"version": "0.1.0", "build": "1860"}});
        assert_eq!(rex_update(&found).as_deref(), Some("0.1.0 (1860)"));
        let bare = json!({"result": "update_available"});
        assert_eq!(rex_update(&bare).as_deref(), Some("update available"));
        let status = json!({"phase": "idle", "update_downloaded": true});
        assert_eq!(rex_update(&status).as_deref(), Some("downloaded"));
        let idle = json!({"phase": "idle", "update_downloaded": false});
        assert_eq!(rex_update(&idle), None);
    }
}

/// The settings a pane needs from the process that starts it, which Rex doesn't pass on: a
/// pane starts with the Rex server's environment, so a sync or a pane with its own state
/// directory hands it on with `--set-env`.
pub const PANE_ENV: &[&str] = &[
    "TR3X_STATE_DIR",
    "T3CODE_HOME",
    "XDG_CONFIG_HOME",
    "TR3X_REX",
    "TR3X_PASSIVE",
    "TR3X_PERF",
    "TR3X_SESSION_LABEL",
];

/// `program`, then `--set-env` for each of `PANE_ENV` set here, then `args`: the command line
/// of a pane this process starts.
pub fn pane_argv(program: &str, args: &[&str]) -> Vec<String> {
    let mut argv = vec![program.to_string()];
    for key in PANE_ENV {
        if let Some(value) = std::env::var_os(key) {
            argv.push("--set-env".into());
            argv.push(format!("{key}={}", value.to_string_lossy()));
        }
    }
    argv.extend(args.iter().map(|a| a.to_string()));
    argv
}

/// A Lua string literal that holds `text` exactly. Long brackets need no escaping, so pick a
/// level the text doesn't close.
pub fn lua_str(text: &str) -> String {
    let mut level = 0;
    loop {
        let close = format!("]{}]", "=".repeat(level));
        // A `]` at the end would run into the closing bracket.
        if !format!("{text}]").contains(&close) {
            let eq = "=".repeat(level);
            // A long string drops a newline right after its opening bracket.
            return format!("[{eq}[\n{text}]{eq}]");
        }
        level += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::{lua_str, owner_in};

    #[test]
    fn owner_here_or_not() {
        let clients = serde_json::json!({ "clients": [
            { "client_id": "client:mini", "principal": { "transport": "unix" } },
            { "client_id": "client:air", "principal": { "transport": "tailscale" } },
        ]});
        assert_eq!(owner_in(&clients, "client:mini"), Some(true));
        assert_eq!(owner_in(&clients, "client:air"), Some(false));
        assert_eq!(owner_in(&clients, "client:gone"), None);
    }

    #[test]
    fn lua_str_escapes_closing_brackets() {
        assert_eq!(lua_str("a"), "[[\na]]");
        assert_eq!(lua_str("x]]y"), "[=[\nx]]y]=]");
        assert_eq!(lua_str("[1]"), "[=[\n[1]]=]");
    }
}
