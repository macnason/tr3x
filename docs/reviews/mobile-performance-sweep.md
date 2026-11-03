# Rex / T3 client performance sweep

Reviewed 2026-11-03 at `39cf8cf`. Source audit; no runtime changes.

## Scope and evidence

Covered the Rust T3 client, thread projection/transcript construction, chat event loop,
composer, image preparation/display, machine subscriptions, Rex CLI adapter and session
reconciliation. Findings below distinguish code-proven work from unmeasured impact.
Rex's native mobile renderer/transport and T3 Code's server/frontend source were not
available in this checkout. No phone, impaired network, terminal wire capture, or live
T3-versus-Rex timing was measured. This is an optimization inventory, not a speedup claim.

Validation: `cargo test --workspace --locked` passed: 146 client tests, 138 Rex unit tests
and 3 integration tests; 1 manual test ignored. These establish the current test baseline,
not live mobile or impaired-network performance. Only this report was added.

The paths are T3 server → Rust client → terminal output → Rex server → Rex phone.
Rust runs on the host; its T3 connection is not necessarily the phone's cellular link.
Only a Rex-side capture can establish how terminal updates and image snapshots become
phone wire bytes. Improvements to host CPU and terminal output remain useful independently.

## Prioritized findings

### 1. High: blocked writes suspend RPC reads and liveness detection

`t3client/src/rpc.rs:295-378`: one select loop reads frames, writes outgoing messages,
and checks heartbeat/silence. Once the outgoing branch wins, it awaits `sink.feed` and
`sink.flush`, selecting only explicit shutdown. Ping sending has the same structure.
A stalled write therefore prevents reading incoming responses and checking the silence
deadline. Unary timeouts can expire while the shared connection remains open but stuck.
Attachment persistence uses this same socket (`env.rs:507-526`).

Fix: isolate the writer from the reader, with bounded outgoing work and a write-progress
deadline that closes/fails the shared connection. Preserve single-write semantics: a
timed-out command may have reached T3, so never automatically replay mutations. Keep the
existing final-owner shutdown, Ack/Interrupt ordering, bounded-stream and malformed-frame
contracts. Verify with a peer that stops reading while still sending replies, including a
large attachment; establish bounded failure and continued reads before failure.

### 2. High: live events redraw immediately, plus animation rebuilds the whole transcript

`t3rex/src/chat.rs:1310-1337,1455-1460,1540-1547`: each thread item makes the pane dirty;
the loop draws after each selected event. The 120 ms timer independently dirties every
animating pane. There is no presentation-rate cap or burst drain in this loop.
`Pane::draw` at line 943 calls `transcript::blocks` even for a spinner tick. That enumerates
and describes all items (`transcript.rs:1131`), and `ThreadState::items` allocates/sorts
the display list (`projection.rs:223`). `draw` also scans embeds and formats the live tail.
It clears every row from the live tail to the conversation region before rewriting it.
Committed history avoids repeat terminal printing, but not repeat transcript construction.

Fix first: apply ordered events immediately and present accumulated state on a short frame
deadline; keep typing, approvals, errors and final completion responsive. Ignore
`Applied::Duplicate`, `Ignored` and pure synchronization markers for visual invalidation.
Then retain described blocks by item revision and width/theme-dependent rendered lines;
draw spinner/timer changes independently. Do not key all invalidation only on item updates:
run status and shell metadata affect descriptions too. Start with caching per projection
generation before introducing per-entity indexes.

Verify terminal bytes and draw CPU for a long active response, tool bursts and a spinner-only
run; compare final screen/scrollback, approvals, drafts, streaming fences and completion.
Measure CPU separately from terminal bytes and actual Rex phone traffic.

### 3. High: background panes keep doing presentation work

`chat.rs:1369-1370` records focus; the check at line 1581 uses it to mark completed threads
visited. Focus does not gate animation, transcript drawing, subscriptions or media capture.
Every active background thread therefore still receives events and runs its redraw path.
This is code-proven host work; whether Rex forwards hidden-pane deltas needs measurement.

