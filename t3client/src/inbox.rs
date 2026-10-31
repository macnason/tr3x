//! T3's sidebar logic without its drawing: the word a thread's card shows, the shelf it sits
//! on, and the order T3 lists threads in, as one list for Rex.
//!
//! Forked from t3term's `tui/sidebar.rs` (krishhgg/t3term, MIT). The rules are T3's:
//!
//! - Visibility is `filterSidebarV2VisibleThreads` in `apps/web/src/components/Sidebar.logic.ts`.
//! - The shelf is `resolveSidebarThreadSection` in the same file, fed by the partition in
//!   `apps/web/src/components/Sidebar.tsx`.
//! - The order inside each shelf comes from `packages/client-runtime/src/state/threadSort.ts`.
//! - Snooze is `effectiveSnoozed` in `packages/client-runtime/src/state/threadSettled.ts`.
//! - The Working shelf is `isThreadWorking`, `sortInboxThreadsByReturn`,
//!   `sortWorkingThreadsBySend` and `createInboxReturnTracker` in
//!   `packages/client-runtime/src/state/threadInbox.ts`.
//! - A card's status word is `resolveSidebarThreadStatus` and `resolveSidebarV2TopStatus` in
//!   `Sidebar.logic.ts`, with the labels and Woke test of `SidebarThreadRow` in `Sidebar.tsx`
//!   and the wake time from `threadWokeAt` in `threadSettled.ts`.
//!
//! Field names are the shell's (`OrchestrationV2ThreadShell` in
//! `packages/contracts/src/orchestrationV2.ts`). Threads from several machines can be ordered
//! together: a thread's `t3rexEnv` field, when set, keeps the return stamps of two machines'
//! threads apart.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

use serde_json::Value;

use crate::projection::{ShellState, is_terminal_status, status};

/// The lifecycle features a server supports, from its environment descriptor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Capabilities {
    pub settlement: bool,
    pub snooze: bool,
}

impl Capabilities {
    /// Both keys are optional in `packages/contracts/src/environment.ts`. A server that leaves
    /// one out predates the feature, so only `true` turns it on.
    pub fn of(capabilities: &Value) -> Capabilities {
        Capabilities {
            settlement: capabilities["threadSettlement"] == true,
            snooze: capabilities["threadSnooze"] == true,
        }
    }
}

/// A sidebar shelf, in the order the list shows them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shelf {
    Pinned,
    /// The threads that are the user's turn, which the GUI calls the inbox. Its heading shows
    /// only under the Pinned shelf, where it marks the end of the pins.
    Inbox,
    /// Threads busy without the user, when the Working shelf is on.
    Working,
    /// The inbox's threads whose latest run finished after the last visit, which read Done.
    Done,
    Snoozed,
    Settled,
}

/// `resolveSidebarThreadSection`: a snooze wins until it wakes, then settlement wins over a
/// stale pin.
fn shelf_for(snoozed: bool, settled: bool, pinned: bool) -> Shelf {
    if snoozed {
        Shelf::Snoozed
    } else if settled {
        Shelf::Settled
    } else if pinned {
        Shelf::Pinned
    } else {
        Shelf::Inbox
    }
}

/// The shelf a visible thread sits on at `now`. Snooze and settlement count only on a server
/// that supports them, and a pin always counts. A thread is settled when its override says so.
/// A `settledAt` stamp alone is history, not the thread's state.
fn shelf(thread: &Value, capabilities: Capabilities, now: i64) -> Shelf {
    shelf_for(
        capabilities.snooze && effective_snoozed(thread, now),
        capabilities.settlement && thread["settledOverride"] == "settled",
        !thread["pinnedAt"].is_null(),
    )
}

/// `filterSidebarV2VisibleThreads`: archived threads and subagents stay out of the sidebar,
/// since a subagent shows under its parent. A fork is an ordinary thread and stays.
fn visible(thread: &Value) -> bool {
    thread["archivedAt"].is_null() && thread["lineage"]["relationshipToParent"] != "subagent"
}

// ---- timestamps ----

/// Milliseconds for a timestamp in the form T3 writes, `2026-10-08T00:06:52.769Z`. Anything
/// else gives `None`, as `Date.parse` gives `NaN`, so a malformed value never hides a thread.
fn instant(value: &Value) -> Option<i64> {
    let text = value.as_str()?;
    let bytes = text.as_bytes();
    let (head, tail) = (bytes.get(..19)?, &bytes[19..]);
    let shape = b"dddd-dd-ddTdd:dd:dd";
    let fits = head.iter().zip(shape).all(|(&byte, &want)| {
        if want == b'd' {
            byte.is_ascii_digit()
        } else {
            byte == want
        }
    });
    let ends = match tail {
        [b'Z'] => true,
        [b'.', digits @ .., b'Z'] => !digits.is_empty() && digits.iter().all(u8::is_ascii_digit),
        _ => false,
    };
    if !fits || !ends {
        return None;
    }
    let two = |from: usize| text[from..from + 2].parse::<u32>().unwrap_or(u32::MAX);
    let (month, day, hour, minute, second) = (two(5), two(8), two(11), two(14), two(17));
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    parse_iso_ms(text)
}

/// `Date.parse(a) > Date.parse(b)`, which is false when either doesn't parse.
fn later(a: &Value, b: &Value) -> bool {
    matches!((instant(a), instant(b)), (Some(a), Some(b)) if a > b)
}

static NULL: Value = Value::Null;

// ---- snooze ----

/// The latest run as `presentThreadShell` gives it to the GUI.
struct LatestRun<'a> {
    completed: bool,
    requested_at: &'a Value,
    started_at: &'a Value,
    completed_at: &'a Value,
}

/// None before the first run. An idle thread's run counts as completed, and when the server
/// leaves out the completion time, a finished run takes `updatedAt` for it.
fn latest_run(thread: &Value) -> Option<LatestRun<'_>> {
    if thread["latestRunId"].is_null() {
        return None;
    }
    let state = status(thread);
    let completed_at = match thread.get("latestRunCompletedAt") {
        Some(value) => value,
        None if state == "idle" || is_terminal_status(state) => &thread["updatedAt"],
        None => &NULL,
    };
    Some(LatestRun {
        completed: state == "idle" || state == "completed",
        requested_at: &thread["latestRunRequestedAt"],
        started_at: &thread["latestRunStartedAt"],
        completed_at,
    })
}

/// The runtime status `shellRuntime` gives the GUI: idle while background work that wakes the
/// agent is pending, unless the run failed, else the activity run's status or the thread's own.
/// None for a thread with no run and no provider thread.
fn runtime_status(thread: &Value) -> Option<&str> {
    let state = status(thread);
    // `backgroundWorkHoldsCompletion` in `packages/shared/src/orchestrationV2PendingBackgroundWork.ts`:
    // every kind holds but a command, including kinds this build doesn't know.
    let park = state != "failed"
        && thread["pendingBackgroundTasks"]
            .as_array()
            .is_some_and(|tasks| tasks.iter().any(|task| task["kind"] != "command"));
    if thread["latestRunId"].is_null() && thread["activeProviderThreadId"].is_null() && !park {
        return None;
    }
    Some(if park {
        "idle"
    } else {
        thread["activityRunStatus"].as_str().unwrap_or(state)
    })
}

/// What a pending request asks of the user, as `presentThreadShell` sorts it into
/// `hasPendingUserInput` and `hasPendingApprovals`: a question is input, an auth refresh is
/// neither, and every other kind, known or not, is an approval.
fn pending(thread: &Value) -> Option<Word> {
    let request = &thread["pendingRuntimeRequest"];
    if request.is_null() {
        return None;
    }
    match request["kind"].as_str() {
        Some("user_input") => Some(Word::Input),
        Some("auth_refresh") => None,
        _ => Some(Word::Approval),
    }
}

/// `threadRaisedHandWhileSnoozed`: the agent is waiting on the user, it failed after the
/// snooze began, or a run completed after the snooze began.
fn raised_hand(thread: &Value) -> bool {
    if pending(thread).is_some() {
        return true;
    }
    // A thread snoozed while already failed stays snoozed. The runtime's `updatedAt` is the
    // thread's.
    let snoozed_at = &thread["snoozedAt"];
    if matches!(runtime_status(thread), Some("error" | "failed"))
        && (snoozed_at.is_null() || later(&thread["updatedAt"], snoozed_at))
    {
        return true;
    }
    !snoozed_at.is_null()
        && latest_run(thread)
            .is_some_and(|run| run.completed && later(run.completed_at, snoozed_at))
}

