//! Client-side copies of T3 projections, kept current from stream items.
//!
//! Every V2 domain event carries the whole updated entity, so applying one is an upsert by id.
//! Entities stay as JSON: fields this build does not know pass through untouched.

use serde_json::{Map, Value};

/// Which projection array an event type upserts into. `None` for events that change no array.
fn collection_for(event_type: &str) -> Option<&'static str> {
    Some(match event_type {
        "run.created" | "run.updated" => "runs",
        "run-attempt.created" | "run-attempt.updated" => "attempts",
        "node.updated" => "nodes",
        "subagent.updated" => "subagents",
        "provider-session.attached" | "provider-session.updated" => "providerSessions",
        "provider-thread.updated" => "providerThreads",
        "provider-turn.updated" => "providerTurns",
        "runtime-request.updated" => "runtimeRequests",
        "message.updated" => "messages",
        "turn-item.updated" => "turnItems",
        "plan.updated" => "plans",
        "checkpoint-scope.created" => "checkpointScopes",
        "checkpoint.captured" => "checkpoints",
        "context-handoff.updated" => "contextHandoffs",
        "context-transfer.created" | "context-transfer.updated" => "contextTransfers",
        _ => return None,
    })
}

fn upsert(array: &mut Vec<Value>, entity: Value) {
    let id = entity.get("id").cloned();
    match array
        .iter_mut()
        .find(|existing| id.is_some() && existing.get("id") == id.as_ref())
    {
        Some(existing) => *existing = entity,
        None => array.push(entity),
    }
}

fn array_mut<'a>(object: &'a mut Map<String, Value>, key: &str) -> &'a mut Vec<Value> {
    let slot = object
        .entry(key)
        .or_insert_with(|| Value::Array(Vec::new()));
    if !slot.is_array() {
        *slot = Value::Array(Vec::new());
    }
    slot.as_array_mut().expect("just made an array")
}

/// What a stream item did to local state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Applied {
    Snapshot,
    Event(String),
    /// Already applied, from the overlap a resume replays.
    Duplicate,
    /// The server finished replaying history; later items are live.
    Synchronized,
    Ignored,
}

#[derive(Debug, Clone, Default)]
pub struct ThreadState {
    pub sequence: u64,
    pub projection: Map<String, Value>,
    pub synchronized: bool,
}

impl ThreadState {
    pub fn from_snapshot(snapshot: &Value) -> Option<ThreadState> {
        let mut state = ThreadState::default();
        state.replace(snapshot)?;
        Some(state)
    }

    fn replace(&mut self, snapshot: &Value) -> Option<()> {
        let projection = snapshot.get("projection")?.as_object()?.clone();
        projection.get("thread")?.get("id")?.as_str()?;
        self.sequence = snapshot
            .get("snapshotSequence")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        self.projection = projection;
        Some(())
    }

    /// Applies one `orchestration.subscribeThread` item.
    pub fn apply(&mut self, item: &Value) -> Applied {
        match item.get("kind").and_then(Value::as_str) {
            Some("synchronized") => {
                self.synchronized = true;
                Applied::Synchronized
            }
            Some("snapshot") => match self.replace(item) {
                Some(()) => Applied::Snapshot,
                None => Applied::Ignored,
            },
            Some("event") => {
                let sequence = item.get("sequence").and_then(Value::as_u64).unwrap_or(0);
                if sequence <= self.sequence {
                    return Applied::Duplicate;
                }
                self.sequence = sequence;
                let Some(event) = item.get("event") else {
                    return Applied::Ignored;
                };
                let event_type = event
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let Some(payload) = event.get("payload").cloned() else {
                    return Applied::Event(event_type);
                };
                if event_type == "thread.pull-request-link-updated" {
                    // The payload is one PR link, not the thread.
                    self.link_pull_request(payload);
                } else if event_type.starts_with("thread.") {
                    // Only a whole thread replaces it: a partial one would lose the id every send needs.
                    if payload.get("id").and_then(Value::as_str).is_some() {
                        self.projection.insert("thread".into(), payload);
                    }
                } else if event_type == "provider-session.detached" {
                    self.detach_session(&payload);
                } else if let Some(key) = collection_for(&event_type) {
                    // Creating the list here would hide every item the snapshot sent only in turnItems.
                    if key == "turnItems" && self.projection.contains_key("visibleTurnItems") {
                        self.update_visible_item(&payload);
                    }
                    upsert(array_mut(&mut self.projection, key), payload);
                }
                Applied::Event(event_type)
            }
            _ => Applied::Ignored,
        }
    }

    /// Drops the provider session a `provider-session.detached` event names, as the nightly's
    /// reducer does (`packages/client-runtime/src/state/orchestrationV2Projection.ts:220`), so
    /// its `lastError` no longer stands for the thread. A payload that names no session drops
    /// none.
    fn detach_session(&mut self, payload: &Value) {
        let Some(id) = payload.get("providerSessionId") else {
            return;
        };
        if let Some(sessions) = self
            .projection
            .get_mut("providerSessions")
            .and_then(Value::as_array_mut)
        {
            sessions.retain(|session| session.get("id") != Some(id));
        }
    }

    /// Keeps `visibleTurnItems`, the server's display order including inherited fork history, in step.
    fn update_visible_item(&mut self, item: &Value) {
        let thread_id = self.thread_id().to_string();
        let visible = array_mut(&mut self.projection, "visibleTurnItems");
        let id = item.get("id");
        if let Some(entry) = visible
            .iter_mut()
            .find(|e| e.get("item").and_then(|i| i.get("id")) == id)
        {
            if let Some(object) = entry.as_object_mut() {
                object.insert("item".into(), item.clone());
            }
            return;
        }
        let position = visible
            .iter()
            .filter_map(|e| e.get("position").and_then(Value::as_u64))
            .max()
            .map_or(0, |p| p + 1);
        visible.push(serde_json::json!({
            "position": position,
            "visibility": "local",
            "sourceThreadId": thread_id,
            "sourceItemId": id.cloned().unwrap_or(Value::Null),
            "item": item,
        }));
    }