Fix: on reliable visibility signals, keep state current but defer presentation and capture
for hidden panes, then draw the latest state on reveal. Stop hidden animation first.
Do not assume terminal focus always means visibility: validate Rex's events on phone and
desktop, with simultaneous clients, before gating content output. Preserve status markers
and pending approvals independently of conversation presentation.

### 4. High: event queues have no memory or lag budget

`rpc.rs:650-661` clones stream values into an unbounded channel and immediately Acks receipt.
`env.rs:800-853` forwards them through another unbounded channel. Slow rendering does not
backpressure T3. Large replay bursts or slow consumers can accumulate stale work and memory.
The existing bounded-stream implementation is not used by these live watchers.

Fix: adapt bounded streaming with explicit byte/item budgets and consumer acceptance.
Preserve ordered projection events; only coalesce presentation, never discard arbitrary
domain events. On overflow, recover from the last safely applied cursor or a fresh bounded
snapshot. The current resume cursor tracks delivery into the queue, not reducer acceptance;
that distinction must change if overflow can discard queued events. Avoid holding the
pending-map lock while waiting on a consumer; one stalled stream must not stop other calls.

Verify deliberately stalled consumers, reconnect during backlog, oversized single events,
multiple streams and cancellation. Record queue depth, queued bytes and apply lag.

### 5. Medium: subscriptions and connections multiply with pane count

`chat.rs:1291-1292` starts a thread and whole-shell watcher per pane. Composer starts its own
config flow (`compose.rs:1579-1591,1626`), and sync also watches shell state. `Env::rpc` shares
a connection within one process, not across the separately launched pane processes. Split
conversation/reply mode repeats thread and shell subscriptions for both roles.

Simple first steps: omit the shell subscription from Reply-only mode if its consumers stay
unused; audit conversation-only composer startup for unnecessary config/logo work. The
larger option is a local broker holding one shell/config subscription per environment and
one thread subscription per used thread. That reduces repeated snapshots and parsing but
adds IPC ownership/recovery complexity; measure pane-count scaling before committing to it.

Verify 1/10/30 tabs, split versus inline, machine reconnects, and broker/pane restart if built.
Do not suspend the shell watcher responsible for pending-request tab markers.

### 6. High mobile opportunity: image memory budget is too coarse for cellular startup

`media.rs:14-16` permits 40 MiB of decoded pictures per pane. `picture` at line 258 sizes
display rows/columns but submits the original file, charging full width × height × 4 bytes.
Thus making a picture visually smaller does not reduce its decoded size. The source comments
say Rex includes RGBA pictures in attach snapshots; the actual current wire encoding and
compression have not been inspected. The 40 MiB ceiling protects a documented snapshot
limit, not a mobile latency budget.

Fix: generate bounded display derivatives for conversation images and HTML snapshots;
retain the original attachment through its link. Use target pixels and byte budgets, not
just terminal column scaling. Prefer visible/recent media and load older media on demand
where Rex supports it. A small explicit low-bandwidth setting is a reasonable first step
until device/network capability negotiation exists. Do not infer cellular state from the
host's network. Verify cold attach, retained scrollback, image clarity and theme/resize.

### 7. Medium: HTML capture fans out without a concurrency limit or timeout

`chat.rs:994-998` asks for every embed in the projection; `media.rs:189-245` spawns Chromium
per uncached embed, using the same profile directory, and awaits process exit without a
deadline. The attachment enters `asked` before work succeeds, so a failed capture cannot
retry during that Media instance. These jobs are not tied to pane visibility.

Fix: a small capture worker queue, unique or serialized profiles, a deadline, bounded retries
and cleanup on pane shutdown. Schedule visible/recent captures first. Theme changes should
invalidate/cancel obsolete jobs so an older theme cannot populate the current snapshot map.
Verify a thread containing many embeds, missing files, a hung Chromium and a theme change
while captures are in flight. Shared workers need a clear cross-process ownership model.

### 8. Medium: ordinary composer resizing can replay all history and images

