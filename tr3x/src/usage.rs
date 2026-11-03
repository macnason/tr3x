//! What a provider has left of its plan, as T3's thread details show it under Usage limits:
//! each window's share left, `Session 62% left`, yellow at 20% or less and red at 10% or less.
//! A window that's used up says when it resets instead. Reply shows it beside the model chip.
//!
//! The windows are the provider's `usageLimits` in T3's server config
//! (`ServerProviderUsageLimits` in `packages/contracts/src/providerUsageLimits.ts`), which T3
//! keeps up to date as the provider reports them.

use serde_json::Value;
use unicode_width::UnicodeWidthStr;

use crate::chat::{DIM, RED, RESET, YELLOW, when};

/// One usage window, such as Claude's five-hour session or its week.
#[derive(Clone, Debug, PartialEq)]
pub struct Window {
    pub label: String,
    /// The share left, 0 to 100, rounded as T3 rounds it.
    pub left: u8,
    pub resets_at: Option<i64>,
}

/// The usage windows of the provider `instance`, in the order T3 lists them. None when T3
/// can't read the provider's limits, or the provider has none.
pub fn windows(config: &Value, instance: &str) -> Vec<Window> {
    let limits = t3client::models::providers(config)
        .iter()
        .find(|p| p["instanceId"].as_str() == Some(instance))
        .map(|p| &p["usageLimits"]);
    let Some(limits) = limits.filter(|l| l["unavailable"].is_null()) else {
        return Vec::new();
    };
    limits["windows"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|w| {
            let label = crate::chat::one_line(w["label"].as_str()?);
            let used = w["usedPercent"].as_f64()?;
            Some(Window {
                label,
                left: (100.0 - used.clamp(0.0, 100.0)).round() as u8,
                resets_at: w["resetsAt"].as_str().and_then(t3client::inbox::parse_iso),
            })
        })
        .filter(|w| !w.label.is_empty())
        .collect()
}

/// What's wrong with the provider `instance`, as Reply warns of it, and its colour. T3 reports a
/// provider that doesn't answer its status check with a `warning` or `error` status and a
/// message. A timeout means the check ran on the machine the provider lives on, so the hint
/// says where to look. None when the provider is fine, or T3 says nothing about it.
pub fn problem(config: &Value, instance: &str) -> Option<(String, &'static str)> {
    let provider = t3client::models::providers(config)
        .iter()
        .find(|p| p["instanceId"].as_str() == Some(instance))?;
    let tone = match provider["status"].as_str()? {
        "error" => RED,
        "warning" => YELLOW,
        _ => return None,
    };
    let name = provider["displayName"]
        .as_str()
        .or(provider["instanceId"].as_str())
        .unwrap_or("Provider");
    let message = crate::chat::one_line(provider["message"].as_str().unwrap_or_default());
    let text = if message.to_lowercase().contains("timed out") {
        match provider["driver"].as_str() {
            Some(driver) => format!(
                "{name} didn't answer on its machine. Run `{driver} --version` there to check it starts."
            ),
            None => format!("{name} didn't answer on its machine. Check it there."),
        }
    } else if message.is_empty() {
        format!("{name} isn't ready.")
    } else {
        format!("{name}: {message}")
    };
    Some((text, tone))
}

/// A window as Reply words it, and its colour: `Session 62% left`, or `Session resets 3pm` for
/// one that's used up until then.
fn word(window: &Window, now: i64, offset_minutes: i64) -> (String, &'static str) {
    let tone = match window.left {
        0..=10 => RED,
        11..=20 => YELLOW,
        _ => DIM,
    };
    let text = match window.resets_at {
        Some(at) if window.left == 0 && at > now => {
            format!("{} resets {}", window.label, when(at, now, offset_minutes))
        }
        _ => format!("{} {}% left", window.label, window.left),
    };
    (text, tone)
}

