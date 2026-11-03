//! One machine's T3: its connection, and the operations tr3x's panes and sync use. Forked
//! from t3term's `client.rs` (krishhgg/t3term, MIT), with t3term's login replaced by the
//! pairing in `auth` and `pair`: a token from `credentials.json`, sent only to a server whose
//! descriptor names the machine it was issued for.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use serde_json::{Value, json};
use tokio::sync::{Mutex, mpsc};

use crate::auth::now_ms;
use crate::descriptor::{self, Descriptor, Local};
use crate::error::{code_of, err, err_exit, exit};
use crate::http::Api;
use crate::models::Plan;
use crate::projection::{ShellState, ThreadState};
use crate::rpc::{RpcClient, RpcError, StreamLimits};
use crate::store::{Machine, Store, Via};

pub(crate) const DISPATCH_TIMEOUT: Duration = Duration::from_secs(60);
/// Pictures go up whole, up to 10 MB each, sometimes over a slow link.
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(180);
const MAX_BACKOFF: Duration = Duration::from_secs(10);

pub struct Env {
    machine: Machine,
    store: Store,
    descriptor: std::sync::Mutex<Option<Descriptor>>,
    rpc: Mutex<Option<RpcClient>>,
    /// When renewing the token last failed, so a token still good isn't renewed on every call.
    renew_failed: std::sync::Mutex<Option<i64>>,
    /// Tests reach a fake T3 at this address, with no descriptor or token.
    fixed: Option<Api>,
}

/// What to do when a message arrives while the thread is already working.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IfBusy {
    Refuse,
    Queue,
    Steer,
}

#[derive(Debug, Clone)]
pub struct SendReceipt {
    pub message_id: String,
    pub sequence: u64,
    pub dispatch_mode: &'static str,
}

/// How long a failed renewal waits before the next try, while the token still works.
const RENEW_RETRY_MS: i64 = 10 * 60 * 1000;

/// The error for a machine tr3x has no working pairing with.
pub fn needs_pairing(machine: &Machine) -> anyhow::Error {
    let name = machine.name();
    let quoted = if name.contains(char::is_whitespace) {
        format!("\"{name}\"")
    } else {
        name.to_string()
    };
    err_exit(
        "PAIRING_NEEDED",
        exit::REJECTED,
        format!("Rex isn't paired with {name}. Run `tr3x pair {quoted}`."),
    )
}

/// Whether `error` means the machine needs pairing again, rather than being out of reach.
pub fn is_pairing_error(error: &anyhow::Error) -> bool {
    matches!(
        code_of(error),
        Some("PAIRING_NEEDED" | "T3_UNAUTHORIZED" | "PAIRING_REFUSED" | "ENVIRONMENT_CHANGED")
    )
}

impl Env {
    pub fn new(store: Store, machine: Machine) -> Env {
        Env {
            machine,
            store,
            descriptor: std::sync::Mutex::new(None),
            rpc: Mutex::new(None),
            renew_failed: std::sync::Mutex::new(None),
            fixed: None,
        }
    }

    pub fn id(&self) -> &str {
        &self.machine.id
    }

    /// What Rex calls the machine: its alias, else T3's label.
    pub fn name(&self) -> &str {
        self.machine.name()
    }

    pub fn machine(&self) -> &Machine {
        &self.machine
    }

    pub fn is_local(&self) -> bool {
        self.machine.via == Via::Local
    }

    /// The descriptor of the last connection, if there has been one.
    pub fn descriptor(&self) -> Option<Descriptor> {
        self.descriptor.lock().ok()?.clone()
    }

    pub fn has_capability(&self, name: &str) -> bool {
        self.descriptor().is_some_and(|d| d.has_capability(name))
    }

    pub fn capabilities(&self) -> Value {
        self.descriptor()
            .map(|d| d.capabilities)
            .unwrap_or(Value::Null)
    }

    /// T3's `userdata` folder, on this machine alone: where its attachments are.
    pub fn state_dir(&self) -> Option<PathBuf> {
        self.is_local().then(|| Local::read().state_dir)
    }

    /// Where the machine may answer, best first. This machine's server says where it listens.
    fn origins(&self) -> Vec<String> {
        let mut origins = Vec::new();
        if self.is_local() {
            origins.push(Local::read().origin_or_default());
        }
        for origin in &self.machine.origins {
            let origin = descriptor::normalize_origin(origin);
            if !origins.contains(&origin) {
                origins.push(origin);
            }
        }
        origins
    }

    /// The first address that answers as this machine, on the protocol tr3x speaks.
    pub async fn route(&self) -> Result<Descriptor> {
        let mut last = None;
        let mut origins = self.origins();
        if let Some(last) = self.descriptor()
            && let Some(index) = origins.iter().position(|origin| origin == &last.origin)
        {
            let first = origins.remove(index);
            origins.insert(0, first);
        }
        for origin in origins {
            match descriptor::fetch(&origin).await {
                Ok(found) => {
                    if let Err(e) = found.require_environment(&self.machine.id) {
                        last = Some(e);
                        continue;
                    }
                    found.require_supported_protocol()?;
                    if let Ok(mut held) = self.descriptor.lock() {
                        *held = Some(found.clone());
                    }
                    return Ok(found);
                }
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| {
            err_exit(
                "T3_UNREACHABLE",
                exit::UNAVAILABLE,
                format!("tr3x knows no address for {}.", self.name()),
            )
        }))
    }

    /// The token to send: the stored one, replaced first when it's close to running out.
    async fn token(&self) -> Result<String> {
        let now = now_ms();
        let resting = self
            .renew_failed
            .lock()
            .ok()
            .and_then(|f| *f)
            .is_some_and(|at| now - at < RENEW_RETRY_MS);
        match self.store.credential(&self.machine.id) {
            Some(c) if !c.due(now) || (resting && !c.expired(now)) => Ok(c.token),
            Some(c) => match self.pair_again(None).await {
                Ok(token) => Ok(token),
                Err(_) if !c.expired(now) => Ok(c.token),
                Err(e) => Err(e),
            },
            None => self.pair_again(None).await,
        }
    }

