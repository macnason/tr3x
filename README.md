# t3rex

Your T3 Code threads, as tabs in [Rex](https://www.superlogical.com/rex).

T3 runs the agents. Rex is where you read and reply. Each open thread is a tab in one Rex
session, **T3 Code**, sorted the way T3's sidebar sorts them: whatever needs you on top.

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
  or the question. The tab's glyph is the thread's provider.
- **Any screen.** Rex's Mac and iPhone apps can watch the same session from another device.
  **Fit to This Device** sizes it to the one you're on, the thread's title opens it in T3 there,
  and pictures follow that screen's light or dark.
- **Close to settle.** Closing a tab settles the thread. A thread that's still working only
  hides until it changes, so nothing gets stopped by accident.

Nothing runs an agent of its own. Every message, approval and setting goes to T3.

## Install

Needs macOS, [Rex Beta](https://www.superlogical.com/rex) and T3 Code on the same Mac.

```sh
git clone https://github.com/macnason/t3rex && cd t3rex
sh install.sh
```

From a checkout, `install.sh` builds with cargo, so you'll need a Rust toolchain. With a
release build there's no Rust needed:

```sh
T3REX_URL=https://…/t3rex-macos-arm64.tar.gz sh install.sh
```

`install.sh` opens Rex and T3 if they aren't running, installs `~/.local/bin/t3rex`, adds its
actions to Rex's config, starts a launchd agent that keeps the session in sync, and switches Rex
to **T3 Code**. [Every step](docs/how-it-works.md#install-in-detail) is undone by:

```sh
t3rex uninstall
```

`t3rex doctor` checks each part if something looks off.

## First run

![The new-thread window a session with no threads opens on](docs/frames/onboarding-welcome-light.png)

Rex opens on **T3 Code** with a tab per open thread. No open threads: you get a **New Thread**
tab instead, with the four keys that matter. New Tab (⌘T or +) in the session always opens
another. No projects in T3 yet: it opens on a folder to add as one. If T3 Code isn't running,
the tab says so and carries on once it is.

## Using it

| | |
|---|---|
| New thread | ⌘T or + in the T3 Code session, or ⌃⌘N anywhere. Type the first message; the chips under it pick the project, model, effort, access, and Local or New worktree and its base branch. **Add folder…** in the project chip adds a project: type a path, Tab completes it. Tab moves to the chips, Enter or a click opens one, typing filters. Enter sends. The tab becomes the thread's. |
| Reply | Type in Reply, Enter to send. A message to a running thread queues; `/steer …` sends it into the running turn instead. Alt+Enter or Ctrl+J for a new line. Typing in the conversation moves you to Reply. A send that fails keeps your text. |
| Model | Click a chip under Reply, press Tab there, type `/model`, or ⌃⌘M. Every enabled provider's models, then the model's efforts; ← → for effort and access. The choice applies to the next message. |
| Approve | Click **Approve**, **Always** or **Decline** over the input, or Tab to them and Enter. `a`, `s` or `d` then Enter does the same. Anything else you type is a reply. |
| Answer | Click an option, or Tab to it. A pending question also takes the input as its answer, and a number picks that option. |
| Open in T3 | Click the thread's title at the top of the conversation. It opens T3's web app on the device you click it on, at the address T3 serves on your tailnet, or T3 Code itself when there's none. ⌃⌘O and `/open` open T3 Code on the Mac running it. |
| Another device | Run **Fit to This Device** from the palette on the laptop or phone you're using. The session's panes take its size until another device runs it. |
| Settle | Close the tab. A thread that's working or waiting on you only hides until it changes. ⌃⌘S or `/settle` settle it whatever it's doing. |
| Snooze | ⌃⌘Z for an hour, or `/snooze 2h`, `/snooze tomorrow`. The way to put a thread away without settling it. |
| Commands | `/steer …` `/settle` `/snooze` `/wake` `/pin` `/unpin` `/archive` `/stop` `/model` `/effort` `/mode` `/open` `/width` |
| Keys | ⌃⌘N new thread · ⌃⌘M model · ⌃⌘I go to the thread that needs you · ⌃⌘S settle · ⌃⌘Z snooze 1h · ⌃⌘P pin · ⌃⌘O open in T3 Code (on this Mac) |
| Palette | Every action is in Rex's command palette under **T3 Code**. |

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
- **Resizing reprints.** The conversation reprints when its width changes, so scrollback
  position resets on resize.
- **Pictures** need kitty graphics (Rex has it). HTML snapshots need a headless Chromium;
  without one an embed is a titled link.
- **One size at a time.** Rex sizes a pane to one device. The others show that size scaled,
  until one of them runs Fit to This Device. Tabs opened after that size to whoever draws them
  first.
- **Linked devices.** T3's linked environments aren't in the new-thread chips yet. T3 launches
  on one only through its agent tools, which t3rex doesn't log in to.
- **Moving targets.** Built against T3 `0.1.6-nightly.20261030` (orchestration protocol 2) and
  Rex Beta 0.1.0 (1642), API revision 7.

More in [how it works](docs/how-it-works.md).

## Credits

- [Rex](https://www.superlogical.com/rex) by Superlogical.
- T3 Code, which does the actual work.
- [t3term](https://github.com/krishhgg/t3term) by krishhgg (MIT), vendored for its T3 client,
  auth and sidebar logic.
