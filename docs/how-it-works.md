# How it works

T3 Code is the source of truth. t3rex never decides a thread's state; it follows T3, on every
machine it's paired with, and draws what T3 says into one Rex session.

## The session

- Rex runs sync. `~/.config/rex/t3rex.lua` starts `t3rex sync` as Rex loads its config, as a
  detached command that outlives the load, a config reload and a restart of Rex's server. Each
  load starts another; the sync already running holds `sync.lock`, so the new one leaves at
  once. The actions that need sync (⌃⌘I, ⌃⌘N) start it again if it isn't running. Sync never
  starts a Rex server itself: while Rex is quit it waits, and picks up when Rex is back. It logs
  to `~/.local/state/t3rex/sync.log`.
- `t3rex sync` follows each machine's shell subscription (`orchestration.subscribeShell`), each
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
  finishes, its result belongs to the parent thread, which is where T3 shows it.
- State lives in `~/.local/state/t3rex/state.json`: the session id, the window of each thread,
  keyed `<environment>/<thread>`, and threads hidden by a closed window. t3rex never touches a
  session or window it didn't make. A state file from before machines moves to this Mac's
  environment as sync starts. Sync writes it only when something changed; a failed write is
  tried again on the next pass.

## Machines

- **One session.** Every machine's threads share **T3 Code** in T3's order, so ⌃⌘I goes to
  whatever needs you wherever it runs. Once two or more machines are paired, each thread on
  another machine carries its name after the title, `Fix the build · studio`. Rex has no badge
  for a window, so the name lives in the label. The name is T3's for the machine, or the one
  `t3rex env rename` gives it; pairing over SSH names it after the SSH host.
- **A session each.** With `"sessions": "per-machine"` in `~/.config/t3rex/settings.json`
  (`t3rex sessions per-machine`), each machine gets **T3 Code · its name**, and a New Tab there
  starts on that machine. Rex can't move a window between sessions, so switching closes
  t3rex's old sessions without settling anything and opens the tabs again in the new ones.
- **Out of reach.** Sync creates, closes or settles a machine's windows only once that
  machine's shell has caught up on its current connection. Until then its windows stay where
  they are, and after 30 seconds out of reach they read `(offline)`. A window closed while its
  machine is away isn't settled. The conversation shows `studio is offline · retrying` and
  Reply keeps what's typed. A machine that no longer accepts Rex's token says
  `Run t3rex pair studio`.
- **Folders.** A thread on another machine works in a folder there, so its panes start in the
  home folder. HTML snapshots read T3's attachments from disk, so they're this Mac's only.

## The panes

- A thread's window is one pane, `t3rex chat`, so a phone looking at the session gets one
  window per thread, not a window per pane. With the `reply` setting at `pane`, `t3rex chat`
  draws only the conversation and `t3rex compose` draws Reply in a pane under it, on the
  alternate screen. That pane moves the split above it (`session.resize_pane`) to fit what it
  shows, once a device has sized both panes, within half the window.
- The input sits on a fill: the screen's background a step lighter, or darker on a light
  screen. Rex doesn't answer OSC 11, so the background is that of the Rex theme the app on this
  Mac uses for the screen's scheme (`Workspace.darkThemeID`, `lightThemeID`, in the themes Rex
  ships). A theme of your own has none, and the input sits between two rules instead. The conversation prints in a scroll region (DECSTBM)
  over Reply's rows: what scrolls off its top goes into Rex's scrollback, and Reply stays at
  the bottom. Reply growing pushes the conversation up into the scrollback; shrinking under a
  conversation that filled its rows prints the thread again, so no gap is left.
- Mouse reporting is on, so the model chip, an approval's or a question's buttons, and the
  slash menu take clicks. It takes the wheel from the scrollback, so a turn of the wheel turns
  it off, and it comes back on a moment and a half after, at the next key, or at the next
  request. With it on, Shift-drag selects text. Tab and `/model` open the model picker too,
  and Backspace takes a picture off.
- The pane subscribes to the thread once. It reports
  the thread's status to Rex with OSC 7501, from the same status word T3's sidebar shows, so
  Rex's markers are T3's. `title` is the thread's title. While it waits on you, `msg` is what
  it waits on: `Run` and the command an approval gates, `Edit` and the file, or the question,
  cut to the protocol's 2048 bytes.