    /// Replaces the token, unless another tr3x process just did: with one the current token
    /// mints, else by pairing again the way the machine was paired. `rejected` is a token the
    /// server turned down.
    async fn pair_again(&self, rejected: Option<&str>) -> Result<String> {
        let _lock = self.store.lock().await?;
        let current = self.store.credential(&self.machine.id);
        if let Some(c) = &current
            && Some(c.token.as_str()) != rejected
            && !c.due(now_ms())
        {
            return Ok(c.token.clone());
        }
        let renewed = match &current {
            Some(c)
                if Some(c.token.as_str()) != rejected
                    && !c.expired(now_ms())
                    && c.renews_itself() =>
            {
                match self.route().await {
                    Ok(found) => {
                        crate::pair::renew_locked(&self.store, &self.machine, &found.origin).await
                    }
                    Err(e) => Err(e),
                }
            }
            _ => Err(needs_pairing(&self.machine)),
        };
        if let Err(e) = renewed {
            let again = match self.machine.via {
                Via::Link | Via::Join => Err(if is_pairing_error(&e) {
                    e
                } else {
                    needs_pairing(&self.machine)
                }),
                _ => crate::pair::again_locked(&self.store, &self.machine)
                    .await
                    .map(drop)
                    .map_err(|e| {
                        if self.is_local() {
                            e
                        } else {
                            e.context(format!(
                                "couldn't pair with {} again; run `tr3x pair {}`",
                                self.name(),
                                self.name()
                            ))
                        }
                    }),
            };
            if let Err(e) = again {
                if let Ok(mut failed) = self.renew_failed.lock() {
                    *failed = Some(now_ms());
                }
                return Err(e);
            }
        }
        if let Ok(mut failed) = self.renew_failed.lock() {
            *failed = None;
        }
        self.store
            .credential(&self.machine.id)
            .map(|c| c.token)
            .ok_or_else(|| needs_pairing(&self.machine))
    }

    /// The HTTP API at the machine's address, with its token.
    pub async fn api(&self) -> Result<Api> {
        if let Some(api) = &self.fixed {
            return Ok(api.clone());
        }
        let found = match self.descriptor() {
            Some(d) => d,
            None => self.route().await?,
        };
        Ok(Api::new(&found.origin, &self.token().await?))
    }

    /// Checks the machine answers and accepts tr3x, without opening a socket.
    pub async fn check(&self) -> Result<Descriptor> {
        let found = self.route().await?;
        let api = Api::new(&found.origin, &self.token().await?);
        match crate::auth::check_session(&api).await? {
            crate::auth::Checked::Accepted => Ok(found),
            crate::auth::Checked::Rejected => {
                self.pair_again(Some(api_token(&api))).await?;
                Ok(found)
            }
        }
    }

    /// Whether Rex's token for the machine was granted `scope`. A token keeps the scopes it was
    /// paired with when it renews, so a scope tr3x asks for later needs pairing again.
    pub fn holds(&self, scope: &str) -> bool {
        self.store
            .credential(&self.machine.id)
            .is_some_and(|c| c.scopes.iter().any(|s| s == scope))
    }

    /// Pairs again the way the machine was paired, for a token with every scope tr3x asks for
    /// now. Only this Mac and machines paired over SSH can; the rest say how to.
    pub async fn pair_fresh(&self) -> Result<()> {
        let _lock = self.store.lock().await?;
        crate::pair::again_locked(&self.store, &self.machine).await?;
        if let Some(rpc) = self.rpc.lock().await.take() {
            drop(rpc);
        }
        Ok(())
    }

    /// The shared RPC connection, reopened if it dropped. Each opening checks the address
    /// still answers as this machine before the token goes out.
    pub async fn rpc(&self) -> Result<RpcClient> {
        let mut slot = self.rpc.lock().await;
        if let Some(rpc) = slot.as_ref().filter(|rpc| !rpc.is_closed()) {
            return Ok(rpc.clone());
        }
        let rpc = self.open().await?;
        *slot = Some(rpc.clone());
        Ok(rpc)
    }

    async fn open(&self) -> Result<RpcClient> {
        if let Some(api) = &self.fixed {
            return RpcClient::connect(api).await;
        }
        let found = self.route().await?;
        let token = self.token().await?;
        match RpcClient::connect(&Api::new(&found.origin, &token)).await {
            Err(e) if code_of(&e) == Some("T3_UNAUTHORIZED") => {
                let token = self.pair_again(Some(&token)).await?;
                RpcClient::connect(&Api::new(&found.origin, &token)).await
            }
            other => other,
        }
    }

    /// Calls one RPC method and returns its answer.
    pub async fn call(&self, method: &str, payload: Value) -> Result<Value> {
        Ok(self
            .rpc()
            .await?
            .call(method, payload, DISPATCH_TIMEOUT)
            .await?)
    }

    pub async fn shell(&self) -> Result<ShellState> {
        Ok(ShellState::from_snapshot(
            &self.api().await?.get("/api/orchestration/shell").await?,
        ))
    }

    /// One thread over HTTP. `bounded` reads a recent window; runs and requests stay complete.
    pub async fn thread(&self, thread_id: &str, bounded: bool) -> Result<ThreadState> {
        let path = format!(
            "/api/orchestration/threads/{thread_id}{}",
            if bounded { "/bounded" } else { "" }
        );
        let snapshot = self.api().await?.get(&path).await.map_err(|e| match e
            .downcast_ref::<crate::error::T3Error>()
        {
            Some(t3) if t3.code == "T3_NOT_FOUND" => err_exit(
                "THREAD_NOT_FOUND",
                exit::NOT_FOUND,
                format!("No T3 Code thread has id {thread_id}."),
            ),
            _ => e,
        })?;
        ThreadState::from_snapshot(&snapshot).ok_or_else(|| {
            err(
                "T3_INVALID_SNAPSHOT",
                format!("T3 returned an invalid projection for thread {thread_id}."),
            )
        })
    }

