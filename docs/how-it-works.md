# How it works

T3 Code is the source of truth. tr3x never decides a thread's state; it follows T3, on every
machine it's paired with, and draws what T3 says into one Rex session.

## The session

- Rex runs sync. `~/.config/rex/tr3x.lua` starts `tr3x sync` as Rex loads its config, as a
  detached command that outlives the load, a config reload and a restart of Rex's server. Each
  load starts another; the sync already running holds `sync.lock`, so the new one leaves at
  once. The actions that need sync (⌃⌘I, ⌃⌘N) start it again if it isn't running. Sync never
  starts a Rex server itself: while Rex is quit it waits, and picks up when Rex is back. It logs
  to `~/.local/state/tr3x/sync.log`.
- `tr3x sync` follows each machine's shell subscription (`orchestration.subscribeShell`), each
  on a connection of its own that drops and resumes on its own, after the last event it had,
  from a quarter of a second up to 10 seconds apart. After each change it reconciles the Rex
  session: it creates, closes, relabels and reorders windows with one Lua script per change
  through `rex do`. Blocks are created and disowned in that same connection, so they size to
  whichever Rex app is looking at them.
- The order is T3's own sidebar logic, a port of `Sidebar.logic.ts` and `threadInbox.ts`
  in `t3client`, over every machine's threads at once. A pinned thread stays put while it
  works, as in T3.
- A window the user closes is noticed on sync's next pass (at most 5 seconds). Sync settles the
  thread with a plain `thread.settle`, never stopping a run. A thread that's running or waiting
  on you is only hidden until it changes. Every window going at once is the session closing,
  so nothing is settled then.
- Subagents get a window under their parent only while they work or need you. Once one
  finishes, its result belongs to the parent thread, which is where T3 shows it. One you're
  looking at (the session's active window) keeps its window until you leave.
- A subagent follows its parent's window: close the parent and its subagents close too. One
  that waits on an approval or an answer, or whose parent has no window because it's settled,
  snoozed or archived, gets a window of its own with the attention threads,
  `↳ title · for Parent`. A failed one doesn't: nothing waits on it.
- A subagent that only works has a steady `updatedAt` as far as a closed window goes, so a
  closed window stays closed until the subagent needs you, not until its next tool call.
- `state.json` also keeps `parents`, each shown subagent's thread to its parent's, which ⌃⌘[
  and ⌃⌘] read.
- State lives in `~/.local/state/tr3x/state.json`: the session id, the window of each thread,
  keyed `<environment>/<thread>`, and threads hidden by a closed window. tr3x never touches a
  session or window it didn't make. A state file from before machines moves to this Mac's
  environment as sync starts. Sync writes it only when something changed; a failed write is
  tried again on the next pass.

## Machines

- **One session.** Every machine's threads share **T3 Code** in T3's order, so ⌃⌘I goes to
  whatever needs you wherever it runs. Once two or more machines are paired, each thread on
  another machine carries its name after the title, `Fix the build · studio`. Rex has no badge
  for a window, so the name lives in the label. The name is T3's for the machine, or the one
  `tr3x env rename` gives it; pairing over SSH names it after the SSH host.
- **A session each.** With `"sessions": "per-machine"` in `~/.config/tr3x/settings.json`
  (`tr3x sessions per-machine`), each machine gets **T3 Code · its name**, and a New Tab there
  starts on that machine. Rex can't move a window between sessions, so switching closes
  tr3x's old sessions without settling anything and opens the tabs again in the new ones.
- **Out of reach.** Sync creates, closes or settles a machine's windows only once that
  machine's shell has caught up on its current connection. Until then its windows stay where
  they are, and after 30 seconds out of reach they read `(offline)`. A window closed while its
  machine is away isn't settled. The conversation shows `studio is offline · retrying` and
  Reply keeps what's typed. A machine that no longer accepts Rex's token says
  `Run tr3x pair studio`. This machine's own T3 is named by how it runs, from its `serverSelfUpdate`
  capability: the T3 Code app (`desktop-managed`) says to open T3 Code, a background service
  (`t3 service install`) that the service isn't answering, and anything else that T3 isn't
  running.
- **Folders.** A thread on another machine works in a folder there, so its panes start in the
  home folder. HTML snapshots read T3's attachments from disk, so they're this Mac's only.

## The panes

- A thread's window is one pane, `tr3x chat`, so a phone looking at the session gets one
  window per thread, not a window per pane. With the `reply` setting at `pane`, `tr3x chat`
  draws only the conversation and `tr3x compose` draws Reply in a pane under it, on the
  alternate screen. That pane moves the split above it (`session.resize_pane`) to fit what it
  shows, once a device has sized both panes, within half the window.
- The input sits on a fill: the screen's background a step lighter, or darker on a light
  screen. Rex doesn't answer OSC 11, so the background is that of the Rex theme the app on this
  Mac uses for the screen's scheme (`Workspace.darkThemeID`, `lightThemeID`, in the themes Rex
  ships). A theme of your own has none, and the input sits between two rules instead. The conversation prints in a scroll region (DECSTBM)
  over Reply's rows: what scrolls off its top goes into Rex's scrollback, and Reply stays at
  the bottom. Reply growing pushes the conversation up into the scrollback; shrinking under a
  conversation that filled its rows prints the thread again, so no gap is left.
