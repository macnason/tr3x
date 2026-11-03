# Frames

Drawn overnight on 31 Oct 2026 while the Mac was locked, with `scripts/render-frame`: the
sidebar, labels and status markers are Rex's own (`session inspect`, `program_status`), and
each pane is Rex's own screen buffer (`com.superlogical.terminal.format`, html). The window
chrome and fonts are an approximation of the app's. Pictures drawn with kitty graphics (HTML
snapshots, images) don't appear here; their rows show blank.

| File | Shows |
|---|---|
| `approval-dark.png`, `approval-light.png` | A thread waiting on a file-change approval. It sits first in the sidebar with Rex's blue marker; the Reply pane offers a / s / d. Same output in both themes. |
| `question-dark.png` | Claude's AskUserQuestion as a T3 question: numbered options in the Reply pane. |
| `question-answered-dark.png` | After typing `2`: the answer reached T3, the agent replied, and the thread moved back down to the working group. |
| `real-thread-dark.png`, `real-thread-light.png` | Real threads: headings, bold, lists, links, the run footer. |
| `html-embed-snapshot-dark.png` | The snapshot t3rex took of an `html_render` embed, which draws inline in Rex. |
| `busy-day-mock-dark.png`, `busy-day-mock-light.png` | **Mock sidebar** (made-up thread names) around the real approval window, to show a busier day: needs-you first, then idle, then working with Rex's `•••`. |
| `pr-drawer-dark.png` | Drawn 1 Nov 2026, the same way. Create Pull Request on this thread: T3 pushed and opened #1 with the text it wrote, and the drawer on the right is sup on it, full height beside the conversation and Reply. Its merge box still showed the conflict with `master` that a rebase then cleared. |
| `one-pane-desktop-dark.png`, `one-pane-phone-dark.png` | Drawn 1 Nov 2026 from `rex capture --format=html` of the dev session, rendered in headless Chrome. A thread's window as one pane: the conversation scrolls above, Reply stays at the bottom. The phone frame is the same pane at 46 columns. |
| `one-pane-approval-dark.png` | An approval in the one pane: the request and its buttons in Reply. Clicks are on only while they show; a click on Approve sent `accept`. |
| `one-pane-phone-slash-dark.png` | The slash menu at phone width, in rows Reply keeps for it above the input. |
| `reply-fill-dark.png` | Drawn 2 Nov 2026, the same way. Reply's input on a fill, the Rex theme's background a step lighter. The thin bands between the fill's rows come from the HTML capture; in Rex the fill is solid. |
| `reply-pane-dark.png` | With `/reply pane`: the conversation in its pane, and Reply in a pane of its own under it. Sync reshaped the open windows both ways. The panes were 100×18 and 100×4 for the frame; nobody was viewing the session, so Reply hadn't fitted its pane. |
| `usage-limit-dark.png` | Taken 2 Nov 2026 from a demo T3 with its own base dir, as CONTRIBUTING.md describes; the real session is folded. A Claude thread stopped on its 5-hour limit, from a stand-in endpoint answering as Anthropic's does when an account is out of usage. The conversation ends on when the limit resets and that T3 resumes it then (T3's Auto-resume limited threads was on), and the line under its label says the same. Failed is Rex's badge for the error state. |
| `usage-left-dark.png` | The same window on another thread: beside Reply's chip, what that provider (Codex, the account's real numbers) has left of its session and weekly limits. |
