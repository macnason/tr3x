# Changelog

## Unreleased

- A message that fails to send comes back with its pictures. If you've started typing something else by then, it waits under **Unsent message** and ⌃R puts it back ahead of your draft, so neither is lost.
- A new thread launches on the machine you picked last, even when an earlier pick answers later. It waits while the pick loads, and a pick that fails says why and stays on the machine before it.
- A failed turn shows the provider's own error when that's the only place it was reported, such as a sign-in problem.
- `tr3x install` says how to run tr3x, and what to add to your shell, when `~/.local/bin` isn't on `PATH`.
- Releases are published only once every platform's archive is built and checked. Each archive carries the license notices of tr3x, the t3client code it grew from, and the crates it links.
- `scripts/dev-restart` finds Rex on `PATH` on Linux and checks for it before closing or deleting anything.
- Tokio 1.53.3 (RUSTSEC-2026-0386). SECURITY.md now says what an agent's HTML preview can reach.

## 0.1.0

First release.

- One Rex session, **T3 Code**, with a tab per open thread, in T3's sidebar order, for every machine Rex is paired with.
- Reply at the bottom of the conversation: T3's chips for model, effort and access, pictures, slash commands, and buttons for approvals and questions.
- Status markers, usage limits, folded finished turns, HTML previews and pull requests in a drawer.
- Pairing other machines over your tailnet, and a machine that stays on as the host others follow.
- macOS and Linux. `tr3x doctor`, `tr3x update` and `tr3x uninstall`.
