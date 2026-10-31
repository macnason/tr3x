//! The T3 side: the machines t3rex is paired with, through t3client, and the thread commands
//! T3's sidebar sends. The command shapes are `OrchestrationV2Command` in T3's
//! `packages/contracts/src/orchestrationV2.ts`.

use std::sync::Arc;

use anyhow::{Context, Result};
use serde_json::{Value, json};
use t3client::env::Env;
use t3client::hub::{Hub, ThreadRef};
use t3client::inbox::Capabilities;

/// Every machine t3rex knows, from its state directory. Reads files only.
pub fn hub() -> Hub {
    Hub::load(crate::state::dir())
}

/// This Mac's T3, connected. The first connection pairs t3rex with it through T3's own CLI.
pub async fn connect() -> Result<Arc<Env>> {
    let env = hub()
        .home()
        .context("T3 Code isn't set up on this Mac yet. Open T3 Code once.")?;
    env.rpc().await?;
    Ok(env)
}

/// The machine `query` names: an id, the start of one, an alias or T3's name, or this Mac's
/// when it's empty. Not connected yet: a machine out of reach still gets a pane.
pub fn machine(query: &str) -> Result<Arc<Env>> {
    let hub = hub();
    let found = if query.is_empty() {
        hub.home()
    } else {
        hub.get(query).or_else(|| hub.by_prefix(query))
    };
    found.with_context(|| {
        if query.is_empty() {
            "T3 Code isn't set up on this Mac yet. Open T3 Code once.".to_string()
        } else {
            format!("Rex isn't paired with a machine called {query:?}. `t3rex envs` lists them.")
        }
    })
}

/// A pane's thread, `env/thread`, or a bare thread id on this Mac, with its machine.
pub fn thread(text: &str) -> Result<(Arc<Env>, ThreadRef)> {
    let home = hub().home_id().unwrap_or_default().to_string();
    let r = ThreadRef::parse(text, &home);
    let env = machine(&r.env)?;
    // An alias or a shortened id names the machine; the reference holds its id.
    Ok((env.clone(), ThreadRef::new(env.id(), r.thread)))
}