- Mouse reporting would take drags from Rex's selection and the wheel from the scrollback, so
  it's on only while Reply has something to click: an approval's or a question's buttons, or
  the slash menu. The rest of the time the conversation selects and copies like any terminal
  output, as in Codex and Claude Code. A turn of the wheel turns it off for a moment and a
  half, or until the next key or request. Tab and `/model` open the model picker, and
  Backspace takes a picture off.
- The pane subscribes to the thread once. It reports
  the thread's status to Rex with OSC 7501, from the same status word T3's sidebar shows, so
  Rex's markers are T3's. `title` is the thread's title, as is the terminal's own title (OSC 2),
  which Go to Anything's Activity list names the pane by. While it waits on you, `msg` is what
  it waits on: `Run` and the command an approval gates, `Edit` and the file, or the question,
  cut to the protocol's 2048 bytes.
- A thread stopped on a usage limit (the shell's `lastErrorClass` is `usage_limit`) ends on
  `Usage limit reached · resets 3pm`, from `usageLimitResetAt`, in place of Failed. With
  `· resumes then` when `limitRecovery` has auto-resume on for that run and that reset, which
  is what T3's own Resume at reset and its Auto-resume setting do. The line under the label
  reads `tr3x · Limited · resumes 3pm` or `· resets 3pm`. Both move on with the clock:
  `tomorrow 9am` becomes `9am`, and `resumes` becomes `resuming` at the reset, until T3 sends
  the thread on.
- A failed turn's last line, `✗ Failed`, has the provider session's own error under it when T3
  has one (`providerSessions[].lastError`, for the thread's provider, as T3 picks the session):
  a sign-in or setup problem the turn itself never recorded. It's left out while a run is
  working, when the session began failing before the latest run started, once the session
  recovers, and when an error in the conversation for that run already says the same.
- Reply follows the machine's config subscription (`subscribeServerConfig`), so the usage
  beside its chip stays current: provider `usageLimits` windows appear at 20% remaining or
  less, or only the tightest when they don't all fit beside the chip whole. A used-up window says when it resets.
- A window's label starts with a mark for T3's status word: `●` Approval, `?` Input, `✕`
  Failed or Limited, `✓` Done or Woke. Working, Goal and Waiting have none, since Rex's spinner
  shows them. Labels are plain text, so the mark has no colour. Sync relabels only when the
  mark or title changes.
- Every input turns on the kitty keyboard protocol's first level (`CSI > 1 u`), and turns it off
  on every way out, a signal or a panic too. Without it Rex sends Shift+Enter as
  `CSI 27 ; 2 ; 13 ~`, which crossterm drops; with it Shift+Enter is `CSI 13 ; 2 u`.
- Reply grows one row per input line, up to 8, within half the pane.
- A run of tool calls folds past the first three into one "more" line, but an HTML embed
  always shows.
- A finished turn folds to `▸ Worked for 2m 14s` above its last reply, by the rule T3 Code's
  timeline uses: tool calls, thinking, approvals and the messages between them fold; your
  messages, the last reply, questions, plans, errors and embeds stay. A failed or running turn
  stays open. A stopped one reads `You stopped after …`. The fold happens as the turn finishes,
  by printing the thread again. Scrollback can't open one fold in place, so `/details` (or
  `tr3x details on`) prints every turn open under `▾ Worked for …`.
- A window whose panes don't match the `reply` setting gets the right ones in place on sync's
  next pass: the new panes split in beside the old ones, which close, so the window keeps its
  spot.
- Reply answers an approval with the same `respond` call T3's buttons make (`accept`,
  `acceptForSession`, `decline`), and a question's option with its value, as typing its number
  does.
