# How it works

T3 Code is the source of truth. t3rex never decides a thread's state; it follows T3 and draws
what T3 says into one Rex session.

## The session

- Rex runs sync. `~/.config/rex/t3rex.lua` starts `t3rex sync` as Rex loads its config, as a
  detached command that outlives the load, a config reload and a restart of Rex's server. Each
  load starts another; the sync already running holds `sync.lock`, so the new one leaves at
  once. The actions that need sync (⌃⌘I, ⌃⌘N) start it again if it isn't running. Sync never
  starts a Rex server itself: while Rex is quit it waits, and picks up when Rex is back. A lost
  connection to T3 starts over after a pause, from 2 seconds up to a minute. It logs to
  `~/.local/state/t3rex/sync.log`.
- `t3rex sync` follows T3's shell subscription (`orchestration.subscribeShell`) and reconciles
  the Rex session after each change: it creates, closes, relabels and reorders windows with one
  Lua script per change through `rex do`. Blocks are created and disowned in that same
  connection, so they size to whichever Rex app is looking at them.
- The order is T3's own sidebar logic, a port of `Sidebar.logic.ts` and `threadInbox.ts`
  in `t3client`. A pinned thread stays put while it works, as in T3.
- A window the user closes is noticed on sync's next pass (at most 5 seconds). Sync settles the
  thread with a plain `thread.settle`, never stopping a run. A thread that's running or waiting
  on you is only hidden until it changes. Every window going at once is the session closing,
  so nothing is settled then.
- Subagents get a window under their parent only while they work or need you. Once one
  finishes, its result belongs to the parent thread, which is where T3 shows it.
- State lives in `~/.local/state/t3rex/state.json`: the session id, the window of each thread,
  and threads hidden by a closed window. t3rex never touches a session or window it didn't
  make.

## The panes

- `t3rex chat` and `t3rex compose` each subscribe to the thread. The conversation pane reports
  the thread's status to Rex with OSC 7501, from the same status word T3's sidebar shows, so
  Rex's markers are T3's. `title` is the thread's title. While it waits on you, `msg` is what
  it waits on: `Run` and the command an approval gates, `Edit` and the file, or the question,
  cut to the protocol's 2048 bytes.
- Reply answers an approval with the same `respond` call T3's buttons make (`accept`,
  `acceptForSession`, `decline`), and a question's option with its value, as typing its number
  does.
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

## Provider glyphs

Rex picks a window's sidebar glyph from the name its focused pane's program was started as. So
t3rex starts both panes through a link named for the provider (`~/.local/state/t3rex/bin/claude`
→ `t3rex`), without a login shell in between. A thread that moves to another provider takes the
new glyph: the conversation starts again under the new name, and Reply does once its input is
empty. Rex has no glyph for a project, and no way to set a custom one.

## New threads

Sync keeps a `rex do` script running with `rex.on("window_created")`. A window that is one
unlabelled shell (Rex's New Tab) gets `t3rex new` split in beside the shell, which then closes,
so the window keeps its place and focus. t3rex labels its own panes as it makes them.

`t3rex new` labels itself `t3:<thread>` before it launches, with the id the launch will use,
then turns into `t3rex chat` (exec) with Reply split under it. Sync adopts the window once T3
reports the thread. Ids longer than Rex's 128-character labels are tagged `t3#` and a hash.

A project added from the project chip goes to T3 as `projects.mutate` with `project.create`,
the call `t3 project add` makes, titled after its folder.

## Pairing with T3

t3rex pairs with T3 the way T3's own apps do. A one-time pairing code, from
`t3 auth pairing create`, is traded at `POST /oauth/token` for a token that lasts 30 days. It
shows in T3 → Settings → Connections as **Rex on** and this Mac's name. The first t3rex process
that needs T3 pairs through T3's own CLI on this Mac, and every later one reads the token from
`credentials.json` in t3rex's state directory (mode 0600). A token is only sent to a server
whose `/.well-known/t3/environment` names the machine it was issued for. One that's rejected
or a day from running out is replaced the same way, under `pair.lock`, so panes that find it
at once pair only once.

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
   arrive in 20 seconds, it prints the end of `sync.log` and exits with an error.

`t3rex uninstall` undoes all of it: it removes the `require` line and the actions, stops sync,
closes only the session t3rex made, and deletes the binary, its state and its caches.

`t3rex doctor` checks Rex, the config line, that Rex loaded the config without errors, T3 and
the capabilities t3rex needs (`threadSettlement`, `threadSnooze`, orchestration protocol 2),
that sync is running and what it last logged, and the session. Each ✗ comes with its fix, and
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
| `t3rex/` | The crate: `sync.rs`, `chat.rs`, `compose.rs`, `ask.rs` (what a thread waits on), `slash.rs` (the `/` menu), `new.rs` (new thread), `pick.rs` (model picker), `picker.rs` (chips, buttons and menus), `icons.rs`, `render.rs` (Markdown to ANSI), `media.rs`, `scheme.rs` (light or dark), `link.rs` (where a thread opens), `install.rs` (install, uninstall, doctor), `fade.rs`, `rex.rs`, `t3.rs`, `state.rs`, `width.rs`. |
| `t3client/` | The T3 client: pairing (`auth.rs`, `pair.rs`), the machines t3rex knows and their tokens (`store.rs`), one connection per machine (`env.rs`, `hub.rs`), T3's wire protocol (`rpc.rs`, `http.rs`, `descriptor.rs`), its state and sidebar logic (`projection.rs`, `inbox.rs`), models, launch and transcripts. The wire client, projection, models, launch, transcript and sidebar code started from [krishhgg/t3term](https://github.com/krishhgg/t3term) at d1d33e8 (MIT). |
| `rex/t3rex.lua` | The Rex config `install` writes out: sync, actions and key bindings. |
| `install.sh` | The one-command install: checks Rex and T3, gets the binary, runs `t3rex install`. |
| `.github/workflows/` | `ci.yml` runs rustfmt, clippy and the tests. `release.yml` builds a universal binary for a `v*` tag and attaches `t3rex-macos-universal.tar.gz` and its SHA-256 to the release. |
| `scripts/dev-restart` | A sync of your own build, with its own state, for development. |
| `docs/` | This file, screenshots (`screens/`) and frames drawn from Rex's screen data (`frames/`). |

## Development

```sh
cargo test --release
cargo clippy --release --all-targets
T3REX_STATE_DIR=/tmp/t3rex-dev scripts/dev-restart   # a sync with its own state, not the installed one
```

To release, push a tag such as `v0.2.0`; `install.sh` downloads the latest release.

## Other limits

- Rex has no per-pane setting for its pane-header buttons (split, float, zoom, close), so the
  Reply pane still shows them. The app has a global `Workspace.hiddenPaneButtons` preference,
  but `defaults write` with the button names didn't take effect in Rex 0.1.0 (1642) while it
  ran, and hiding them everywhere would also hide them in your other sessions. Fading is
  likewise only global; see [pane fading](#pane-fading).
- Launching on a linked device: `orchestration.launchThread` has no environment to name, so it
  starts threads only where it's called. T3 forwards its agent tools, `t3_thread_launch` among
  them, to a linked environment named by `environmentId`, over the peer link (no attachments,
  and only with Linked environments on in Labs). That needs a session for T3's `/mcp` endpoint,
  which `t3 thread start` issues in-process and t3rex's login doesn't have, so linked
  environments stay out of the chips until t3rex pairs with each one. Cloning a project to
  another machine (`projectClone.start`) isn't wired up.