`chat.rs:964-978` handles reflow by freeing images, clearing screen and scrollback, resetting
committed blocks and printing again. Terminal resize sets reflow; the documented region
shrink path can also reprint when Reply shrinks. This creates work and loses reading position
precisely during keyboard/composer/device size changes.

Fix: distinguish viewport resize, composer-region change and actual content invalidation.
Preserve committed history whenever Rex's terminal semantics allow it; coalesce intermediate
resize events and avoid repeated image retransmission. A bounded initial/reflow history with
an explicit older-history action is an alternative if full terminal reflow cannot be retained.
That changes UX and must be deliberate. Verify keyboard show/hide, multiline send, rotation,
Fit to This Device, scrolled-up reading, images and concurrent devices.

### 9. Medium: HTTP setup and routing repeat avoidable work

`env.rs:260-268` constructs a new Api per access; `http.rs:34-40` builds a fresh reqwest Client.
Repeated calls cannot reuse that client's connection pool. `env.rs:147-165` tests origins
serially with a 2.5 second descriptor timeout (`descriptor.rs:132`).

Fix: share the direct HTTP client per environment and remember the last verified working
origin; consider staggered fallback probes. Preserve identity/protocol checks before sending
credentials and route refresh on reconnect. Add jitter to independent watcher backoffs
(`env.rs:812-875`) to reduce synchronized retry bursts. Measure a dead preferred origin,
multiple reachable origins, token renewal and intermittent failures.

### 10. Medium: sync serially waits for unbounded Rex subprocess work

`sync.rs:1186,1229-1245` reconciles shell changes and every five-second tick, with burst
draining already present. Reconciliation inspects sessions (`sync.rs:875`) and applies
changes through the CLI. `rex.rs:42-63` launches a subprocess per operation and awaits
stdin/output with no deadline. Groups reconcile serially (`sync.rs:1394-1404`), so one stuck
CLI operation can stop all groups from progressing while input queues grow.

Fix first: operation deadlines and explicit errors; retain the periodic safety reconciliation
because closing a Rex tab changes T3 settlement. Coalesce invalidations, batch independent
mutations through the existing Lua path and inspect only affected groups between safety
passes. Consider persistent Rex API access only if supported; do not invent a second server.
Verify hung CLI, Rex quit/restart, close-to-settle, offline machines and session modes.

## Existing choices worth keeping

- Thread watches already request bounded snapshots and resume after sequence.
- Independent environment reconnects keep an offline machine from blocking the rest.
- Settled blocks print once in steady state; optimize the live tail around that model.
- Reply already avoids some redundant output and config redraws.
- Picture preparation runs in spawn_blocking, limits edges to 2000 pixels and picks encodings.
- Sync drains bursts and persistence avoids unchanged writes. Preserve atomic state replacement.
- Unknown JSON fields, protocol checks, mutation uncertainty and reconnect cursors are useful
  contracts, not simplification targets.

## Implementation order and acceptance evidence

1. Instrument frame build time, PTY bytes, queue bytes/depth, input latency and reconnect
   duration. Keep counters opt-in and exclude message contents/tokens.
2. Coalesce content presentation; isolate spinner updates; avoid duplicate-event redraws.
3. Add RPC write deadlines/read independence and bounded live-stream consumption.
4. Bound captures and display image pixels; suppress verified hidden-pane animation.
5. Reuse HTTP clients, trim role-specific subscriptions, bound Rex CLI calls.
6. Revisit broker ownership and reflow only with measurements from the simpler fixes.

For mobile validation use the same real thread in T3 and Rex, cold and warm, with short and
long histories, 1 and many working tabs, image-heavy history, approval during streaming,
keyboard/rotation and a dropped connection. Exercise explicit throughput/RTT/loss profiles;
record the chosen values rather than treating “3G” as a reproducible specification. Record
phone attach-to-readable time, tap-to-local-feedback, send/approve round trips, wire bytes,
peak host/phone memory, scrolling frame time and replay correctness. Separate host→T3
impairment from phone→Rex impairment. No numerical speedup target is justified yet.