pub fn capabilities(env: &Env) -> Capabilities {
    Capabilities::of(&env.capabilities())
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn iso(ms: i64) -> String {
    // Civil-from-days, so no date crate is needed for one timestamp format.
    let secs = ms.div_euclid(1000);
    let millis = ms.rem_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (h, m, s) = (rem / 3600, rem % 3600 / 60, rem % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}.{millis:03}Z")
}

/// What the user can do to a thread from Rex.
#[derive(Clone, Debug)]
pub enum Lifecycle {
    Settle,
    Unsettle,
    /// Until this many milliseconds from now.
    Snooze(i64),
    Wake,
    Pin,
    Unpin,
    Archive,
    Delete,
}

pub fn command(thread_id: &str, action: &Lifecycle) -> Value {
    let id = format!("t3rex:{}", uuid::Uuid::new_v4());
    match action {
        Lifecycle::Settle => {
            json!({"type": "thread.settle", "commandId": id, "threadId": thread_id})
        }
        Lifecycle::Unsettle => {
            json!({"type": "thread.unsettle", "commandId": id, "threadId": thread_id, "reason": "user"})
        }
        Lifecycle::Snooze(ms) => json!({
            "type": "thread.snooze", "commandId": id, "threadId": thread_id,
            "snoozedUntil": iso(now_ms() + ms),
        }),
        Lifecycle::Wake => {
            json!({"type": "thread.unsnooze", "commandId": id, "threadId": thread_id, "reason": "user"})
        }
        Lifecycle::Pin => json!({"type": "thread.pin", "commandId": id, "threadId": thread_id}),
        Lifecycle::Unpin => json!({"type": "thread.unpin", "commandId": id, "threadId": thread_id}),
        Lifecycle::Archive => {
            json!({"type": "thread.archive", "commandId": id, "threadId": thread_id})
        }
        Lifecycle::Delete => {
            json!({"type": "thread.delete", "commandId": id, "threadId": thread_id})
        }
    }
}

/// Fails unless the machine's T3 has `capability`, saying where to update.
pub async fn require(env: &Env, capability: &str) -> Result<()> {
    env.rpc().await?;
    if !env.has_capability(capability) {
        anyhow::bail!(
            "T3 on {} is too old for this. Update T3 Code there.",
            env.name()
        );
    }
    Ok(())
}

/// Gives the thread a title, or with `None` has T3 write one from the conversation.
pub async fn rename(env: &Env, thread_id: &str, title: Option<&str>) -> Result<()> {
    let mut command = json!({
        "type": "thread.metadata.update",
        "commandId": format!("t3rex:{}", uuid::Uuid::new_v4()),
        "threadId": thread_id,
    });
    match title.map(str::trim) {
        Some("") => anyhow::bail!("a title can't be empty"),
        Some(title) => command["title"] = json!(title),
        None => {
            require(env, "threadTitleRegeneration").await?;
            command["regenerateTitle"] = json!(true);
        }
    }
    env.dispatch(command).await.map(drop)
}

/// A new thread that carries on from this one's latest settled point, as T3's Fork does.
/// Returns the new thread's id.
pub async fn fork(env: &Env, thread_id: &str, title: Option<&str>) -> Result<String> {
    let target = uuid::Uuid::new_v4().to_string();
    let mut command = json!({
        "type": "thread.fork",
        "commandId": format!("t3rex:{}", uuid::Uuid::new_v4()),
        "createdBy": "user",
        "creationSource": "web",
        "sourceThreadId": thread_id,
        "targetThreadId": target,
        "sourcePoint": {"type": "latest_stable"},
    });
    if let Some(title) = title.map(str::trim).filter(|t| !t.is_empty()) {
        command["title"] = json!(title);
    }
    env.dispatch(command).await?;
    Ok(target)
}

/// Stops the thread, as T3's Stop does: its run and the tasks it delegated. A T3 without
/// `thread.stop` gets the running turn interrupted instead. Returns whether anything ran.
pub async fn stop(env: &Env, thread_id: &str, run_id: Option<&str>) -> Result<bool> {
    env.rpc().await?;
    if env.has_capability("threadStop") {
        env.dispatch(json!({
            "type": "thread.stop",
            "commandId": format!("t3rex:{}", uuid::Uuid::new_v4()),
            "threadId": thread_id,
            "reason": "Stopped from Rex",
        }))
        .await?;
        return Ok(true);
    }
    let Some(run) = run_id else {
        return Ok(false);
    };
    env.interrupt(thread_id, run).await?;
    Ok(true)
}

/// Kills the agent of a thread that ignores Stop, as T3's Force stop does. `run_id` is the
/// run seen ignoring it; T3 refuses once a newer one has started.
pub async fn force_stop(env: &Env, thread_id: &str, run_id: Option<&str>) -> Result<String> {
    require(env, "threadForceStop").await?;
    let mut input = json!({"threadId": thread_id});
    if let Some(run) = run_id {
        input["runId"] = json!(run);
    }
    let result = env.call("orchestration.forceStopThread", input).await?;
    Ok(match result["outcome"].as_str() {
        Some("killed") => "Killed the agent".into(),
        Some("released" | "releasing") => "Ended the agent's session".into(),
        // The agent also works for other threads, so T3 only ended this one's turn.
        Some("shared") => "Ended this thread's turn".into(),
        _ => "Force stopped".into(),
    })
}

/// Records that the user looked at the thread, which clears Done and Woke in T3.
pub async fn visit(client: &Env, thread_id: &str) -> Result<()> {
    client
        .dispatch(json!({
            "type": "thread.visit",
            "commandId": format!("t3rex:{}", uuid::Uuid::new_v4()),
            "threadId": thread_id,
            "visitedAt": iso(now_ms()),
            "opened": true,
        }))
        .await
        .map(drop)
}

/// `2h`, `30m`, `1d`, `90s`, or a bare number of minutes.
pub fn parse_duration(text: &str) -> Option<i64> {
    let text = text.trim();
    let (number, unit) = text
        .find(|c: char| !c.is_ascii_digit())
        .map_or((text, "m"), |at| text.split_at(at));
    let n: i64 = number.parse().ok()?;
    let per = match unit.trim() {
        "s" | "sec" => 1_000,
        "m" | "min" | "" => 60_000,
        "h" | "hr" | "hour" | "hours" => 3_600_000,
        "d" | "day" | "days" => 86_400_000,
        "w" | "week" | "weeks" => 7 * 86_400_000,
        _ => return None,
    };
    n.checked_mul(per)
}

/// How long a snooze lasts: a duration, or `tomorrow` for 9 AM local time the next day.
pub fn snooze_ms(text: &str) -> Option<i64> {
    if text.trim() != "tomorrow" {
        return parse_duration(text);
    }
    let offset = crate::chat::local_offset_minutes() * 60_000;
    let day = 86_400_000;
    let local_now = now_ms() + offset;
    let tomorrow_nine = (local_now.div_euclid(day) + 1) * day + 9 * 3_600_000;
    Some(tomorrow_nine - local_now)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_formats_like_t3() {
        assert_eq!(iso(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso(1_793_371_504_939), "2026-10-30T14:45:04.939Z");
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("2h"), Some(7_200_000));
        assert_eq!(parse_duration("30"), Some(1_800_000));
        assert_eq!(parse_duration("1d"), Some(86_400_000));
        assert_eq!(parse_duration("soon"), None);
    }
}