    fn link_pull_request(&mut self, link: Value) {
        let Some(thread) = self
            .projection
            .get_mut("thread")
            .and_then(Value::as_object_mut)
        else {
            return;
        };
        let key = |pr: &Value| ["host", "repository", "number"].map(|field| pr.get(field).cloned());
        let links = array_mut(thread, "pullRequests");
        match links
            .iter_mut()
            .find(|existing| key(existing) == key(&link))
        {
            Some(existing) => *existing = link,
            None => links.push(link),
        }
    }

    pub fn thread(&self) -> &Value {
        self.projection.get("thread").unwrap_or(&Value::Null)
    }

    pub fn thread_id(&self) -> &str {
        self.thread()
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }

    pub fn list(&self, key: &str) -> &[Value] {
        self.projection
            .get(key)
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Turn items in display order. Falls back to `turnItems` by ordinal when the server sent no visible list.
    pub fn items(&self) -> Vec<&Value> {
        let visible = self.list("visibleTurnItems");
        if !visible.is_empty() {
            let mut entries: Vec<&Value> = visible.iter().collect();
            entries.sort_by_key(|e| e.get("position").and_then(Value::as_u64).unwrap_or(0));
            return entries.into_iter().filter_map(|e| e.get("item")).collect();
        }
        let mut items: Vec<&Value> = self.list("turnItems").iter().collect();
        items.sort_by_key(|i| i.get("ordinal").and_then(Value::as_u64).unwrap_or(0));
        items
    }

    /// The runs of the thread a turn item belongs to, as far as this projection holds them:
    /// every run for the thread's own items, and none for an item inherited from the thread
    /// it was forked from, whose runs stay with that thread. An item that names no thread
    /// counts as the thread's own.
    pub fn runs_for(&self, item: &Value) -> &[Value] {
        match item.get("threadId").and_then(Value::as_str) {
            Some(owner) if owner != self.thread_id() => &[],
            _ => self.list("runs"),
        }
    }

    pub fn run(&self, run_id: &str) -> Option<&Value> {
        self.list("runs")
            .iter()
            .find(|r| r.get("id").and_then(Value::as_str) == Some(run_id))
    }

    /// The run working right now, if any.
    pub fn active_run(&self) -> Option<&Value> {
        self.list("runs")
            .iter()
            .filter(|r| is_active_status(status(r)))
            .max_by_key(|r| ordinal(r))
    }

    /// When the work in progress began, as T3's working timer counts it: the active run's
    /// `workStartedAt`, else its `startedAt`, else its `requestedAt` (`activityStartedAt` in the
    /// nightly's web client). A steer joins the running run and changes none of them, so the
    /// timer runs on; `workStartedAt` can also predate the run, carried from the one before.
    pub fn activity_started_at(&self) -> Option<&str> {
        let run = self.active_run()?;
        ["workStartedAt", "startedAt", "requestedAt"]
            .iter()
            .find_map(|k| run.get(*k).and_then(Value::as_str))
    }

    pub fn latest_run(&self) -> Option<&Value> {
        self.list("runs").iter().max_by_key(|r| ordinal(r))
    }

    /// The provider session's error, when it says why the thread is stopped and nothing in the
    /// thread already says it: the thread's session is the last one on the thread's provider
    /// instance, as T3 picks it, and its `lastError` stands while no run is working and the
    /// latest run failed, or the session itself is in `error`. A failed run began before the
    /// error, so an older session error doesn't explain it. An `error` turn item of the latest
    /// run that carries the same words already shows it, whatever else it adds.
    pub fn session_error(&self) -> Option<String> {
        if self.active_run().is_some() {
            return None;
        }
        let thread = self.thread();
        let instance = thread
            .get("providerInstanceId")
            .and_then(Value::as_str)
            .or_else(|| {
                thread
                    .get("modelSelection")
                    .and_then(|m| m.get("instanceId"))
                    .and_then(Value::as_str)
            })?;
        let session = self
            .list("providerSessions")
            .iter()
            .rfind(|s| s.get("providerInstanceId").and_then(Value::as_str) == Some(instance))?;
        let message = session
            .get("lastError")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|m| !m.is_empty())?;
        let run = self.latest_run();
        let failed = run.is_some_and(|r| status(r) == "failed");
        if !failed && status(session) != "error" {
            return None;
        }
        if let Some(run) = run.filter(|_| failed) {
            let began = ["startedAt", "requestedAt"]
                .iter()
                .find_map(|k| run.get(*k).and_then(Value::as_str))
                .and_then(crate::inbox::parse_iso_ms);
            let updated = session
                .get("updatedAt")
                .and_then(Value::as_str)
                .and_then(crate::inbox::parse_iso_ms);
            if began
                .zip(updated)
                .is_some_and(|(began, updated)| updated < began)
            {
                return None;
            }
        }
        let words = |text: &str| {
            text.split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase()
        };
        let wanted = words(message);
        let run_id = run.and_then(|r| r.get("id"));
        let shown = self.items().into_iter().any(|item| {
            if item.get("type").and_then(Value::as_str) != Some("error")
                || run_id.is_none()
                || item.get("runId") != run_id
            {
                return false;
            }
            let said = item
                .get("failure")
                .and_then(|f| f.get("message"))
                .and_then(Value::as_str)
                .or_else(|| item.get("title").and_then(Value::as_str))
                .map(words)
                .unwrap_or_default();
            !said.is_empty() && (said.contains(&wanted) || wanted.contains(&said))
        });
        (!shown).then(|| message.to_string())
    }

