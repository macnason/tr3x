# Changelog

## Unreleased

- A status line under Reply shows what T3 Code's thread details do. Beside the model: how much of the context window is left, as a bar, its tokens (`100k/1M`), and how long the prompt cache lasts. Under it: the machine, branch, pull request with what it changes and its checks, and what's left of each usage limit. Shares left turn yellow at 20% and red at 10%, and the cache in its last 10 minutes. By default it's one line, `compact`, dropping the rest as the pane narrows. `/status full` shows both lines, `/status auto` full where it fits and compact where it doesn't, as on a phone, and `/status chip` the model alone, as before. `tr3x status` sets it from a terminal.

## 0.1.6

- Works with Rex, not only Rex Beta. Once Rex replaced Rex Beta, sync couldn't reach Rex, so `/settle`, `/snooze` and `/archive` left the thread's tab open, and new threads got no tab. A running sync now finds Rex again without a restart.

## 0.1.5

- Threads on a machine running T3 0.1.10 or later show again. Sync dropped and reopened its feed from that machine every few seconds (`a chunk would go over the limit of 1 chunk` in the log), as that T3 sends several updates before it waits for tr3x to take them.
- Rex's Activity list says what a working thread is doing: the program its latest command runs (`$ pnpm test`), the file it edits or the tool it uses, else the start of what it's writing. It said `project · Working`. The rest of a command stays in the thread, since the line shows on every device.
- A thread on a machine paired over SSH forwards each `localhost` port its latest reply links to onto the same port on this Mac, once a web server answers there, so the link opens as written. On a laptop or phone, Rex's ⌘-click forwarding carries it the rest of the way. A port already in use here, and the ports databases and Docker use, are left alone. The forwards close with the tab.
- **Approve**, **Approve Always** and **Decline** in the palette answer an approval. They answer the thread in front of you, or else, for Approve and Decline, the one ⌃⌘I goes to, so from any tab they answer what Activity shows blocked. Always answers only the thread you're in. Bind one with `rex.bind`.

- With a Rex app that can ask in its own palette (Rex API revision 17 or later), the palette asks its questions in Rex's own command palette, so they work the same on a phone or iPad as on the Mac. **Find Thread…** lists every thread there, with its state, project and when, to filter and pick. **Rename Thread…** opens on the current title. **Delete Thread…** asks, with Keep it first. An older Rex app gets the floating windows as before.
- **Open Project…** (⌃⌘O) picks a project on any machine, then where to start: its folder, a worktree it already has, or a new one. New Thread's composer opens on that, for the first message.
- **Answer…** (⌃⌘A) answers what the thread in front of you waits on, else the next thread that waits, in the palette, from any tab or device. An approval offers Approve, Approve Always or Decline, with the command or file it allows. An agent's questions come one after another with their options, several at once where the question takes several, or a reply typed in. **Go to Thread** opens it instead. **Answer Next…** goes through everything waiting on every machine, with **Skip** for one to leave. A secret request still opens its tab, since the palette can't hide what's typed.
- **Snooze Thread…** picks 1 hour, 3 hours, tomorrow or a week, or takes a time you type, such as `45m`.

## 0.1.4

- ⌘V of a picture no longer says "Update Rex to paste pictures from another device" when Rex is up to date. When Rex passes ⌘V on as a key, Reply reads the clipboard on this Mac itself, or asks Rex on the other device to paste again.

## 0.1.3

