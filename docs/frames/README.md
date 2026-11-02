# Frames

Frames drawn from Rex's own screen data rather than screenshots: each pane is Rex's screen
buffer (`rex capture --format=html`), and the sidebar, labels and status markers are Rex's
(`session inspect`, `program_status`). The window chrome and fonts approximate the app's.
Pictures drawn with kitty graphics (HTML snapshots, images) don't appear; their rows show blank.

For a real screenshot, see [Screenshots without your own threads](../../CONTRIBUTING.md#screenshots-without-your-own-threads).

| File | Shows |
|---|---|
| `onboarding-welcome-light.png`, `onboarding-welcome-dark.png` | The New Thread tab a session with no threads opens on, with the keys that matter. |
| `question-dark.png` | Claude's AskUserQuestion as a T3 question: numbered options in Reply. |
| `question-answered-dark.png` | After typing `2`: the answer reached T3, the agent replied, and the thread moved back down to the working group. |
| `one-pane-desktop-dark.png`, `one-pane-phone-dark.png` | A thread's window as one pane: the conversation scrolls above, Reply stays at the bottom. The phone frame is the same pane at 46 columns. |
| `one-pane-approval-dark.png` | An approval in the one pane: the request and its buttons in Reply. Clicks are on only while they show. |
| `one-pane-phone-slash-dark.png` | The slash menu at phone width, in rows Reply keeps for it above the input. |
| `reply-fill-dark.png` | Reply's input on a fill, the Rex theme's background a step lighter. The thin bands between the fill's rows come from the HTML capture; in Rex the fill is solid. |
| `reply-pane-dark.png` | With `/reply pane`: the conversation in its pane, and Reply in a pane of its own under it. |
| `usage-limit-dark.png` | Taken 2 Nov 2026 from a demo T3 with its own base dir, as CONTRIBUTING.md describes; the real session is folded. A Claude thread stopped on its 5-hour limit, from a stand-in endpoint answering as Anthropic's does when an account is out of usage. The conversation ends on when the limit resets and that T3 resumes it then (T3's Auto-resume limited threads was on), and the line under its label says the same. Failed is Rex's badge for the error state. |
| `usage-left-dark.png` | The same window on another thread: beside Reply's chip, what that provider (Codex, the account's real numbers) has left of its session and weekly limits. |
