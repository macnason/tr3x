# Mobile performance implementation

This PR implements the host/client improvements from the sweep. It does not change Rex's
native iPhone renderer or T3 Code's frontend/server. No mobile speedup or T3 parity is claimed.

## Changes

- RPC reading/liveness runs independently of writing; writes have a 30-second deadline plus 1 s per 32 KiB of frame.
  Uncertain mutations are never automatically replayed. TCP_NODELAY is enabled.
- Live watchers use one bounded downstream value plus one unaccepted RPC chunk, capped at
  64 MiB encoded JSON per chunk. Values move instead of cloning. Ack waits for bounded
  forwarding; reconnect resumes after the last successfully forwarded sequence. Parsed
  JSON memory exceeds encoded byte counts; this is not a total-process-memory limit.
- HTTP calls share a connection pool, keep credentials request-local, prefer the last verified
  working origin, and jitter reconnect delays. Fallback origin probing stays serial.
- Content updates accumulate between presentation deadlines (33 ms); input remains immediate.
  Duplicate/ignored events do not invalidate presentation. Described blocks/embeds are cached
  per projection sequence. Stable live tails update only changed rows, including spinners.
  Cosmetic ticks skip missed frames and stop advancing when the terminal reports lost focus.
- Reply-only panes omit the unused whole-shell subscription. Initial config retries and
  config subscriptions stop when their consumer disappears.
- Images get asynchronous cached display derivatives bounded by pane width and 1536 × 1920
  pixels. The decoded per-pane budget is 16 MiB. Original attachments remain intact.
- Chromium capture uses bounded per-pane jobs, one cross-process lock, isolated profiles,
  a deadline and one retry. Theme generations reject obsolete results; cancellation cleans
  temporary capture files. Picture completions are drained together before reflow.
- Resize events coalesce for 50 ms before a history reflow. Existing terminal picture/scroll
  handling is preserved. Region changes that require reflow still reprint history.
- Sync reconciliations coalesce within 100 ms, drain a bounded burst, retain the five-second
  close-detection safety pass, and skip missed ticks. Rex CLI operations time out after 15 s.
- Markdown file links resolve against the owning worktree and request temporary signed T3
  asset downloads through its advertised tailnet endpoint. Requests are bounded, parsed
  bodies cached within 1 MiB, links refreshed before expiry, stale context results rejected,
  and unresolved targets rendered as plain text. Web links keep their destinations.

## File-link boundary

The installed T3 contracts support `assets.createUrl` with `media-file` and attachment
`disposition`; numeric `expiresAt` is epoch milliseconds. `workspace-file` preview rejects
Markdown, so the implementation uses downloads. A read-only probe downloaded this audit's
Markdown through the advertised private HTTPS hostname: HTTP 200, attachment disposition,
13,642 bytes. No signed URLs or credentials are included in this report.

A click offers the browser/OS download and open/share flow. Automatic editor selection and
line navigation are not provided: T3's native file viewer is client state with no supported
file deep-link route found in the installed frontend. Line suffixes are removed from the
asset path so they cannot turn into nonexistent filenames. A host `file://` URL would not
resolve on a phone or another computer.

## Validation

- `cargo test --workspace --locked`: 149 client tests + 146 Rex tests + 3 integration tests
  passed, one manual renderer test ignored.
- `cargo clippy --locked --workspace --all-targets -- -D warnings`: passed.
- `cargo fmt --all -- --check` and `git diff --check`: required before publication.
- Regression coverage includes reading while a write stalls, delayed stream Ack with ordered
  delivery, HTTP pool credential isolation, unchanged text omitted from spinner updates,
  image geometry/cache/original preservation, capture cancellation, theme generations,
  file-path parsing, expiry/context changes and unresolved versus web link rendering.
- The spinner regression was observed failing on the previous renderer, then passing with
  changed-row output. Transport and media tests were run after parallel implementation;
  no red-before-fix claim is made for those cases.

No actual Rex phone session, impaired cellular path, native-client profiling or direct
T3/Rex latency comparison was performed. The live file download verifies server support,
not native phone tap handling. Terminal resize behavior is covered by existing region tests,
with actual keyboard/rotation/scrollback checks still needed on Rex devices.

## Diagnostics and remaining measurements

Set `T3REX_PERF=1` when starting a scratch chat/sync to log per-pane frame counts, terminal
bytes and frame-build microseconds every five seconds of output. No conversation contents
or credentials are logged. These counters cover host output, not Rex's phone wire encoding.
Keep diagnostic sessions isolated from the installed sync.

The next device pass should measure cold/warm attach, tap feedback, send/approval round trips,
scrolling, keyboard/rotation, image-heavy history, reconnect, peak memory and phone wire bytes.
Use explicit throughput, RTT and loss settings, separately on host→T3 and phone→Rex paths.

A process-wide subscription broker, verified visibility-gated content/capture, full scrollback
preservation across geometry changes, server delta payloads/compression and native mobile
transport changes remain conditional follow-ups. This PR trims known wasted work without
introducing a new daemon or assuming unsupported Rex/T3 APIs.
