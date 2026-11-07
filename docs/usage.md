# Using tr3x

Every key, command and setting. New to it? Start with [getting started](../README.md#getting-started).

## Keys and commands

| | |
|---|---|
| Empty session | With no open threads you get a **New Thread** tab with the keys that matter. With no projects in T3 yet, it opens on a folder to add as one. If T3 Code isn't running, the tab says so and carries on once it is. |
| New thread | ⌘T or + in the T3 Code session, or ⌃⌘N anywhere. Type the first message; the chips under it pick the project, model, effort, access, and Local or New worktree and its base branch. **Add folder…** in the project chip adds a project: type a path, Tab completes it. Tab moves to the chips, Enter or a click opens one, typing filters. Enter sends. The tab becomes the thread's. |
| Reply | Type in Reply, Enter to send. One chip under it shows the model, effort and access, and beside it any provider usage limits at 20% remaining or less, yellow at 20% and red at 10%. A message to a running thread steers it or queues behind it, as T3's Follow-up behavior setting says; ⌘Enter does the other for one message, as in T3. Shift+Enter, Alt+Enter or Ctrl+J for a new line; Reply grows with the text, up to 8 lines or 35% of the window, and shrinks back once sent. Typing in the conversation moves you to Reply. A send that fails puts your text and pictures back; if you've started something new by then, they wait under a red **Unsent message** line and ⌃R brings them back ahead of it. |
| Typing keys | As in Claude Code and Codex: Esc twice clears the input and its pictures, and ⌘Z brings the text back; Esc on an empty input stops a running thread; ↑ at the start brings back what you sent before, ↓ goes forward again and back to what you'd typed. As in any Mac text field: ↑ ↓ and ⌘← ⌘→ by the rows on screen, ⌥← ⌥→ by word, ⌥↑ ⌥↓ by paragraph, ⌘↑ ⌘↓ to the start and end; ⌘⌫ ⌥⌫ ⌘⌦ ⌥⌦ delete to the row's start, a word, to the row's end, the next word; ⌘Z ⌘⇧Z undo and redo; ⌃A ⌃E ⌃B ⌃F ⌃P ⌃N ⌃D ⌃H ⌃K ⌃Y ⌃T ⌃O, with ⌃U and ⌃W as in a shell. The same keys work in New Thread and Rename. |
| Pictures | Paste a picture or screenshot (⌘V), or drop an image file, into Reply. It shows as a chip above the input and goes with the next message; click its ✕, or Backspace at the start of the input, to take it off. Pictures you send show in the conversation. From another device, it needs Rex 1853 or later, which copies the picture to the machine the pane runs on. |
| Model | Click the chip under Reply, press Tab there, or type `/model`. One picker: every enabled provider's models, then the model's efforts and access; ← → switch between them. The choice applies to the next message. |
| Approve | Click **Approve**, **Always** or **Decline** over the input, or Tab to them and Enter. `a`, `s` or `d` then Enter does the same. Anything else you type is a reply. |
| Answer | Click an option, or Tab to it. A pending question also takes the input as its answer, and a number picks that option. |
| Give a secret | When the agent asks for a login, token or code, Reply names it and the input is the secret: it shows as dots, and Enter gives it to T3, which keeps it out of the thread. **Decline** says no. |
| Another machine | `tr3x pair` finds it on your tailnet and pairs it with one confirm: through its Rex host when it's in `rex hosts` (no SSH or keys), else over SSH where `ssh <host>` already works. `tr3x pair <rex host>` names one. Otherwise, a link from T3 → Settings → Connections there works too (`tr3x pair 'https://…/pair#token=…'`). **Pair Another Machine…** in the palette does the same from any device. `tr3x envs` lists them, `tr3x env rename box studio` names one, `tr3x env forget studio` unpairs it. |
| Machine | In a new thread, the first chip picks the machine, once there's more than one. It loads that machine's projects, models and branches; **Pair…** pairs one that isn't yet. |
| Sessions | One **T3 Code** session for every machine, or `tr3x sessions per-machine` for a session each (**Sessions: One per Machine** in the palette). ⌃⌘I still goes to whatever needs you, on any machine. Switching opens the tabs again in their new sessions and settles nothing. |
| Reply pane | `/reply pane` in Reply, or `tr3x reply pane`, gives Reply a pane of its own under the conversation, sized to fit it; `/reply inline` puts it back. Open windows change within a few seconds. Rex's iPhone app shows each pane as a window of its own, so inline suits a phone. |
| Open in T3 | Click the thread's title at the top of the conversation. It opens T3's web app on the device you click it on, at the address T3 serves on your tailnet, or T3 Code itself when there's none. `/open` opens T3 Code on the Mac running it. |
| Another device | Run **Fit to This Device** from the palette on the laptop or phone you're using. The session's panes take its size until another device runs it. |
| Pull request | **Create Pull Request** in the palette, or `/pr create` in Reply: T3 commits, pushes and opens one with the title and body it writes, as its own Create PR does, on a new branch if the thread is on the default one. **Show Pull Request** or `/pr` opens the thread's. Either way it opens in a drawer floating over the right of the thread, full height when the window is wide enough, else over nearly all of it. [sup](https://github.com/superultrainc/sup) shows it there: description, conversation, review threads, checks and files; `m` merges, `A` approves, `M` comments, `q` steps back to sup's list of your pull requests and again closes the drawer. sup is optional: a drawer that opens without it shows `gh pr view`, and Enter installs sup into `~/.local/bin`, checked against its release, and shows the pull request in it. |
| Settle | Close the tab. A thread that's working or waiting on you only hides until it changes. `/settle` settles it whatever it's doing. **Reopen Last Settled** in the palette undoes a tab closed by accident. |
| Find | **Find Thread…** in the palette lists every thread on every machine, grouped Open, Snoozed, Settled and Archived, newest first. Typing filters. Enter brings one back to its tab, unsettling, waking or unarchiving it as needed; ⌥Enter opens it in T3. |
| Fresh session | `/clear` starts the next message in a new provider session, in the same thread. Esc on the empty input keeps the current one. `/clear message` sends that message fresh. |
| Snooze | `/snooze 1h`, `/snooze 2h`, `/snooze tomorrow`. The way to put a thread away without settling it. |
| Commands | `/steer …` `/clear` `/settle` `/unsettle` `/snooze` `/wake` `/pin` `/unpin` `/rename` `/retitle` `/fork` `/archive` `/delete` `/stop` `/force-stop` `/model` `/effort` `/mode` `/open` `/pr` `/width` `/details`. `/` lists these with the agent's own commands and skills; typing filters, Tab or Enter completes. A command the thread's machine is too old for isn't listed. |
| Updates | tr3x checks once a day and installs its own updates; `tr3x updates notify` makes it ask first, `tr3x updates off` stops it checking. A tab picks up a new version once nothing's typed in it. When a machine's T3 is behind, Reply says `T3 update · /update`. `/update`, **Update tr3x, T3 and Rex…** in the palette, or `tr3x update` in a terminal lists what's out and installs it on Enter: the T3 Code app downloads its update and restarts to install it, asking first while threads run there (T3 picks them up again after); a machine running T3's background service updates and restarts by itself. A machine running a plain `t3 serve` is updated there. The Rex app on each Mac attached here updates last and relaunches; your tabs keep running (needs Remote Control on in Rex's settings there). `tr3x update --check` only lists. |
| Keys | ⌃⌘N new thread · ⌃⌘I go to the thread that needs you · ⌃⌘[ from a subagent to the thread it works for · ⌃⌘] from a thread to its subagent |
| Palette | Everything else is in Rex's command palette (⌘⇧P) under **T3 Code**. To give an action a key, add a line like `rex.bind("ctrl+cmd+s", "t3_settle")` to `~/.config/rex/init.lua`. |
| Shortcuts | Shortcuts' **Run Rex Action** lists every **T3 Code** action, for Siri, a widget or a hotkey. Go to Thread That Needs You, New Thread and Find Thread… work from anywhere. The rest act on the thread in Rex's last window, and stop with "Go to a T3 thread first" when it isn't one. A Rex action takes no input, so to start a thread by voice or from a widget, use Shortcuts' **Run Command** on the host instead: `tr3x new --prompt "…"`, with **Ask for Input** or **Dictate Text** before it. It launches in the project you worked in last (`--project name` or `--folder path` to pick), with T3's defaults for it, and opens the tab. It runs on iPhone and iPad as well as the Mac, since the command runs on the host. |
| Finder | **New T3 Thread Here** in a folder's Quick Actions (or Services menu) asks what to do, then starts a thread in the project that holds that folder, or makes the folder one. `tr3x install` adds it on a Mac; give it a key in System Settings → Keyboard → Keyboard Shortcuts → Services. |

Subagents show under the thread they work for, as `↳ title`, while they work or need you. A
finished subagent's result is in its parent thread.

- A subagent's header says which thread it works for. ⌃⌘[ goes there, and ⌃⌘] goes from a thread
  to its subagent (the one that needs you, first). Each goes on the screen you pressed it on.
- A subagent you're reading keeps its tab when it finishes, until you leave it.
- Closing a thread closes its subagents' tabs. One that waits on an approval or an
  answer stays, as `↳ title · for Parent`. The same goes for a subagent of a thread that's
  settled, snoozed or archived: its request would otherwise wait, unseen.
- Closing a subagent that's only working hides it until it needs you or finishes.

## Chat width

The conversation, Reply and the new-thread composer share one centred column, like T3's chat
lane. It follows T3 Code's **Chat width** setting, so it works as installed:

| T3 setting | T3 lane | tr3x column |
|---|---|---|
| Comfortable (default) | 46rem | 96 columns |
| Wide | 72rem | 150 columns |
| Full | the window | the pane, 2 columns clear each side |

46rem of T3's sans holds about 92 to 100 characters a line, so 96 sits in that range. Wide keeps
T3's 72/46 ratio. A pane narrower than the column uses its full width.

To differ from T3: `/width wide` in Reply, or `tr3x width comfortable|wide|full|N` (a number is
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
- **Pull requests on GitHub.** sup reads GitHub with the host's `gh` login, whichever machine
  the thread runs on. T3 makes the pull request with its own login there.
- **Tailscale for other machines.** Finding and pairing machines goes over your tailnet. Without
  Tailscale, pair through a Rex host, SSH or a link. Pairings renew on their own; one made before tr3x asked
  for `access:write` renews the old way, so a linked machine from then needs one more link. A machine joined with `curl …/join | sh`
  (before 0.1.3) pairs again through its Rex host, SSH or a link.
- **Moving targets.** Works with T3 Code 0.1.6 and its nightlies (orchestration
  protocol 2), and Rex Beta 0.1.0 (1642), API revision 7.

More in [how it works](how-it-works.md).
