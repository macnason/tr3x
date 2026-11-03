---
name: tr3x
description: Install, check, fix and drive tr3x, which shows T3 Code threads as tabs in the Rex terminal, on a Mac or Linux. Use when the user mentions tr3x, or the T3 Code session in Rex; asks to set it up, why a thread's tab is missing or wrong, to pair another machine, to change chat width, Reply placement or sessions; or wants a T3 thread settled, snoozed, pinned, renamed, forked, stopped, opened in T3, or its pull request shown, from a terminal.
---

# tr3x

tr3x keeps one Rex session, **T3 Code**, in step with T3 Code: a tab per open thread, in T3's
sidebar order, for every machine Rex is paired with. T3 runs the agents and holds every
thread's state. tr3x only follows it and sends T3 what the user does, so a fix is almost never
"edit tr3x's state"; it's "make T3 or Rex right, and let sync catch up".

## Start with doctor

```sh
tr3x doctor
```

It checks Rex, the Rex config line, T3 on each machine and the capabilities tr3x needs, that
sync is running and what it last logged, and the session. Every ✗ prints its fix, and any ✗
exits non-zero. Run it before guessing, and again after a fix to confirm it.

No `tr3x` on the PATH means it isn't installed, or `~/.local/bin` isn't on the PATH. Install
with the one command below. It needs a Mac (Rex Beta) or Linux (Rex), and T3 Code running with
a project. Never use sudo; nothing tr3x does needs it.

```sh
curl -fsSL https://raw.githubusercontent.com/macnason/tr3x/master/install.sh | sh
```

`install.sh` asks about other machines on the terminal. Without one, as in an agent's shell, it
lists them with the command for each instead, and `tr3x pair` does that part later.

## Commands

| Command | What it does |
|---|---|
| `tr3x doctor` | Checks everything; prints a fix for each ✗. |
| `tr3x envs` | Machines Rex follows, and the ones on the tailnet it could pair. |
| `tr3x pair` | Finds machines on the tailnet that run T3 and pairs them. |
| `tr3x pair <ssh-host>` | Pairs one machine over SSH. Needs `ssh <host>` to work without a password. |
| `tr3x pair '<link>'` | Pairs with a link from T3 → Settings → Connections on that machine. |
| `tr3x env rename <machine> <name>` | What Rex calls a machine. An empty name goes back to T3's. |
| `tr3x env forget <machine>` | Unpairs a machine. |
| `tr3x width [comfortable\|wide\|full\|N\|t3]` | How wide conversations read. `t3` follows T3's Chat width. |
| `tr3x details [on\|off]` | Show each finished turn's work, or fold it to `Worked for …` (default). Alone, switches it. |
| `tr3x reply [inline\|pane]` | Reply at the bottom of the conversation, or in a pane of its own. |
| `tr3x sessions [merged\|per-machine]` | One T3 Code session, or one per machine. |
| `tr3x update` | Lists updates for tr3x and each machine's T3, and installs them on Enter. `--check` only lists. |
| `tr3x updates [auto\|notify\|off]` | Whether tr3x installs its own updates, asks first, or never checks. |
| `tr3x uninstall` | Stops sync, removes the config, closes tr3x's session. |

With no argument, `width`, `reply`, `sessions` and `updates` print the current value.

Thread actions take `--thread <env/thread>` or a thread id on this Mac. Inside a tr3x pane,
`REX_BLOCK` already names the thread, so `--thread` can be left out:

`settle`, `unsettle`, `snooze [30m|2h|1d]`, `wake`, `pin [--off]`, `archive`,
`rename [title] [--regenerate]`, `fork [title]`, `stop [--force]`, `open`, `pr [--create]`,
`delete [--yes]`.

## Files

| Path | |
|---|---|
| `~/.local/bin/tr3x` | The binary. |
| `~/.config/rex/tr3x.lua` | The Rex config `install` writes: starts sync, adds actions and keys. |
| `~/.config/tr3x/settings.json` | Width, Reply, sessions, pane fading. The commands above edit it. |
| `~/.local/state/tr3x/sync.log` | What sync did last. Read it when a tab is missing or stale. |
| `~/.local/state/tr3x/state.json` | The session and a window per thread. Sync owns it. |
| `~/.local/state/tr3x/updates.json` | What the last update check found. |
| `~/.local/state/tr3x/envs.json`, `credentials.json` | Paired machines and their tokens (mode 0600). |

## Rules

- **Don't edit `state.json` or `credentials.json`, and don't kill `tr3x sync`.** Sync rewrites
  state on every change, and a token edited by hand stops working. To restart sync, run
  `rex config reload`: the config starts it. ⌃⌘I and ⌃⌘N also start it if it's down. Outside
  a Rex pane `rex` isn't on the PATH on a Mac; it's
  `/Applications/Rex Beta.app/Contents/Helpers/Rex Server.rex/Contents/MacOS/rex`. On Linux it's
  `rex`.
- **Closing a tab settles its thread.** Don't close tr3x's windows with `rex` to tidy up.
  Use `tr3x snooze` to put one away, or leave it.
- **`delete` removes the thread in T3 too, and can't be undone.** Ask the user first, then pass
  `--yes`. `archive` or `settle` is almost always what they want.
- **Esc in an empty Reply stops the running thread.** When driving a pane with `rex send-key`,
  never send Escape unless stopping it is the point. Same for `stop --force`, which kills the
  agent.
- **Updating T3 restarts it.** On a Mac running the T3 Code app, `tr3x update` restarts the
  app to install, which stops running threads until T3 picks them up again. Ask the user before
  running it, and never script past its question. `tr3x update --check` is always safe.
- **Leave other people's sessions alone.** tr3x never touches a Rex session it didn't make;
  neither should you while working on it.
- **A machine out of reach isn't an error to fix.** Its tabs read `(offline)` and come back on
  their own. Only `Run tr3x pair <machine>` in its conversation means it needs pairing again.

## When a tab is missing or wrong

1. `tr3x doctor`. Fix any ✗ the way it says.
2. `tail -20 ~/.local/state/tr3x/sync.log`. Each pass lists the threads it shows, in order,
   with their markers.
3. A thread that's settled, snoozed or archived has no tab by design. **Find Thread…** in the
   palette, or `tr3x unsettle --thread …` / `tr3x wake --thread …`, brings it back.
4. A thread on another machine: `tr3x envs`. Not paired means `tr3x pair <machine>`.

## Writing for a thread someone reads in tr3x

If you're the agent in a T3 thread, the user may be reading you in Rex, on a Mac or a phone.

- The conversation is a centred column, 96 columns at T3's default Chat width. Keep tables
  narrower than that; a wider row wraps.
- Pictures you attach show inline. An HTML preview shows as a snapshot of the page, so make it
  read at a glance: one chart or one layout, not a scrolling document.
- Questions and approvals get buttons. Short option labels fit a phone.
