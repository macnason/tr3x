//! How many idle threads keep a tab. Every tab is a pane process, a pseudo-terminal and a
//! connection to T3, so a session that mirrors every open thread grows without bound: a few
//! hundred threads left open exhaust the Mac's pseudo-terminals and load T3's server with a
//! subscription each.
//!
//! The budget is off until it's set, and only ever touches threads with nothing to say. A
//! thread that needs you, works or is pinned always has its tab. Of the rest, the most recently
//! active keep theirs, up to `open`, and only while they were active within `idle`. The others
//! are parked: they stay open in T3, untouched, and Find Thread brings any of them back.

use anyhow::{Result, bail};
use serde_json::{Value, json};

/// The `openTabs` and `openTabsIdle` settings in `~/.config/tr3x/settings.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// At most this many idle open threads keep a tab. None for no limit.
    pub open: Option<usize>,
    /// An idle open thread keeps its tab only while it was active within this many
    /// milliseconds. None for no limit.
    pub idle: Option<i64>,
}

const HOUR: i64 = 60 * 60 * 1000;

impl Budget {
    /// Unset: a tab for every open thread.
    pub const DEFAULT: Budget = Budget {
        open: None,
        idle: None,
    };
}

pub fn budget() -> Budget {
    let settings = crate::width::settings().unwrap_or_default();
    let mut budget = Budget::DEFAULT;
    match settings.get("openTabs") {
        Some(Value::String(s)) if s == "all" => budget.open = None,
        Some(v) => {
            if let Some(n) = v.as_u64() {
                budget.open = Some(n as usize);
            }
        }
        None => {}
    }
    match settings.get("openTabsIdle") {
        Some(Value::String(s)) if s == "off" => budget.idle = None,
        Some(Value::String(s)) => {
            if let Some(ms) = duration_ms(s) {
                budget.idle = Some(ms);
            }
        }
        _ => {}
    }
    budget
}

/// `90m`, `36h`, `3d` or `2w`, in milliseconds. None for one too long to count.
fn duration_ms(text: &str) -> Option<i64> {
    let text = text.trim();
    let split = text.find(|c: char| !c.is_ascii_digit())?;
    let (n, unit) = text.split_at(split);
    let n: i64 = n.parse().ok()?;
    let unit = match unit.trim() {
        "m" | "min" | "mins" => 60 * 1000,
        "h" | "hr" | "hrs" | "hour" | "hours" => HOUR,
        "d" | "day" | "days" => 24 * HOUR,
        "w" | "week" | "weeks" => 7 * 24 * HOUR,
        _ => return None,
    };
    n.checked_mul(unit).filter(|_| n > 0)
}

fn say_duration(ms: i64) -> String {
    let hours = ms / HOUR;
    match hours {
        0 => format!("{}m", ms / 60_000),
        h if h % (24 * 7) == 0 => format!("{}w", h / (24 * 7)),
        h if h % 24 == 0 => format!("{}d", h / 24),
        h => format!("{h}h"),
    }
}

fn say(budget: Budget) -> String {
    match (budget.open, budget.idle) {
        (None, None) => "Tabs: every open thread".to_string(),
        (Some(n), None) => format!("Tabs: the {n} most recent idle open threads"),
        (None, Some(idle)) => format!(
            "Tabs: idle open threads active in the last {}",
            say_duration(idle)
        ),
        (Some(n), Some(idle)) => format!(
            "Tabs: up to {n} idle open threads, active in the last {}",
            say_duration(idle)
        ),
    }
}

/// `tr3x tabs [N|all|default] [idle DURATION|off]`.
pub fn apply(args: &[String]) -> Result<String> {
    let mut args = args.iter().map(|a| a.trim()).filter(|a| !a.is_empty());
    let mut open: Option<Option<Value>> = None;
    let mut idle: Option<Option<Value>> = None;
    while let Some(arg) = args.next() {
        match arg {
            "all" | "unlimited" => {
                open = Some(Some(json!("all")));
                idle = Some(Some(json!("off")));
            }
            "default" | "reset" => {
                open = Some(None);
                idle = Some(None);
            }
            "idle" => match args.next() {
                Some("off" | "never" | "none") => idle = Some(Some(json!("off"))),
                Some(d) if duration_ms(d).is_some() => idle = Some(Some(json!(d))),
                Some(other) => bail!("Can't read {other:?}: a duration such as 36h or 3d, or off"),
                None => bail!("idle takes a duration such as 36h or 3d, or off"),
            },
            n => match n.parse::<u64>() {
                Ok(n) => open = Some(Some(json!(n))),
                Err(_) => {
                    bail!("Can't read {n:?}: a number of tabs, all, default, or idle DURATION")
                }
            },
        }
    }
    if let Some(open) = open {
        crate::width::set_setting("openTabs", open)?;
    }
    if let Some(idle) = idle {
        crate::width::set_setting("openTabsIdle", idle)?;
    }
    Ok(say(budget()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_in_minutes_hours_days_and_weeks() {
        assert_eq!(duration_ms("90m"), Some(90 * 60 * 1000));
        assert_eq!(duration_ms("36h"), Some(36 * HOUR));
        assert_eq!(duration_ms("3d"), Some(72 * HOUR));
        assert_eq!(duration_ms("2w"), Some(14 * 24 * HOUR));
        assert_eq!(duration_ms("0h"), None);
        assert_eq!(duration_ms("3"), None);
        assert_eq!(duration_ms("soon"), None);
        assert_eq!(duration_ms("9223372036854775807w"), None);
        assert!(apply(&["idle".into(), "99999999999999999w".into()]).is_err());
    }

    #[test]
    fn durations_say_themselves_in_the_largest_whole_unit() {
        assert_eq!(say_duration(48 * HOUR), "2d");
        assert_eq!(say_duration(36 * HOUR), "36h");
        assert_eq!(say_duration(14 * 24 * HOUR), "2w");
        assert_eq!(say_duration(30 * 60 * 1000), "30m");
    }
}