    /// The run that owns the thread's work: the newest one working, else the newest run not
    /// held in a queue, as the nightly's `deriveThreadActivityRun` picks it
    /// (`packages/client-runtime/src/state/threadExecution.ts:101`). A newer queued run doesn't
    /// take over from one still working.
    pub fn activity_run(&self) -> Option<&Value> {
        self.active_run().or_else(|| {
            self.list("runs")
                .iter()
                .filter(|r| {
                    !(status(r) == "queued"
                        && r.get("queueHeld").and_then(Value::as_bool) == Some(true))
                })
                .max_by_key(|r| ordinal(r))
        })
    }

    /// Whether the thread holds the user message with this id.
    pub fn has_message(&self, message_id: &str) -> bool {
        let id = Some(message_id);
        self.list("messages")
            .iter()
            .any(|m| m.get("id").and_then(Value::as_str) == id)
            || self
                .list("runs")
                .iter()
                .any(|r| r.get("userMessageId").and_then(Value::as_str) == id)
    }

    pub fn run_for_message(&self, message_id: &str) -> Option<&Value> {
        let runs = self.list("runs");
        if let Some(run) = runs
            .iter()
            .find(|r| r.get("userMessageId").and_then(Value::as_str) == Some(message_id))
        {
            return Some(run);
        }
        let run_id = self
            .list("messages")
            .iter()
            .find(|m| m.get("id").and_then(Value::as_str) == Some(message_id))
            .and_then(|m| m.get("runId"))
            .and_then(Value::as_str)?;
        self.run(run_id)
    }

    pub fn pending_requests(&self) -> Vec<&Value> {
        self.list("runtimeRequests")
            .iter()
            .filter(|r| r.get("status").and_then(Value::as_str) == Some("pending"))
            .collect()
    }

    /// The turn item that shows a runtime request to the user, which carries its prompt and options.
    pub fn request_item(&self, request_id: &str) -> Option<&Value> {
        self.list("turnItems")
            .iter()
            .rev()
            .find(|i| i.get("requestId").and_then(Value::as_str) == Some(request_id))
    }

    /// The turn item a runtime request is about, such as the command an approval gates. Providers
    /// put the request's node under the node of that item.
    pub fn request_subject(&self, request_id: &str) -> Option<&Value> {
        let id_of = |v: &Value, key: &str| v.get(key).and_then(Value::as_str).map(str::to_string);
        let request = self
            .list("runtimeRequests")
            .iter()
            .find(|r| r.get("id").and_then(Value::as_str) == Some(request_id))?;
        let node_id = id_of(request, "nodeId")?;
        let parent = self
            .list("nodes")
            .iter()
            .find(|n| n.get("id").and_then(Value::as_str) == Some(node_id.as_str()))
            .and_then(|n| id_of(n, "parentNodeId"))?;
        self.list("turnItems")
            .iter()
            .chain(
                self.list("visibleTurnItems")
                    .iter()
                    .filter_map(|e| e.get("item")),
            )
            .find(|i| i.get("nodeId").and_then(Value::as_str) == Some(parent.as_str()))
    }
}