- `tr3x update`, `/update` and **Update tr3x, T3 and Rex…** also update the Rex app on each Mac attached here, with Rex's own updater. It goes last, and the app relaunches with your tabs still running. It needs Remote Control on in that Rex. `tr3x doctor` shows each Rex app's build, and says when one holds an update.
- `tr3x new --prompt "…"` starts a thread without the composer and opens its tab: for Shortcuts' Run Command (with Dictate Text, that's Siri), a widget or a script. `--project`, `--folder` and `--env` pick where; `--prompt -` reads standard input.
- **New T3 Thread Here** in Finder's Quick Actions, for a folder. `tr3x install` adds it on a Mac.
- `tr3x pair` pairs a machine through its Rex host (`rex -S <host>`), no SSH or keys needed. It goes first when `rex hosts` has the machine, and `tr3x pair <rex host>` names one.
- A tab shows its provider's icon and name (Claude, Codex, Cursor, Grok, Gemini, OpenCode, Pi…) from the pane's own status report. A thread that changes provider no longer restarts its pane for it.
- Tab labels are the thread's title. Rex's own dot shows what a thread waits on, on the Mac, iPhone and iPad, so the `●` `?` `✕` `✓` marks are gone.
- A closed tab settles its thread at once, on Rex's `window_closed` event, rather than on sync's next pass.
- An HTML preview links its page, **Preview**, when it's on this Mac: a click opens it, and Rex's Quick Look (⇧⌘-click) previews it.
- On Linux the installer ends with Rex's QR code for the host's tailnet address (`rex server tailscale pair`), to scan with **Pair via QR Code** in Rex on a phone or iPad.
- Pictures pasted or dropped into Reply from another device attach, with Rex 1853 or later: Rex copies them to the host. tr3x no longer receives pictures by Taildrop.
- Removed: joining with `curl …/join | sh`. Sync no longer listens on port 3779. A machine joined that way pairs again through its Rex host, SSH or a link.
- Removed: `"paneFade": "auto"`. Rex's Settings → Interface → Active Pane covers it; installing puts back the setting tr3x saved, if it changed one.

- `/clear` starts a fresh session in the same thread, as in T3 0.1.8. The next message goes to a new provider session, the conversation marks it **New session**, and Esc keeps the current one. It replaces the agent's own `/clear`, which ended a session T3 still treated as live.
- When an agent asks for a secret, Reply asks for it. What you type shows as dots, Enter gives it to T3 and Decline says no; the thread only says what was asked for. Before, the request didn't show and the agent waited. Rex marks the tab `?` while it waits.
- A request to link another machine shows in the conversation, with what became of it.

- Rex's Go to Anything lists a thread by its title. It showed `T3 · project · branch`, the pane's terminal title.
- An action on a thread, run from Shortcuts' Run Rex Action or `rex do` without one in front, says "Go to a T3 thread first".
- `tr3x doctor` warns when Rex's server is older than the app, and says how to restart it.
- A new thread's workspace menu lists the project's existing worktrees, by branch, under Local and New worktree. Pick one and the thread starts there, as T3 does.
- Long lines in code blocks wrap, with continuation rows indented past the gutter. They were cut off with `…`.
- A thread's tab that Rex brought back after a restart with a shell where the conversation was starts the conversation again, in the same place. Before, it stayed a bare shell, and closing it by hand settled the thread.

## 0.1.2

- A message that fails to send comes back with its pictures. If you've started typing something else by then, it waits under **Unsent message** and ⌃R puts it back ahead of your draft, so neither is lost.
- A new thread launches on the machine you picked last, even when an earlier pick answers later. It waits while the pick loads, and a pick that fails says why and stays on the machine before it.
- A failed turn shows the provider's own error when that's the only place it was reported, such as a sign-in problem.
- `tr3x install` says how to run tr3x, and what to add to your shell, when `~/.local/bin` isn't on `PATH`.
- Releases are published only once every platform's archive is built and checked. Each archive carries the license notices of tr3x, the t3client code it grew from, and the crates it links.
- `scripts/dev-restart` finds Rex on `PATH` on Linux and checks for it before closing or deleting anything.
- Tokio 1.53.3 (RUSTSEC-2026-0386). SECURITY.md now says what an agent's HTML preview can reach.
- A subagent's header says which thread it works for. ⌃⌘[ goes up to it and ⌃⌘] goes down to a subagent, on the screen you pressed it on.
- A subagent you're reading keeps its tab when it finishes, until you leave.
- Closing a thread closes its subagents' tabs, except one waiting on an approval or an answer.
- A subagent waiting on an approval or an answer now shows even when its thread is settled, snoozed or archived. Before, its request was invisible and the agent waited.
- Closing a subagent that's only working keeps it closed until it needs you.

## 0.1.0

First release.

- One Rex session, **T3 Code**, with a tab per open thread, in T3's sidebar order, for every machine Rex is paired with.
- Reply at the bottom of the conversation: T3's chips for model, effort and access, pictures, slash commands, and buttons for approvals and questions.
- Status markers, usage limits, folded finished turns, HTML previews and pull requests in a drawer.
- Pairing other machines over your tailnet, and a machine that stays on as the host others follow.
- macOS and Linux. `tr3x doctor`, `tr3x update` and `tr3x uninstall`.
