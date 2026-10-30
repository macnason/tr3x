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

const BUNDLED: &str = "/Applications/Rex Beta.app/Contents/Helpers/Rex Server.rex/Contents/MacOS/rex";

#[derive(Clone, Debug)]
pub struct Rex {
    pub bin: PathBuf,
}

impl Rex {
    /// `$T3REX_REX`, else Rex's own copy (`$REX_BIN_DIR`, set in Rex terminals), else `rex` on
    /// `PATH`, else the copy in the app bundle.
    pub fn find() -> Rex {
        let from_env = std::env::var_os("T3REX_REX").map(PathBuf::from);
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
            .args(args)
            .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .with_context(|| format!("couldn't run {}", self.bin.display()))?;
        if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
            pipe.write_all(text.as_bytes()).await?;
        }
        let out = child.wait_with_output().await?;
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

    /// Every session, in Rex's order, as `{session_id, label}`.
    pub async fn sessions(&self) -> Result<Vec<Value>> {
        let list = self.json(&["ls", "--json"], None).await?;
        Ok(list["sessions"].as_array().cloned().unwrap_or_default())
    }

    /// The session's windows and blocks, as `session inspect --json` prints them.
    pub async fn inspect(&self, session: &str) -> Result<Value> {
        self.json(&["session", "inspect", session, "--json"], None).await
    }

    pub async fn create_session(&self, label: &str) -> Result<String> {
        let args = serde_json::json!({ "label": label }).to_string();
        let made = self.json(&["api", "call", "session.create", &args], None).await?;
        made["session_id"]
            .as_str()
            .map(str::to_string)
            .context("session.create returned no id")
    }

    pub async fn move_session(&self, session: &str, index: usize) -> Result<()> {
        self.run(&["session", "move", session, "--index", &index.to_string()], None)
            .await
            .map(drop)
    }

    /// Runs a Lua script against `session` and returns what it returned.
    pub async fn lua(&self, session: &str, script: &str) -> Result<Value> {
        self.json(&["do", "-s", session, "-"], Some(script)).await
    }

    /// Asks the app clients to do one of their actions, such as `session.select`. A missing
    /// client is not an error: nothing is looking.
    pub async fn client_do(&self, action: &str, pairs: &[(&str, &str)]) -> Result<()> {
        let clients = self.json(&["client", "ls", "--json"], None).await?;
        let apps = clients["clients"].as_array().cloned().unwrap_or_default();
        for app in apps.iter().filter(|c| c["info"]["kind"] == "app") {
            let Some(id) = app["client_id"].as_str() else { continue };
            let mut args = vec!["-C", id, "do", action];
            let kv: Vec<String> = pairs.iter().map(|(k, v)| format!("{k}={v}")).collect();
            args.extend(kv.iter().map(String::as_str));
            let _ = self.run(&args, None).await;
        }
        Ok(())
    }
}

/// A Lua string literal that holds `text` exactly. Long brackets need no escaping, so pick a
/// level the text doesn't close.
pub fn lua_str(text: &str) -> String {
    let mut level = 0;
    loop {
        let close = format!("]{}]", "=".repeat(level));
        if !text.contains(&close) {
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
    }
}