pub fn status(entity: &Value) -> &str {
    entity
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

fn ordinal(entity: &Value) -> u64 {
    entity.get("ordinal").and_then(Value::as_u64).unwrap_or(0)
}

pub fn is_active_status(status: &str) -> bool {
    matches!(status, "preparing" | "starting" | "running" | "waiting")
}

pub fn is_terminal_status(status: &str) -> bool {
    matches!(
        status,
        "completed" | "interrupted" | "failed" | "cancelled" | "rolled_back"
    )
}

/// Projects and threads, kept current from `orchestration.subscribeShell`.
#[derive(Debug, Clone, Default)]
pub struct ShellState {
    pub sequence: u64,
    pub projects: Vec<Value>,
    pub threads: Vec<Value>,
    pub synchronized: bool,
}

impl ShellState {
    pub fn from_snapshot(snapshot: &Value) -> ShellState {
        let list = |key: &str| {
            snapshot
                .get(key)
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        };
        ShellState {
            sequence: snapshot
                .get("snapshotSequence")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            projects: list("projects")
                .into_iter()
                .filter(|p| p.get("deletedAt").is_none_or(Value::is_null))
                .collect(),
            threads: list("threads"),
            synchronized: false,
        }
    }

    /// An enrichment snapshot only refreshes repository identity; its thread list is empty by design.
    fn apply_enrichment(&mut self, snapshot: &Value, roots: &[Value]) {
        let candidates = snapshot
            .get("projects")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for project in &mut self.projects {
            let Some(candidate) = candidates.iter().find(|c| c.get("id") == project.get("id"))
            else {
                continue;
            };
            if candidate.get("workspaceRoot") != project.get("workspaceRoot") {
                continue;
            }
            let resolved = roots
                .iter()
                .any(|r| Some(r) == project.get("workspaceRoot"));
            let identity = candidate
                .get("repositoryIdentity")
                .cloned()
                .unwrap_or(Value::Null);
            let missing = project.get("repositoryIdentity").is_none_or(Value::is_null);
            if resolved || (missing && !identity.is_null()) {
                project["repositoryIdentity"] = identity;
            }
        }
    }

    pub fn apply(&mut self, item: &Value) -> Applied {
        let kind = item.get("kind").and_then(Value::as_str).unwrap_or_default();
        match kind {
            "synchronized" => {
                self.synchronized = true;
                return Applied::Synchronized;
            }
            "snapshot" => {
                let Some(snapshot) = item.get("snapshot") else {
                    return Applied::Ignored;
                };
                if let Some(roots) = item
                    .get("resolvedRepositoryIdentityRoots")
                    .and_then(Value::as_array)
                {
                    self.apply_enrichment(snapshot, roots);
                    return Applied::Event("enrichment".into());
                }
                let synchronized = self.synchronized;
                *self = ShellState::from_snapshot(snapshot);
                self.synchronized = synchronized;
                return Applied::Snapshot;
            }
            _ => {}
        }
        let sequence = item.get("sequence").and_then(Value::as_u64).unwrap_or(0);
        if sequence <= self.sequence {
            return Applied::Duplicate;
        }
        self.sequence = sequence;
        let id_of = |v: &Value| v.get("id").and_then(Value::as_str).map(str::to_string);
        match kind {
            "project.updated" => {
                if let Some(project) = item.get("project").cloned() {
                    upsert(&mut self.projects, project);
                }
            }
            "project.removed" => {
                let id = item.get("projectId").and_then(Value::as_str);
                self.projects
                    .retain(|p| p.get("id").and_then(Value::as_str) != id);
            }
            "thread.updated" => {
                let Some(thread) = item.get("thread").cloned() else {
                    return Applied::Ignored;
                };
                if item.get("location").and_then(Value::as_str) == Some("archive") {
                    let id = id_of(&thread);
                    self.threads.retain(|t| id_of(t) != id);
                } else {
                    upsert(&mut self.threads, thread);
                }
            }
            "thread.removed" => {
                let id = item.get("threadId").and_then(Value::as_str);
                self.threads
                    .retain(|t| t.get("id").and_then(Value::as_str) != id);
            }
            _ => return Applied::Ignored,
        }
        Applied::Event(kind.to_string())
    }
}

/// Applies one item from T3's config subscription, `subscribeServerConfig`, to `config`, as T3's
/// own client does in packages/client-runtime/src/state/serverConfigProjection.ts.
///
/// A snapshot replaces the whole configuration, so a provider it leaves out is gone. T3 sends
/// one when a subscription opens and again whenever the configuration changes as a whole.
/// `providerStatuses` replaces the whole provider list, never one provider. `settingsUpdated`
/// replaces the settings, and `keybindingsUpdated` the keybindings and their issues. Each
/// leaves every other field as it was, fields this build doesn't know included.
///
/// A change that comes before any configuration has nothing to apply to, and is ignored. So is
/// an item of another version or type, such as the theme and usage-limit events tr3x doesn't
/// ask for, and one that lacks the part it changes. None of them touches the configuration held.
pub fn apply_config(config: &mut Option<Value>, item: &Value) -> Applied {
    if item.get("version").and_then(Value::as_u64) != Some(1) {
        return Applied::Ignored;
    }
    let kind = item.get("type").and_then(Value::as_str).unwrap_or_default();
    if kind == "snapshot" {
        let Some(snapshot) = item.get("config").filter(|c| c["providers"].is_array()) else {
            return Applied::Ignored;
        };
        *config = Some(snapshot.clone());
        return Applied::Snapshot;
    }
    let Some(held) = config.as_mut().and_then(Value::as_object_mut) else {
        return Applied::Ignored;
    };
    let payload = &item["payload"];
    let keybindings = payload["keybindings"].is_array() && payload["issues"].is_array();
    match kind {
        "providerStatuses" if payload["providers"].is_array() => {
            held.insert("providers".into(), payload["providers"].clone());
        }
        "settingsUpdated" if payload["settings"].is_object() => {
            held.insert("settings".into(), payload["settings"].clone());
        }
        "keybindingsUpdated" if keybindings => {
            held.insert("keybindings".into(), payload["keybindings"].clone());
            held.insert("issues".into(), payload["issues"].clone());
        }
        _ => return Applied::Ignored,
    }
    Applied::Event(kind.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn snapshot() -> Value {
        json!({
            "kind": "snapshot",
            "snapshotSequence": 10,
            "projection": {
                "thread": {"id": "t1", "title": "Test"},
                "runs": [{"id": "r1", "ordinal": 1, "status": "completed", "userMessageId": "m1"}],
                "runtimeRequests": [],
                "turnItems": [{"id": "i1", "type": "user_message", "ordinal": 0, "text": "hi"}],
                "visibleTurnItems": [{"position": 0, "item": {"id": "i1", "type": "user_message", "ordinal": 0, "text": "hi"}}]
            }
        })
    }

    fn event(sequence: u64, event_type: &str, payload: Value) -> Value {
        json!({"kind": "event", "sequence": sequence, "event": {"type": event_type, "payload": payload}})
    }

    #[test]
    fn a_pull_request_link_keeps_the_thread() {
        let mut state = ThreadState::from_snapshot(&snapshot()).unwrap();
        let link = |state: &str| {
            json!({"host": "github.com", "repository": "o/r", "number": 6,
            "url": "https://github.com/o/r/pull/6", "source": "agent", "state": state})
        };
        state.apply(&event(11, "thread.pull-request-link-updated", link("open")));
        state.apply(&event(
            12,
            "thread.pull-request-link-updated",
            link("merged"),
        ));
        assert_eq!(state.thread_id(), "t1");
        assert_eq!(state.thread()["title"], "Test");
        assert_eq!(state.thread()["pullRequests"], json!([link("merged")]));

        state.apply(&event(
            13,
            "thread.something-new",
            json!({"threadId": "t1"}),
        ));
        assert_eq!(
            state.thread_id(),
            "t1",
            "a partial thread event leaves the thread alone"
        );
        state.apply(&event(
            14,
            "thread.created",
            json!({"id": "t1", "title": "Renamed"}),
        ));
        assert_eq!(state.thread()["title"], "Renamed");
    }

    #[test]
    fn applies_events_as_upserts_and_skips_replayed_sequences() {
        let mut state = ThreadState::default();
        assert_eq!(state.apply(&snapshot()), Applied::Snapshot);
        let delta = |text: &str| json!({"id": "i2", "type": "assistant_message", "ordinal": 1, "text": text, "streaming": true});

        assert_eq!(
            state.apply(&event(11, "turn-item.updated", delta("Hel"))),
            Applied::Event("turn-item.updated".into())
        );
        assert_eq!(
            state.apply(&event(12, "turn-item.updated", delta("Hello"))),
            Applied::Event("turn-item.updated".into())
        );
        // A reconnect replays from the last sequence the client confirmed; overlap must not double-apply.
        assert_eq!(
            state.apply(&event(12, "turn-item.updated", delta("stale"))),
            Applied::Duplicate
        );

        let items = state.items();
        assert_eq!(items.len(), 2);
        assert_eq!(items[1]["text"], "Hello");
        assert_eq!(state.list("turnItems").len(), 2);
    }

    #[test]
    fn finds_a_message_by_its_run_or_its_message_row() {
        let mut state = ThreadState::default();
        state.apply(&snapshot());
        assert!(state.has_message("m1"));
        assert!(!state.has_message("m9"));
        // A message T3 took late, after the send had given up waiting.
        state.apply(&event(
            11,
            "message.updated",
            json!({"id": "m9", "role": "user", "text": "late"}),
        ));
        assert!(state.has_message("m9"));
    }

    #[test]
    fn tracks_runs_and_pending_requests() {
        let mut state = ThreadState::from_snapshot(&snapshot()).unwrap();
        state.apply(&event(
            11,
            "run.created",
            json!({"id": "r2", "ordinal": 2, "status": "running", "userMessageId": "m2"}),
        ));
        state.apply(&event(
            12,
            "runtime-request.updated",
            json!({"id": "q1", "status": "pending", "kind": "command"}),
        ));
        assert_eq!(state.active_run().unwrap()["id"], "r2");
        assert_eq!(state.run_for_message("m2").unwrap()["id"], "r2");
        assert_eq!(state.pending_requests().len(), 1);

        state.apply(&event(
            13,
            "runtime-request.updated",
            json!({"id": "q1", "status": "resolved", "kind": "command"}),
        ));
        state.apply(&event(
            14,
            "run.updated",
            json!({"id": "r2", "ordinal": 2, "status": "completed", "userMessageId": "m2"}),
        ));
        assert!(state.pending_requests().is_empty());
        assert!(state.active_run().is_none());
    }

    /// As this Mac's T3 reported a run that steers had joined: work began before the run did.
    #[test]
    fn work_starts_when_t3_says_it_did() {
        let mut state = ThreadState::from_snapshot(&snapshot()).unwrap();
        let run = |seq: u64, extra: Value| {
            let mut run = json!({"id": "r2", "ordinal": 2, "status": "running",
                "requestedAt": "2026-10-31T14:16:57.057Z"});
            run.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            event(seq, "run.updated", run)
        };
        state.apply(&run(11, json!({})));
        assert_eq!(
            state.activity_started_at(),
            Some("2026-10-31T14:16:57.057Z")
        );
        state.apply(&run(12, json!({"startedAt": "2026-10-31T14:31:28.413Z"})));
        assert_eq!(
            state.activity_started_at(),
            Some("2026-10-31T14:31:28.413Z")
        );
        state.apply(&run(
            13,
            json!({"startedAt": "2026-10-31T14:31:28.413Z",
            "workStartedAt": "2026-10-31T14:16:58.126Z"}),
        ));
        assert_eq!(
            state.activity_started_at(),
            Some("2026-10-31T14:16:58.126Z")
        );
        // Queued isn't working, so there's no timer.
        state.apply(&event(
            14,
            "run.updated",
            json!({"id": "r2", "ordinal": 2, "status": "queued",
            "requestedAt": "2026-10-31T14:16:57.057Z"}),
        ));
        assert_eq!(state.activity_started_at(), None);
    }

    #[test]
    fn the_activity_run_is_the_one_working_before_any_newer_queued_run() {
        // r1 finished. With nothing working, the newest run stands for the thread.
        let mut state = ThreadState::from_snapshot(&snapshot()).unwrap();
        assert_eq!(state.activity_run().unwrap()["id"], "r1");

        // A follow-up queued while r2 works doesn't take over from it.
        state.apply(&event(
            11,
            "run.created",
            json!({"id": "r2", "ordinal": 2, "status": "running"}),
        ));
        state.apply(&event(
            12,
            "run.created",
            json!({"id": "r3", "ordinal": 3, "status": "queued"}),
        ));
        assert_eq!(state.activity_run().unwrap()["id"], "r2");

        // Once r2 ends, the queued run is the newest, unless its queue waits on the user.
        state.apply(&event(
            13,
            "run.updated",
            json!({"id": "r2", "ordinal": 2, "status": "completed"}),
        ));
        assert_eq!(state.activity_run().unwrap()["id"], "r3");
        state.apply(&event(
            14,
            "run.updated",
            json!({"id": "r3", "ordinal": 3, "status": "queued", "queueHeld": true}),
        ));
        assert_eq!(state.activity_run().unwrap()["id"], "r2");

        // A thread that never ran has none.
        let mut empty = snapshot();
        empty["projection"]["runs"] = json!([]);
        assert!(
            ThreadState::from_snapshot(&empty)
                .unwrap()
                .activity_run()
                .is_none()
        );
    }

    #[test]
    fn finds_the_command_each_request_gates() {
        let mut state = ThreadState::from_snapshot(&snapshot()).unwrap();
        let mut sequence = 10;
        let mut apply = |event_type: &str, payload: Value| {
            sequence += 1;
            state.apply(&event(sequence, event_type, payload));
        };
        // Two commands wait at once. The newer one's request is listed first.
        for (n, command) in [(1, "rm -rf build"), (2, "ls")] {
            apply(
                "turn-item.updated",
                json!({"id": format!("c{n}"), "type": "command_execution", "ordinal": n,
                       "status": "running", "nodeId": format!("tool{n}"), "input": command}),
            );
            apply(
                "node.updated",
                json!({"id": format!("ask{n}"), "parentNodeId": format!("tool{n}")}),
            );
        }
        for n in [2, 1] {
            apply(
                "runtime-request.updated",
                json!({"id": format!("q{n}"), "nodeId": format!("ask{n}"),
                       "status": "pending", "kind": "command"}),
            );
        }
        assert_eq!(
            state.request_subject("q1").unwrap()["input"],
            "rm -rf build"
        );
        assert_eq!(state.request_subject("q2").unwrap()["input"], "ls");
        assert!(state.request_subject("missing").is_none());
    }

    #[test]
    fn thread_events_replace_the_thread_and_unknown_events_still_advance() {
        let mut state = ThreadState::from_snapshot(&snapshot()).unwrap();
        state.apply(&event(
            11,
            "thread.metadata-updated",
            json!({"id": "t1", "title": "Renamed"}),
        ));
        assert_eq!(state.thread()["title"], "Renamed");
        assert_eq!(
            state.apply(&event(12, "brand-new.event", json!({}))),
            Applied::Event("brand-new.event".into())
        );
        assert_eq!(state.sequence, 12);
    }

    #[test]
    fn shell_moves_archived_threads_out_of_the_active_list() {
        let mut shell = ShellState::from_snapshot(&json!({
            "snapshotSequence": 5,
            "projects": [{"id": "p1"}, {"id": "p2", "deletedAt": "2026-01-01T00:00:00Z"}],
            "threads": [{"id": "t1", "title": "One"}]
        }));
        assert_eq!(shell.projects.len(), 1);
        shell.apply(&json!({"kind": "thread.updated", "sequence": 6, "location": "active", "thread": {"id": "t2"}}));
        shell.apply(&json!({"kind": "thread.updated", "sequence": 7, "location": "archive", "thread": {"id": "t1"}}));
        assert_eq!(shell.apply(&json!({"kind": "thread.removed", "sequence": 7, "location": "active", "threadId": "t2"})), Applied::Duplicate);
        let ids: Vec<_> = shell
            .threads
            .iter()
            .map(|t| t["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["t2"]);
    }

    #[test]
    fn a_detached_provider_session_leaves_the_thread() {
        let mut state = ThreadState::default();
        state.apply(&snapshot());
        let attach = |sequence, id: &str| {
            let session = json!({"id": id, "providerInstanceId": "codex", "lastError": "boom"});
            event(sequence, "provider-session.attached", session)
        };
        let detach = |sequence, payload| event(sequence, "provider-session.detached", payload);
        let ids = |state: &ThreadState| {
            state.projection["providerSessions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|session| session["id"].as_str().unwrap_or_default().to_string())
                .collect::<Vec<_>>()
        };
        state.apply(&attach(11, "s1"));
        state.apply(&attach(12, "s2"));

        assert_eq!(
            state.apply(&detach(13, json!({"providerSessionId": "s1"}))),
            Applied::Event("provider-session.detached".into())
        );
        assert_eq!(ids(&state), ["s2"]);
        // The detach names the session rather than being one, so it never lands in the list.
        state.apply(&detach(14, json!({"providerSessionId": "s9"})));
        state.apply(&detach(15, json!({"threadId": "t1"})));
        assert_eq!(ids(&state), ["s2"]);
        assert_eq!(
            state.apply(&detach(13, json!({"providerSessionId": "s2"}))),
            Applied::Duplicate
        );
        assert_eq!(ids(&state), ["s2"]);
        state.apply(&detach(16, json!({"providerSessionId": "s2"})));
        assert!(ids(&state).is_empty());
    }

    /// A thread as T3's V2 projection sends it after a run failed at the provider.
    fn failed_thread() -> ThreadState {
        ThreadState::from_snapshot(&json!({
            "kind": "snapshot",
            "snapshotSequence": 10,
            "projection": {
                "thread": {"id": "t1", "title": "Test", "providerInstanceId": "codex",
                    "modelSelection": {"instanceId": "codex", "model": "gpt-5"}},
                "runs": [{"id": "r1", "ordinal": 1, "status": "failed", "userMessageId": "m1",
                    "requestedAt": "2026-11-04T01:00:00.000Z", "startedAt": "2026-11-04T01:00:01.000Z",
                    "completedAt": "2026-11-04T01:00:09.000Z"}],
                "runtimeRequests": [],
                "providerSessions": [],
                "turnItems": [{"id": "i1", "type": "user_message", "ordinal": 0, "text": "hi"}],
                "visibleTurnItems": [{"position": 0, "item": {"id": "i1", "type": "user_message", "ordinal": 0, "text": "hi"}}]
            }
        }))
        .unwrap()
    }

    fn session(id: &str, instance: &str, status: &str, updated: &str, error: Value) -> Value {
        json!({"id": id, "driver": "codex", "providerInstanceId": instance, "status": status,
            "cwd": "/work/app", "model": "gpt-5", "capabilities": {},
            "createdAt": "2026-11-04T00:59:00.000Z", "updatedAt": updated, "lastError": error})
    }

    fn error_item(run: &str, message: &str) -> Value {
        json!({"id": format!("e-{run}"), "type": "error", "runId": run, "nodeId": "n1", "ordinal": 3,
            "status": "failed", "updatedAt": "2026-11-04T01:00:09.000Z",
            "failure": {"message": message, "class": "provider_error"}})
    }

    #[test]
    fn a_session_error_explains_a_failed_run_that_has_no_error_item() {
        let mut state = failed_thread();
        assert_eq!(state.session_error(), None);
        let failed = session(
            "s1",
            "codex",
            "error",
            "2026-11-04T01:00:08.000Z",
            json!("Codex isn't signed in. Run `codex login`, then send again."),
        );
        state.apply(&event(11, "provider-session.updated", failed));
        assert_eq!(
            state.session_error().as_deref(),
            Some("Codex isn't signed in. Run `codex login`, then send again.")
        );
    }

    #[test]
    fn an_error_item_with_the_same_words_already_shows_it() {
        let mut state = failed_thread();
        state.apply(&event(
            11,
            "provider-session.updated",
            session(
                "s1",
                "codex",
                "error",
                "2026-11-04T01:00:08.000Z",
                json!("Rate  limited by the provider"),
            ),
        ));
        assert!(state.session_error().is_some());
        // The turn item wraps the message in more words and its own spacing and case.
        state.apply(&event(
            12,
            "turn-item.updated",
            error_item("r1", "Turn failed: rate limited by the provider (429)"),
        ));
        assert_eq!(state.session_error(), None);
    }

    #[test]
    fn an_error_item_saying_something_else_leaves_the_session_error_showing() {
        let mut state = failed_thread();
        state.apply(&event(
            11,
            "provider-session.updated",
            session(
                "s1",
                "codex",
                "error",
                "2026-11-04T01:00:08.000Z",
                json!("Not signed in"),
            ),
        ));
        state.apply(&event(
            12,
            "turn-item.updated",
            error_item("r1", "Process exited with code 1"),
        ));
        assert_eq!(state.session_error().as_deref(), Some("Not signed in"));
        // An error of an earlier run doesn't count as this run's.
        let mut state = failed_thread();
        state.apply(&event(
            11,
            "provider-session.updated",
            session(
                "s1",
                "codex",
                "error",
                "2026-11-04T01:00:08.000Z",
                json!("Not signed in"),
            ),
        ));
        state.apply(&event(
            12,
            "turn-item.updated",
            error_item("r0", "Not signed in"),
        ));
        assert_eq!(state.session_error().as_deref(), Some("Not signed in"));
    }

    #[test]
    fn only_the_session_of_the_threads_provider_counts_and_the_last_one() {
        let mut state = failed_thread();
        state.apply(&event(
            11,
            "provider-session.updated",
            session(
                "s0",
                "claudeAgent",
                "error",
                "2026-11-04T01:00:08.000Z",
                json!("Claude is down"),
            ),
        ));
        assert_eq!(state.session_error(), None);
        state.apply(&event(
            12,
            "provider-session.updated",
            session(
                "s1",
                "codex",
                "error",
                "2026-11-04T01:00:05.000Z",
                json!("old codex error"),
            ),
        ));
        state.apply(&event(
            13,
            "provider-session.attached",
            session(
                "s2",
                "codex",
                "error",
                "2026-11-04T01:00:08.000Z",
                json!("new codex error"),
            ),
        ));
        assert_eq!(state.session_error().as_deref(), Some("new codex error"));
        // Detached, it no longer stands for the thread; the one before it doesn't either.
        state.apply(&event(
            14,
            "provider-session.detached",
            json!({"providerSessionId": "s2", "detachedAt": "2026-11-04T01:01:00.000Z"}),
        ));
        assert_eq!(state.session_error().as_deref(), Some("old codex error"));
    }

    #[test]
    fn a_session_error_from_before_the_failed_run_is_stale() {
        let mut state = failed_thread();
        state.apply(&event(
            11,
            "provider-session.updated",
            session(
                "s1",
                "codex",
                "error",
                "2026-11-04T00:50:00.000Z",
                json!("Not signed in"),
            ),
        ));
        assert_eq!(state.session_error(), None);
    }

    #[test]
    fn a_working_thread_or_a_recovered_session_shows_no_error() {
        let mut state = failed_thread();
        state.apply(&event(
            11,
            "provider-session.updated",
            session(
                "s1",
                "codex",
                "error",
                "2026-11-04T01:00:08.000Z",
                json!("Not signed in"),
            ),
        ));
        // A new run is working: the error belongs to the one before.
        state.apply(&event(
            12,
            "run.created",
            json!({"id": "r2", "ordinal": 2, "status": "running",
            "requestedAt": "2026-11-04T01:05:00.000Z"}),
        ));
        assert_eq!(state.session_error(), None);
        // It completed and the session is ready again, with the old words still on it.
        state.apply(&event(
            13,
            "run.updated",
            json!({"id": "r2", "ordinal": 2, "status": "completed",
            "requestedAt": "2026-11-04T01:05:00.000Z"}),
        ));
        state.apply(&event(
            14,
            "provider-session.updated",
            session(
                "s1",
                "codex",
                "ready",
                "2026-11-04T01:05:30.000Z",
                json!("Not signed in"),
            ),
        ));
        assert_eq!(state.session_error(), None);
        // Blank or missing errors say nothing.
        state.apply(&event(
            15,
            "provider-session.updated",
            session(
                "s1",
                "codex",
                "error",
                "2026-11-04T01:06:00.000Z",
                json!("  "),
            ),
        ));
        assert_eq!(state.session_error(), None);
        state.apply(&event(
            16,
            "provider-session.updated",
            session(
                "s1",
                "codex",
                "error",
                "2026-11-04T01:06:00.000Z",
                Value::Null,
            ),
        ));
        assert_eq!(state.session_error(), None);
    }

    #[test]
    fn a_session_in_error_after_a_finished_run_still_says_why() {
        let mut state = failed_thread();
        state.apply(&event(
            11,
            "run.updated",
            json!({"id": "r1", "ordinal": 1, "status": "completed"}),
        ));
        state.apply(&event(
            12,
            "provider-session.updated",
            session(
                "s1",
                "codex",
                "error",
                "2026-11-04T02:00:00.000Z",
                json!("Session expired"),
            ),
        ));
        assert_eq!(state.session_error().as_deref(), Some("Session expired"));
    }

    #[test]
    fn enrichment_snapshot_keeps_threads_and_sequence() {
        let mut shell = ShellState::default();
        shell.apply(&json!({"kind": "snapshot", "snapshot": {
            "snapshotSequence": 5, "projects": [{"id": "p1", "workspaceRoot": "/r"}], "threads": [{"id": "t1"}]
        }}));
        shell.apply(&json!({"kind": "snapshot", "resolvedRepositoryIdentityRoots": ["/r"], "snapshot": {
            "snapshotSequence": 9, "projects": [{"id": "p1", "workspaceRoot": "/r", "repositoryIdentity": {"name": "r"}}], "threads": []
        }}));
        assert_eq!(shell.threads.len(), 1);
        assert_eq!(shell.sequence, 5);
        assert_eq!(shell.projects[0]["repositoryIdentity"]["name"], "r");
    }

    /// A snapshot from T3's config subscription.
    fn config_snapshot(config: Value) -> Value {
        json!({"version": 1, "type": "snapshot", "config": config})
    }

    /// A change from T3's config subscription.
    fn config_change(kind: &str, payload: Value) -> Value {
        json!({"version": 1, "type": kind, "payload": payload})
    }

    fn provider(id: &str, status: &str) -> Value {
        let model = format!("{id}-model");
        json!({"instanceId": id, "status": status, "enabled": true, "models": [{"slug": model}]})
    }

    /// A configuration with every field the TUI reads and some it doesn't, including one no
    /// build knows yet.
    fn full_config(providers: Value) -> Value {
        json!({
            "environment": {"capabilities": {"threadSnooze": true, "threadSettlement": true}},
            "providers": providers,
            "settings": {"enableAssistantStreaming": true, "providers": {"codex": {}}},
            "keybindings": [{"key": "mod+k", "command": "commandPalette.toggle"}],
            "issues": [],
            "availableEditors": ["cursor"],
            "futureField": {"kept": true},
        })
    }

    #[test]
    fn a_config_snapshot_replaces_the_whole_config_and_drops_providers_it_leaves_out() {
        let mut config = None;
        let providers = json!([provider("codex", "ready"), provider("claude", "ready")]);
        let first = full_config(providers);
        assert_eq!(
            apply_config(&mut config, &config_snapshot(first.clone())),
            Applied::Snapshot
        );
        assert_eq!(config.as_ref(), Some(&first));

        // A later snapshot is the whole configuration, so what it leaves out is gone, fields
        // and providers alike.
        let providers = json!([provider("codex", "error")]);
        let second = json!({"environment": {"capabilities": {}}, "providers": providers});
        assert_eq!(
            apply_config(&mut config, &config_snapshot(second.clone())),
            Applied::Snapshot
        );
        assert_eq!(config.as_ref(), Some(&second));
    }

    #[test]
    fn config_changes_replace_their_own_fields_and_keep_the_rest() {
        let providers = json!([provider("codex", "ready"), provider("claude", "ready")]);
        let before = full_config(providers);
        let mut config = None;
        apply_config(&mut config, &config_snapshot(before.clone()));

        // The new list is the whole list: Codex is gone though the change doesn't name it.
        let providers = json!([provider("claude", "error"), provider("cursor", "ready")]);
        let change = config_change("providerStatuses", json!({"providers": providers}));
        assert_eq!(
            apply_config(&mut config, &change),
            Applied::Event("providerStatuses".into())
        );
        let mut expected = before.clone();
        expected["providers"] = providers;
        assert_eq!(config.as_ref(), Some(&expected));

        let settings = json!({"enableAssistantStreaming": false});
        let change = config_change("settingsUpdated", json!({"settings": settings}));
        assert_eq!(
            apply_config(&mut config, &change),
            Applied::Event("settingsUpdated".into())
        );
        expected["settings"] = settings;
        assert_eq!(config.as_ref(), Some(&expected));

        let keybindings = json!([]);
        let issues = json!([{"kind": "keybindings.malformed-config", "message": "bad"}]);
        let payload = json!({"keybindings": keybindings, "issues": issues});
        let change = config_change("keybindingsUpdated", payload);
        assert_eq!(
            apply_config(&mut config, &change),
            Applied::Event("keybindingsUpdated".into())
        );
        expected["keybindings"] = keybindings;
        expected["issues"] = issues;
        assert_eq!(config.as_ref(), Some(&expected));
    }

    #[test]
    fn config_items_that_change_nothing_leave_the_config_as_it_was() {
        // A change before any snapshot has no configuration to apply to.
        let mut config = None;
        let providers = json!([provider("codex", "ready")]);
        let early = config_change("providerStatuses", json!({"providers": providers}));
        assert_eq!(apply_config(&mut config, &early), Applied::Ignored);
        assert_eq!(config, None);
        let malformed = config_snapshot(json!({"environment": {}}));
        assert_eq!(apply_config(&mut config, &malformed), Applied::Ignored);
        assert_eq!(config, None);

        let good = full_config(providers);
        apply_config(&mut config, &config_snapshot(good.clone()));
        for item in [
            // Another version, or none.
            json!({"version": 2, "type": "snapshot", "config": {"providers": []}}),
            json!({"type": "providerStatuses", "payload": {"providers": []}}),
            // Snapshots without a provider list.
            config_snapshot(json!({"environment": {}})),
            config_snapshot(json!({"providers": {"codex": {}}})),
            config_snapshot(Value::Null),
            json!({"version": 1, "type": "snapshot"}),
            // Changes without the part they change.
            config_change("providerStatuses", json!({})),
            config_change("providerStatuses", json!({"providers": null})),
            json!({"version": 1, "type": "providerStatuses"}),
            config_change("settingsUpdated", json!({"settings": []})),
            config_change("keybindingsUpdated", json!({"keybindings": []})),
            config_change(
                "keybindingsUpdated",
                json!({"keybindings": {}, "issues": []}),
            ),
            // Events tr3x doesn't ask for, or that no build knows yet.
            config_change("environmentThemesUpdated", json!({"themes": []})),
            config_change("usageLimitSourcesUpdated", json!({"sources": []})),
            config_change("somethingNew", json!({"providers": []})),
            // Not a config item at all.
            json!({"kind": "synchronized"}),
            json!("snapshot"),
        ] {
            let applied = apply_config(&mut config, &item);
            assert_eq!(applied, Applied::Ignored, "{item}");
            assert_eq!(config.as_ref(), Some(&good), "{item}");
        }
    }
}