    /// Dispatches one orchestration command and returns the event sequence T3 assigned it.
    pub async fn dispatch(&self, mut command: Value) -> Result<u64> {
        if command.get("commandId").is_none() {
            command["commandId"] = json!(uuid::Uuid::new_v4().to_string());
        }
        let command_type = command["type"].as_str().unwrap_or("command").to_string();
        let result = self
            .rpc()
            .await?
            .call("orchestration.dispatchCommand", command, DISPATCH_TIMEOUT)
            .await
            .map_err(|e| match e {
                RpcError::Failed { message, .. } => err_exit(
                    "THREAD_COMMAND_REJECTED",
                    exit::REJECTED,
                    format!("T3 rejected {command_type}: {message}"),
                ),
                other => other.into(),
            })?;
        result
            .get("sequence")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                err(
                    "T3_INVALID_DISPATCH",
                    format!("T3 returned no event sequence for {command_type}."),
                )
            })
    }

    /// The server's configuration, including every provider and model it offers.
    pub async fn server_config(&self) -> Result<Value> {
        Ok(self
            .rpc()
            .await?
            .call("server.getConfig", json!({}), DISPATCH_TIMEOUT)
            .await?)
    }

    /// One turn item with the output the projection leaves out. T3 strips a tool's output
    /// from the timeline so a large result can't stall the socket, marking the item
    /// `outputOmitted`, and hands it over one item at a time here. `revision` is the item's
    /// `updatedAt`, which keys T3's own cache.
    pub async fn turn_item(&self, thread_id: &str, item_id: &str, revision: &str) -> Result<Value> {
        let mut input = json!({ "threadId": thread_id, "itemId": item_id });
        if !revision.is_empty() {
            input["revision"] = json!(revision);
        }
        let result = self
            .rpc()
            .await?
            .call("orchestration.getTurnItem", input, DISPATCH_TIMEOUT)
            .await?;
        Ok(result.get("item").cloned().unwrap_or(Value::Null))
    }

    pub async fn send_message(
        &self,
        state: &ThreadState,
        text: &str,
        if_busy: IfBusy,
        images: &[Value],
    ) -> Result<SendReceipt> {
        self.send_message_with(state, text, if_busy, &Plan::default(), images)
            .await
    }

    /// Sends a message after applying a model and mode plan, the way the desktop composer does:
    /// mode changes go first as their own commands, and the model rides on the message.
    pub async fn send_message_with(
        &self,
        state: &ThreadState,
        text: &str,
        if_busy: IfBusy,
        plan: &Plan,
        images: &[Value],
    ) -> Result<SendReceipt> {
        let message_id = uuid::Uuid::new_v4().to_string();
        self.send_message_as(&message_id, state, text, if_busy, plan, images)
            .await
    }

    /// `send_message_with` under a message id the caller chose. A send that times out can still
    /// reach T3, and the id is how the caller finds out. `images` are uploads as
    /// `assets.persistChatAttachments` takes them; T3 stores them before the message goes.
    pub async fn send_message_as(
        &self,
        message_id: &str,
        state: &ThreadState,
        text: &str,
        if_busy: IfBusy,
        plan: &Plan,
        images: &[Value],
    ) -> Result<SendReceipt> {
        if text.trim().is_empty() && images.is_empty() {
            return Err(err_exit(
                "PROMPT_REQUIRED",
                exit::USAGE,
                "The message is empty.",
            ));
        }
        let thread = state.thread();
        if thread.get("deletedAt").is_some_and(|v| !v.is_null()) {
            return Err(err_exit(
                "THREAD_NOT_FOUND",
                exit::NOT_FOUND,
                "That thread was deleted.",
            ));
        }
        let (dispatch_mode, mode_name) = match (state.active_run(), if_busy) {
            (None, _) => (json!({"type": "start_immediately"}), "start_immediately"),
            (Some(_), IfBusy::Queue) => {
                (json!({"type": "queue_after_active"}), "queue_after_active")
            }
            (Some(run), IfBusy::Steer) => (
                json!({"type": "steer_active", "targetRunId": run["id"]}),
                "steer_active",
            ),
            (Some(run), IfBusy::Refuse) => {
                return Err(err_exit(
                    "THREAD_BUSY",
                    exit::REJECTED,
                    format!(
                        "The thread is working on run {}. Wait for it, or choose to queue or steer.",
                        run["id"].as_str().unwrap_or("?")
                    ),
                ));
            }
        };
        self.check_plan(state, plan)?;
        let attachments = self
            .persist_images(state.thread_id(), message_id, images)
            .await?;
        self.dispatch_modes(state.thread_id(), plan).await?;
        let mut command = json!({
            "type": "message.dispatch",
            "threadId": state.thread_id(),
            "messageId": message_id,
            "text": plan.message_text(text),
            "attachments": attachments,
            // T3 has no creation source for terminal clients; its own CLI reports "web" too.
            "createdBy": "user",
            "creationSource": "web",
            "dispatchMode": dispatch_mode,
        });
        if let Some(selection) = &plan.model_selection {
            command["modelSelection"] = selection.clone();
        }
        let sequence = self.dispatch(command).await?;
        Ok(SendReceipt {
            message_id: message_id.to_string(),
            sequence,
            dispatch_mode: mode_name,
        })
    }

    /// Stores pictures for message `message_id`, returning the attachments it goes out with.
    pub async fn persist_images(
        &self,
        thread_id: &str,
        message_id: &str,
        images: &[Value],
    ) -> Result<Value> {
        if images.is_empty() {
            return Ok(json!([]));
        }
        let input =
            json!({ "threadId": thread_id, "messageId": message_id, "attachments": images });
        let result = self
            .rpc()
            .await?
            .call("assets.persistChatAttachments", input, UPLOAD_TIMEOUT)
            .await
            .map_err(|e| match e {
                RpcError::Failed { message, .. } => err_exit(
                    "ATTACHMENT_REJECTED",
                    exit::REJECTED,
                    format!("T3 didn't take the picture: {message}"),
                ),
                other => other.into(),
            })?;
        match result.get("attachments") {
            Some(list @ Value::Array(_)) => Ok(list.clone()),
            _ => Err(err(
                "T3_INVALID_ATTACHMENTS",
                "T3 stored the pictures but returned no attachments.".to_string(),
            )),
        }
    }

    /// Changes the thread's model and modes now, without sending a message. Returns the sequence
    /// of the last event it caused, or `None` when nothing changed.
    pub async fn apply_settings(&self, state: &ThreadState, plan: &Plan) -> Result<Option<u64>> {
        if plan.prompt_effort.is_some() {
            return Err(err_exit(
                "INVALID_CHOICE",
                exit::USAGE,
                "That effort applies to one message. Pass it to `tr3x send` instead.",
            ));
        }
        if let Some(run) = state.active_run().filter(|_| *plan != Plan::default()) {
            return Err(busy_for_settings(run));
        }
        self.check_plan(state, plan)?;
        let mut last = None;
        if let Some(selection) = &plan.model_selection {
            last = Some(
                self.dispatch(json!({
                    "type": "thread.model-selection.set",
                    "threadId": state.thread_id(),
                    "modelSelection": selection,
                }))
                .await?,
            );
        }
        Ok(self.dispatch_modes(state.thread_id(), plan).await?.or(last))
    }

    /// Reads a thread once its snapshot includes event `sequence`, so it shows a change just made.
    pub async fn thread_after(&self, thread_id: &str, sequence: u64) -> Result<ThreadState> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let state = self.thread(thread_id, true).await?;
            if state.sequence >= sequence {
                return Ok(state);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(err(
                    "T3_STALE_SNAPSHOT",
                    format!(
                        "T3 accepted the change, but thread {thread_id} still showed event {} of {sequence} after 5 seconds.",
                        state.sequence
                    ),
                ));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    fn check_plan(&self, state: &ThreadState, plan: &Plan) -> Result<()> {
        if plan.changes_modes()
            && let Some(run) = state.active_run()
        {
            return Err(busy_for_settings(run));
        }
        let switches_provider = plan.model_selection.as_ref().is_some_and(|selection| {
            selection["instanceId"] != state.thread()["modelSelection"]["instanceId"]
        });
        // Older servers need a separate provider.switch command, which tr3x does not send.
        if switches_provider && !self.has_capability("serverResolvedCommandContext") {
            return Err(err_exit(
                "UNSUPPORTED_SERVER",
                exit::REJECTED,
                "This T3 server is too old to switch a thread's provider from tr3x. Update T3 Code.",
            ));
        }
        Ok(())
    }

    /// Returns the sequence of the last mode change, or `None` when the modes stay the same.
    async fn dispatch_modes(&self, thread_id: &str, plan: &Plan) -> Result<Option<u64>> {
        let mut last = None;
        if let Some(mode) = &plan.runtime_mode {
            last = Some(
                self.dispatch(json!({
                    "type": "thread.runtime-mode.set",
                    "threadId": thread_id,
                    "runtimeMode": mode,
                }))
                .await?,
            );
        }
        if let Some(mode) = &plan.interaction_mode {
            last = Some(
                self.dispatch(json!({
                    "type": "thread.interaction-mode.set",
                    "threadId": thread_id,
                    "interactionMode": mode,
                }))
                .await?,
            );
        }
        Ok(last)
    }

    pub async fn respond(&self, thread_id: &str, request_id: &str, decision: &str) -> Result<u64> {
        self.dispatch(json!({
            "type": "runtime-request.respond",
            "threadId": thread_id,
            "requestId": request_id,
            "decision": decision,
        }))
        .await
    }

    /// Answers a question request. `answers` maps question id to the chosen label or free text.
    pub async fn answer(&self, thread_id: &str, request_id: &str, answers: Value) -> Result<u64> {
        self.dispatch(json!({
            "type": "runtime-request.respond",
            "threadId": thread_id,
            "requestId": request_id,
            "answers": answers,
        }))
        .await
    }

    pub async fn interrupt(&self, thread_id: &str, run_id: &str) -> Result<u64> {
        self.dispatch(json!({
            "type": "run.interrupt",
            "threadId": thread_id,
            "runId": run_id,
            "reason": "Interrupted from Rex",
        }))
        .await
    }

    /// Streams one thread's items, resubscribing after the last applied sequence when the connection drops.
    pub fn watch_thread(
        self: &Arc<Self>,
        thread_id: &str,
        after_sequence: Option<u64>,
    ) -> mpsc::Receiver<WatchEvent> {
        let client = self.clone();
        let payload = json!({"threadId": thread_id, "requestCompletionMarker": true, "acceptBoundedSnapshot": true});
        spawn_watch(
            move || {
                let client = client.clone();
                Box::pin(async move { client.rpc().await })
            },
            "orchestration.subscribeThread",
            payload,
            after_sequence,
        )
    }

    pub fn watch_shell(
        self: &Arc<Self>,
        after_sequence: Option<u64>,
    ) -> mpsc::Receiver<WatchEvent> {
        let client = self.clone();
        spawn_watch(
            move || {
                let client = client.clone();
                Box::pin(async move { client.rpc().await })
            },
            "orchestration.subscribeShell",
            json!({"requestCompletionMarker": true}),
            after_sequence,
        )
    }

    /// Streams the server's configuration, as `spawn_config_watch` describes.
    pub fn watch_config(self: &Arc<Self>) -> mpsc::Receiver<WatchEvent> {
        let client = self.clone();
        spawn_config_watch(move || {
            let client = client.clone();
            Box::pin(async move { client.rpc().await })
        })
    }
}