/// What fits in `room` columns: every window, else the one with the least left, else nothing.
/// Each in its own colour, two columns apart as chips are, since a label can hold a ` · `
/// itself (`Weekly · Fable`).
pub fn line(windows: &[Window], now: i64, offset_minutes: i64, room: usize) -> Option<String> {
    let words: Vec<(String, &str)> = windows
        .iter()
        .map(|w| word(w, now, offset_minutes))
        .collect();
    let width = |words: &[&(String, &str)]| {
        words.iter().map(|(t, _)| t.width()).sum::<usize>() + 2 * words.len().saturating_sub(1)
    };
    let all: Vec<&(String, &str)> = words.iter().collect();
    let tightest = windows
        .iter()
        .enumerate()
        .min_by_key(|(_, w)| w.left)
        .map(|(i, _)| vec![&words[i]]);
    let shown = [Some(all), tightest]
        .into_iter()
        .flatten()
        .find(|words| !words.is_empty() && width(words) <= room)?;
    Some(
        shown
            .iter()
            .map(|(text, tone)| format!("{tone}{text}{RESET}"))
            .collect::<Vec<_>>()
            .join("  "),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 2026-10-31T00:00:00Z.
    fn now() -> i64 {
        t3client::inbox::parse_iso("2026-10-31T00:00:00.000Z").unwrap()
    }

    fn config(limits: Value) -> Value {
        json!({"providers": [
            {"instanceId": "codex", "usageLimits": {"checkedAt": "2026-10-31T00:00:00.000Z", "windows": []}},
            {"instanceId": "claudeAgent", "usageLimits": limits},
        ]})
    }

    fn window(label: &str, used: f64, resets: Option<&str>) -> Value {
        let mut w = json!({"id": label.to_lowercase(), "kind": "other", "label": label, "usedPercent": used});
        if let Some(at) = resets {
            w["resetsAt"] = json!(at);
        }
        w
    }

    #[test]
    fn windows_are_the_providers_share_left() {
        let limits = json!({"checkedAt": "2026-10-31T00:00:00.000Z", "windows": [
            window("Session", 37.6, Some("2026-10-31T03:00:00.000Z")),
            window("Weekly", 112.0, None),
            {"id": "broken", "label": "Broken"},
        ]});
        assert_eq!(
            windows(&config(limits), "claudeAgent"),
            vec![
                Window {
                    label: "Session".into(),
                    left: 62,
                    resets_at: Some(now() + 3 * 3_600_000)
                },
                Window {
                    label: "Weekly".into(),
                    left: 0,
                    resets_at: None
                },
            ]
        );
        // Another provider's, none at all, or limits T3 couldn't read.
        assert!(windows(&config(json!(null)), "claudeAgent").is_empty());
        assert!(windows(&config(json!(null)), "cursor").is_empty());
        let unread = json!({"checkedAt": "2026-10-31T00:00:00.000Z", "windows": [window("Session", 10.0, None)],
            "unavailable": {"reason": "probeFailed"}});
        assert!(windows(&config(unread), "claudeAgent").is_empty());
    }

    #[test]
    fn a_provider_that_isnt_ready_says_why() {
        let codex = |status: &str, message: &str| {
            json!({"providers": [{"instanceId": "codex", "driver": "codex", "displayName": "Codex",
                "status": status, "message": message}]})
        };
        let timeout = "Timed out while checking Codex app-server provider status.";
        assert_eq!(
            problem(&codex("error", timeout), "codex"),
            Some((
                "Codex didn't answer on its machine. Run `codex --version` there to check it starts.".into(),
                RED
            ))
        );
        assert_eq!(
            problem(&codex("warning", "Signed out."), "codex"),
            Some(("Codex: Signed out.".into(), YELLOW))
        );
        assert_eq!(
            problem(&codex("error", ""), "codex"),
            Some(("Codex isn't ready.".into(), RED))
        );
        // Fine, disabled, unknown, or another provider's: nothing to warn of.
        assert_eq!(problem(&codex("ready", timeout), "codex"), None);
        assert_eq!(problem(&codex("disabled", timeout), "codex"), None);
        assert_eq!(problem(&codex("error", timeout), "claudeAgent"), None);
    }

    fn w(label: &str, left: u8, resets_at: Option<i64>) -> Window {
        Window {
            label: label.into(),
            left,
            resets_at,
        }
    }

    #[test]
    fn the_line_shows_every_window_that_fits_else_the_tightest() {
        let h = 3_600_000;
        let both = [w("Session", 62, Some(now() + h)), w("Weekly", 15, None)];
        let all = format!("{DIM}Session 62% left{RESET}  {YELLOW}Weekly 15% left{RESET}");
        assert_eq!(line(&both, now(), 0, 40).as_deref(), Some(all.as_str()));
        assert_eq!(line(&both, now(), 0, 33).as_deref(), Some(all.as_str()));
        let tightest = format!("{YELLOW}Weekly 15% left{RESET}");
        assert_eq!(
            line(&both, now(), 0, 32).as_deref(),
            Some(tightest.as_str())
        );
        // A label with a separator of its own stays one window.
        let scoped = [w("Weekly · Fable", 100, None)];
        assert_eq!(
            line(&scoped, now(), 0, 80),
            Some(format!("{DIM}Weekly · Fable 100% left{RESET}"))
        );
        assert_eq!(line(&both, now(), 0, 14), None);
        assert_eq!(line(&[], now(), 0, 80), None);
    }

    #[test]
    fn a_used_up_window_says_when_it_resets() {
        let h = 3_600_000;
        let out = [w("Session", 0, Some(now() + 15 * h))];
        assert_eq!(
            line(&out, now(), 0, 80),
            Some(format!("{RED}Session resets 3pm{RESET}"))
        );
        // Tomorrow, in local time ten hours ahead.
        assert_eq!(
            line(&out, now(), 600, 80),
            Some(format!("{RED}Session resets tomorrow 1am{RESET}"))
        );
        // A reset T3 hasn't caught up with yet: the share it last reported.
        let stale = [w("Session", 0, Some(now() - h))];
        assert_eq!(
            line(&stale, now(), 0, 80),
            Some(format!("{RED}Session 0% left{RESET}"))
        );
        assert_eq!(
            line(&[w("Weekly", 9, None)], now(), 0, 80),
            Some(format!("{RED}Weekly 9% left{RESET}"))
        );
    }
}