- A thread stopped on a usage limit (the shell's `lastErrorClass` is `usage_limit`) ends on
  `Usage limit reached · resets 3pm`, from `usageLimitResetAt`, in place of Failed. With
  `· resumes then` when `limitRecovery` has auto-resume on for that run and that reset, which
  is what T3's own Resume at reset and its Auto-resume setting do. The line under the label
  reads `t3rex · Limited · resumes 3pm` or `· resets 3pm`. Both move on with the clock:
  `tomorrow 9am` becomes `9am`, and `resumes` becomes `resuming` at the reset, until T3 sends
  the thread on.
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
- Markdown renders to ANSI with only the terminal's 16 colours, so it follows any light or dark
  theme. Code is syntax-highlighted; links are OSC 8.
- Pictures draw with kitty graphics. HTML embeds (`html_render`) are snapshotted with a headless
  Chromium when one is installed (Playwright's, or Chrome), in the screen's light or dark.
- Project icons follow T3: the icon chosen in project settings (emoji or monogram), else the
  favicon T3 serves through `assets.createUrl`, converted to PNG with `sips` and cached in
  `~/Library/Caches/t3rex/icons`, else T3's monogram and colour. Provider logos are T3's, in
  `t3rex/assets/logos`.

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
  address T3 lists in `server.getConfig` (`directEndpoints`, HTTPS first). `T3REX_WEB_URL` sets
  another, and `none` keeps T3 Code's `t3code://threads/…` link. `t3_open` runs `open` only when
  the client that ran it connects over the local socket. `/open` in Reply opens T3 Code when no
  app is attached from elsewhere, and otherwise shows the link to click.

## Actions

Every action on a thread is one entry in `t3rex/src/actions.rs`: a palette action under
**T3 Code**, a `/` command in Reply, or both. `install` writes the palette's half into
`t3rex.lua` (`@ACTIONS@`); Reply's `/` menu reads the rest. An entry can name the capability
T3 needs for it (`threadForceStop`, `threadTitleRegeneration`): Reply leaves it out of `/` on a
machine whose T3 lacks it, and the palette's action says to update T3 there. An action that
asks first, Rename Thread… and Delete Thread…, runs in a floating layer (`prompt.rs`); `/delete`
takes two in a row.

Each sends what T3's own menu sends: `thread.metadata.update` with a title or
`regenerateTitle`, `thread.fork` from the thread's latest stable point, `thread.delete`,
`thread.stop` (else `run.interrupt` on an older T3), and `orchestration.forceStopThread`.

## Keeping up with T3

T3 ships a nightly most days. `t3rex/t3-surface.json` is what t3rex was last checked against:
T3's capability flags, the commands `orchestration.dispatchCommand` and `projects.mutate` take,
its RPC methods and its keybinding commands, which are also what its command palette runs.
`t3rex surface` reads the same lists from a T3 build's own source (`surface.rs` in t3client),
which every build carries as text: a runtime's `t3`, the desktop app's `app.asar`, or a
release tarball.

```sh
t3rex surface --check t3rex/t3-surface.json                 # the newest T3 on this Mac
t3rex surface --from ./t3 --label 0.1.6-nightly.… --check t3rex/t3-surface.json --write t3rex/t3-surface.json
```

`.github/workflows/surface.yml` does that each day with the newest `t3@nightly` release
(checked against its `SHA256SUMS`). When something changed it pushes the new surface to the
`t3-surface` branch and keeps one issue up to date: what T3 added, which is what t3rex could
offer next, what it removed, and whether t3rex's tests pass against it.
`t3rex_only_uses_what_t3_has` fails when a command, method or capability t3rex's source uses
isn't in the surface, so a nightly that drops one shows there. Merging the branch moves the
baseline.

## Find Thread

**Find Thread…** opens `t3rex find` in a floating layer. It reads each machine's shell, its
archived threads (`orchestration.getArchivedShellSnapshot`) and its older settled threads
(`orchestration.getOlderSettledThreads`, 50 a page, up to a thousand), and lists them as they
come. Enter unarchives (`thread.unarchive`), wakes or unsettles the thread as it needs, then
writes it to `restore` in the state directory and wakes sync with SIGUSR1, which forgets a
window closed while the thread worked. Once sync has made the tab, the finder selects it on the
device that ran it. **Reopen Last Settled** (`t3rex reopen`) does the same for the thread with
the latest `settledAt`. Nothing comes back on its own: a settled thread stays settled until one
of these brings it back.

## Provider glyphs

Rex picks a window's sidebar glyph from the name its focused pane's program was started as. So
t3rex starts the pane through a link named for the provider (`~/.local/state/t3rex/bin/claude`
→ `t3rex`), without a login shell in between. A thread that moves to another provider takes the
new glyph: the pane starts again under the new name once nothing is typed in Reply. Rex has no glyph for a project, and no way to set a custom one.

## New threads

Sync keeps a `rex do` script running with `rex.on("window_created")`. A window that is one
unlabelled shell (Rex's New Tab) gets `t3rex new` split in beside the shell, which then closes,
so the window keeps its place and focus. t3rex labels its own panes as it makes them.

`t3rex new` labels itself `t3:<env8>/<thread>`, with the first eight characters of the
machine's environment id and the id the launch will use, before it launches through
`orchestration.launchThread` on that machine's connection. Then it turns into `t3rex chat`
(exec), with Reply under the conversation. Sync adopts the window once T3 reports the thread. Tags
longer than Rex's 128-character labels are `t3#` and a hash. Windows tagged before machines,
`t3:<thread>`, are adopted as this Mac's.

With more than one machine to choose from, a machine chip comes first. It lists the paired
machines, marked `offline` when they don't answer, and the machines `t3rex pair` would find,
as **Pair…**, which pairs in a floating window. Choosing one loads its config and shell, so the
project, model, effort and branch chips are that machine's. When this Mac's T3 isn't running,
the window opens on the first paired machine that answers.

A project added from the project chip goes to T3 as `projects.mutate` with `project.create`,
the call `t3 project add` makes, titled after its folder. It's offered on this Mac's machine
only, since the folders it lists are this Mac's.

## Pairing with T3

t3rex pairs with T3 the way T3's own apps do. A one-time pairing code is traded at
`POST /oauth/token` for a token that lasts 30 days. It shows in T3 → Settings → Connections as
**Rex on** and this Mac's name. The code comes from:

- **This Mac**: T3's own CLI, `t3 auth pairing create`. The first t3rex process that needs T3
  does it, so there's nothing to set up.
- **Joining**: `curl -fsSL http://<this Mac>:3779/join | sh` on the other machine. Sync
  listens on this Mac's tailnet addresses only, and answers only machines `tailscale whois`
  says are the same person's. The script runs the same command against that machine's own T3
  and posts the code back; sync trades it with T3 at the machine's tailnet address, never one
  the request names. `T3REX_JOIN_PORT` moves the port.
- **SSH**: the same command on the other machine, over `ssh -o BatchMode=yes <host>`, as T3's
  desktop app does for its SSH machines. It finds `t3` on the PATH, in `~/.local/bin` or in
  `~/.t3/runtime/versions`. SSH never prompts; a host that wants a password says so and stops.
  A tailnet name's first host key is accepted, since Tailscale has already checked the machine.
- **A link**: every form T3 accepts. `https://host/pair#token=…`, `?token=…`, the hosted
  `?host=…&token=…`, `t3code://pair?pairingUrl=…`, or a host and a code.

`t3rex pair` finds machines from `tailscale status` and the linked environments T3 lists
(`peerLinks.list`), each checked at `:3773/.well-known/t3/environment`, and matches them to
the hosts in `~/.ssh/config` (and the files it includes) by `ssh -G`. A machine with no entry
there is tried at its tailnet name, and counts as SSH-reachable once `ssh` gets in with a key.
The rest are offered the join command. Before a code is traded, the server's descriptor has to
name the machine it came from.

Each machine's id, name, alias, addresses and how it was paired are in `envs.json`; its token,
scopes and expiry in `credentials.json` (mode 0600), both in t3rex's state directory and read
by every t3rex process. A token is only sent to a server whose descriptor names the machine it
was issued for, checked on every connection. A day from running out, a token renews itself:
it mints a pairing code at `/api/auth/pairing-token`, trades it, and the new token revokes the
old session (T3 lets a token revoke another session, not its own). That needs `access:write`,
so it works however the machine was paired. A token without it, or one that's rejected, is
replaced the way the machine was paired: again through the CLI here or over SSH, while a
joined or linked machine needs joining again. Either way it happens under `pair.lock`, so
panes that find it at once pair only once, and a failed renewal waits 10 minutes before the
next try. `t3rex env forget` revokes Rex's session through the machine's CLI where t3rex can
run it. Rex asks for `orchestration:read`, `orchestration:operate`, `access:read`,
`access:write`, `filesystem:read` and `source-control:write`.

Panes Rex starts get the Rex server's environment, not their starter's, so sync and the panes
hand on t3rex's own settings (`T3REX_STATE_DIR`, `T3CODE_HOME`, `XDG_CONFIG_HOME`) on the
command line as `--set-env`.

## Install, in detail

`t3rex install`:

1. checks Rex and T3 answer,
2. installs the binary: a build or a download is copied to `~/.local/bin/t3rex`; one that
   already lives somewhere else, such as a package manager's, stays where it is,
3. moves an earlier install over: removes the background agent it ran sync as
   (`com.t3rex.sync`), stops a running sync, and puts back the pane fading earlier versions
   turned off,
4. writes `~/.config/rex/t3rex.lua` (actions, keys and sync) and adds one line,
   `require("t3rex")`, to `~/.config/rex/init.lua`, keeping a copy of the old file as
   `init.lua.before-t3rex`,
5. reloads Rex's config, which starts sync,
6. waits for the first sync and switches Rex to the **T3 Code** session. If sync doesn't
   arrive in 20 seconds, it prints the end of `sync.log` and exits with an error,
7. offers each machine on the tailnet that runs T3: over SSH with one confirm, else the join
   command to run there, or a link.

`t3rex uninstall` undoes all of it: it removes the `require` line and the actions, stops sync,
closes only the sessions t3rex made, revokes Rex's session on each machine it can, and deletes
the binary, its state and its caches.

`t3rex doctor` checks Rex, the config line, that Rex loaded the config without errors, T3 on
each machine and the capabilities t3rex needs (`threadSettlement`, `threadSnooze`,
orchestration protocol 2), that sync is running and what it last logged, and the session. A
machine that needs pairing is a ✗; one that's out of reach, or a token that can't renew itself
and ends within a week, is a warning. Machines on the tailnet that run T3 and aren't paired are listed. Each ✗ comes with its fix, and
any ✗ exits non-zero. A missing headless Chromium is only a warning.

## Pane fading

Rex fades inactive panes, which dims the conversation while you type in Reply. It's one
setting for the whole app (Settings → Interface → Pane Behavior → Fade inactive panes), and
t3rex leaves it alone by default. With `"paneFade": "auto"` in `~/.config/t3rex/settings.json`,
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
| `t3rex/` | The crate: `sync.rs`, `machines.rs` (pair, envs, sessions), `chat.rs`, `compose.rs`, `ask.rs` (what a thread waits on), `actions.rs` (every action, for the palette and `/`), `slash.rs` (the `/` menu), `prompt.rs` (Rename and Delete's question), `surface.rs` (`t3rex surface`), `new.rs` (new thread), `find.rs` (Find Thread), `pick.rs` (model picker), `picker.rs` (chips, buttons and menus), `icons.rs`, `render.rs` (Markdown to ANSI), `media.rs`, `scheme.rs` (light or dark), `link.rs` (where a thread opens), `install.rs` (install, uninstall, doctor), `fade.rs`, `rex.rs`, `t3.rs`, `state.rs`, `width.rs`. |
| `t3client/` | The T3 client: pairing (`auth.rs`, `pair.rs`, `join.rs`), the machines t3rex knows and their tokens (`store.rs`), one connection per machine (`env.rs`, `hub.rs`), T3's wire protocol (`rpc.rs`, `http.rs`, `descriptor.rs`), its state and sidebar logic (`projection.rs`, `inbox.rs`), models, launch, transcripts, and what a T3 build offers (`surface.rs`). The wire client, projection, models, launch, transcript and sidebar code started from [krishhgg/t3term](https://github.com/krishhgg/t3term) at d1d33e8 (MIT). |
| `rex/t3rex.lua` | The Rex config `install` writes out: sync, actions and key bindings. |
| `install.sh` | The one-command install: checks Rex and T3, gets the binary, runs `t3rex install`. |
| `.github/workflows/` | `ci.yml` runs rustfmt, clippy and the tests. `surface.yml` checks each T3 nightly against `t3rex/t3-surface.json`. `release.yml` builds a universal binary for a `v*` tag and attaches `t3rex-macos-universal.tar.gz` and its SHA-256 to the release. |
| `scripts/dev-restart` | A sync of your own build, with its own state, for development. |
| `docs/` | This file, [usage](usage.md), the README's pictures (`assets/`) and frames drawn from Rex's screen data (`frames/`). |
| `skills/t3rex/` | The skill that teaches coding agents to install, check and drive t3rex. |

## Development

```sh
cargo test --release
cargo clippy --release --all-targets
T3REX_STATE_DIR=/tmp/t3rex-dev scripts/dev-restart   # a sync with its own state, not the installed one
```

`dev-restart` stops at a failed build, before it closes or deletes anything. It names its session **t3x-dev** (`T3REX_SESSION_LABEL`) and starts its panes with
`--passive` (`T3REX_PASSIVE`), so they never mark a thread visited beside an installed sync.
Pair machines into its state with `T3REX_STATE_DIR=… t3rex pair`. `t3rex rpc --env studio
<method> '<json>'` calls one machine.

To release, push a tag such as `v0.2.0`; `install.sh` downloads the latest release.

## Other limits

- Rex has no per-pane setting for its pane-header buttons (split, float, zoom, close), so a
  thread's pane still shows them. The app has a global `Workspace.hiddenPaneButtons` preference,
  but `defaults write` with the button names didn't take effect in Rex 0.1.0 (1642) while it
  ran, and hiding them everywhere would also hide them in your other sessions. Fading is
  likewise only global; see [pane fading](#pane-fading).
- Other machines' pictures and HTML snapshots: attachments are read from T3's folder on this
  Mac. Over HTTP, `attachments` and `assets.createUrl` would serve them; not wired up yet.