/// T3 can restart the agent's session to apply these settings, so tr3x changes them only
/// while the thread is idle.
fn busy_for_settings(run: &Value) -> anyhow::Error {
    err_exit(
        "THREAD_BUSY",
        exit::REJECTED,
        format!(
            "The thread is working on run {}. Changing its model or mode can restart the agent, so wait for the run to finish.",
            run["id"].as_str().unwrap_or("?")
        ),
    )
}

#[derive(Debug, Clone)]

pub enum WatchEvent {
    Item(Value),
    /// The connection dropped. The watcher is reconnecting, and resumes after the last sequence
    /// or starts again from the current state, as its stream does.
    Reconnecting {
        reason: String,
        retry_in: Duration,
    },
    /// The server refused the subscription. The watcher stopped.
    Failed(String),
}

/// Opens a connection for a watcher. Each reconnect calls it again.
type Connect = dyn Fn() -> Pin<Box<dyn Future<Output = Result<RpcClient>> + Send>> + Send + Sync;

/// Where a watcher's stream starts again after a dropped connection.
#[derive(Clone, Copy)]
enum Resume {
    /// After the highest event sequence delivered, from this one at first. The orchestration
    /// streams work this way, and their `{"kind": "synchronized"}` marker says a subscription
    /// has caught up.
    AfterSequence(Option<u64>),
    /// From a new snapshot. A stream with no cursor, such as the server config, sends the whole
    /// state first, as `{"type": "snapshot"}`.
    Snapshot,
}

