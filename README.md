# t3rex

Rex as the interface to T3 Code. T3 keeps running the agents and owns every thread's state.
Rex shows each open thread as a window in one session, **T3 Code**, ordered the way T3's
sidebar is: what needs you first (approval, question, failed, done, woke), then the rest of
the inbox with pins first, then what's working. Settled, snoozed and archived threads aren't
listed until T3 brings them back.

![A thread waiting on an approval, at the top of the T3 Code session](docs/frames/approval-dark.png)

```
T3 Code                         ← one Rex session, made and kept by t3rex
  Fix Stripe webhook retry   •  ← Rex's own marker: blocked on an approval
  Which auth lib for mobile  •  ← blocked on a question
  Sidebar v2 polish          •  ← done, not looked at yet
  Release notes 0.1.7
  Audit prod release       •••  ← working
dusty-creek                     ← your sessions, untouched
  zsh ~/Developer/macstack
```

Each thread window is two panes:

- **Conversation** (top): the thread as plain terminal output, so Rex's scrollback, find,
  selection and links all work. Markdown renders with headings, lists, tables, syntax-highlighted
  code and OSC 8 links, using only the terminal's 16 colours so it follows any light or dark
  theme. Its header reads `T3 · project · branch`. Text reads in a centred column, as in T3
  (see [Chat width](#chat-width)).
- **Reply** (bottom): one input, with T3's composer chips under it: the provider's logo and
  model, the effort, and the access the next message goes out with. Click a chip, or press Tab,
  to change it. A pending approval or question shows here.

Each window's sidebar icon is its thread's provider: Claude, Codex, Cursor, Grok, Pi, OpenCode,
Gemini or Antigravity. Rex picks a window's glyph from the name its focused pane's program was
started as, so t3rex starts both panes through a link named for the provider
(`~/.local/state/t3rex/bin/claude` → `t3rex`), without a login shell in between. A thread
that moves to another provider takes the new glyph: the conversation starts again under the
new name, and Reply does once its input is empty. Rex has no glyph for a project, and no way to
set a custom one.

Pictures draw inline with the kitty graphics protocol: local images an agent links in a reply,
and a snapshot of each HTML embed (`html_render`) taken with a headless Chromium when one is
installed (Playwright's, or Chrome), in the Mac's light or dark appearance. Each embed has a
link that opens the thread in T3 Code, where the page is interactive.

Nothing runs an agent of its own. Every message, approval and setting goes to T3.

## Install

Needs Rex Beta and T3 Code on the same Mac. One command, no Rust needed when a release URL is
given:

```sh
T3REX_URL=https://…/t3rex-macos-arm64.tar.gz sh install.sh   # prebuilt
sh install.sh                                               # from a checkout: builds with cargo
```

`install.sh` checks Rex and T3 Code are installed, opens them if they aren't running, gets the
binary (`T3REX_URL`, optionally checked against `T3REX_SHA256`; else `cargo build`), and runs
`t3rex install`, which:

1. checks Rex and T3 answer,
2. copies the binary to `~/.local/bin/t3rex`,
3. writes `~/.config/rex/t3rex.lua` (actions and keys) and adds one line,
   `require("t3rex")`, to `~/.config/rex/init.lua`, keeping a copy of the old file as
   `init.lua.before-t3rex`,
4. sets one Rex app preference, `defaults write com.superlogical.rex
   Workspace.inactivePaneFadeMode none` (Settings → Interface → Pane Behavior → Fade inactive
   panes: Off), so the conversation keeps full contrast while you type in Reply. The old value
   is kept in `~/.local/state/t3rex/rex-defaults.json` and put back by `uninstall`,
5. loads a launchd agent, `com.t3rex.sync`, that keeps `t3rex sync` running across logins,
6. waits for the first sync and switches Rex to the **T3 Code** session.

`scripts/release` builds `dist/t3rex-macos-<arch>.tar.gz` and its SHA-256 for `T3REX_URL`.
`t3rex doctor` checks each part. `t3rex uninstall` undoes all of it: it stops the agent,
closes only the session t3rex made, removes the `require` line and the actions, restores the
Rex preference, and deletes the binary and its state.

## First run

![The new-thread window a session with no threads opens on](docs/frames/onboarding-welcome-light.png)

Rex opens on **T3 Code** with a window per open thread. With no open threads, the session
holds one **New Thread** window instead, with the four keys that matter under it. New Tab
(⌘T or +) in the session always opens another.

## Using it

| | |
|---|---|
| New thread | ⌘T or + in the T3 Code session, or ⌃⌘N anywhere. Type the first message; the chips under it pick the project (with T3's icons), model, effort, access, Local or New worktree and its base branch, and the device. Tab moves to the chips, Enter or a click opens one, typing filters. Enter sends. The window becomes the thread's. |
| Reply | Type in the Reply pane, Enter to send. A message to a running thread queues; `/steer …` sends it into the running turn instead. Alt+Enter or Ctrl+J for a new line. Typing in the conversation pane moves you to Reply. Sending brings the conversation back to the bottom. A send that fails keeps your text. |
| Model | Click a chip under Reply, press Tab there, type `/model` alone, or ⌃⌘M: a picker opens over the window with every enabled provider's models under its logo, then the model's efforts; ← → for effort and access. A choice applies to the next message and the chips follow. `/model haiku`, `/effort high`, `/mode ask` still work. |
| Approve | With an approval pending: `a` then Enter accepts, `s` then Enter accepts for the session, `d` then Enter declines. Anything else you type is a reply. |
| Answer | A pending question takes the input as its answer. A number picks that option. |
| Commands | `/steer …`, `/settle`, `/snooze 2h`, `/snooze tomorrow`, `/wake`, `/pin`, `/unpin`, `/archive`, `/stop`, `/model`, `/effort`, `/mode`, `/open`, `/width` |
| Keys | ⌃⌘N new thread · ⌃⌘M model · ⌃⌘I go to the thread that needs you · ⌃⌘S settle · ⌃⌘Z snooze 1h · ⌃⌘P pin · ⌃⌘O open in T3 Code |
| Palette | Every action is in Rex's command palette under **T3 Code**. |

### Chat width

The conversation, Reply and the new-thread composer share one centred column, like T3's chat
lane. It follows T3 Code's **Chat width** setting (`chatWidth` in
`~/.t3/userdata/client-settings.json`), so it works as installed:

| T3 setting | T3 lane | t3rex column |
|---|---|---|
| Comfortable (default) | 46rem | 96 columns |
| Wide | 72rem | 150 columns |
| Full | the window | the pane, 2 columns clear each side |

46rem of T3's 15–16px sans holds about 92–100 characters a line; 96 sits in that range, and
wide keeps T3's 72/46 ratio. A pane narrower than the column uses its full width.

To differ from T3, `/width wide` in Reply, or `t3rex width comfortable|wide|full|N`, writes
`chatWidth` to `~/.config/t3rex/settings.json` (a number is a column count). `/width t3` goes
back to following T3. Open panes pick a change up within a couple of seconds.

| Full (before) | Comfortable |
|---|---|
| ![](docs/frames/width-full-dark.png) | ![](docs/frames/width-comfortable-dark.png) |

Subagents show under the thread they work for, as `↳ title`: a task delegated through T3 until
it settles, and any subagent while it works or needs you. A provider's own subagent at rest is
history, which T3 shows inside the parent thread.

Settling, snoozing or archiving a thread closes its window. T3 brings it back when the thread
needs you again, and so does t3rex. Closing a window yourself hides that thread until it
changes. Looking at a finished thread clears its Done marker in T3, as opening it in the app
does.

## How it works

- `t3rex sync` follows T3's shell subscription (`orchestration.subscribeShell`) and reconciles
  the Rex session after each change: it creates, closes, relabels and reorders windows with one
  Lua script per change through `rex do`. Blocks are created and disowned in that same
  connection, so they size to whichever Rex app is looking at them.
- The order is T3's own sidebar logic, from t3term's port of `Sidebar.logic.ts` and
  `threadInbox.ts`. A pinned thread stays put while it works, as in T3.
- `t3rex chat` and `t3rex compose` each subscribe to the thread. The conversation pane reports
  the thread's status to Rex with OSC 7501, from the same status word T3's sidebar shows, so
  Rex's markers are T3's.
- New Tab in the session: sync keeps a `rex do` script running with `rex.on("window_created")`.
  A window that is one unlabelled shell gets `t3rex new` split in beside the shell, which then
  closes, so the window keeps its place and focus. t3rex labels its own panes as it makes them.
  `t3rex new` labels itself `t3:<thread>` before it launches, with the id the launch will use,
  then turns into `t3rex chat` (exec) with Reply split under it; sync adopts the window once T3
  reports the thread. Ids longer than Rex's 128-character labels are tagged `t3#` and a hash.
- Project icons follow T3: the icon chosen in project settings (emoji or monogram), else the
  favicon T3 finds and serves through `assets.createUrl`, converted to PNG with `sips` and
  cached in `~/Library/Caches/t3rex/icons`, else T3's monogram and colour. Provider logos are
  T3's, in `t3rex/assets/logos`. Pictures draw with kitty graphics.
- State lives in `~/.local/state/t3rex/state.json`: the session id and the window of each
  thread. t3rex never touches a session or window it didn't make.
- Each process logs in to T3 with a temporary session and revokes it on exit, so the Keychain
  never prompts. Set `T3REX_SAVED_LOGIN=1` to use t3term's saved login instead.

## Layout of this repo

| Path | |
|---|---|
| `t3rex/` | The t3rex crate: `sync.rs`, `chat.rs`, `compose.rs`, `new.rs` (new thread), `pick.rs` (model picker), `picker.rs` (chips and menus), `icons.rs`, `render.rs` (Markdown to ANSI), `install.rs`, `rex.rs`, `t3.rs`, `state.rs`. |
| `t3term/` | [krishhgg/t3term](https://github.com/krishhgg/t3term) at d1d33e8 (MIT), vendored. t3rex uses its T3 client, auth and sidebar logic. The only changes are a small public API at the end of `src/tui/sidebar.rs`, re-exported from `src/tui/mod.rs`, and an `access:read` scope in `src/auth.rs`. |
| `rex/t3rex.lua` | The Rex actions and key bindings `install` writes out. |
| `scripts/` | `shot` screenshots the Rex window. `frame.py` and `render-frame` draw a window from Rex's own screen data when the Mac is locked (Rex draws nothing then). `morning-shots` waits for an unlock and takes the real screenshots. `dev-restart` and `size-panes` are for development. |
| `docs/frames/` | Windows drawn from Rex's screen data overnight: an approval in dark and light, a question before and after answering, two real threads, and an HTML embed snapshot. `docs/screens/` gets real screenshots once the Mac is unlocked. |

## Known limits

- Rex has no per-pane setting for its pane-header buttons (split, float, zoom, close), so the
  Reply pane still shows them. The app has a global `Workspace.hiddenPaneButtons` preference,
  but `defaults write` with the button names (`splitRight splitDown float zoom close`, as an
  array or a JSON string) didn't take effect in Rex 0.1.0 (1642) while it ran, and hiding them
  everywhere would also hide them in your other sessions. Fading is likewise only global.
- Launching on a linked device: T3's linked environments (`peerLinks.list`) show in the device
  chip, but a client can only launch on a server it is paired with. Picking one opens T3 Code
  with the message on the clipboard. Cloning a project to another machine
  (`projectClone.start`) isn't wired up.
- The conversation pane reprints the whole thread when its width changes, so Rex's scrollback
  position resets on resize.
- Pictures need a terminal with kitty graphics (Rex has it). HTML snapshots need a headless
  Chromium; without one an embed is a titled link.
- Built against T3 `0.1.6-nightly.20261030` on orchestration protocol 2, and Rex Beta 0.1.0
  (1642), API revision 7. Both are moving targets.