- Enter to a running thread steers or queues as T3 Code's `followUpBehavior` client setting
  says (default steer); ⌘Enter does the other, as T3's composer does.
- Reply turns on kitty paste events (mode 5522). A paste then arrives as the clipboard's MIME
  types (OSC 5522) with a one-time password; Reply reads a picture when there is one, else the
  text, so the clipboard is the pasting device's, a phone's too. The input filter takes the
  replies out before crossterm. A pasted path to an image, as a drop pastes it, attaches the
  file. Each picture is made ready off the input's thread (`picture.rs`): at most 2000 pixels on
  the long edge, as T3's web composer sends, then lossless PNG or WebP while that's under 1 MB,
  else the smallest of those and JPEG (opaque pictures only). HEIC goes through `sips` first.
  Pictures go up with `assets.persistChatAttachments`
  first, and the message carries what that returns, as T3's composer does.
- On a pane that runs on another machine, Rex 1853 and later upload what's dropped or pasted
  (`com.superlogical.terminal.upload_begin`, then a `rex.data.bytes.v1` stream) and paste the
  file's shell-escaped path on the host, which attaches it as above. An older Rex sends ⌘V with a
  picture on the clipboard as the key itself (`CSI 118 ; 9 u`), and Reply says to update Rex.
- Markdown renders to ANSI with only the terminal's 16 colours, so it follows any light or dark
  theme. Code is syntax-highlighted; links are OSC 8.