/// `effectiveSnoozed`: off the active list until the wake time, unless the thread raised its
/// hand first. A wake time that has passed or doesn't parse leaves the thread where it was.
pub fn effective_snoozed(thread: &Value, now: i64) -> bool {
    instant(&thread["snoozedUntil"]).is_some_and(|wake| wake > now) && !raised_hand(thread)
}

// ---- status ----

/// The word on a card's first line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Word {
    Approval,
    Input,
    Working,
    /// Working toward a native `/goal`, which keeps the agent going across turns until it is
    /// met.
    Goal,
    /// Stopped, with background work pending that will wake the agent, so not the user's
    /// turn yet.
    Waiting,
    /// Failed on a usage limit.
    Limited,
    Failed,
    Woke,
    /// A run finished that nobody has looked at since.
    Done,
}

impl Word {
    fn label(self) -> &'static str {
        match self {
            Word::Approval => "Approval",
            Word::Input => "Input",
            Word::Working => "Working",
            Word::Goal => "Goal",
            Word::Waiting => "Waiting",
            Word::Limited => "Limited",
            Word::Failed => "Failed",
            Word::Woke => "Woke",
            Word::Done => "Done",
        }
    }
}

/// `resolveSidebarThreadStatus`: what the thread is doing, or None when it is at rest. A
/// request for the user comes before anything the runtime is doing.
fn activity(thread: &Value) -> Option<Word> {
    if let Some(word) = pending(thread) {
        return Some(word);
    }
    match runtime_status(thread)? {
        "preparing" | "queued" | "starting" | "running" | "waiting" => {
            Some(if thread["goal"]["status"] == "active" {
                Word::Goal
            } else {
                Word::Working
            })
        }
        "idle" => Some(Word::Waiting),
        "failed" if thread["lastErrorClass"] == "usage_limit" => Some(Word::Limited),
        "failed" => Some(Word::Failed),
        _ => None,
    }
}

/// The word a card shows at `now`, after `resolveSidebarV2TopStatus`: what the thread is
/// doing, else a wake, else a finish nobody has seen. None leaves the card showing its age.
fn status_word(thread: &Value, now: i64) -> Option<Word> {
    activity(thread).or_else(|| {
        if woke(thread, now) {
            Some(Word::Woke)
        } else if unseen_completion(thread) {
            Some(Word::Done)
        } else {
            None
        }
    })
}

/// `threadWokeAt`: when a snoozed thread came back, or None while it sleeps or if it never
/// slept. A thread that raised its hand woke at that moment, and keeps that time after the
/// wake time passes, so a visit between the two still clears the word.
fn woke_at(thread: &Value, now: i64) -> Option<&Value> {
    let wake = instant(&thread["snoozedUntil"])?;
    if !raised_hand(thread) {
        return (wake <= now).then_some(&thread["snoozedUntil"]);
    }
    let snoozed_at = &thread["snoozedAt"];
    let finished =
        latest_run(thread).filter(|run| run.completed && later(run.completed_at, snoozed_at));
    if let Some(run) = finished {
        return Some(run.completed_at);
    }
    // The runtime's `updatedAt` is the thread's. Without a runtime, the snooze's start.
    let runtime_at = runtime_status(thread).map(|_| &thread["updatedAt"]);
    runtime_at
        .into_iter()
        .chain([snoozed_at])
        .find(|at| !at.is_null())
}

/// `isWoke` in `SidebarThreadRow`: the thread woke after the last visit, and it isn't
/// settled. A visit time that is missing or doesn't parse counts as no visit.
fn woke(thread: &Value, now: i64) -> bool {
    let Some(at) = woke_at(thread, now).and_then(instant) else {
        return false;
    };
    thread["settledOverride"] != "settled"
        && instant(&thread["lastVisitedAt"]).is_none_or(|visited| visited < at)
}

/// `hasUnseenCompletion`: the latest run finished after the last visit. A thread nobody has
/// visited counts as seen, so a new server doesn't mark its whole history unread, and so
/// does every thread on a server that doesn't send `lastVisitedAt`. A visit time that doesn't
/// parse counts as before the finish.
fn unseen_completion(thread: &Value) -> bool {
    let Some(completed) = latest_run(thread).and_then(|run| instant(run.completed_at)) else {
        return false;
    };
    let visited = &thread["lastVisitedAt"];
    if visited.as_str().is_none_or(str::is_empty) {
        return false;
    }
    instant(visited).is_none_or(|visited| completed > visited)
}

// ---- order ----

fn id(thread: &Value) -> &str {
    str_of(thread, "id")
}

fn str_of<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// A thread's id with the machine it's on, so the same id on two machines stays two threads.
fn key(thread: &Value) -> String {
    format!("{}\0{}", str_of(thread, "t3rexEnv"), id(thread))
}

/// `sortPinnedThreadsByOrderKey`: threads the user arranged, by their keys, then the rest
/// newest created first. Ties go by id.
fn sort_pinned(threads: &mut [&Value]) {
    threads.sort_by_cached_key(|&t| match t["pinOrderKey"].as_str() {
        Some(key) => (0, key, Reverse(0), id(t)),
        None => (1, "", Reverse(instant(&t["createdAt"]).unwrap_or(0)), id(t)),
    });
}

/// `sortActiveThreadsByOrderKey`: new and reopened threads first, newest first, then the
/// threads the user arranged, by their keys. Ties go by id.
fn sort_active(threads: &mut [&Value]) {
    threads.sort_by_cached_key(|&t| match t["activeOrderKey"].as_str() {
        None => (0, "", Reverse(anchor(t)), id(t)),
        Some(key) => (1, key, Reverse(0), id(t)),
    });
}

/// `activeThreadAnchorTimestampMs`: when the thread was created, or when it last came back to
/// the active list if that is later.
fn anchor(thread: &Value) -> i64 {
    let at = |key: &str| instant(&thread[key]).unwrap_or(0);
    at("createdAt").max(at("unsettledAt"))
}

/// `sortSettledThreads`: newest first by `resolveSettledThreadTimestamp`. Ties go by id.
fn sort_settled(threads: &mut [&Value]) {
    threads.sort_by_cached_key(|&t| (Reverse(settled_ms(t)), id(t)));
}

/// `resolveSettledThreadTimestamp`: when the thread settled, else its latest message or run
/// time, else `updatedAt`. A thread with none of them sorts as 0.
pub fn settled_ms(thread: &Value) -> i64 {
    if let Some(at) = instant(&thread["settledAt"]) {
        return at;
    }
    let run = latest_run(thread);
    let run_times = run
        .iter()
        .flat_map(|run| [run.requested_at, run.started_at, run.completed_at]);
    std::iter::once(&thread["latestUserMessageAt"])
        .chain(run_times)
        .filter_map(instant)
        .max()
        .or_else(|| instant(&thread["updatedAt"]))
        .unwrap_or(0)
}

// ---- working ----

/// `isThreadWorking`: the agent is busy and nothing waits on the user. That is a run under
/// way, or one stopped with background work that will wake it. A request for the user, a
/// failure and a plan waiting for a reply keep the thread in the inbox. An auth refresh isn't
/// the user's to answer, so a thread waiting on one can still be working.
fn belongs_in_working(thread: &Value) -> bool {
    if pending(thread).is_some() {
        return false;
    }
    if !matches!(
        runtime_status(thread),
        Some("preparing" | "queued" | "starting" | "running" | "waiting" | "idle")
    ) {
        return false;
    }
    // A plan the user has to answer outranks background work still pending. `presentThreadShell`
    // gives the latest run the thread's status, with idle read as completed.
    let run_settled = !thread["latestRunId"].is_null()
        && !matches!(
            status(thread),
            "preparing" | "queued" | "starting" | "running" | "waiting"
        )
        && thread["activeRunId"] != thread["latestRunId"];
    !(thread["interactionMode"] == "plan"
        && thread["hasActionableProposedPlan"] == true
        && run_settled)
}

/// `createInboxReturnTracker`: when this client saw each thread leave the Working shelf. The
/// server stamps a run's request and its finish, but nothing marks an approval asked partway
/// through a run or background work ending, so the moment t3rex sees the change stands in.
/// The stamps last as long as t3rex runs.
#[derive(Debug, Default)]
struct Returns {
    /// The threads that belonged in Working at the last look, or None before the first.
    working: Option<HashSet<String>>,
    at: HashMap<String, i64>,
}

