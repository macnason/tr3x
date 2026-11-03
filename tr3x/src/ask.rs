//! What a thread is waiting on you for, in one line: the command, file or question. Reply
//! shows it over its chips, and the pane reports it to Rex.

use serde_json::Value;
use t3client::projection::ThreadState;

/// Agent-written text as one plain line: no escape sequences or other controls, which could
/// redraw the pane and show something other than what is being asked.
pub fn clean(text: &str) -> String {
    t3client::transcript::plain(&crate::render::strip_ansi(text))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The first pending request and the turn item that shows it, which carries its prompt,
/// questions and options.
pub fn pending(state: &ThreadState) -> Option<(Value, Value)> {
    let request = state.pending_requests().into_iter().next()?.clone();
    let item = state
        .request_item(request["id"].as_str().unwrap_or_default())
        .cloned()
        .unwrap_or(Value::Null);
    Some((request, item))
}

pub fn is_question(request: &Value) -> bool {
    request["kind"] == "user_input"
}

/// What an approval allows, as a verb and its object: `Run` and the command, `Edit` and the
/// file, else what the agent wrote.
pub fn approval(state: &ThreadState, request: &Value, item: &Value) -> (&'static str, String) {
    let kind = request["kind"]
        .as_str()
        .or(item["requestKind"].as_str())
        .unwrap_or_default();
    let verb = match kind {
        "command" => "Run",
        "file-change" => "Edit",
        "file-read" => "Read",
        _ => "Allow",
    };
    let subject = state.request_subject(request["id"].as_str().unwrap_or_default());
    let named = subject.and_then(|s| match s["type"].as_str() {
        Some("command_execution") => s["input"].as_str().map(str::to_string),
        Some("file_change") => s["fileName"].as_str().map(crate::chat::short_home),
        _ => None,
    });
    let what = named
        .filter(|w| !w.trim().is_empty())
        .or_else(|| {
            [
                &item["prompt"],
                &item["reason"],
                &item["title"],
                &request["summary"],
            ]
            .into_iter()
            .find_map(|v| v.as_str().filter(|s| !s.trim().is_empty()))
            .map(str::to_string)
        })
        .unwrap_or_else(|| "the agent to continue".into());
    (verb, clean(&what))
}

/// A question's text.
pub fn question_text(q: &Value) -> String {
    clean(
        q["question"]
            .as_str()
            .or(q["header"].as_str())
            .unwrap_or("Question"),
    )
}

/// The one line Rex's status carries for a pending request: `Run pnpm test?`, or the question.
pub fn line(state: &ThreadState) -> Option<String> {
    let (request, item) = pending(state)?;
    if is_question(&request) {
        let q = item["questions"].as_array()?.first()?;
        return Some(question_text(q));
    }
    let (verb, what) = approval(state, &request, &item);
    Some(format!("{verb} {what}?"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn state(requests: Value, items: Value, nodes: Value) -> ThreadState {
        ThreadState::from_snapshot(&json!({
            "kind": "snapshot",
            "snapshotSequence": 1,
            "projection": {
                "thread": {"id": "t1", "title": "Test"},
                "runtimeRequests": requests,
                "turnItems": items,
                "nodes": nodes,
            }
        }))
        .unwrap()
    }

    #[test]
    fn an_approval_names_its_command() {
        let s = state(
            json!([{"id": "q1", "nodeId": "ask", "status": "pending", "kind": "command"}]),
            json!([
                {"id": "c1", "type": "command_execution", "nodeId": "tool", "input": "pnpm   test\n--run"},
                {"id": "a1", "type": "approval_request", "requestId": "q1", "requestKind": "command"}
            ]),
            json!([{"id": "ask", "parentNodeId": "tool"}]),
        );
        assert_eq!(line(&s).as_deref(), Some("Run pnpm test --run?"));
    }

    #[test]
    fn an_approval_without_a_subject_reads_its_prompt() {
        let s = state(
            json!([{"id": "q1", "status": "pending", "kind": "permission"}]),
            json!([{"id": "a1", "type": "approval_request", "requestId": "q1",
                    "prompt": "Use the \u{1b}[31mnetwork\u{1b}[0m"}]),
            json!([]),
        );
        assert_eq!(line(&s).as_deref(), Some("Allow Use the network?"));
    }

    #[test]
    fn a_question_reads_as_itself() {
        let s = state(
            json!([{"id": "q1", "status": "pending", "kind": "user_input"}]),
            json!([{"id": "u1", "type": "user_input_request", "requestId": "q1",
                    "questions": [{"id": "x", "question": "Which branch?", "options": []}]}]),
            json!([]),
        );
        assert_eq!(line(&s).as_deref(), Some("Which branch?"));
        assert!(line(&state(json!([]), json!([]), json!([]))).is_none());
    }
}