- Pictures draw with kitty graphics. HTML embeds (`html_render`) are snapshotted with a headless
  Chromium when one is installed (Playwright's, or Chrome), in the screen's light or dark.
- Project icons follow T3: the icon chosen in project settings (emoji or monogram), else the
  favicon T3 serves through `assets.createUrl`, converted to PNG with `sips` and cached in
  `~/Library/Caches/tr3x/icons`, else T3's monogram and colour. Provider logos are T3's, in
  `tr3x/assets/logos`.

## Other devices

Everything runs on the Mac with T3 and Rex's server. A laptop's or phone's Rex app attached
over the tailnet only draws.

- **Size.** Rex sizes a pane to the client that owns it. **Fit to This Device** (`t3_fit`) calls
  `session.own_block` with `take` for every block in the session, as the client that ran it
  (`ctx.client_id`), so the panes take its size at once. The other clients keep the size they
  asked for and get the panes back when that client leaves.
- **Light or dark.** Each pane turns on mode 2031, and Rex reports the owning client's scheme
  (`CSI ? 997 ; 1 n` dark, `2 n` light) and reports it again when it changes. Snapshots and
  logos follow it, else the Mac's `AppleInterfaceStyle`. crossterm would take that report for
  an unfinished key and wait on it, so input passes through a pseudo-terminal that keeps the
  reports and hands every other byte on (`scheme.rs`).
- **Opening a thread.** A link is opened by the device it's clicked on, so the conversation's
  title links to `<origin>/<environment>/<thread>`, T3's web route for a thread, at the tailnet
  address T3 lists in `server.getConfig` (`directEndpoints`, HTTPS first). `TR3X_WEB_URL` sets
  another, and `none` keeps T3 Code's `t3code://threads/…` link. `t3_open` runs `open` only when
  the client that ran it connects over the local socket. `/open` in Reply opens T3 Code when no
  app is attached from elsewhere, and otherwise shows the link to click.

## Actions

Every action on a thread is one entry in `tr3x/src/actions.rs`: a palette action under
**T3 Code**, a `/` command in Reply, or both. `install` writes the palette's half into
`tr3x.lua` (`@ACTIONS@`); Reply's `/` menu reads the rest. An entry can name the capability
T3 needs for it (`threadForceStop`, `threadTitleRegeneration`): Reply leaves it out of `/` on a
machine whose T3 lacks it, and the palette's action says to update T3 there. An action that
asks first, Rename Thread… and Delete Thread…, runs in a floating layer (`prompt.rs`); `/delete`
takes two in a row.

Each sends what T3's own menu sends: `thread.metadata.update` with a title or
`regenerateTitle`, `thread.fork` from the thread's latest stable point, `thread.delete`,
`thread.stop` (else `run.interrupt` on an older T3), and `orchestration.forceStopThread`.

## Keeping up with T3

T3 ships a nightly most days. `tr3x/t3-surface.json` is what tr3x was last checked against:
T3's capability flags, the commands `orchestration.dispatchCommand` and `projects.mutate` take,
its RPC methods and its keybinding commands, which are also what its command palette runs.
`tr3x surface` reads the same lists from a T3 build's own source (`surface.rs` in t3client),
which every build carries as text: a runtime's `t3`, the desktop app's `app.asar`, or a
release tarball.

```sh
tr3x surface --check tr3x/t3-surface.json                 # the newest T3 on this Mac
tr3x surface --from ./t3 --label 0.1.6-nightly.… --check tr3x/t3-surface.json --write tr3x/t3-surface.json
```

`.github/workflows/surface.yml` does that each day with the newest `t3@nightly` release
(checked against its `SHA256SUMS`). When something changed it pushes the new surface to the
`t3-surface` branch and keeps one issue up to date: what T3 added, which is what tr3x could
offer next, what it removed, and whether tr3x's tests pass against it.
`tr3x_only_uses_what_t3_has` fails when a command, method or capability tr3x's source uses
isn't in the surface, so a nightly that drops one shows there. Merging the branch moves the
baseline.

## Updates

`update.rs`. Sync checks once a day, a minute and a half after it starts when the last check is
a day old, and writes what it found to `updates.json` in the state directory, which Reply reads
for its notice.

- **tr3x**: the latest release from GitHub's API for the repository the binary was built from
  (`TR3X_RELEASES` points it at another, `file://` too). On `auto`, the default, sync starts
  `tr3x update --auto` in a process group of its own, since installing stops sync: it downloads
  `tr3x-macos-universal.tar.gz`, checks it against the release's `.sha256`, checks the binary
  inside says that version, and runs the new binary's `install --update`. That copies it over
  `~/.local/bin/tr3x` beside-and-rename, rewrites `tr3x.lua`, stops the old sync and reloads
  Rex's config, which starts sync on the new one, without selecting the session or asking about
  machines. A tr3x that runs from anywhere else, such as a build, is never replaced.
- **Panes** note the binary they started from (device and inode). Every few seconds each one
  looks again; once the file at that path is a different one and nothing is typed, the pane
  `exec`s itself with its own arguments, in place, so Rex keeps the block and the tab. Reply's
  draft, pictures and answers hold it back until they're empty; New Thread waits while its
  folder picker is open.
- **T3**: each machine's version from its descriptor, and the newest on its track from
  `server.getNpmRelease` with `spec` `nightly`, `preview` or `latest`, from the version's
  suffix. T3's `serverSelfUpdate` capability says how it can update: `desktop-managed` (the T3
  Code app runs it), `boot-service` or `respawn` (`t3 service install`), or nothing, for a plain
  `t3 serve`, which tr3x leaves alone. `tr3x update` calls `server.updateServer` with the
  exact `targetVersion`. The desktop app answers once it has downloaded its own update, with a
  `desktopUpdateToken`; `server.commitDesktopUpdate` with that token has it quit and install, so
  the connection dropping is the success, and tr3x waits for it to come back. With threads
  running there it asks first, since a restart stops them until T3 continues them
  (`threadRestartContinuation`). A background service hands the version to its launcher.
  Both need `environment:maintain`, which Rex asks for when it pairs. A token from before then
  pairs again on its own where tr3x can (this Mac, SSH); a joined or linked machine is told
  to pair again.

