---
name: t3rex
description: Install, check, fix and drive t3rex, which shows T3 Code threads as tabs in the Rex terminal on macOS. Use when the user mentions t3rex, or the T3 Code session in Rex; asks to set it up, why a thread's tab is missing or wrong, to pair another machine, to change chat width, Reply placement or sessions; or wants a T3 thread settled, snoozed, pinned, renamed, forked, stopped, opened in T3, or its pull request shown, from a terminal.
---

# t3rex

t3rex keeps one Rex session, **T3 Code**, in step with T3 Code: a tab per open thread, in T3's
sidebar order, for every machine Rex is paired with. T3 runs the agents and holds every
thread's state. t3rex only follows it and sends T3 what the user does, so a fix is almost never
"edit t3rex's state"; it's "make T3 or Rex right, and let sync catch up".

## Start with doctor

```sh
t3rex doctor
```

It checks Rex, the Rex config line, T3 on each machine and the capabilities t3rex needs, that
sync is running and what it last logged, and the session. Every ✗ prints its fix, and any ✗
exits non-zero. Run it before guessing, and again after a fix to confirm it.

No `t3rex` on the PATH means it isn't installed, or `~/.local/bin` isn't on the PATH. Install
with the one command below. It needs macOS, Rex Beta, and T3 Code running with a project. Never
use sudo; nothing t3rex does needs it.

```sh
curl -fsSL https://raw.githubusercontent.com/macnason/t3rex/master/install.sh | sh
```

`install.sh` asks about other machines on the terminal. Without one, as in an agent's shell, it
lists them with the command for each instead, and `t3rex pair` does that part later.

## Commands

| Command | What it does |
|---|---|
| `t3rex doctor` | Checks everything; prints a fix for each ✗. |
| `t3rex envs` | Machines Rex follows, and the ones on the tailnet it could pair. |
| `t3rex pair` | Finds machines on the tailnet that run T3 and pairs them. |
| `t3rex pair <ssh-host>` | Pairs one machine over SSH. Needs `ssh <host>` to work without a password. |
| `t3rex pair '<link>'` | Pairs with a link from T3 → Settings → Connections on that machine. |
| `t3rex env rename <machine> <name>` | What Rex calls a machine. An empty name goes back to T3's. |
| `t3rex env forget <machine>` | Unpairs a machine. |
| `t3rex width [comfortable\|wide\|full\|N\|t3]` | How wide conversations read. `t3` follows T3's Chat width. |
| `t3rex reply [inline\|pane]` | Reply at the bottom of the conversation, or in a pane of its own. |
| `t3rex sessions [merged\|per-machine]` | One T3 Code session, or one per machine. |
| `t3rex update` | Lists updates for t3rex and each machine's T3, and installs them on Enter. `--check` only lists. |
| `t3rex updates [auto\|notify\|off]` | Whether t3rex installs its own updates, asks first, or never checks. |
| `t3rex uninstall` | Stops sync, removes the config, closes t3rex's session. |

With no argument, `width`, `reply`, `sessions` and `updates` print the current value.

Thread actions take `--thread <env/thread>` or a thread id on this Mac. Inside a t3rex pane,
`REX_BLOCK` already names the thread, so `--thread` can be left out:

`settle`, `unsettle`, `snooze [30m|2h|1d]`, `wake`, `pin [--off]`, `archive`,
`rename [title] [--regenerate]`, `fork [title]`, `stop [--force]`, `open`, `pr [--create]`,
`delete [--yes]`.

## Files

| Path | |
|---|---|
| `~/.local/bin/t3rex` | The binary. |
| `~/.config/rex/t3rex.lua` | The Rex config `install` writes: starts sync, adds actions and keys. |
| `~/.config/t3rex/settings.json` | Width, Reply, sessions, pane fading. The commands above edit it. |
| `~/.local/state/t3rex/sync.log` | What sync did last. Read it when a tab is missing or stale. |
| `~/.local/state/t3rex/state.json` | The session and a window per thread. Sync owns it. |
| `~/.local/state/t3rex/updates.json` | What the last update check found. |
| `~/.local/state/t3rex/envs.json`, `credentials.json` | Paired machines and their tokens (mode 0600). |

## Rules

- **Don't edit `state.json` or `credentials.json`, and don't kill `t3rex sync`.** Sync rewrites
  state on every change, and a token edited by hand stops working. To restart sync, run
  `rex config reload`: the config starts it. ⌃⌘I and ⌃⌘N also start it if it's down. Outside
  a Rex pane `rex` isn't on the PATH; it's
  `/Applications/Rex Beta.app/Contents/Helpers/Rex Server.rex/Contents/MacOS/rex`.
- **Closing a tab settles its thread.** Don't close t3rex's windows with `rex` to tidy up.
  Use `t3rex snooze` to put one away, or leave it.
- **`delete` removes the thread in T3 too, and can't be undone.** Ask the user first, then pass
  `--yes`. `archive` or `settle` is almost always what they want.
- **Esc in an empty Reply stops the running thread.** When driving a pane with `rex send-key`,
  never send Escape unless stopping it is the point. Same for `stop --force`, which kills the
  agent.
- **Updating T3 restarts it.** On a Mac running the T3 Code app, `t3rex update` restarts the
  app to install, which stops running threads until T3 picks them up again. Ask the user before
  running it, and never script past its question. `t3rex update --check` is always safe.
- **Leave other people's sessions alone.** t3rex never touches a Rex session it didn't make;
  neither should you while working on it.
- **A machine out of reach isn't an error to fix.** Its tabs read `(offline)` and come back on
  their own. Only `Run t3rex pair <machine>` in its conversation means it needs pairing again.

## When a tab is missing or wrong

1. `t3rex doctor`. Fix any ✗ the way it says.
2. `tail -20 ~/.local/state/t3rex/sync.log`. Each pass lists the threads it shows, in order,
   with their markers.
3. A thread that's settled, snoozed or archived has no tab by design. **Find Thread…** in the
   palette, or `t3rex unsettle --thread …` / `t3rex wake --thread …`, brings it back.
4. A thread on another machine: `t3rex envs`. Not paired means `t3rex pair <machine>`.

## Writing for a thread someone reads in t3rex

If you're the agent in a T3 thread, the user may be reading you in Rex, on a Mac or a phone.

- The conversation is a centred column, 96 columns at T3's default Chat width. Keep tables
  narrower than that; a wider row wraps.
- Pictures you attach show inline. An HTML preview shows as a snapshot of the page, so make it
  read at a glance: one chart or one layout, not a scrolling document.
- Questions and approvals get buttons. Short option labels fit a phone.
