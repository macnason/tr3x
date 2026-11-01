# t3rex

Your T3 Code threads, as tabs in [Rex](https://www.superlogical.com/rex).

T3 runs the agents. Rex is where you read and reply. Each open thread is a tab in one Rex
session, **T3 Code**, sorted the way T3's sidebar sorts them: whatever needs you on top. That
goes for every machine you run T3 on, without the T3 app.

![The T3 Code session in Rex: threads that need you first, an approval waiting in Reply](docs/frames/busy-day-mock-dark.png)

## What it does

- **One tab per thread.** Approvals, questions, failures and unread results go first. Then the
  inbox, pins first. Then whatever's working. Settled, snoozed and archived threads stay out of
  the way until T3 brings them back.
- **Read like a terminal.** The conversation is plain terminal output, so Rex's scrollback,
  find, selection and links all work. Markdown, tables, highlighted code and inline images
  render in a centred column at T3's chat width.
- **Reply where you read.** One input under the conversation, with T3's chips for model,
  effort and access. Approvals and questions get buttons right there: one click on a Mac, one
  tap on a phone.
- **Rex's markers are T3's.** Each tab reports its thread's status to Rex, so the dot on a tab
  means what it means in T3, and a waiting tab says what it's waiting on: the command, the file
  or the question. The tab's glyph is the thread's provider, and its label starts with what T3
  is waiting on you for: `●` an approval, `?` a question, `✕` failed, `✓` done and unread.
- **Any screen.** Rex's Mac and iPhone apps can watch the same session from another device.
  **Fit to This Device** sizes it to the one you're on, the thread's title opens it in T3 there,
  and pictures follow that screen's light or dark.
- **Every machine.** Threads from every machine Rex is paired with share the session, in one
  order. Once there's more than one, a tab says which machine its thread runs on:
  `Fix the build · studio`. New threads start on whichever machine you pick.
- **Close to settle.** Closing a tab settles the thread. A thread that's still working only
  hides until it changes, so nothing gets stopped by accident. **Find Thread…** brings back
  anything settled, snoozed or archived.

Nothing runs an agent of its own. Every message, approval and setting goes to T3.

## Install

Needs macOS, [Rex Beta](https://www.superlogical.com/rex), and T3 Code running with at least one
project.

```sh
curl -fsSL https://raw.githubusercontent.com/macnason/t3rex/master/install.sh | sh
```

That opens Rex and T3 if they aren't running, downloads the latest release and checks its
SHA-256, installs `~/.local/bin/t3rex`, adds its actions and keys to Rex's config, and switches
Rex to **T3 Code**. Rex starts t3rex's sync as its config loads, so it runs whenever Rex does.
Rex pairs with this Mac's T3 on its own. Other machines on your tailnet that run T3 are found
and offered: one confirm where SSH reaches them, else one command to run there.

From a clone, `install.sh` builds with cargo instead:

```sh
git clone https://github.com/macnason/t3rex && cd t3rex
sh install.sh
```

`t3rex doctor` checks each part and says how to fix what isn't right.
[Every step](docs/how-it-works.md#install-in-detail) is undone by:

```sh
t3rex uninstall
```

## First run

![The new-thread window a session with no threads opens on](docs/frames/onboarding-welcome-light.png)

Rex opens on **T3 Code** with a tab per open thread. No open threads: you get a **New Thread**
tab instead, with the keys that matter. New Tab (⌘T or +) in the session always opens
another. No projects in T3 yet: it opens on a folder to add as one. If T3 Code isn't running,
the tab says so and carries on once it is.

## Using it

| | |
|---|---|
| New thread | ⌘T or + in the T3 Code session, or ⌃⌘N anywhere. Type the first message; the chips under it pick the project, model, effort, access, and Local or New worktree and its base branch. **Add folder…** in the project chip adds a project: type a path, Tab completes it. Tab moves to the chips, Enter or a click opens one, typing filters. Enter sends. The tab becomes the thread's. |
| Reply | Type in Reply, Enter to send. One chip under it shows the model, effort and access. A message to a running thread steers it or queues behind it, as T3's Follow-up behavior setting says; ⌘Enter does the other for one message, as in T3. Shift+Enter, Alt+Enter or Ctrl+J for a new line; Reply grows with the text, up to 8 lines or 35% of the window, and shrinks back once sent. Typing in the conversation moves you to Reply. A send that fails keeps your text. |
| Typing keys | As in Claude Code and Codex: Esc stops a running thread; Esc twice clears the input and its pictures, and ⌘Z brings the text back; ↑ at the start brings back what you sent before, ↓ goes forward again and back to what you'd typed. As in any Mac text field: ↑ ↓ and ⌘← ⌘→ by the rows on screen, ⌥← ⌥→ by word, ⌥↑ ⌥↓ by paragraph, ⌘↑ ⌘↓ to the start and end; ⌘⌫ ⌥⌫ ⌘⌦ ⌥⌦ delete to the row's start, a word, to the row's end, the next word; ⌘Z ⌘⇧Z undo and redo; ⌃A ⌃E ⌃B ⌃F ⌃P ⌃N ⌃D ⌃H ⌃K ⌃Y ⌃T ⌃O, with ⌃U and ⌃W as in a shell. The same keys work in New Thread and Rename. |
| Pictures | Paste a picture or screenshot (⌘V), or drop an image file, into Reply. It shows as a chip above the input and goes with the next message; click its ✕, or Backspace at the start of the input, to take it off. Pictures you send show in the conversation. |
| Model | Click the chip under Reply, press Tab there, or type `/model`. One picker: every enabled provider's models, then the model's efforts and access; ← → switch between them. The choice applies to the next message. |
| Approve | Click **Approve**, **Always** or **Decline** over the input, or Tab to them and Enter. `a`, `s` or `d` then Enter does the same. Anything else you type is a reply. |
| Answer | Click an option, or Tab to it. A pending question also takes the input as its answer, and a number picks that option. |
| Another machine | On it, run `curl -fsSL http://<this Mac>:3779/join \| sh`; `t3rex pair` prints the exact command. That's all: it pairs that machine's T3 with Rex over your tailnet, and only your own machines can. Where `ssh <host>` already works, `t3rex pair` does it from here with one confirm instead. A link from T3 → Settings → Connections there works too (`t3rex pair 'https://…/pair#token=…'`). **Pair Another Machine…** in the palette does the same from any device. `t3rex envs` lists them, `t3rex env rename box studio` names one, `t3rex env forget studio` unpairs it. |
| Machine | In a new thread, the first chip picks the machine, once there's more than one. It loads that machine's projects, models and branches; **Pair…** pairs one that isn't yet. |
| Sessions | One **T3 Code** session for every machine, or `t3rex sessions per-machine` for a session each (**Sessions: One per Machine** in the palette). ⌃⌘I still goes to whatever needs you, on any machine. Switching opens the tabs again in their new sessions and settles nothing. |
| Open in T3 | Click the thread's title at the top of the conversation. It opens T3's web app on the device you click it on, at the address T3 serves on your tailnet, or T3 Code itself when there's none. `/open` opens T3 Code on the Mac running it. |
| Another device | Run **Fit to This Device** from the palette on the laptop or phone you're using. The session's panes take its size until another device runs it. |
| Settle | Close the tab. A thread that's working or waiting on you only hides until it changes. `/settle` settles it whatever it's doing. **Reopen Last Settled** in the palette undoes a tab closed by accident. |
| Find | **Find Thread…** in the palette lists every thread on every machine, grouped Open, Snoozed, Settled and Archived, newest first. Typing filters. Enter brings one back to its tab, unsettling, waking or unarchiving it as needed; ⌥Enter opens it in T3. |
| Snooze | `/snooze 1h`, `/snooze 2h`, `/snooze tomorrow`. The way to put a thread away without settling it. |
| Commands | `/steer …` `/settle` `/unsettle` `/snooze` `/wake` `/pin` `/unpin` `/rename` `/retitle` `/fork` `/archive` `/delete` `/stop` `/force-stop` `/model` `/effort` `/mode` `/open` `/width`. `/` lists these with the agent's own commands and skills; typing filters, Tab or Enter completes. A command the thread's machine is too old for isn't listed. |
| Keys | ⌃⌘N new thread · ⌃⌘I go to the thread that needs you |
| Palette | Everything else is in Rex's command palette (⌘⇧P) under **T3 Code**. To give an action a key, add a line like `rex.bind("ctrl+cmd+s", "t3_settle")` to `~/.config/rex/init.lua`. |

Subagents show under the thread they work for, as `↳ title`, while they work or need you. A
finished subagent's result is in its parent thread.

### Chat width

The conversation, Reply and the new-thread composer share one centred column, like T3's chat
lane. It follows T3 Code's **Chat width** setting, so it works as installed:

| T3 setting | T3 lane | t3rex column |
|---|---|---|
| Comfortable (default) | 46rem | 96 columns |
| Wide | 72rem | 150 columns |
| Full | the window | the pane, 2 columns clear each side |

46rem of T3's sans holds about 92 to 100 characters a line, so 96 sits in that range. Wide keeps
T3's 72/46 ratio. A pane narrower than the column uses its full width.

To differ from T3: `/width wide` in Reply, or `t3rex width comfortable|wide|full|N` (a number is
a column count). `/width t3` goes back to following T3.

| Full | Comfortable |
|---|---|
| ![](docs/frames/width-full-dark.png) | ![](docs/frames/width-comfortable-dark.png) |

## Known limits

- **One click to settle, not two.** Rex has no API for context-menu items or buttons on a tab,
  so closing the tab is the click. Sync notices within 5 seconds.
- **Resizing reprints.** The conversation reprints when its size changes, so scrollback
  position resets on resize, including when Reply grows for a longer message.
- **Rex keeps some keys.** ⌘Enter never reaches a pane (Find Thread uses ⌥Enter). Rex turns
  ⌘← ⌘→ and ⌘⌫ into Ctrl+A, Ctrl+E and Ctrl+U on the way, which do the same.
- **Pictures** need kitty graphics (Rex has it). HTML snapshots need a headless Chromium;
  without one an embed is a titled link.
- **One size at a time.** Rex sizes a pane to one device. The others show that size scaled,
  until one of them runs Fit to This Device. Tabs opened after that size to whoever draws them
  first.
- **Offline machines.** A machine out of reach keeps its tabs where they are, marked
  `(offline)` after 30 seconds. Its conversation says so and Reply keeps what you type until it
  answers again. Nothing is settled or closed for a machine sync can't see.
- **Other machines' files.** HTML snapshots need the attachment on this Mac, so on another
  machine an embed is a titled link. Add folder adds folders on this Mac only.
- **Tailscale for other machines.** Finding and joining machines goes over your tailnet. Without
  Tailscale, pair with SSH or a link. Pairings renew on their own; one made before t3rex asked
  for `access:write` renews the old way, so a linked machine from then needs one more link.
- **Moving targets.** Built against T3 `0.1.6-nightly.20261030` (orchestration protocol 2) and
  Rex Beta 0.1.0 (1642), API revision 7.

More in [how it works](docs/how-it-works.md).

## Credits

- [Rex](https://www.superlogical.com/rex) by Superlogical.
- T3 Code, which does the actual work.
- [t3term](https://github.com/krishhgg/t3term) by krishhgg (MIT), which t3rex's T3 client
  started from.

## License

MIT, by Mac ([macnason](https://github.com/macnason)).