impl Resume {
    /// Whether `item` shows the subscription working, so the next drop retries quickly.
    fn healthy(self, item: &Value) -> bool {
        match self {
            Resume::AfterSequence(_) => {
                item.get("kind").and_then(Value::as_str) == Some("synchronized")
            }
            Resume::Snapshot => item.get("type").and_then(Value::as_str) == Some("snapshot"),
        }
    }
}

/// Runs a subscription until its receiver is dropped. Tracks the highest sequence it delivered so a
/// resubscribe replays only what the consumer has not seen.
pub fn spawn_watch<F>(
    connect: F,
    tag: &'static str,
    payload: Value,
    after_sequence: Option<u64>,
) -> mpsc::Receiver<WatchEvent>
where
    F: Fn() -> Pin<Box<dyn Future<Output = Result<RpcClient>> + Send>> + Send + Sync + 'static,
{
    watch(
        Box::new(connect),
        tag,
        payload,
        Resume::AfterSequence(after_sequence),
    )
}

/// Runs T3's config subscription, `subscribeServerConfig`, until its receiver is dropped. Each
/// subscription opens with a snapshot of the whole configuration and goes on with changes to
/// parts of it. The stream has no cursor, so a resubscribe asks for nothing but a new snapshot.
/// The payload is empty, which leaves out the theme and usage-limit events tr3x doesn't read.
pub fn spawn_config_watch<F>(connect: F) -> mpsc::Receiver<WatchEvent>
where
    F: Fn() -> Pin<Box<dyn Future<Output = Result<RpcClient>> + Send>> + Send + Sync + 'static,
{
    watch(
        Box::new(connect),
        "subscribeServerConfig",
        json!({}),
        Resume::Snapshot,
    )
}

fn watch(
    connect: Box<Connect>,
    tag: &'static str,
    payload: Value,
    resume: Resume,
) -> mpsc::Receiver<WatchEvent> {
    // One forwarded value plus one unaccepted RPC chunk. Each is bounded by the
    // 64 MiB protocol message limit; moving values avoids cloning large snapshots.
    let (events, receiver) = mpsc::channel(1);
    tokio::spawn(async move {
        let mut last_sequence = match resume {
            Resume::AfterSequence(sequence) => sequence,
            Resume::Snapshot => None,
        };
        let mut backoff = Duration::from_millis(250);
        loop {
            let connected = tokio::select! {
                connected = connect() => connected,
                // The consumer went away, such as a TUI that quit.
                _ = events.closed() => return,
            };
            let reason = match connected {
                Err(e) => e.to_string(),
                Ok(rpc) => {
                    let mut request = payload.clone();
                    if let Some(sequence) = last_sequence {
                        request["afterSequence"] = json!(sequence);
                    }
                    let limits = StreamLimits::new(1, 64 << 20).expect("nonzero watch limits");
                    match rpc.bounded_stream(tag, request, limits).await {
                        Err(e) => e.to_string(),
                        Ok(mut stream) => loop {
                            let next = tokio::select! {
                                next = stream.next() => next,
                                // The consumer went away, such as a TUI switching threads.
                                _ = events.closed() => return,
                            };
                            match next {
                                Some(Ok(mut chunk)) => {
                                    // Keep the chunk unacked until every value has entered bounded
                                    // downstream state. Backpressure never holds the RPC reader.
                                    for item in chunk.take_values() {
                                        if resume.healthy(&item) {
                                            backoff = Duration::from_millis(250);
                                        }
                                        let sequence = delivered_sequence(&item);
                                        if events.send(WatchEvent::Item(item)).await.is_err() {
                                            return;
                                        }
                                        if let Resume::AfterSequence(_) = resume
                                            && let Some(sequence) = sequence
                                        {
                                            last_sequence = Some(
                                                last_sequence.map_or(sequence, |l| l.max(sequence)),
                                            );
                                        }
                                    }
                                    chunk.accept();
                                }
                                Some(Err(RpcError::Failed { message, .. })) => {
                                    let _ = events.send(WatchEvent::Failed(message)).await;
                                    return;
                                }
                                Some(Err(e)) => break e.to_string(),
                                // Dropping the sender tells the consumer the watch is over.
                                None => break "the server ended the subscription".to_string(),
                            }
                        },
                    }
                }
            };
            let jitter_ms = uuid::Uuid::new_v4().as_u128() % (backoff.as_millis() / 4 + 1);
            let retry_in = backoff + Duration::from_millis(jitter_ms as u64);
            if events
                .send(WatchEvent::Reconnecting { reason, retry_in })
                .await
                .is_err()
            {
                return;
            }
            tokio::select! {
                _ = tokio::time::sleep(retry_in) => {}
                _ = events.closed() => return,
            }
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }
    });
    receiver
}

