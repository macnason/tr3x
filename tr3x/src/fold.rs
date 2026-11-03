//! A finished turn folds to one line, `▸ Worked for 2m 14s`, above its last reply, as T3 Code's
//! timeline does (`deriveRunFolds` in the nightly's web client). What it folds is the turn's
//! work: tool calls, thinking, approvals and the messages between them. Your messages, the
//! last reply, questions, plans, errors, notices and HTML embeds stay. A run that failed or
//! is still going doesn't fold, and neither does one with a message still streaming.
//!
//! The conversation is scrollback, so a fold can't open in place. `/details` (the `details`
//! setting) prints the thread again with every turn open, each under `▾ Worked for …`.

use std::collections::{HashMap, HashSet};

use serde_json::Value;
use t3client::projection::{self, ThreadState};
use t3client::transcript::{Block, BlockKind};

use crate::chat::duration;

/// Item ids of the lines this adds start with this, so they never match a real item.
const PREFIX: &str = "fold:";

/// What a fold line says for a finished run, or None for one that doesn't fold.
fn label(run: &Value) -> Option<String> {
    let status = projection::status(run);
    let stopped = matches!(status, "interrupted" | "cancelled");
    if status != "completed" && !stopped {
        return None;
    }
    let took = run["completedAt"].as_str().and_then(|done| {
        let start = run["startedAt"].as_str().or(run["requestedAt"].as_str())?;
        Some(t3client::inbox::parse_iso(done)? - t3client::inbox::parse_iso(start)?)
    });
    Some(match (stopped, took.map(duration)) {
        (true, Some(t)) => format!("You stopped after {t}"),
        (true, None) => "You stopped this response".into(),
        (false, Some(t)) => format!("Worked for {t}"),
        (false, None) => "Worked".into(),
    })
}

/// Whether a run's turn shows folded, so its last line need not say how long it took.
pub fn folds(run: &Value, open: bool) -> bool {
    !open && label(run).is_some()
}

/// Whether a block stays when its turn folds.
fn stays(block: &Block, last_reply: bool, embeds: &HashSet<&str>) -> bool {
    if embeds.contains(block.item_id.as_str()) {
        return true;
    }
    match block.kind {
        BlockKind::User | BlockKind::Plan | BlockKind::Notice | BlockKind::Error => true,
        BlockKind::Assistant => last_reply || block.streaming,
        BlockKind::Request => {
            block.item_type.contains("input") || block.header.to_lowercase().contains("question")
        }
        BlockKind::Tool | BlockKind::Reasoning => false,
    }
}

/// The blocks as the conversation shows them: each finished turn's work folded behind one
/// line, or with `open`, all of it under that line.
pub fn fold(
    blocks: Vec<Block>,
    state: &ThreadState,
    embeds: &HashMap<String, crate::media::Embed>,
    open: bool,
) -> Vec<Block> {
    let embeds: HashSet<&str> = embeds.keys().map(String::as_str).collect();
    let mut last_reply: HashMap<&str, usize> = HashMap::new();
    let mut streaming: HashSet<&str> = HashSet::new();
    for (i, b) in blocks.iter().enumerate() {
        if b.kind == BlockKind::Assistant && !b.run_id.is_empty() {
            last_reply.insert(&b.run_id, i);
            if b.streaming {
                streaming.insert(&b.run_id);
            }
        }
    }
    let labels: HashMap<&str, String> = blocks
        .iter()
        .map(|b| b.run_id.as_str())
        .filter(|id| !id.is_empty() && !streaming.contains(id))
        .collect::<HashSet<_>>()
        .into_iter()
        .filter_map(|id| Some((id, label(state.run(id)?)?)))
        .collect();
    let mut out = Vec::with_capacity(blocks.len());
    let mut labelled: HashSet<String> = HashSet::new();
    for (i, block) in blocks.iter().enumerate() {
        let Some(text) = labels.get(block.run_id.as_str()) else {
            out.push(block.clone());
            continue;
        };
        let last = last_reply.get(block.run_id.as_str()) == Some(&i);
        if stays(block, last, &embeds) {
            out.push(block.clone());
            continue;
        }
        if labelled.insert(block.run_id.clone()) {
            let mark = if open { '▾' } else { '▸' };
            out.push(Block {
                item_id: format!("{PREFIX}{}:{open}", block.run_id),
                kind: BlockKind::Notice,
                header: format!("{mark} {text}"),
                body: String::new(),
                streaming: false,
                status: "completed".into(),
                item_type: "fold".into(),
                detail: String::new(),
                title: String::new(),
                output_omitted: false,
                updated_at: block.updated_at.clone(),
                exit_code: None,
                run_id: block.run_id.clone(),
                tool_name: String::new(),
                request_id: String::new(),
                change: None,
                images: Vec::new(),
            });
        }
        if open {
            out.push(block.clone());
        }
    }
    out
}

/// Whether a block is a fold's line, which draws as a notice does.
#[cfg(test)]
fn is_fold(block: &Block) -> bool {
    block.item_id.starts_with(PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn block(id: &str, kind: BlockKind, run: &str) -> Block {
        Block {
            item_id: id.into(),
            kind,
            header: String::new(),
            body: String::new(),
            streaming: false,
            status: "completed".into(),
            item_type: String::new(),
            detail: String::new(),
            title: String::new(),
            output_omitted: false,
            updated_at: String::new(),
            exit_code: None,
            run_id: run.into(),
            tool_name: String::new(),
            request_id: String::new(),
            change: None,
            images: Vec::new(),
        }
    }

    fn state(status: &str) -> ThreadState {
        ThreadState::from_snapshot(&json!({"projection": {
            "thread": {"id": "t"},
            "runs": [{"id": "r", "status": status,
                "startedAt": "2026-11-03T10:00:00Z", "completedAt": "2026-11-03T10:02:14Z"}],
            "turnItems": [],
        }}))
        .unwrap()
    }

    fn turn() -> Vec<Block> {
        vec![
            block("u", BlockKind::User, "r"),
            block("a1", BlockKind::Assistant, "r"),
            block("t1", BlockKind::Tool, "r"),
            block("th", BlockKind::Reasoning, "r"),
            block("t2", BlockKind::Tool, "r"),
            block("a2", BlockKind::Assistant, "r"),
        ]
    }

    fn ids(blocks: &[Block]) -> Vec<String> {
        blocks
            .iter()
            .map(|b| {
                if is_fold(b) {
                    b.header.clone()
                } else {
                    b.item_id.clone()
                }
            })
            .collect()
    }

    #[test]
    fn a_finished_turn_folds_to_its_last_reply() {
        let out = fold(turn(), &state("completed"), &HashMap::new(), false);
        assert_eq!(ids(&out), ["u", "▸ Worked for 2m 14s", "a2"]);
    }

    #[test]
    fn details_keep_the_work_under_the_line() {
        let out = fold(turn(), &state("completed"), &HashMap::new(), true);
        assert_eq!(
            ids(&out),
            ["u", "▾ Worked for 2m 14s", "a1", "t1", "th", "t2", "a2"]
        );
    }

    #[test]
    fn a_stopped_turn_says_so() {
        let out = fold(turn(), &state("interrupted"), &HashMap::new(), false);
        assert_eq!(ids(&out)[1], "▸ You stopped after 2m 14s");
    }

    #[test]
    fn running_and_failed_turns_stay_open() {
        for status in ["running", "failed"] {
            let out = fold(turn(), &state(status), &HashMap::new(), false);
            assert_eq!(ids(&out), ids(&turn()), "{status}");
        }
    }
}
