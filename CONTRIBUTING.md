# Contributing

## Build and test

Needs Rust (stable), a Mac or Linux machine, Rex (Rex Beta on a Mac) and T3 Code.

```sh
cargo build --release -p tr3x
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

CI runs the last three on every pull request. Tests use a fake T3 server, so they don't need
T3 running.

## Run your build beside an installed one

```sh
TR3X_STATE_DIR=/tmp/tr3x-dev scripts/dev-restart
```

That builds, then starts a sync of your build with its own state, in a session called
**t3x-dev**. Its panes never mark a thread visited, so the installed sync carries on as before.
Pair machines into that state with `TR3X_STATE_DIR=/tmp/tr3x-dev tr3x pair`.
[How it works](docs/how-it-works.md#development) has more.

It finds Rex the way tr3x does: `TR3X_REX`, then `rex` on `PATH` (the usual case on Linux), then
the Mac app's copy. It stops before it builds, closes or deletes anything when there is none.

## Dependency advisories

`cargo audit` reports no vulnerabilities. It does warn that `bincode` 1.3.3 is unmaintained
(RUSTSEC-2025-0141): it comes in through `syntect` 5.3.0, the syntax highlighter, which uses it
to load its bundled syntaxes. That's maintenance debt, not a known flaw in how tr3x uses it; it
goes when syntect moves off bincode 1. Update locked dependencies one at a time, so a fix like
Tokio's 1.53.3 (RUSTSEC-2026-0386) is a two-line change to `Cargo.lock`.

## T3 moves fast

`tr3x/t3-surface.json` records what the T3 build tr3x was made against offers: capabilities,
commands and RPC methods. A [daily workflow](.github/workflows/surface.yml) compares each T3
nightly with it and keeps an issue up to date with what was added or removed, and whether the
tests still pass. Check it locally with:

```sh
target/release/tr3x surface --check tr3x/t3-surface.json
```

Rex does the same at a slower pace. `rex/surface.txt` lists every command, flag, API method and
field, Lua call, event, action and setting in Rex's reference docs, Bleeding Edge included. A
[daily workflow](.github/workflows/rex.yml) keeps one issue up to date with what was added,
removed or promoted to Preview, and marks what tr3x already calls. Rex's release notes are in
its beta Discord, so the docs are the part a workflow can read. Check it locally with:

```sh
scripts/rex-surface --check rex/surface.txt
```

## Releasing

Push a tag such as `v0.2.0` and `release.yml` builds each platform, checks every archive, puts
them in a draft release and publishes it only once all of them are there, so `install.sh` never
sees a half-built release. Every install picks it up within a day, on its own, so a release is a
deploy: run the build on a Mac beside an installed one first (above). Bump `version` in
`tr3x/Cargo.toml` to match the tag; the workflow stops if they differ, and the updater checks the
binary says the version its release claims.

Run the workflow by hand (Actions → release → Run workflow) to build and check the same archives
without a tag; it keeps them as a run artifact and never touches a release. A failed run leaves
at most a draft: re-run its failed jobs to retry, and the draft's files are replaced. Runs for
one tag take turns. A tag whose release is already published (a retag) keeps its notes and has
its files replaced. Each archive carries the licenses: `LICENSE`, t3client's upstream
`LICENSE-t3client`, and `THIRD-PARTY-LICENSES.txt` from `scripts/third-party-notices`, which
reads every linked crate's own license files and fails if one has none.

## Pull requests

- [Conventional commits](https://www.conventionalcommits.org): `feat:`, `fix:`, `docs:`,
  `refactor:`, `test:`, `chore:`.
- Say what changed for someone using tr3x, as the README does: plain sentences, no internals
  unless they matter.
- A change to what people see comes with a screenshot. Take it the way below.

## Screenshots without your own threads

A screenshot of your real T3 Code session shows your threads, projects, paths and machines.
Take them from a T3 of their own instead, with made-up projects:

```sh
D=$(mktemp -d); mkdir -p "$D/demo" && git -C "$D/demo" init -q
APP="/Applications/T3 Code.app"            # or "T3 Code (Nightly).app"
t3() { ELECTRON_RUN_AS_NODE=1 "$APP/Contents/MacOS/$(basename "$APP" .app)" \
  "$APP/Contents/Resources/app.asar/apps/server/dist/bin.mjs" "$@"; }

t3 serve --base-dir "$D/t3" --host 127.0.0.1 --port 3795 --no-browser &   # a second T3, on loopback
t3 project add --base-dir "$D/t3" --title demo "$D/demo"

export T3CODE_HOME="$D/t3" TR3X_STATE_DIR="$D/state" TR3X_SESSION_LABEL="T3 Code"
target/release/tr3x sync --log --no-first &                              # its own Rex session
```

Start threads in the new session with ⌘T, and let them get to the state you want to show.
Agents use your own provider logins.

1. In Rex, **File → New Window**, and pick the new session in that window's sidebar. Click
   your real session's header there to fold it away; folding is per window.
2. Find the window's number with `CGWindowListCopyWindowInfo`, and take it with
   `screencapture -o -l <number> shot.png`.
3. Look at every pixel before you commit it: the sidebar, the pane titles, paths in tool rows.

To finish, stop the sync, run `rex kill <session>`, then stop the second T3. Killing the
session closes any Rex window still showing it.