/// The event sequence an orchestration item carries, which a resubscribe resumes after.
fn delivered_sequence(item: &Value) -> Option<u64> {
    // Enrichment snapshots do not move the resume cursor.
    if item.get("resolvedRepositoryIdentityRoots").is_some() {
        return None;
    }
    item.get("sequence")
        .or_else(|| item.get("snapshotSequence"))
        .or_else(|| item.get("snapshot").and_then(|s| s.get("snapshotSequence")))
        .and_then(Value::as_u64)
}

fn api_token(api: &Api) -> &str {
    api.token()
}

#[cfg(test)]
impl Env {
    /// A machine for tests that never reach a server: a closed port on 127.0.0.1, no token.
    pub(crate) fn offline() -> Env {
        Env::offline_as("offline")
    }

    pub(crate) fn offline_as(id: &str) -> Env {
        let mut env = Env::new(
            Store::new("/nonexistent"),
            Machine {
                id: id.into(),
                label: "Test".into(),
                alias: None,
                origins: vec!["http://127.0.0.1:9".into()],
                ssh: None,
                via: Via::Link,
                version: String::new(),
            },
        );
        env.fixed = Some(Api::without_proxy("http://127.0.0.1:9"));
        env
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::HashMap;

    use futures_util::{SinkExt, StreamExt};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio_tungstenite::WebSocketStream;
    use tokio_tungstenite::tungstenite::Message;
    use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
    use tokio_tungstenite::tungstenite::protocol::Role;

    use super::*;
    use crate::projection::Applied::{Duplicate, Snapshot, Synchronized};
    use crate::projection::{Applied, apply_config};

    pub(crate) type Socket = WebSocketStream<TcpStream>;
    type Events = mpsc::Receiver<WatchEvent>;

    const SHELL: &str = "orchestration.subscribeShell";
    const THREAD: &str = "orchestration.subscribeThread";
    const CONFIG: &str = "subscribeServerConfig";

    /// A machine at the fake T3 at `origin`. Like `Env::offline`, it holds no token. Its
    /// ticket requests ignore proxies, so they reach the fake whatever the environment says.
    pub(crate) fn client_at(origin: &str) -> Arc<Env> {
        let mut env = Env::offline();
        env.fixed = Some(Api::without_proxy(origin));
        Arc::new(env)
    }

    /// Plays T3's HTTP side until a WebSocket opens, as `serve_sockets` in tests/cli.rs does.
    /// Each ticket request gets the next ticket, so the path a socket opens on says how many
    /// tickets came before it. Returns that path and the socket.
    pub(crate) async fn open(listener: &TcpListener, tickets: &mut usize) -> (String, Socket) {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            // One byte at a time, so the socket gets every byte after the head.
            let mut head = Vec::new();
            while !head.ends_with(b"\r\n\r\n") {
                head.push(stream.read_u8().await.unwrap());
            }
            let head = String::from_utf8(head).unwrap();
            let path = head.split_whitespace().nth(1).unwrap().to_string();
            let key = head.lines().find_map(|line| {
                let (name, value) = line.split_once(':')?;
                let is_key = name.eq_ignore_ascii_case("sec-websocket-key");
                is_key.then(|| value.trim().to_string())
            });
            if let Some(key) = key {
                let accept = derive_accept_key(key.as_bytes());
                let reply = format!(
                    "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
                );
                stream.write_all(reply.as_bytes()).await.unwrap();
                let socket = WebSocketStream::from_raw_socket(stream, Role::Server, None).await;
                return (path, socket);
            }
            // Anything else is another program on this machine trying local ports, not tr3x.
            if !head.starts_with("POST /api/auth/websocket-ticket/rpc ") {
                let gone =
                    "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                let _ = stream.write_all(gone.as_bytes()).await;
                continue;
            }
            *tickets += 1;
            let body = json!({"ticket": format!("ticket-{tickets}")}).to_string();
            let reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(reply.as_bytes()).await.unwrap();
        }
    }

    /// The next frame from tr3x other than a ping. Fails the test after five seconds.
    pub(crate) async fn recv(socket: &mut Socket) -> Value {
        loop {
            let frame = tokio::time::timeout(Duration::from_secs(5), socket.next())
                .await
                .expect("a frame within five seconds")
                .expect("an open socket")
                .unwrap();
            if let Message::Text(text) = frame {
                let value: Value = serde_json::from_str(&text).unwrap();
                if value["_tag"] != "Ping" {
                    return value;
                }
            }
        }
    }

    /// The three subscriptions' requests on a new socket, by tag. They can come in any order,
    /// and each comes once.
    async fn requests(socket: &mut Socket) -> HashMap<String, Value> {
        let mut requests = HashMap::new();
        while requests.len() < 3 {
            let request = recv(socket).await;
            assert_eq!(request["_tag"], "Request", "{request}");
            let tag = request["tag"].as_str().unwrap().to_string();
            let earlier = requests.insert(tag, request.clone());
            assert!(earlier.is_none(), "{request} came twice");
        }
        requests
    }