impl Returns {
    /// Looks at every thread the shell has, listed or not, at `now`. The first look only notes
    /// which threads are working, so starting t3rex stamps nothing and moves no card.
    fn observe(&mut self, threads: &[Value], now: i64) {
        let present: HashSet<String> = threads.iter().map(key).collect();
        let working: HashSet<String> = threads
            .iter()
            .filter(|t| belongs_in_working(t))
            .map(key)
            .collect();
        // A thread that has gone takes its stamp with it, so the map holds only live threads.
        self.at.retain(|k, _| present.contains(k));
        for left in self.working.iter().flatten() {
            if present.contains(left) && !working.contains(left) {
                self.at.insert(left.clone(), now);
            }
        }
        self.working = Some(working);
    }

    fn returned_at(&self, thread: &Value) -> Option<i64> {
        self.at.get(&key(thread)).copied()
    }
}

/// `sortInboxThreadsByReturn`: with the Working shelf on, the inbox lists threads newest first
/// by when each last came back to the user, so a thread that leaves Working lands on top. The
/// order the user arranged doesn't apply. Ties go by id.
fn sort_by_return(threads: &mut [&Value], returns: &Returns) {
    threads.sort_by_cached_key(|&t| (Reverse(returned_ms(t, returns)), id(t)));
}

/// When the thread last came back: created, reopened, its latest run asked for or finished, or
/// seen leaving Working, whichever is latest.
fn returned_ms(thread: &Value, returns: &Returns) -> i64 {
    let at = |key: &str| instant(&thread[key]).unwrap_or(0);
    let run = latest_run(thread);
    let run_times = run
        .iter()
        .flat_map(|run| [run.requested_at, run.completed_at])
        .filter_map(instant);
    [
        at("createdAt"),
        at("unsettledAt"),
        returns.returned_at(thread).unwrap_or(0),
    ]
    .into_iter()
    .chain(run_times)
    .max()
    .unwrap_or(0)
}

/// `sortWorkingThreadsBySend`: Working lists threads newest first by the last message the user
/// sent, so a run finishing or waking again doesn't move a card. Ties go by id.
fn sort_by_send(threads: &mut [&Value]) {
    threads.sort_by_cached_key(|&t| (Reverse(sent_ms(t)), id(t)));
}

/// When the user last sent the thread a message, or when it was created if that is later. A
/// server that leaves out `latestUserAuthoredMessageAt` gives the latest run's request time
/// instead, which a wake moves. One that sends it as null gives 0.
fn sent_ms(thread: &Value) -> i64 {
    let sent = match thread.get("latestUserAuthoredMessageAt") {
        Some(at) => instant(at),
        None => latest_run(thread).and_then(|run| instant(run.requested_at)),
    };
    instant(&thread["createdAt"])
        .unwrap_or(0)
        .max(sent.unwrap_or(0))
}

/// The visible threads on each shelf, in the order the GUI lists them.
#[derive(Default)]
struct Shelves<'a> {
    pinned: Vec<&'a Value>,
    inbox: Vec<&'a Value>,
    working: Vec<&'a Value>,
    done: Vec<&'a Value>,
    snoozed: Vec<&'a Value>,
    settled: Vec<&'a Value>,
}

/// The partition in `Sidebar.tsx`. `returns` is None while the Working shelf is off. With it
/// on, threads in the inbox that belong in Working go there instead. Only the inbox folds
/// away: a pinned, snoozed or settled thread keeps its shelf while it works. Then the inbox's
/// threads that read Done move to their own shelf in the inbox's order. A thread that works
/// never reads Done, so the two never compete.
fn shelves<'a>(
    threads: &'a [Value],
    capabilities: Capabilities,
    now: i64,
    returns: Option<&Returns>,
) -> Shelves<'a> {
    let mut shelves = Shelves::default();
    for thread in threads.iter().filter(|t| visible(t)) {
        let list = match shelf(thread, capabilities, now) {
            Shelf::Pinned => &mut shelves.pinned,
            Shelf::Inbox if returns.is_some() && belongs_in_working(thread) => &mut shelves.working,
            Shelf::Inbox => &mut shelves.inbox,
            Shelf::Working => &mut shelves.working,
            Shelf::Done => &mut shelves.done,
            Shelf::Snoozed => &mut shelves.snoozed,
            Shelf::Settled => &mut shelves.settled,
        };
        list.push(thread);
    }
    sort_pinned(&mut shelves.pinned);
    match returns {
        Some(returns) => sort_by_return(&mut shelves.inbox, returns),
        None => sort_active(&mut shelves.inbox),
    }
    (shelves.done, shelves.inbox) = std::mem::take(&mut shelves.inbox)
        .into_iter()
        .partition::<Vec<_>, _>(|t| status_word(t, now) == Some(Word::Done));
    sort_by_send(&mut shelves.working);
    // Soonest wake first. Every wake here parses, or the thread wouldn't be snoozed.
    shelves
        .snoozed
        .sort_by_cached_key(|&t| instant(&t["snoozedUntil"]).unwrap_or(0));
    sort_settled(&mut shelves.settled);
    shelves
}

// ---- rows ----

/// Where t3rex lists a thread in Rex, which has one list of windows and no shelves. A snoozed,
/// settled, archived or subagent thread isn't listed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    /// It's the user's turn: Approval, Input, Failed, Limited, Woke or Done.
    Attention,
    /// Pinned or in the inbox, with nothing to say.
    Open,
    /// On the Working shelf. A pinned thread stays Open while it works, as in the GUI.
    Working,
}

/// T3's sidebar order as one list: what needs the user, then the pins and the rest of the
/// inbox, then Working. It keeps the Working shelf's return stamps between calls, as the
/// sidebar does.
#[derive(Debug, Default)]
pub struct Inbox {
    returns: Returns,
}

impl Inbox {
    pub fn order<'a>(
        &mut self,
        threads: &'a [Value],
        capabilities: Capabilities,
        now: i64,
    ) -> Vec<(&'a Value, Place)> {
        self.returns.observe(threads, now);
        let shelves = shelves(threads, capabilities, now, Some(&self.returns));
        let attention = |t: &Value| {
            matches!(
                status_word(t, now),
                Some(
                    Word::Approval
                        | Word::Input
                        | Word::Failed
                        | Word::Limited
                        | Word::Woke
                        | Word::Done
                )
            )
        };
        let open = || shelves.pinned.iter().chain(&shelves.inbox);
        let mut list: Vec<(&Value, Place)> = open()
            .chain(&shelves.done)
            .filter(|t| attention(t))
            .map(|t| (*t, Place::Attention))
            .collect();
        list.extend(open().filter(|t| !attention(t)).map(|t| (*t, Place::Open)));
        list.extend(shelves.working.iter().map(|t| (*t, Place::Working)));
        list
    }
}

/// The word T3's sidebar shows on the thread's card at `now`, such as Approval or Working.
pub fn status_label(thread: &Value, now: i64) -> Option<&'static str> {
    status_word(thread, now).map(Word::label)
}

/// What T3's sidebar would ask of the user for this thread: `user_input` for a question,
/// another request kind for an approval, or None.
pub fn pending_kind(thread: &Value) -> Option<&str> {
    pending(thread).and(thread["pendingRuntimeRequest"]["kind"].as_str())
}

/// A thread from the shell, by id.
pub fn find_thread<'a>(shell: &'a ShellState, id: &str) -> Option<&'a Value> {
    shell.threads.iter().find(|t| str_of(t, "id") == id)
}

/// A project's title as T3 sent it, or `Project` when the shell doesn't have the project.
pub fn project_title<'a>(shell: Option<&'a ShellState>, project_id: &str) -> &'a str {
    shell
        .and_then(|s| s.projects.iter().find(|p| str_of(p, "id") == project_id))
        .map_or("Project", |p| str_of(p, "title"))
}

