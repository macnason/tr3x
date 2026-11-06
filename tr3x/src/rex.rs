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
    use super::lua_str;

    #[test]
    fn lua_str_escapes_closing_brackets() {
        assert_eq!(lua_str("a"), "[[\na]]");
        assert_eq!(lua_str("x]]y"), "[=[\nx]]y]=]");
        assert_eq!(lua_str("[1]"), "[=[\n[1]]=]");
    }
}
