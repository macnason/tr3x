//! The T3 side: connecting through t3term's client, and the thread commands T3's sidebar sends.
//! The command shapes are `OrchestrationV2Command` in T3's
//! `packages/contracts/src/orchestrationV2.ts`.

use std::sync::Arc;

use anyhow::Result;
use serde_json::{Value, json};
use t3term::auth::Scope;
use t3term::client::Client;
use t3term::tui::Capabilities;

pub async fn connect(operate: bool) -> Result<Arc<Client>> {
    let scopes: &[Scope] = if operate {
        &[Scope::Read, Scope::Operate]
    } else {
        &[Scope::Read]
    };
    let target = t3term::environments::target(None).await?;
    Ok(Arc::new(Client::connect(&target, scopes, "12h").await?))
}

pub fn capabilities(client: &Client) -> Capabilities {
    Capabilities::of(&client.runtime.capabilities)
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
    }
}

/// Records that the user looked at the thread, which clears Done and Woke in T3.
pub async fn visit(client: &Client, thread_id: &str) -> Result<()> {
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
    Some(n * per)
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