/// Milliseconds since the Unix epoch for an ISO-8601 UTC timestamp such as
/// `2026-10-08T00:06:52.769Z`. Offsets other than `Z` are not needed: the server sends UTC.
pub fn parse_iso_ms(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() < 19 {
        return None;
    }
    let num = |from: usize, to: usize| -> Option<i64> { text.get(from..to)?.parse().ok() };
    let (year, month, day) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (hour, minute, second) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    let mut millis = 0;
    if bytes.get(19) == Some(&b'.') {
        let digits: String = text[20..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        let mut fraction = digits.clone();
        fraction.truncate(3);
        while fraction.len() < 3 {
            fraction.push('0');
        }
        millis = fraction.parse().ok()?;
    }
    // Days from civil, after Howard Hinnant.
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some((((days * 24 + hour) * 60 + minute) * 60 + second) * 1000 + millis)
}

/// `parse_iso_ms`, under the name t3rex uses.
pub use parse_iso_ms as parse_iso;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ALL: Capabilities = Capabilities {
        settlement: true,
        snooze: true,
    };

    /// The tests' clock: noon on 2026-10-08.
    fn now() -> i64 {
        parse_iso_ms("2026-10-08T12:00:00.000Z").unwrap()
    }

    fn ms(iso: &str) -> i64 {
        parse_iso_ms(iso).unwrap()
    }

    /// An idle top-level thread that has never run, with every field the sidebar reads.
    fn thread(id: &str) -> Value {
        json!({
            "id": id,
            "projectId": "p1",
            "title": format!("Thread {id}"),
            "branch": null,
            "modelSelection": {"instanceId": "codex"},
            "lineage": {"parentThreadId": null, "relationshipToParent": null, "rootThreadId": id},
            "latestRunId": null,
            "activeProviderThreadId": null,
            "status": "idle",
            "pendingRuntimeRequest": null,
            "latestUserMessageAt": null,
            "createdAt": "2026-10-01T00:00:00.000Z",
            "updatedAt": "2026-10-01T00:00:00.000Z",
            "archivedAt": null,
            "settledOverride": null,
            "settledAt": null,
        })
    }

    fn with(mut thread: Value, fields: Value) -> Value {
        for (key, value) in fields.as_object().unwrap() {
            thread[key.as_str()] = value.clone();
        }
        thread
    }

    fn ids<'a>(threads: &[&'a Value]) -> Vec<&'a str> {
        threads.iter().map(|&t| id(t)).collect()
    }

    /// Snoozed for an hour from 11:00, so asleep at noon.
    fn snoozed(id: &str) -> Value {
        with(
            thread(id),
            json!({"snoozedUntil": "2026-10-08T13:00:00.000Z", "snoozedAt": "2026-10-08T11:00:00.000Z"}),
        )
    }

    #[test]
    fn a_snooze_outranks_settlement_which_outranks_a_pin() {
        assert_eq!(shelf_for(true, true, true), Shelf::Snoozed);
        assert_eq!(shelf_for(false, true, true), Shelf::Settled);
        assert_eq!(shelf_for(false, false, true), Shelf::Pinned);
        assert_eq!(shelf_for(false, false, false), Shelf::Inbox);

        let everything = with(
            snoozed("t"),
            json!({"settledOverride": "settled", "pinnedAt": "2026-10-02T00:00:00.000Z"}),
        );
        assert_eq!(shelf(&everything, ALL, now()), Shelf::Snoozed);
        let woken = with(
            everything,
            json!({"snoozedUntil": "2026-10-08T11:30:00.000Z"}),
        );
        assert_eq!(shelf(&woken, ALL, now()), Shelf::Settled);
    }

    #[test]
    fn capabilities_gate_snooze_and_settlement_but_never_a_pin() {
        let everything = with(
            snoozed("t"),
            json!({"settledOverride": "settled", "pinnedAt": "2026-10-02T00:00:00.000Z"}),
        );
        let none = Capabilities::default();
        let settlement = Capabilities {
            settlement: true,
            snooze: false,
        };
        let snooze = Capabilities {
            settlement: false,
            snooze: true,
        };
        assert_eq!(shelf(&everything, none, now()), Shelf::Pinned);
        assert_eq!(shelf(&everything, settlement, now()), Shelf::Settled);
        assert_eq!(shelf(&everything, snooze, now()), Shelf::Snoozed);

        assert_eq!(
            Capabilities::of(&json!({"threadSettlement": true, "threadSnooze": "yes"})),
            settlement
        );
        assert_eq!(Capabilities::of(&Value::Null), none);
    }

    #[test]
    fn a_settled_at_stamp_alone_does_not_settle_a_thread() {
        let stamped = with(
            thread("t"),
            json!({"settledAt": "2026-10-05T00:00:00.000Z"}),
        );
        assert_eq!(shelf(&stamped, ALL, now()), Shelf::Inbox);
        let reopened = with(stamped, json!({"settledOverride": "active"}));
        assert_eq!(shelf(&reopened, ALL, now()), Shelf::Inbox);
    }

    #[test]
    fn pinned_threads_follow_their_keys_then_newest_created() {
        let pinned = |id: &str, fields: Value| {
            with(
                with(thread(id), json!({"pinnedAt": "2026-10-02T00:00:00.000Z"})),
                fields,
            )
        };
        let threads = vec![
            pinned("n1", json!({"createdAt": "2026-10-03T00:00:00.000Z"})),
            pinned("k1", json!({"pinOrderKey": "m"})),
            pinned("n3", json!({"createdAt": "not a time"})),
            pinned("k3", json!({"pinOrderKey": "c"})),
            pinned("n2", json!({"createdAt": "2026-10-05T00:00:00.000Z"})),
            pinned("k2", json!({"pinOrderKey": "c"})),
        ];
        // Capabilities don't change the pinned order.
        for capabilities in [ALL, Capabilities::default()] {
            let shelves = shelves(&threads, capabilities, now(), None);
            assert_eq!(ids(&shelves.pinned), ["k2", "k3", "k1", "n2", "n1", "n3"]);
        }
    }

    #[test]
    fn active_threads_lead_with_new_and_reopened_then_arranged() {
        let threads = vec![
            with(
                thread("a1"),
                json!({"createdAt": "2026-10-02T00:00:00.000Z"}),
            ),
            with(thread("k1"), json!({"activeOrderKey": "b"})),
            with(
                thread("a2"),
                json!({"createdAt": "2026-10-01T00:00:00.000Z", "unsettledAt": "2026-10-06T00:00:00.000Z"}),
            ),
            with(
                thread("a4"),
                json!({"createdAt": "2026-10-04T00:00:00.000Z"}),
            ),
            with(thread("k2"), json!({"activeOrderKey": "a"})),
            with(
                thread("a3"),
                json!({"createdAt": "2026-10-04T00:00:00.000Z"}),
            ),
        ];
        let shelves = shelves(&threads, ALL, now(), None);
        assert_eq!(ids(&shelves.inbox), ["a2", "a3", "a4", "a1", "k2", "k1"]);
    }

    #[test]
    fn settled_threads_sort_by_when_they_settled_with_fallbacks() {
        let settled = |id: &str, fields: Value| {
            with(
                with(thread(id), json!({"settledOverride": "settled"})),
                fields,
            )
        };
        let threads = vec![
            settled(
                "s3",
                json!({"settledAt": null, "updatedAt": "2026-10-04T00:00:00.000Z"}),
            ),
            settled("s5", json!({"settledAt": "2026-10-05T00:00:00.000Z"})),
            settled(
                "s2",
                json!({
                    "settledAt": "soon",
                    "latestUserMessageAt": "2026-10-03T00:00:00.000Z",
                    "latestRunId": "r",
                    "latestRunCompletedAt": "2026-10-06T00:00:00.000Z",
                }),
            ),
            settled("s4", json!({"updatedAt": "bad", "createdAt": "bad"})),
            settled("s1", json!({"settledAt": "2026-10-05T00:00:00.000Z"})),
        ];
        let shelves = shelves(&threads, ALL, now(), None);
        assert_eq!(ids(&shelves.settled), ["s2", "s1", "s5", "s3", "s4"]);
        assert_eq!(settled_ms(&threads[2]), ms("2026-10-06T00:00:00.000Z"));
        assert_eq!(settled_ms(&threads[3]), 0);
    }

    #[test]
    fn timestamps_parse_only_in_the_form_t3_writes() {
        let at = |text: &str| instant(&json!(text));
        assert_eq!(
            at("2026-10-08T12:00:00.000Z"),
            Some(ms("2026-10-08T12:00:00.000Z"))
        );
        assert_eq!(at("2026-10-08T12:00:00Z"), Some(now()));
        assert_eq!(at("2026-10-08T12:00:00.5Z"), Some(now() + 500));
        for bad in [
            "",
            "tomorrow",
            "2026-10-08",
            "2026-10-08 12:00:00Z",
            "2026-10-08T12:00:00",
            "2026-10-08T12:00:00.Z",
            "2026-10-08T12:00:00+02:00",
            "2026-13-08T12:00:00Z",
            "2026-10-32T12:00:00Z",
            "2026-10-08T24:00:00Z",
            "2026-10-08T12:60:00Z",
        ] {
            assert_eq!(at(bad), None, "{bad}");
        }
        assert_eq!(instant(&json!(12345)), None);
        assert_eq!(instant(&Value::Null), None);
    }

    #[test]
    fn a_snooze_ends_at_its_wake_time() {
        let thread = with(
            snoozed("t"),
            json!({"snoozedUntil": "2026-10-08T12:00:30.000Z"}),
        );
        let wake = ms("2026-10-08T12:00:30.000Z");
        assert!(effective_snoozed(&thread, now()));
        assert!(effective_snoozed(&thread, wake - 1));
        assert!(!effective_snoozed(&thread, wake));
        assert_eq!(shelf(&thread, ALL, wake), Shelf::Inbox);
    }

    #[test]
    fn a_malformed_or_missing_wake_time_never_hides_a_thread() {
        for wake in [
            json!("tomorrow"),
            json!("2026-13-01T00:00:00Z"),
            json!("2026-10-08 13:00:00Z"),
            json!(12345),
            Value::Null,
        ] {
            let thread = with(snoozed("t"), json!({"snoozedUntil": wake.clone()}));
            assert!(!effective_snoozed(&thread, now()), "{wake}");
            assert_eq!(shelf(&thread, ALL, now()), Shelf::Inbox, "{wake}");
        }
        let mut missing = snoozed("t");
        missing.as_object_mut().unwrap().remove("snoozedUntil");
        assert!(!effective_snoozed(&missing, now()));
    }

    #[test]
    fn a_raised_hand_wakes_a_snooze_early() {
        let woke = |fields: Value| !effective_snoozed(&with(snoozed("t"), fields), now());
        assert!(!woke(json!({})));

        // The agent waits on the user.
        assert!(woke(json!({"pendingRuntimeRequest": {"kind": "approval"}})));
        assert!(woke(
            json!({"pendingRuntimeRequest": {"kind": "user_input"}})
        ));
        assert!(!woke(
            json!({"pendingRuntimeRequest": {"kind": "auth_refresh"}})
        ));

        // A failure counts when it is newer than the snooze.
        let failed =
            |updated: &str| json!({"latestRunId": "r", "status": "failed", "updatedAt": updated});
        assert!(woke(failed("2026-10-08T11:30:00.000Z")));
        assert!(!woke(failed("2026-10-08T10:30:00.000Z")));
        assert!(woke(with(
            failed("2026-10-08T10:30:00.000Z"),
            json!({"snoozedAt": null})
        )));
        // Without a run or a provider thread there is no runtime to fail.
        assert!(!woke(
            json!({"status": "failed", "updatedAt": "2026-10-08T11:30:00.000Z"})
        ));
        // A live activity run outranks the thread's failed status.
        assert!(!woke(with(
            failed("2026-10-08T11:30:00.000Z"),
            json!({"activityRunStatus": "running"})
        )));

        // A run that completed after the snooze began.
        let completed =
            |at: &str| json!({"latestRunId": "r", "status": "idle", "latestRunCompletedAt": at});
        assert!(woke(completed("2026-10-08T11:30:00.000Z")));
        assert!(!woke(completed("2026-10-08T10:30:00.000Z")));
        assert!(!woke(with(
            completed("2026-10-08T11:30:00.000Z"),
            json!({"status": "running"})
        )));
        // A server that leaves out the completion time has it read from `updatedAt`.
        assert!(woke(json!({
            "latestRunId": "r",
            "status": "completed",
            "updatedAt": "2026-10-08T11:30:00.000Z",
        })));
        // Without `snoozedAt`, a completion can't be placed after the snooze.
        assert!(!woke(with(
            completed("2026-10-08T11:30:00.000Z"),
            json!({"snoozedAt": null})
        )));
    }

    #[test]
    fn archived_threads_and_subagents_hide_but_forks_stay() {
        let archived = with(
            thread("archived"),
            json!({"archivedAt": "2026-10-05T00:00:00.000Z"}),
        );
        let subagent = with(
            thread("subagent"),
            json!({"lineage": {"parentThreadId": "a", "relationshipToParent": "subagent", "rootThreadId": "a"}}),
        );
        let fork = with(
            thread("fork"),
            json!({"lineage": {"parentThreadId": "a", "relationshipToParent": "fork", "rootThreadId": "a"}}),
        );
        assert!(!visible(&archived));
        assert!(!visible(&subagent));
        assert!(visible(&fork));
        let threads = vec![thread("a"), archived, subagent, fork];
        let shelves = shelves(&threads, ALL, now(), None);
        let mut shown = ids(&shelves.inbox);
        shown.sort();
        assert_eq!(shown, ["a", "fork"]);
    }

    /// The word a thread made of `fields` shows at noon.
    fn word_of(fields: Value) -> Option<Word> {
        status_word(&with(thread("t"), fields), now())
    }

    /// The fields of a thread whose run started at 11:57 and is still going, as a nightly
    /// server sends them.
    fn running() -> Value {
        json!({
            "latestRunId": "r",
            "activeRunId": "r",
            "status": "running",
            "latestRunRequestedAt": "2026-10-08T11:56:50.000Z",
            "latestRunStartedAt": "2026-10-08T11:57:00.000Z",
            "latestRunCompletedAt": null,
            "activityRunStatus": "running",
            "activityRunStartedAt": "2026-10-08T11:57:00.000Z",
        })
    }

    /// The fields of a thread whose run finished at 11:00 and was last visited at `visited`.
    fn finished(visited: Value) -> Value {
        json!({
            "latestRunId": "r",
            "status": "completed",
            "latestRunCompletedAt": "2026-10-08T11:00:00.000Z",
            "lastVisitedAt": visited,
        })
    }

    fn without(mut thread: Value, key: &str) -> Value {
        thread.as_object_mut().unwrap().remove(key);
        thread
    }

    #[test]
    fn a_request_for_the_user_outranks_what_the_agent_is_doing() {
        let asking =
            |kind: Value| with(running(), json!({"pendingRuntimeRequest": {"kind": kind}}));
        // The shell carries one request, so Approval and Input are never pending together.
        // Every kind but a question is an approval, including kinds this build doesn't know.
        for kind in [
            "command",
            "file-read",
            "file-change",
            "mcp-elicitation",
            "permission",
            "dynamic_tool_call",
            "something_new",
        ] {
            assert_eq!(word_of(asking(json!(kind))), Some(Word::Approval), "{kind}");
        }
        assert_eq!(word_of(asking(json!("user_input"))), Some(Word::Input));
        assert_eq!(
            word_of(json!({"pendingRuntimeRequest": {"id": "q"}})),
            Some(Word::Approval),
            "a request without a kind still waits on the user"
        );

        // An auth refresh isn't the user's to answer, so the runtime's word shows instead.
        assert_eq!(word_of(asking(json!("auth_refresh"))), Some(Word::Working));
        assert_eq!(
            word_of(json!({"pendingRuntimeRequest": {"kind": "auth_refresh"}})),
            None
        );

        // A request outranks a failure, a goal and a wake.
        let failed = json!({"latestRunId": "r", "status": "failed", "pendingRuntimeRequest": {"kind": "permission"}});
        assert_eq!(word_of(failed), Some(Word::Approval));
        let goal = with(
            asking(json!("user_input")),
            json!({"goal": {"objective": "Ship it", "status": "active"}}),
        );
        assert_eq!(word_of(goal), Some(Word::Input));
        let woken = json!({
            "snoozedAt": "2026-10-08T10:00:00.000Z",
            "snoozedUntil": "2026-10-08T11:30:00.000Z",
            "pendingRuntimeRequest": {"kind": "user_input"},
        });
        assert_eq!(word_of(woken), Some(Word::Input));
    }

    #[test]
    fn the_runtime_decides_working_waiting_limited_and_failed() {
        for state in ["preparing", "queued", "starting", "running", "waiting"] {
            assert_eq!(
                word_of(json!({"latestRunId": "r", "status": state})),
                Some(Word::Working),
                "{state}"
            );
        }
        // The activity run outranks the thread's status, as when a wake continues the work.
        assert_eq!(
            word_of(
                json!({"latestRunId": "r", "status": "completed", "activityRunStatus": "starting"})
            ),
            Some(Word::Working)
        );

        // Background work that will wake the agent parks it at Waiting. A command it left
        // running, such as a dev server, doesn't.
        let background = |kinds: &[&str]| {
            let tasks: Vec<Value> = kinds
                .iter()
                .map(|kind| json!({"taskId": format!("task-{kind}"), "kind": kind}))
                .collect();
            json!({"latestRunId": "r", "status": "completed", "pendingBackgroundTasks": tasks})
        };
        for kinds in [
            &["subagent"][..],
            &["monitor"],
            &["background_task"],
            &["kind_from_the_future"],
            &["command", "subagent"],
        ] {
            assert_eq!(word_of(background(kinds)), Some(Word::Waiting), "{kinds:?}");
        }
        assert_eq!(word_of(background(&["command"])), None);
        assert_eq!(word_of(background(&[])), None);
        // Work that holds a thread can hold one that never ran.
        let unrun = json!({"pendingBackgroundTasks": [{"taskId": "w", "kind": "monitor"}]});
        assert_eq!(word_of(unrun), Some(Word::Waiting));

        // A usage limit is Limited. Every other failure, or one with no class, is Failed.
        let failed =
            |class: Value| json!({"latestRunId": "r", "status": "failed", "lastErrorClass": class});
        assert_eq!(word_of(failed(json!("usage_limit"))), Some(Word::Limited));
        for class in [
            json!("provider_error"),
            json!("transport_error"),
            json!("permission_error"),
            json!("validation_error"),
            json!("unknown"),
            Value::Null,
        ] {
            assert_eq!(
                word_of(failed(class.clone())),
                Some(Word::Failed),
                "{class}"
            );
        }
        assert_eq!(
            word_of(without(failed(Value::Null), "lastErrorClass")),
            Some(Word::Failed)
        );
        // A failure outranks the background roster, so it stays visible.
        let held = with(
            failed(json!("usage_limit")),
            json!({"pendingBackgroundTasks": [{"taskId": "s", "kind": "subagent"}]}),
        );
        assert_eq!(word_of(held), Some(Word::Limited));
        // A live activity run outranks the thread's failed status.
        assert_eq!(
            word_of(with(
                failed(json!("usage_limit")),
                json!({"activityRunStatus": "running"})
            )),
            Some(Word::Working)
        );
        // Without a run or a provider thread there is no runtime, so nothing failed.
        assert_eq!(
            word_of(json!({"status": "failed", "lastErrorClass": "usage_limit"})),
            None
        );

        // A run that ended for any other reason leaves the thread at rest.
        for state in [
            "completed",
            "interrupted",
            "cancelled",
            "rolled_back",
            "nonsense",
        ] {
            assert_eq!(
                word_of(json!({"latestRunId": "r", "status": state})),
                None,
                "{state}"
            );
        }
        assert_eq!(word_of(json!({"latestRunId": "r", "status": 7})), None);
        assert_eq!(word_of(json!({})), None, "a thread that never ran");
        // The server sends `idle` only before the first run. The GUI reads it as Waiting
        // whenever a runtime exists, as it does for a provider thread with no run yet.
        assert_eq!(
            word_of(json!({"activeProviderThreadId": "pt"})),
            Some(Word::Waiting)
        );
        assert_eq!(
            word_of(json!({"latestRunId": "r", "status": "idle"})),
            Some(Word::Waiting)
        );
    }

    #[test]
    fn working_reads_goal_only_while_the_goal_is_active() {
        let goal = |status: &str| json!({"goal": {"objective": "Ship it", "status": status}});
        assert_eq!(word_of(with(running(), goal("active"))), Some(Word::Goal));
        for status in [
            "paused",
            "blocked",
            "usage_limited",
            "budget_limited",
            "complete",
        ] {
            assert_eq!(
                word_of(with(running(), goal(status))),
                Some(Word::Working),
                "{status}"
            );
        }
        // A server without goals leaves the field out, and a malformed one is no goal.
        assert_eq!(word_of(running()), Some(Word::Working));
        assert_eq!(
            word_of(with(running(), json!({"goal": null}))),
            Some(Word::Working)
        );
        assert_eq!(
            word_of(with(running(), json!({"goal": "active"}))),
            Some(Word::Working)
        );
        // An active goal on a thread at rest adds no word.
        assert_eq!(
            word_of(with(
                json!({"latestRunId": "r", "status": "completed"}),
                goal("active")
            )),
            None
        );
    }

    #[test]
    fn done_is_a_finish_nobody_has_seen() {
        assert_eq!(
            word_of(finished(json!("2026-10-08T10:00:00.000Z"))),
            Some(Word::Done)
        );
        assert_eq!(
            word_of(finished(json!("2026-10-08T11:00:00.000Z"))),
            None,
            "seen as it finished"
        );
        assert_eq!(word_of(finished(json!("2026-10-08T11:30:00.000Z"))), None);

        // A thread nobody has visited counts as seen, so a server doesn't mark its whole
        // history unread. A server without visit tracking leaves the field out, and then
        // nothing is Done.
        assert_eq!(word_of(finished(Value::Null)), None);
        assert_eq!(word_of(finished(json!(""))), None);
        assert_eq!(
            word_of(without(finished(Value::Null), "lastVisitedAt")),
            None
        );
        // A visit time that doesn't parse can't show the finish was seen.
        assert_eq!(word_of(finished(json!("yesterday"))), Some(Word::Done));

        // No finish time that parses, or no run, means nothing finished.
        let visited = json!("2026-10-08T10:00:00.000Z");
        assert_eq!(
            word_of(with(
                finished(visited.clone()),
                json!({"latestRunCompletedAt": "soon"})
            )),
            None
        );
        assert_eq!(
            word_of(with(
                finished(visited.clone()),
                json!({"latestRunCompletedAt": null})
            )),
            None
        );
        assert_eq!(
            word_of(json!({"lastVisitedAt": "2026-10-08T10:00:00.000Z"})),
            None
        );
        // A server that leaves out the finish time has it read from `updatedAt`.
        let implied = with(
            without(finished(visited.clone()), "latestRunCompletedAt"),
            json!({"updatedAt": "2026-10-08T11:00:00.000Z"}),
        );
        assert_eq!(word_of(implied), Some(Word::Done));
        // An interrupted or cancelled run finished too.
        for state in ["interrupted", "cancelled"] {
            assert_eq!(
                word_of(with(finished(visited.clone()), json!({"status": state}))),
                Some(Word::Done),
                "{state}"
            );
        }
        // What the thread is doing outranks an unseen finish.
        assert_eq!(
            word_of(with(finished(visited.clone()), json!({"status": "failed"}))),
            Some(Word::Failed)
        );
        assert_eq!(
            word_of(with(
                finished(visited),
                json!({"activityRunStatus": "running"})
            )),
            Some(Word::Working)
        );
    }

    #[test]
    fn a_thread_moves_through_its_words_as_the_shell_changes() {
        let mut t = with(
            thread("t"),
            json!({"lastVisitedAt": "2026-10-08T09:00:00.000Z"}),
        );
        let mut step = |fields: Value| {
            t = with(t.clone(), fields);
            status_word(&t, now())
        };
        assert_eq!(step(json!({})), None, "never ran");
        let queued = json!({
            "latestRunId": "r",
            "activeRunId": "r",
            "status": "queued",
            "latestRunRequestedAt": "2026-10-08T11:58:00.000Z",
            "latestRunCompletedAt": null,
        });
        assert_eq!(step(queued), Some(Word::Working));
        assert_eq!(
            step(json!({"status": "running", "pendingRuntimeRequest": {"kind": "command"}})),
            Some(Word::Approval)
        );
        assert_eq!(
            step(json!({"pendingRuntimeRequest": null})),
            Some(Word::Working)
        );
        // The run ends while a subagent it started is still out.
        let ended = json!({
            "status": "completed",
            "activeRunId": null,
            "latestRunCompletedAt": "2026-10-08T11:59:00.000Z",
            "pendingBackgroundTasks": [{"taskId": "s", "kind": "subagent"}],
        });
        assert_eq!(step(ended), Some(Word::Waiting));
        assert_eq!(
            step(json!({"pendingBackgroundTasks": []})),
            Some(Word::Done)
        );
        // Another client opens the thread.
        assert_eq!(
            step(json!({"lastVisitedAt": "2026-10-08T11:59:30.000Z"})),
            None
        );
    }

    #[test]
    fn woke_shows_until_a_visit_after_the_wake() {
        // Snoozed at 10:00 until 11:30, so its timer woke it half an hour before noon.
        let slept = |fields: Value| {
            with(
                json!({
                    "snoozedAt": "2026-10-08T10:00:00.000Z",
                    "snoozedUntil": "2026-10-08T11:30:00.000Z",
                }),
                fields,
            )
        };
        let visited = |at: Value| slept(json!({"lastVisitedAt": at}));
        assert_eq!(word_of(slept(json!({}))), Some(Word::Woke), "never visited");
        assert_eq!(word_of(visited(Value::Null)), Some(Word::Woke));
        // The GUI's visit records the thread's `updatedAt`, which a timer wake doesn't move,
        // so a visit after a timer wake still lands before it.
        assert_eq!(
            word_of(visited(json!("2026-10-08T11:00:00.000Z"))),
            Some(Word::Woke),
            "visited before the wake"
        );
        // Dismissing Woke in the GUI records a visit at the wake time.
        assert_eq!(word_of(visited(json!("2026-10-08T11:30:00.000Z"))), None);
        assert_eq!(word_of(visited(json!("2026-10-08T11:45:00.000Z"))), None);
        assert_eq!(
            word_of(visited(json!("not a time"))),
            Some(Word::Woke),
            "a visit time that doesn't parse is no visit"
        );

        // A settled thread isn't Woke. Any other override is.
        assert_eq!(word_of(slept(json!({"settledOverride": "settled"}))), None);
        assert_eq!(
            word_of(slept(json!({"settledOverride": "active"}))),
            Some(Word::Woke)
        );
        // A wake time that is missing or doesn't parse never woke anything.
        assert_eq!(word_of(slept(json!({"snoozedUntil": "soon"}))), None);
        assert_eq!(word_of(slept(json!({"snoozedUntil": null}))), None);
        assert_eq!(word_of(without(slept(json!({})), "snoozedUntil")), None);

        // Woke outranks an unseen finish, and what the thread is doing outranks Woke.
        let unseen = finished(json!("2026-10-08T08:00:00.000Z"));
        assert_eq!(
            word_of(with(
                unseen.clone(),
                json!({"latestRunCompletedAt": "2026-10-08T09:00:00.000Z"})
            )),
            Some(Word::Done)
        );
        assert_eq!(
            word_of(slept(with(
                unseen,
                json!({"latestRunCompletedAt": "2026-10-08T09:00:00.000Z"})
            ))),
            Some(Word::Woke)
        );
        assert_eq!(word_of(slept(running())), Some(Word::Working));
    }

    #[test]
    fn a_timer_wake_shows_woke_from_the_wake_time() {
        let t = with(
            snoozed("t"),
            json!({"snoozedUntil": "2026-10-08T12:00:30.000Z"}),
        );
        let wake = ms("2026-10-08T12:00:30.000Z");
        assert_eq!(status_word(&t, wake - 1), None, "still asleep");
        assert_eq!(status_word(&t, wake), Some(Word::Woke));
        assert_eq!(woke_at(&t, wake), Some(&t["snoozedUntil"]));
    }

    #[test]
    fn an_early_wake_dates_from_when_the_hand_went_up() {
        // Snoozed at 11:00 until 13:00. A run finished at 11:30 and woke it early.
        let early = with(
            snoozed("t"),
            json!({"latestRunId": "r", "status": "completed", "latestRunCompletedAt": "2026-10-08T11:30:00.000Z"}),
        );
        let after_wake = ms("2026-10-08T14:00:00.000Z");
        assert_eq!(status_word(&early, now()), Some(Word::Woke));
        assert_eq!(
            woke_at(&early, now()),
            Some(&json!("2026-10-08T11:30:00.000Z"))
        );
        let visited = |at: &str| with(early.clone(), json!({"lastVisitedAt": at}));
        assert_eq!(
            status_word(&visited("2026-10-08T11:15:00.000Z"), now()),
            Some(Word::Woke),
            "a visit before the early wake doesn't count"
        );
        let seen = visited("2026-10-08T11:45:00.000Z");
        assert_eq!(status_word(&seen, now()), None);
        // The wake still dates from 11:30 once 13:00 passes, so the 11:45 visit still counts.
        assert_eq!(status_word(&seen, after_wake), None);
        assert_eq!(
            woke_at(&seen, after_wake),
            Some(&json!("2026-10-08T11:30:00.000Z"))
        );

        // A fresh failure woke it at the runtime's last update. The card says Failed.
        let failed = with(
            snoozed("t"),
            json!({"latestRunId": "r", "status": "failed", "updatedAt": "2026-10-08T11:40:00.000Z"}),
        );
        assert_eq!(
            woke_at(&failed, now()),
            Some(&json!("2026-10-08T11:40:00.000Z"))
        );
        assert_eq!(status_word(&failed, now()), Some(Word::Failed));
        // A request woke a thread with no runtime at the snooze's start.
        let asked = with(
            snoozed("t"),
            json!({"pendingRuntimeRequest": {"kind": "user_input"}}),
        );
        assert_eq!(
            woke_at(&asked, now()),
            Some(&json!("2026-10-08T11:00:00.000Z"))
        );
        // Once answered, the hand is down and the thread sleeps until 13:00 again.
        let answered = with(asked, json!({"pendingRuntimeRequest": null}));
        assert_eq!(woke_at(&answered, now()), None);
        assert_eq!(status_word(&answered, now()), None);
        // An auth refresh never raised a hand.
        let refreshing = with(
            snoozed("t"),
            json!({"pendingRuntimeRequest": {"kind": "auth_refresh"}}),
        );
        assert_eq!(woke_at(&refreshing, now()), None);
        assert!(effective_snoozed(&refreshing, now()));
    }

    // ---- drawing ----

    /// A thread whose run started at 11:57 and is still going, so it belongs in Working.
    fn busy(id: &str, fields: Value) -> Value {
        with(with(thread(id), running()), fields)
    }

    #[test]
    fn only_busy_threads_with_nothing_for_the_user_belong_in_working() {
        let works = |fields: Value| belongs_in_working(&with(thread("t"), fields));
        // A run under way, in each of its live states.
        for state in ["preparing", "queued", "starting", "running", "waiting"] {
            let run = json!({"latestRunId": "r", "activeRunId": "r", "status": state});
            assert!(works(run), "{state}");
        }
        assert!(works(running()));
        // A live activity run outranks the thread's status, as when a wake continues the work.
        assert!(works(
            json!({"latestRunId": "r", "status": "failed", "activityRunStatus": "running"})
        ));

        // Stopped with background work that will wake it, which presents as idle. So does a
        // provider thread with no run yet, and the `idle` the server sends before a first run.
        let monitored = json!({
            "latestRunId": "r",
            "status": "completed",
            "pendingBackgroundTasks": [{"taskId": "m", "kind": "monitor"}],
        });
        assert!(works(monitored.clone()));
        assert!(works(json!({"activeProviderThreadId": "pt"})));
        assert!(works(json!({"latestRunId": "r", "status": "idle"})));
        // A command it left running, such as a dev server, doesn't hold it.
        assert!(!works(
            json!({"latestRunId": "r", "status": "completed", "pendingBackgroundTasks": [{"taskId": "c", "kind": "command"}]})
        ));

        // At rest, never run, or ended any other way.
        assert!(!works(json!({})));
        for state in [
            "completed",
            "interrupted",
            "cancelled",
            "rolled_back",
            "failed",
        ] {
            assert!(
                !works(json!({"latestRunId": "r", "status": state})),
                "{state}"
            );
        }
        // A failure outranks the background work, so the thread stays where the user sees it.
        assert!(!works(with(monitored.clone(), json!({"status": "failed"}))));

        // A request for the user keeps the thread in Active. An auth refresh isn't one.
        for kind in [
            "command",
            "file-change",
            "permission",
            "user_input",
            "something_new",
        ] {
            let asking = with(running(), json!({"pendingRuntimeRequest": {"kind": kind}}));
            assert!(!works(asking), "{kind}");
        }
        assert!(works(with(
            running(),
            json!({"pendingRuntimeRequest": {"kind": "auth_refresh"}})
        )));
        assert!(!works(with(
            monitored.clone(),
            json!({"pendingRuntimeRequest": {"kind": "user_input"}})
        )));

        // A plan ready for a reply outranks background work once its run has settled.
        let plan = json!({"interactionMode": "plan", "hasActionableProposedPlan": true});
        assert!(!works(with(monitored.clone(), plan.clone())));
        assert!(!works(with(
            json!({"latestRunId": "r", "status": "idle"}),
            plan.clone()
        )));
        // It does while an activity run other than the latest goes on, too, since the latest
        // run has settled.
        assert!(!works(with(
            json!({"latestRunId": "r", "activeRunId": "r2", "status": "completed", "activityRunStatus": "running"}),
            plan.clone()
        )));
        // While the run that writes the plan is going, or the active run is still the latest,
        // the thread works.
        assert!(works(with(running(), plan.clone())));
        assert!(works(with(
            json!({"latestRunId": "r", "activeRunId": "r", "status": "completed", "activityRunStatus": "running"}),
            plan
        )));
        // Plan mode without a plan, or a plan outside plan mode, holds nothing back.
        assert!(works(with(
            monitored.clone(),
            json!({"interactionMode": "plan", "hasActionableProposedPlan": false})
        )));
        assert!(works(with(
            monitored,
            json!({"interactionMode": "default", "hasActionableProposedPlan": true})
        )));
    }

    #[test]
    fn working_lists_the_latest_send_first() {
        // As in the nightly's test: a wake run asked for after the other thread's send doesn't
        // move `sent-first`, and a thread an agent launched has no send, so its creation counts.
        let sent_first = busy(
            "sent-first",
            json!({
                "latestUserAuthoredMessageAt": "2026-10-08T09:00:00.000Z",
                "latestRunRequestedAt": "2026-10-08T11:59:00.000Z",
            }),
        );
        let sent_last = busy(
            "sent-last",
            json!({"latestUserAuthoredMessageAt": "2026-10-08T10:00:00.000Z"}),
        );
        let launched = busy(
            "launched",
            json!({"latestUserAuthoredMessageAt": null, "createdAt": "2026-10-08T08:00:00.000Z"}),
        );
        // A server that leaves the field out has the run's request stand in for the send.
        let older = busy(
            "older",
            json!({"latestRunRequestedAt": "2026-10-08T09:30:00.000Z"}),
        );
        assert!(older.get("latestUserAuthoredMessageAt").is_none());
        let mut threads = vec![launched, sent_first, older, sent_last];
        let order = |threads: &[Value]| {
            let returns = Returns::default();
            let shelves = shelves(threads, ALL, now(), Some(&returns));
            ids(&shelves.working)
                .into_iter()
                .map(str::to_string)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            order(&threads),
            ["sent-last", "older", "sent-first", "launched"]
        );
        assert_eq!(sent_ms(&threads[0]), ms("2026-10-08T08:00:00.000Z"));
        assert_eq!(sent_ms(&threads[1]), ms("2026-10-08T09:00:00.000Z"));

        // A wake that asks for another run doesn't move `sent-first`.
        threads[1] = with(
            threads[1].clone(),
            json!({"latestRunRequestedAt": "2026-10-08T11:59:30.000Z"}),
        );
        assert_eq!(
            order(&threads),
            ["sent-last", "older", "sent-first", "launched"]
        );
        // A message the user sends does.
        threads[1] = with(
            threads[1].clone(),
            json!({"latestUserAuthoredMessageAt": "2026-10-08T11:59:40.000Z"}),
        );
        assert_eq!(
            order(&threads),
            ["sent-first", "sent-last", "older", "launched"]
        );
        // On the older server a wake moves the thread, as it does in the GUI.
        threads[2] = with(
            threads[2].clone(),
            json!({"latestRunRequestedAt": "2026-10-08T11:59:50.000Z"}),
        );
        assert_eq!(
            order(&threads),
            ["older", "sent-first", "sent-last", "launched"]
        );
    }

    #[test]
    fn active_lists_the_latest_return_first() {
        // As in the nightly's test: the thread that finished last tops a newer one and a
        // reopened one.
        let new = with(
            thread("new"),
            json!({"createdAt": "2026-10-08T11:00:00.000Z"}),
        );
        let finished_now = with(
            thread("old-finished-now"),
            json!({
                "createdAt": "2026-10-01T09:00:00.000Z",
                "latestRunId": "r",
                "status": "completed",
                "latestRunRequestedAt": "2026-10-01T09:00:00.000Z",
                "latestRunCompletedAt": "2026-10-08T11:50:00.000Z",
            }),
        );
        let reopened = with(
            thread("reopened"),
            json!({"createdAt": "2026-10-02T09:00:00.000Z", "unsettledAt": "2026-10-08T11:30:00.000Z"}),
        );
        let threads = vec![new, finished_now, reopened];
        let returns = Returns::default();
        let shelves = shelves(&threads, ALL, now(), Some(&returns));
        assert_eq!(ids(&shelves.inbox), ["old-finished-now", "reopened", "new"]);

        // A return the server doesn't stamp, seen at 11:55, tops them all.
        let returns = Returns {
            working: Some(HashSet::new()),
            at: HashMap::from([("\0new".to_string(), ms("2026-10-08T11:55:00.000Z"))]),
        };
        assert_eq!(
            returned_ms(&threads[0], &returns),
            ms("2026-10-08T11:55:00.000Z")
        );
        let shelves = super::shelves(&threads, ALL, now(), Some(&returns));
        assert_eq!(ids(&shelves.inbox), ["new", "old-finished-now", "reopened"]);
        // A run's request counts as a return too, and a finish time that doesn't parse
        // counts as none.
        let asked = with(
            thread("asked"),
            json!({"latestRunId": "r", "status": "interrupted", "latestRunRequestedAt": "2026-10-08T11:40:00.000Z", "latestRunCompletedAt": "soon"}),
        );
        assert_eq!(
            returned_ms(&asked, &Returns::default()),
            ms("2026-10-08T11:40:00.000Z")
        );
    }

    /// A thread whose run finished at `at` on 2026-10-08, after the visit at 09:00, so it
    /// reads Done.
    fn unseen(id: &str, at: &str) -> Value {
        with(
            thread(id),
            json!({
                "latestRunId": "r",
                "status": "completed",
                "latestRunCompletedAt": format!("2026-10-08T{at}:00.000Z"),
                "lastVisitedAt": "2026-10-08T09:00:00.000Z",
            }),
        )
    }

    #[test]
    fn unseen_finishes_leave_the_inbox_for_the_done_shelf() {
        let seen = with(
            unseen("seen", "10:00"),
            json!({"lastVisitedAt": "2026-10-08T10:30:00.000Z"}),
        );
        let threads = vec![
            unseen("d1", "10:00"),
            seen,
            unseen("d2", "11:00"),
            with(
                unseen("pinned", "11:30"),
                json!({"pinnedAt": "2026-10-02T00:00:00.000Z"}),
            ),
            with(
                unseen("settled", "11:30"),
                json!({"settledOverride": "settled"}),
            ),
            thread("rest"),
        ];
        // With the Working shelf on, the inbox goes by return, so the latest finish leads.
        let returns = Returns::default();
        let on = shelves(&threads, ALL, now(), Some(&returns));
        assert_eq!(ids(&on.done), ["d2", "d1"]);
        assert_eq!(ids(&on.inbox), ["seen", "rest"]);
        // A pinned or settled thread keeps its shelf whatever its word.
        assert_eq!(status_word(&threads[3], now()), Some(Word::Done));
        assert_eq!(ids(&on.pinned), ["pinned"]);
        assert_eq!(ids(&on.settled), ["settled"]);

        // With it off, Done keeps its shelf, in the inbox's arranged order.
        let off = shelves(&threads, ALL, now(), None);
        assert_eq!(ids(&off.done), ["d1", "d2"]);
        assert_eq!(ids(&off.inbox), ["rest", "seen"]);
        assert!(off.working.is_empty());
    }

    /// Two machines can have a thread with the same id. Each keeps its own return stamp.
    #[test]
    fn the_same_id_on_two_machines_is_two_threads() {
        let busy = |env: &str| {
            with(
                thread("same"),
                json!({"t3rexEnv": env, "latestRunId": "r", "activeRunId": "r", "status": "running"}),
            )
        };
        let idle = |env: &str| with(thread("same"), json!({"t3rexEnv": env}));
        let mut returns = Returns::default();
        returns.observe(&[busy("a"), busy("b")], 1);
        returns.observe(&[idle("a"), busy("b")], 2);
        assert_eq!(returns.returned_at(&idle("a")), Some(2));
        assert_eq!(returns.returned_at(&busy("b")), None);
    }
}
