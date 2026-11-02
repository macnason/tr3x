# t3rex

Your [T3 Code](https://github.com/pingdotgg/t3code) threads, as tabs in [Rex](https://www.superlogical.com/rex).

T3 runs the agents. Rex is where you read and reply. Each open thread is a tab in one Rex
session, **T3 Code**, sorted the way T3's sidebar sorts them: whatever needs you on top. That
goes for every machine you run T3 on, without the T3 app.

![Rex showing the T3 Code session: an approval and a question waiting at the top of the sidebar, two threads working on Claude and Codex, and a chart an agent drew in the conversation](docs/assets/hero.png)

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
[Every step](docs/how-it-works.md#install-in-detail) is undone by `t3rex uninstall`.

## Getting started

1. **Open Rex.** The **T3 Code** session has a tab for each open thread. Whatever waits on you
   is at the top.
2. **Start a thread.** ⌘T in the session, or ⌃⌘N from anywhere. Type the first message; the
   chips under it pick the project, model and worktree. Enter sends, and the tab becomes the
   thread's.
3. **Reply.** Type at the bottom of the conversation and press Enter. `/` lists t3rex's
   commands and the agent's own commands and skills.
4. **Answer what's waiting.** An approval puts **Approve**, **Always** and **Decline** over the
   input: click one, or type `a`, `s` or `d` and Enter. A question's options are buttons too.
5. **Go to what needs you.** ⌃⌘I jumps to the thread that most needs you, on any machine.
6. **Settle it.** Close the tab when you're done. A thread that's still working only hides
   until it changes. **Find Thread…** in the palette (⌘⇧P) brings anything back.

![The New Thread tab a session with no threads opens on](docs/frames/onboarding-welcome-light.png)

With no open threads you get a **New Thread** tab with the keys that matter. With no projects
in T3 yet, it opens on a folder to add as one. If T3 Code isn't running, the tab says so and
carries on once it is.

T3 Code runs this Mac's agents, so they stop when it quits (⌘Q, an update, a restart). Closing
its window doesn't quit it. To keep agents going with the app quit, run T3 on a machine that
stays on (`t3 service install` there) and pair it: threads you start on it keep running.

**Another machine.** Run `t3rex pair`. It finds the machines on your tailnet that run T3 and
pairs each one: from here where `ssh` reaches it, else with one command to run there.

**Your phone.** Rex for iPhone shows the same session. Run **Fit to This Device** from the
palette on the phone, and the tabs take its size.

## What it does

- **One tab per thread.** Approvals, questions, failures and unread results go first. Then the
  inbox, pins first. Then whatever's working. Settled, snoozed and archived threads stay out of
  the way until T3 brings them back.
- **Read like a terminal.** The conversation is plain terminal output, so Rex's scrollback,
  find, selection and links all work. Markdown, tables and highlighted code render in a centred
  column at T3's chat width. Pictures show inline, and so do an agent's HTML previews, as a
  snapshot.
- **Reply where you read.** One input at the bottom of the conversation, with T3's chips for
  model, effort and access, or in a pane of its own with `/reply pane`. Approvals and questions
  get buttons right there: one click on a Mac, one tap on a phone.
- **Rex's markers are T3's.** Each tab reports its thread's status to Rex, so the dot on a tab
  means what it means in T3, and a waiting tab says what it's waiting on: the command, the file
  or the question. The tab's glyph is the thread's provider, and its label starts with what T3
  is waiting on you for: `●` an approval, `?` a question, `✕` failed, `✓` done and unread.
- **Usage limits.** A thread stopped on its provider's usage limit says so, in the
  conversation and under its tab: when the limit resets, and that T3 picks it up then if
  auto-resume is on. Reply shows what the model's provider has left, `Session 62% left`.
- **Any screen.** Rex's Mac and iPhone apps can watch the same session from another device.
  **Fit to This Device** sizes it to the one you're on, the thread's title opens it in T3 there,
  and pictures follow that screen's light or dark.
- **Every machine.** Threads from every machine Rex is paired with share the session, in one
  order. Once there's more than one, a tab says which machine its thread runs on:
  `Fix the build · studio`. New threads start on whichever machine you pick.
- **Pull requests beside the thread.** `/pr create` has T3 commit, push and open one; `/pr`
  shows it in a drawer over the right of the thread.
- **Close to settle.** Closing a tab settles the thread. A thread that's still working only
  hides until it changes, so nothing gets stopped by accident.

Nothing runs an agent of its own. Every message, approval and setting goes to T3.

## Let your agent drive it

t3rex comes with a [skill](skills/t3rex/SKILL.md) for coding agents: install and check t3rex,
fix a tab that's missing, pair a machine, change a setting, or settle, snooze and rename
threads from a terminal.

```sh
npx skills add macnason/t3rex
```

Or copy `skills/t3rex` into your agent's skills folder, such as `~/.claude/skills`. Then ask it
to "set up t3rex", or "why isn't my new thread showing in Rex?".

## Docs

- [Using t3rex](docs/usage.md): every key, command and setting, chat width, and known limits.
- [How it works](docs/how-it-works.md): sync, machines, pairing, the panes, and install in
  detail.
- [Contributing](CONTRIBUTING.md): building, testing, and taking screenshots without your own
  threads in them.
- [Security](SECURITY.md): what t3rex can reach, where its tokens live, and how to report a
  problem.

Works with T3 Code 0.1.6 and its nightlies (orchestration protocol 2), and Rex Beta 0.1.0.
Both move fast; a [daily check](.github/workflows/surface.yml) flags what each T3 nightly
changes.

## Credits

- [Rex](https://www.superlogical.com/rex) by Superlogical.
- [T3 Code](https://github.com/pingdotgg/t3code), which does the actual work.
- [t3term](https://github.com/krishhgg/t3term) by krishhgg (MIT), which t3rex's T3 client
  started from.

## License

[MIT](LICENSE), by Mac ([macnason](https://github.com/macnason)). `t3client` keeps t3term's
[MIT license](t3client/LICENSE).