    /// The id of the request for `tag`, once it is checked to ask for exactly `payload`.
    fn id(requests: &HashMap<String, Value>, tag: &str, payload: Value) -> Value {
        let id = requests[tag]["id"].clone();
        let expected = json!({
            "_tag": "Request", "id": id, "tag": tag, "payload": payload, "headers": []
        });
        assert_eq!(requests[tag], expected);
        id
    }

    /// Sends `values` on the stream of request `id`, and checks that tr3x acks the chunk.
    async fn chunk(socket: &mut Socket, id: &Value, values: Value) {
        let chunk = json!({"_tag": "Chunk", "requestId": id, "values": values});
        let frame = Message::Text(chunk.to_string().into());
        socket.send(frame).await.unwrap();
        let ack = json!({"_tag": "Ack", "requestId": id});
        assert_eq!(recv(socket).await, ack);
    }

    /// What tr3x sends when it drops the stream of request `id`.
    fn interrupt(id: &Value) -> Value {
        json!({"_tag": "Interrupt", "requestId": id, "interruptors": []})
    }

    /// The watch's next event. Fails the test after five seconds, or if the watch stopped.
    async fn next_event(events: &mut Events) -> WatchEvent {
        tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .expect("a watch event within five seconds")
            .expect("a watch that goes on")
    }

    /// The watch's next `count` events, each of them an item.
    async fn items(events: &mut Events, count: usize) -> Vec<Value> {
        let mut items = Vec::new();
        while items.len() < count {
            match next_event(events).await {
                WatchEvent::Item(item) => items.push(item),
                other => panic!("expected an item, got {other:?}"),
            }
        }
        items
    }

    /// Checks that the watch's next event reports the dropped connection.
    async fn reconnecting(events: &mut Events) {
        match next_event(events).await {
            WatchEvent::Reconnecting { reason, retry_in } => {
                assert!(reason.starts_with("T3 connection lost"), "{reason}");
                assert!(
                    (Duration::from_millis(250)..=Duration::from_millis(312)).contains(&retry_in)
                );
            }
            other => panic!("expected a reconnect, got {other:?}"),
        }
    }

    fn renamed(sequence: u64, id: &str, title: &str) -> Value {
        let thread = json!({"id": id, "title": title});
        json!({"kind": "thread.updated", "sequence": sequence, "thread": thread})
    }

    fn reply(sequence: u64, text: &str) -> Value {
        let item = json!({"id": "a1", "type": "assistant_message", "ordinal": 1, "text": text});
        let event = json!({"type": "turn-item.updated", "payload": item});
        json!({"kind": "event", "sequence": sequence, "event": event})
    }

    /// A config snapshot that lists the providers `ids`.
    fn config(ids: &[&str]) -> Value {
        let config = json!({"providers": providers(ids), "settings": {}});
        json!({"version": 1, "type": "snapshot", "config": config})
    }

    fn providers(ids: &[&str]) -> Value {
        ids.iter()
            .map(|id| json!({"instanceId": id, "status": "ready"}))
            .collect()
    }

    /// The ids of the providers in the configuration held.
    fn provider_ids(held: &Option<Value>) -> Vec<&str> {
        let config = held.as_ref().unwrap();
        let providers = config["providers"].as_array().unwrap();
        providers
            .iter()
            .map(|provider| provider["instanceId"].as_str().unwrap())
            .collect()
    }

