# Using t3rex

Every key, command and setting. New to it? Start with [getting started](../README.md#getting-started).

## Keys and commands

| | |
|---|---|
| New thread | ⌘T or + in the T3 Code session, or ⌃⌘N anywhere. Type the first message; the chips under it pick the project, model, effort, access, and Local or New worktree and its base branch. **Add folder…** in the project chip adds a project: type a path, Tab completes it. Tab moves to the chips, Enter or a click opens one, typing filters. Enter sends. The tab becomes the thread's. |
| Reply | Type in Reply, Enter to send. One chip under it shows the model, effort and access, and beside it any provider usage limits at 20% remaining or less, yellow at 20% and red at 10%. A message to a running thread steers it or queues behind it, as T3's Follow-up behavior setting says; ⌘Enter does the other for one message, as in T3. Shift+Enter, Alt+Enter or Ctrl+J for a new line; Reply grows with the text, up to 8 lines or 35% of the window, and shrinks back once sent. Typing in the conversation moves you to Reply. A send that fails keeps your text. |
| Typing keys | As in Claude Code and Codex: Esc twice clears the input and its pictures, and ⌘Z brings the text back; Esc on an empty input stops a running thread; ↑ at the start brings back what you sent before, ↓ goes forward again and back to what you'd typed. As in any Mac text field: ↑ ↓ and ⌘← ⌘→ by the rows on screen, ⌥← ⌥→ by word, ⌥↑ ⌥↓ by paragraph, ⌘↑ ⌘↓ to the start and end; ⌘⌫ ⌥⌫ ⌘⌦ ⌥⌦ delete to the row's start, a word, to the row's end, the next word; ⌘Z ⌘⇧Z undo and redo; ⌃A ⌃E ⌃B ⌃F ⌃P ⌃N ⌃D ⌃H ⌃K ⌃Y ⌃T ⌃O, with ⌃U and ⌃W as in a shell. The same keys work in New Thread and Rename. |
| Pictures | Paste a picture or screenshot (⌘V), or drop an image file, into Reply. It shows as a chip above the input and goes with the next message; click its ✕, or Backspace at the start of the input, to take it off. Pictures you send show in the conversation. |
| Model | Click the chip under Reply, press Tab there, or type `/model`. One picker: every enabled provider's models, then the model's efforts and access; ← → switch between them. The choice applies to the next message. |
| Approve | Click **Approve**, **Always** or **Decline** over the input, or Tab to them and Enter. `a`, `s` or `d` then Enter does the same. Anything else you type is a reply. |
| Answer | Click an option, or Tab to it. A pending question also takes the input as its answer, and a number picks that option. |
| Another machine | On it, run `curl -fsSL http://<this Mac>:3779/join \| sh`; `t3rex pair` prints the exact command. That's all: it pairs that machine's T3 with Rex over your tailnet, and only your own machines can. Where `ssh <host>` already works, `t3rex pair` does it from here with one confirm instead. A link from T3 → Settings → Connections there works too (`t3rex pair 'https://…/pair#token=…'`). **Pair Another Machine…** in the palette does the same from any device. `t3rex envs` lists them, `t3rex env rename box studio` names one, `t3rex env forget studio` unpairs it. |
| Machine | In a new thread, the first chip picks the machine, once there's more than one. It loads that machine's projects, models and branches; **Pair…** pairs one that isn't yet. |
| Sessions | One **T3 Code** session for every machine, or `t3rex sessions per-machine` for a session each (**Sessions: One per Machine** in the palette). ⌃⌘I still goes to whatever needs you, on any machine. Switching opens the tabs again in their new sessions and settles nothing. |
| Reply pane | `/reply pane` in Reply, or `t3rex reply pane`, gives Reply a pane of its own under the conversation, sized to fit it; `/reply inline` puts it back. Open windows change within a few seconds. Rex's iPhone app shows each pane as a window of its own, so inline suits a phone. |
| Open in T3 | Click the thread's title at the top of the conversation. It opens T3's web app on the device you click it on, at the address T3 serves on your tailnet, or T3 Code itself when there's none. `/open` opens T3 Code on the Mac running it. |
| Another device | Run **Fit to This Device** from the palette on the laptop or phone you're using. The session's panes take its size until another device runs it. |
| Pull request | **Create Pull Request** in the palette, or `/pr create` in Reply: T3 commits, pushes and opens one with the title and body it writes, as its own Create PR does, on a new branch if the thread is on the default one. **Show Pull Request** or `/pr` opens the thread's. Either way it opens in a drawer floating over the right of the thread, full height when the window is wide enough, else over nearly all of it. [sup](https://github.com/superultrainc/sup) shows it there: description, conversation, review threads, checks and files; `m` merges, `A` approves, `M` comments, `q` steps back to sup's list of your pull requests and again closes the drawer. Without sup, the drawer shows `gh pr view`. |
| Settle | Close the tab. A thread that's working or waiting on you only hides until it changes. `/settle` settles it whatever it's doing. **Reopen Last Settled** in the palette undoes a tab closed by accident. |
| Find | **Find Thread…** in the palette lists every thread on every machine, grouped Open, Snoozed, Settled and Archived, newest first. Typing filters. Enter brings one back to its tab, unsettling, waking or unarchiving it as needed; ⌥Enter opens it in T3. |
| Snooze | `/snooze 1h`, `/snooze 2h`, `/snooze tomorrow`. The way to put a thread away without settling it. |
| Commands | `/steer …` `/settle` `/unsettle` `/snooze` `/wake` `/pin` `/unpin` `/rename` `/retitle` `/fork` `/archive` `/delete` `/stop` `/force-stop` `/model` `/effort` `/mode` `/open` `/pr` `/width`. `/` lists these with the agent's own commands and skills; typing filters, Tab or Enter completes. A command the thread's machine is too old for isn't listed. |
| Keys | ⌃⌘N new thread · ⌃⌘I go to the thread that needs you |
| Palette | Everything else is in Rex's command palette (⌘⇧P) under **T3 Code**. To give an action a key, add a line like `rex.bind("ctrl+cmd+s", "t3_settle")` to `~/.config/rex/init.lua`. |

Subagents show under the thread they work for, as `↳ title`, while they work or need you. A
finished subagent's result is in its parent thread.

## Chat width

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
- **Pull requests on GitHub.** sup reads GitHub with this Mac's `gh` login, whichever machine
  the thread runs on. T3 makes the pull request with its own login there.
- **Tailscale for other machines.** Finding and joining machines goes over your tailnet. Without
  Tailscale, pair with SSH or a link. Pairings renew on their own; one made before t3rex asked
  for `access:write` renews the old way, so a linked machine from then needs one more link.
- **Moving targets.** Works with T3 Code 0.1.6 and its nightlies (orchestration
  protocol 2), and Rex Beta 0.1.0 (1642), API revision 7.

More in [how it works](how-it-works.md).
