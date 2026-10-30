# t3rex

Rex as the interface to T3 Code. T3 keeps running the agents and owns every thread's state.
Rex shows each open thread as a window in one session, **T3 Code**, ordered the way T3's
sidebar is: what needs you first, then the rest, then what's working.

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
  theme. Its header reads `T3 · project · branch`.
- **Reply** (bottom): one input. Its header names the model, effort and access the next message
  goes out with. A pending approval or question shows here.

Nothing runs an agent of its own. Every message, approval and setting goes to T3.

## Install

Needs Rex Beta and T3 Code running on the same Mac, and a Rust toolchain to build.

```sh
cargo build --release -p t3rex
./target/release/t3rex install
```

`install`:

1. checks Rex and T3 answer,
2. copies the binary to `~/.local/bin/t3rex`,
3. writes `~/.config/rex/t3rex.lua` (actions and keys) and adds one line,
   `require("t3rex")`, to `~/.config/rex/init.lua`, keeping a copy of the old file as
   `init.lua.before-t3rex`,
4. loads a launchd agent, `com.t3rex.sync`, that keeps `t3rex sync` running across logins,
5. waits for the first sync and switches Rex to the **T3 Code** session.

`t3rex doctor` checks each part. `t3rex uninstall` undoes all of it: it stops the agent,
closes only the session t3rex made, removes the `require` line and the actions, and deletes
the binary and its state.

## Using it

| | |
|---|---|
| Reply | Type in the Reply pane, Enter to send. A message to a running thread queues. Alt+Enter or Ctrl+J for a new line. Typing in the conversation pane moves you to Reply. |
| Approve | With a pending approval and an empty input: `a` accept, `s` accept for the session, `d` decline. |
| Answer | A pending question takes the input as its answer. A number picks that option. |
| Commands | `/settle`, `/snooze 2h`, `/snooze tomorrow`, `/wake`, `/pin`, `/unpin`, `/archive`, `/stop`, `/model haiku`, `/effort high`, `/mode ask|edits|full`, `/open` |
| Keys | ⌃⌘I go to the thread that needs you · ⌃⌘S settle · ⌃⌘Z snooze 1h · ⌃⌘P pin · ⌃⌘O open in T3 Code |
| Palette | Every action is in Rex's command palette under **T3 Code**. |

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
- State lives in `~/.local/state/t3rex/state.json`: the session id and the window of each
  thread. t3rex never touches a session or window it didn't make.
- Each process logs in to T3 with a temporary session and revokes it on exit, so the Keychain
  never prompts. Set `T3REX_SAVED_LOGIN=1` to use t3term's saved login instead.

## Layout of this repo

| Path | |
|---|---|
| `t3rex/` | The t3rex crate: `sync.rs`, `chat.rs`, `compose.rs`, `render.rs` (Markdown to ANSI), `install.rs`, `rex.rs`, `t3.rs`, `state.rs`. |
| `t3term/` | [krishhgg/t3term](https://github.com/krishhgg/t3term) at d1d33e8 (MIT), vendored. t3rex uses its T3 client, auth and sidebar logic. The only change is a small public API at the end of `src/tui/sidebar.rs`, re-exported from `src/tui/mod.rs`. |
| `rex/t3rex.lua` | The Rex actions and key bindings `install` writes out. |
| `scripts/` | `shot` screenshots the Rex window. `frame.py` and `render-frame` draw a window from Rex's own screen data when the Mac is locked. `dev-restart` and `size-panes` are for development. |
