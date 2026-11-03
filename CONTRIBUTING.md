# Contributing

## Build and test

Needs Rust (stable), macOS, Rex Beta and T3 Code.

```sh
cargo build --release -p t3rex
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

CI runs the last three on every pull request. Tests use a fake T3 server, so they don't need
T3 running.

## Run your build beside an installed one

```sh
T3REX_STATE_DIR=/tmp/t3rex-dev scripts/dev-restart
```

That builds, then starts a sync of your build with its own state, in a session called
**t3x-dev**. Its panes never mark a thread visited, so the installed sync carries on as before.
Pair machines into that state with `T3REX_STATE_DIR=/tmp/t3rex-dev t3rex pair`.
[How it works](docs/how-it-works.md#development) has more.

## T3 moves fast

`t3rex/t3-surface.json` records what the T3 build t3rex was made against offers: capabilities,
commands and RPC methods. A [daily workflow](.github/workflows/surface.yml) compares each T3
nightly with it and keeps an issue up to date with what was added or removed, and whether the
tests still pass. Check it locally with:

```sh
target/release/t3rex surface --check t3rex/t3-surface.json
```

## Releasing

Push a tag such as `v0.2.0` and `release.yml` builds, checksums and publishes it. Every
install picks it up within a day, on its own, so a release is a deploy: run the build on a Mac
beside an installed one first (above). Bump `version` in `t3rex/Cargo.toml` to match the tag;
the updater checks the binary says the version its release claims.

## Pull requests

- [Conventional commits](https://www.conventionalcommits.org): `feat:`, `fix:`, `docs:`,
  `refactor:`, `test:`, `chore:`.
- Say what changed for someone using t3rex, as the README does: plain sentences, no internals
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

export T3CODE_HOME="$D/t3" T3REX_STATE_DIR="$D/state" T3REX_SESSION_LABEL="T3 Code"
target/release/t3rex sync --log --no-first &                              # its own Rex session
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

`docs/assets/hero.png` was made this way, on Rex's gradient.