## Find Thread

**Find Thread…** opens `tr3x find` in a floating layer. It reads each machine's shell, its
archived threads (`orchestration.getArchivedShellSnapshot`) and its older settled threads
(`orchestration.getOlderSettledThreads`, 50 a page, up to a thousand), and lists them as they
come. Enter unarchives (`thread.unarchive`), wakes or unsettles the thread as it needs, then
writes it to `restore` in the state directory and wakes sync with SIGUSR1, which forgets a
window closed while the thread worked. Once sync has made the tab, the finder selects it on the
device that ran it. **Reopen Last Settled** (`tr3x reopen`) does the same for the thread with
the latest `settledAt`. Nothing comes back on its own: a settled thread stays settled until one
of these brings it back.

## Provider glyphs

Rex picks a window's sidebar glyph from the name its focused pane's program was started as. So
tr3x starts the pane through a link named for the provider (`~/.local/state/tr3x/bin/claude`
→ `tr3x`), without a login shell in between. A thread that moves to another provider takes the
new glyph: the pane starts again under the new name once nothing is typed in Reply. Rex has no glyph for a project, and no way to set a custom one.

## New threads

Sync keeps a `rex do` script running with `rex.on("window_created")`. A window that is one
unlabelled shell (Rex's New Tab) gets `tr3x new` split in beside the shell, which then closes,
so the window keeps its place and focus. tr3x labels its own panes as it makes them.

`tr3x new` labels itself `t3:<env8>/<thread>`, with the first eight characters of the
machine's environment id and the id the launch will use, before it launches through
`orchestration.launchThread` on that machine's connection. Then it turns into `tr3x chat`
(exec), with Reply under the conversation. Sync adopts the window once T3 reports the thread. Tags
longer than Rex's 128-character labels are `t3#` and a hash. Windows tagged before machines,
`t3:<thread>`, are adopted as this Mac's.

With more than one machine to choose from, a machine chip comes first. It lists the paired
machines, marked `offline` when they don't answer, and the machines `tr3x pair` would find,
as **Pair…**, which pairs in a floating window. Choosing one loads its config and shell, so the
project, model, effort and branch chips are that machine's. When this Mac's T3 isn't running,
the window opens on the first paired machine that answers.

A project added from the project chip goes to T3 as `projects.mutate` with `project.create`,
the call `t3 project add` makes, titled after its folder. It's offered on this Mac's machine
only, since the folders it lists are this Mac's.

## Pairing with T3

tr3x pairs with T3 the way T3's own apps do. A one-time pairing code is traded at
`POST /oauth/token` for a token that lasts 30 days. It shows in T3 → Settings → Connections as
**Rex on** and this Mac's name. The code comes from:

- **This Mac**: T3's own CLI, `t3 auth pairing create`. The first tr3x process that needs T3
  does it, so there's nothing to set up.
- **Joining**: `curl -fsSL http://<this Mac>:3779/join | sh` on the other machine. Sync
  listens on this Mac's tailnet addresses only, and answers only machines `tailscale whois`
  says are the same person's. The script runs the same command against that machine's own T3
  and posts the code back; sync trades it with T3 at the machine's tailnet address, never one
  the request names. `TR3X_JOIN_PORT` moves the port.
- **SSH**: the same command on the other machine, over `ssh -o BatchMode=yes <host>`, as T3's
  desktop app does for its SSH machines. It finds `t3` on the PATH, in `~/.local/bin` or in
  `~/.t3/runtime/versions`. SSH never prompts; a host that wants a password says so and stops.
  A tailnet name's first host key is accepted, since Tailscale has already checked the machine.
- **A link**: every form T3 accepts. `https://host/pair#token=…`, `?token=…`, the hosted
  `?host=…&token=…`, `t3code://pair?pairingUrl=…`, or a host and a code.

`tr3x pair` finds machines from `tailscale status` and the linked environments T3 lists
(`peerLinks.list`), each checked at `:3773/.well-known/t3/environment`, and matches them to
the hosts in `~/.ssh/config` (and the files it includes) by `ssh -G`. A machine with no entry
there is tried at its tailnet name, and counts as SSH-reachable once `ssh` gets in with a key.
The rest are offered the join command. Before a code is traded, the server's descriptor has to
name the machine it came from.

Each machine's id, name, alias, addresses and how it was paired are in `envs.json`; its token,
scopes and expiry in `credentials.json` (mode 0600), both in tr3x's state directory and read
by every tr3x process. A token is only sent to a server whose descriptor names the machine it
was issued for, checked on every connection. A day from running out, a token renews itself:
it mints a pairing code at `/api/auth/pairing-token`, trades it, and the new token revokes the
old session (T3 lets a token revoke another session, not its own). That needs `access:write`,
so it works however the machine was paired. A token without it, or one that's rejected, is
replaced the way the machine was paired: again through the CLI here or over SSH, while a
joined or linked machine needs joining again. Either way it happens under `pair.lock`, so
panes that find it at once pair only once, and a failed renewal waits 10 minutes before the
next try. `tr3x env forget` revokes Rex's session through the machine's CLI where tr3x can
run it. Rex asks for `orchestration:read`, `orchestration:operate`, `access:read`,
`access:write`, `filesystem:read`, `source-control:write` and `environment:maintain`.

Panes Rex starts get the Rex server's environment, not their starter's, so sync and the panes
hand on tr3x's own settings (`TR3X_STATE_DIR`, `T3CODE_HOME`, `XDG_CONFIG_HOME`) on the
command line as `--set-env`.

## Install, in detail

`tr3x install`:

1. checks Rex and T3 answer,
2. installs the binary: a build or a download is copied to `~/.local/bin/tr3x`; one that
   already lives somewhere else, such as a package manager's, stays where it is. If that folder
   isn't on `PATH`, it says so, with a doctor command that works now (the whole path, quoted)
   and the line to add to the shell's startup file, which it never edits,
3. moves an earlier install over: removes the background agent it ran sync as
   (`com.tr3x.sync`), stops a running sync, and puts back the pane fading earlier versions
   turned off,
4. writes `~/.config/rex/tr3x.lua` (actions, keys and sync) and adds one line,
   `require("tr3x")`, to `~/.config/rex/init.lua`, keeping a copy of the old file as
   `init.lua.before-tr3x`,
5. reloads Rex's config, which starts sync,
6. waits for the first sync and switches Rex to the **T3 Code** session. If sync doesn't
   arrive in 20 seconds, it prints the end of `sync.log` and exits with an error,
7. offers each machine on the tailnet that runs T3: over SSH with one confirm, else the join
   command to run there, or a link.

`tr3x uninstall` undoes all of it: it removes the `require` line and the actions, stops sync,
closes only the sessions tr3x made, revokes Rex's session on each machine it can, and deletes
the binary, its state and its caches.

`tr3x doctor` checks Rex, the config line, that Rex loaded the config without errors, T3 on
each machine and the capabilities tr3x needs (`threadSettlement`, `threadSnooze`,
orchestration protocol 2), that sync is running and what it last logged, and the session. A
machine that needs pairing is a ✗; one that's out of reach, or a token that can't renew itself
and ends within a week, is a warning. Machines on the tailnet that run T3 and aren't paired are listed. Each ✗ comes with its fix, and
any ✗ exits non-zero. A missing headless Chromium is only a warning.

## Pane fading

Rex fades inactive panes, which dims the conversation while you type in Reply. It's one
setting for the whole app (Settings → Interface → Pane Behavior → Fade inactive panes), and
tr3x leaves it alone by default. With `"paneFade": "auto"` in `~/.config/tr3x/settings.json`,
sync turns fading off while the Rex app on this Mac shows **T3 Code** and puts your own setting
back everywhere else. A change you make in Rex's settings outside T3 Code becomes your
setting. Taking the key out puts your setting back.

Rex's server can't say which session an app shows, since every app is attached to every
session. The app's own `Workspace.lastFocusedSession` preference follows it within a second, so
sync reads that. Whether the running app applies a changed fade setting without a restart is
the open question; the app only links the in-process change notification for its preferences.

## Repo layout

| Path | |
|---|---|
| `tr3x/` | The crate: `sync.rs`, `machines.rs` (pair, envs, sessions), `chat.rs`, `compose.rs`, `ask.rs` (what a thread waits on), `actions.rs` (every action, for the palette and `/`), `slash.rs` (the `/` menu), `prompt.rs` (Rename and Delete's question), `surface.rs` (`tr3x surface`), `new.rs` (new thread), `find.rs` (Find Thread), `pick.rs` (model picker), `picker.rs` (chips, buttons and menus), `icons.rs`, `render.rs` (Markdown to ANSI), `media.rs`, `scheme.rs` (light or dark), `link.rs` (where a thread opens), `install.rs` (install, uninstall, doctor), `fade.rs`, `rex.rs`, `t3.rs`, `state.rs`, `width.rs`. |
| `t3client/` | The T3 client: pairing (`auth.rs`, `pair.rs`, `join.rs`), the machines tr3x knows and their tokens (`store.rs`), one connection per machine (`env.rs`, `hub.rs`), T3's wire protocol (`rpc.rs`, `http.rs`, `descriptor.rs`), its state and sidebar logic (`projection.rs`, `inbox.rs`), models, launch, transcripts, and what a T3 build offers (`surface.rs`). The wire client, projection, models, launch, transcript and sidebar code started from [krishhgg/t3term](https://github.com/krishhgg/t3term) at d1d33e8 (MIT). |
| `rex/tr3x.lua` | The Rex config `install` writes out: sync, actions and key bindings. |
| `install.sh` | The one-command install: checks Rex and T3, gets the binary, runs `tr3x install`. |
| `.github/workflows/` | `ci.yml` runs rustfmt, clippy and the tests. `surface.yml` checks each T3 nightly against `tr3x/t3-surface.json`. `release.yml` builds a universal macOS binary and Linux x86_64 and aarch64 binaries for a `v*` tag, checks them (`scripts/release-check`), stages them in a draft release with their SHA-256s and licenses, and publishes it once all are there. |
| `scripts/dev-restart` | A sync of your own build, with its own state, for development. |
| `docs/` | This file, [usage](usage.md), [hosting](hosting.md) and the README's pictures (`assets/`). |
| `skills/tr3x/` | The skill that teaches coding agents to install, check and drive tr3x. |

## Development

```sh
cargo test --release
cargo clippy --release --all-targets
TR3X_STATE_DIR=/tmp/tr3x-dev scripts/dev-restart   # a sync with its own state, not the installed one
```

`dev-restart` stops at a missing Rex or a failed build, before it closes or deletes anything. It names its session **t3x-dev** (`TR3X_SESSION_LABEL`) and starts its panes with
`--passive` (`TR3X_PASSIVE`), so they never mark a thread visited beside an installed sync.
Pair machines into its state with `TR3X_STATE_DIR=… tr3x pair`. `tr3x rpc --env studio
<method> '<json>'` calls one machine.

To release, push a tag such as `v0.2.0`; once every platform's archive is built and checked,
the workflow publishes the draft it staged, and `install.sh` downloads the latest release. See
[Releasing](../CONTRIBUTING.md#releasing).

## Other limits

- Rex has no per-pane setting for its pane-header buttons (split, float, zoom, close), so a
  thread's pane still shows them. The app has a global `Workspace.hiddenPaneButtons` preference,
  but `defaults write` with the button names didn't take effect in Rex 0.1.0 (1642) while it
  ran, and hiding them everywhere would also hide them in your other sessions. Fading is
  likewise only global; see [pane fading](#pane-fading).
- Other machines' pictures and HTML snapshots: attachments are read from T3's folder on this
  Mac. Over HTTP, `attachments` and `assets.createUrl` would serve them; not wired up yet.