    #[tokio::test]
    async fn a_slow_watch_consumer_delays_ack_without_losing_events() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = client_at(&format!("http://{}", listener.local_addr().unwrap()));
        let (checked, waiting) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let mut tickets = 0;
            let (_, mut socket) = open(&listener, &mut tickets).await;
            let request = recv(&mut socket).await;
            assert_eq!(request["tag"], SHELL);
            let id = &request["id"];
            let values = json!([
                renamed(1, "t", "one"),
                renamed(2, "t", "two"),
                renamed(3, "t", "three")
            ]);
            socket
                .send(Message::Text(
                    json!({"_tag": "Chunk", "requestId": id, "values": values})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
            assert!(
                tokio::time::timeout(Duration::from_millis(100), socket.next())
                    .await
                    .is_err()
            );
            checked.send(()).unwrap();
            assert_eq!(
                recv(&mut socket).await,
                json!({"_tag": "Ack", "requestId": id})
            );
            assert_eq!(recv(&mut socket).await, interrupt(id));
        });
        let mut events = client.watch_shell(None);
        waiting.await.unwrap();
        let received = items(&mut events, 3).await;
        assert_eq!(
            received
                .iter()
                .map(|item| item["sequence"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            [1, 2, 3]
        );
        // Let the watcher accept the fully forwarded chunk before cancellation.
        tokio::task::yield_now().await;
        drop(events);
        server.await.unwrap();
    }

    /// The three watches a pane runs for an open thread share the connection `Client::rpc`
    /// holds. When it drops, each watch says so once, and all three come back on one new
    /// connection with one request each. The shell and thread streams resume after the last
    /// sequence they delivered, and the config stream asks again with no cursor. The
    /// watcher passes on whatever T3 sends, and the reducers drop what they already have.
    #[tokio::test]
    async fn shared_watches_come_back_on_one_new_connection_after_a_drop() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let client = client_at(&origin);
        let shell_payload = json!({"requestCompletionMarker": true});
        let thread_payload = json!({
            "threadId": "t1", "requestCompletionMarker": true, "acceptBoundedSnapshot": true
        });

        let server = tokio::spawn(async move {
            let synchronized = json!({"kind": "synchronized"});
            let mut tickets = 0;
            let (path, mut socket) = open(&listener, &mut tickets).await;
            assert_eq!(path, "/ws?wsTicket=ticket-1&orchestrationProtocol=2");
            let asked = requests(&mut socket).await;
            let shell_id = id(&asked, SHELL, shell_payload.clone());
            let thread_id = id(&asked, THREAD, thread_payload.clone());
            let config_id = id(&asked, CONFIG, json!({}));
            let threads = json!([{"id": "t1", "title": "First"}]);
            let snapshot = json!({"snapshotSequence": 5, "projects": [], "threads": threads});
            let snapshot = json!({"kind": "snapshot", "snapshot": snapshot});
            let values = json!([snapshot, synchronized, renamed(6, "t1", "Renamed")]);
            chunk(&mut socket, &shell_id, values).await;
            let user = json!({"id": "u1", "type": "user_message", "ordinal": 0, "text": "hi"});
            let projection = json!({"thread": {"id": "t1"}, "turnItems": [user]});
            let snapshot =
                json!({"kind": "snapshot", "snapshotSequence": 10, "projection": projection});
            let values = json!([snapshot, reply(11, "Hel"), synchronized]);
            chunk(&mut socket, &thread_id, values).await;
            chunk(&mut socket, &config_id, json!([config(&["codex"])])).await;
            // T3 goes away without closing the socket.
            drop(socket);

            // One more ticket, and one new socket carries all three again.
            let (path, mut socket) = open(&listener, &mut tickets).await;
            assert_eq!(path, "/ws?wsTicket=ticket-2&orchestrationProtocol=2");
            let asked = requests(&mut socket).await;
            let mut resumed = shell_payload;
            resumed["afterSequence"] = json!(6);
            let shell_id = id(&asked, SHELL, resumed);
            let mut resumed = thread_payload;
            resumed["afterSequence"] = json!(11);
            let thread_id = id(&asked, THREAD, resumed);
            let config_id = id(&asked, CONFIG, json!({}));
            // Each orchestration stream sends again the last event the client had.
            let replayed = renamed(6, "t1", "Renamed");
            let values = json!([replayed, renamed(7, "t2", "Second"), synchronized]);
            chunk(&mut socket, &shell_id, values).await;
            let values = json!([reply(11, "Hel"), reply(12, "Hello"), synchronized]);
            chunk(&mut socket, &thread_id, values).await;
            let values = json!([config(&["codex", "claude"])]);
            chunk(&mut socket, &config_id, values).await;
            let values = json!([renamed(8, "t2", "Third")]);
            chunk(&mut socket, &shell_id, values).await;
            let values = json!([reply(13, "Hello world")]);
            chunk(&mut socket, &thread_id, values).await;
            let payload = json!({"providers": providers(&["claude"])});
            let statuses = json!({"version": 1, "type": "providerStatuses", "payload": payload});
            chunk(&mut socket, &config_id, json!([statuses])).await;
            let mut ended = Vec::new();
            for _ in 0..3 {
                ended.push(recv(&mut socket).await);
            }
            for request in [&shell_id, &thread_id, &config_id] {
                assert!(ended.contains(&interrupt(request)), "{ended:?}");
            }
        });

        let mut shell_events = client.watch_shell(None);
        let mut thread_events = client.watch_thread("t1", None);
        let mut config_events = client.watch_config();

        // Drain independent streams concurrently: a slow consumer now backpressures its
        // own Ack, rather than forcing the watcher to retain an unbounded backlog.
        let (shell_first, thread_first, config_first) = tokio::join!(
            items(&mut shell_events, 3),
            items(&mut thread_events, 3),
            items(&mut config_events, 1)
        );
        tokio::join!(
            reconnecting(&mut shell_events),
            reconnecting(&mut thread_events),
            reconnecting(&mut config_events)
        );
        let (shell_again, thread_again, config_again) = tokio::join!(
            items(&mut shell_events, 3),
            items(&mut thread_events, 3),
            items(&mut config_events, 1)
        );
        let rename = Applied::Event("thread.updated".into());
        let mut shell = ShellState::default();
        let first: Vec<_> = shell_first.iter().map(|item| shell.apply(item)).collect();
        assert_eq!(first, [Snapshot, Synchronized, rename.clone()]);
        let again: Vec<_> = shell_again.iter().map(|item| shell.apply(item)).collect();
        assert_eq!(again, [Duplicate, rename.clone(), Synchronized]);
        let delta = Applied::Event("turn-item.updated".into());
        let mut thread = ThreadState::default();
        let first: Vec<_> = thread_first.iter().map(|item| thread.apply(item)).collect();
        assert_eq!(first, [Snapshot, delta.clone(), Synchronized]);
        let again: Vec<_> = thread_again.iter().map(|item| thread.apply(item)).collect();
        assert_eq!(again, [Duplicate, delta.clone(), Synchronized]);
        let mut held = None;
        assert_eq!(apply_config(&mut held, &config_first[0]), Snapshot);
        assert_eq!(apply_config(&mut held, &config_again[0]), Snapshot);
        assert_eq!(provider_ids(&held), ["codex", "claude"]);

        // The other three go on over the same socket, with no reconnect.
        let next = items(&mut shell_events, 1).await;
        assert_eq!(shell.apply(&next[0]), rename);
        let next = items(&mut thread_events, 1).await;
        assert_eq!(thread.apply(&next[0]), delta);
        let next = items(&mut config_events, 1).await;
        let changed = Applied::Event("providerStatuses".into());
        assert_eq!(apply_config(&mut held, &next[0]), changed);

        // T3 sent some events twice, and none of them changed the state twice.
        let first = json!({"id": "t1", "title": "Renamed"});
        let second = json!({"id": "t2", "title": "Third"});
        assert_eq!(shell.threads, [first, second]);
        assert_eq!(shell.sequence, 8);
        let shown = thread.items();
        assert_eq!(shown.len(), 2);
        assert_eq!(shown[0]["text"], "hi");
        assert_eq!(shown[1]["text"], "Hello world");
        assert_eq!(thread.sequence, 13);
        assert_eq!(provider_ids(&held), ["claude"]);

        drop((shell_events, thread_events, config_events));
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .expect("the server saw every interrupt")
            .unwrap();
    }
}
