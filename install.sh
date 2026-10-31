#!/bin/sh
# t3rex: your T3 Code threads, as tabs in Rex. One command:
#
#   curl -fsSL https://raw.githubusercontent.com/macnason/t3rex/master/install.sh | sh
#   sh install.sh                        # from a clone, building with cargo
#
# Checks that Rex and T3 Code are installed and running, gets the t3rex binary (a build when
# run from a clone with Rust installed, else the latest release, checked against its SHA-256),
# and runs `t3rex install`. T3REX_URL (and T3REX_SHA256) pick another tarball. Nothing needs
# sudo. `t3rex uninstall` undoes it.
set -eu

REPO=macnason/t3rex
ASSET=t3rex-macos-universal.tar.gz

if [ -t 1 ]; then
  ok() { printf '\033[32m✓\033[0m %s\n' "$*"; }
  no() { printf '\033[31m✗\033[0m %s\n' "$*" >&2; }
else
  ok() { printf '✓ %s\n' "$*"; }
  no() { printf '✗ %s\n' "$*" >&2; }
fi
say() { printf '%s\n' "$*"; }

[ "$(uname -s)" = Darwin ] || { no "t3rex needs macOS"; exit 1; }

REX_APP=${T3REX_REX_APP:-"/Applications/Rex Beta.app"}
REX="$REX_APP/Contents/Helpers/Rex Server.rex/Contents/MacOS/rex"
T3_APP=${T3REX_T3_APP:-}
if [ -z "$T3_APP" ]; then
  for app in "/Applications/T3 Code.app" "/Applications/T3 Code (Nightly).app" "/Applications/T3 Code (Beta).app"; do
    [ -d "$app" ] && { T3_APP=$app; break; }
  done
fi

# Rex: installed, and its server answering.
if [ ! -x "$REX" ]; then
  no "Rex isn't installed"
  say "  Get Rex Beta from https://www.superlogical.com/rex, open it once, then run this again."
  exit 1
fi
if ! "$REX" ls >/dev/null 2>&1; then
  open -a "$REX_APP"
  i=0; while ! "$REX" ls >/dev/null 2>&1; do i=$((i+1)); [ $i -gt 30 ] && break; sleep 1; done
fi
"$REX" ls >/dev/null 2>&1 || { no "Rex is installed but its server isn't answering"; say "  Open Rex Beta and finish its setup, then run this again."; exit 1; }
ok "Rex"

# T3 Code: installed, and its server written down in ~/.t3.
RUNTIME=${T3CODE_HOME:-$HOME/.t3}/userdata/server-runtime.json
t3_up() {
  [ -f "$RUNTIME" ] || return 1
  ORIGIN=$(/usr/bin/python3 -I -c 'import json,sys;print(json.load(open(sys.argv[1]))["origin"])' "$RUNTIME" 2>/dev/null) || return 1
  /usr/bin/curl -fsS --max-time 3 -o /dev/null "$ORIGIN/" 2>/dev/null
}
if ! t3_up; then
  if [ -z "$T3_APP" ]; then
    no "T3 Code isn't installed"
    say "  Install T3 Code, open it and add a project, then run this again."
    exit 1
  fi
  open -a "$T3_APP"
  i=0; while ! t3_up; do i=$((i+1)); [ $i -gt 60 ] && break; sleep 1; done
fi
t3_up || { no "T3 Code is installed but its server isn't answering"; say "  Open T3 Code and sign in, then run this again."; exit 1; }
ok "T3 Code"

# The binary: a build from this clone, else a release. Piped into sh, $0 is the shell, so a
# clone is only one when its files are there.
HERE=$(cd "$(dirname "$0")" 2>/dev/null && pwd || pwd)
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
BIN=""
if [ -z "${T3REX_URL:-}" ] && [ -f "$HERE/t3rex/Cargo.toml" ] && command -v cargo >/dev/null 2>&1; then
  say "Building t3rex (a minute or two the first time)…"
  cargo build --release --locked -p t3rex --manifest-path "$HERE/Cargo.toml"
  BIN="$HERE/target/release/t3rex"
else
  URL=${T3REX_URL:-https://github.com/$REPO/releases/latest/download/$ASSET}
  SUM=${T3REX_SHA256:-}
  if [ -z "${T3REX_URL:-}" ] && [ -z "$SUM" ]; then
    /usr/bin/curl -fsSL -o "$WORK/sum" "$URL.sha256" || {
      no "Couldn't download t3rex from github.com/$REPO"
      say "  Check your connection, or run this from a clone with Rust installed (https://rustup.rs)."
      exit 1
    }
    SUM=$(cut -d' ' -f1 < "$WORK/sum")
  fi
  say "Downloading t3rex…"
  /usr/bin/curl -fL --progress-bar -o "$WORK/t3rex.tar.gz" "$URL" || { no "Couldn't download $URL"; exit 1; }
  if [ -n "$SUM" ]; then
    GOT=$(/usr/bin/shasum -a 256 "$WORK/t3rex.tar.gz" | cut -d' ' -f1)
    [ "$GOT" = "$SUM" ] || { no "The download's SHA-256 doesn't match"; say "  Expected $SUM, got $GOT."; exit 1; }
  fi
  mkdir "$WORK/x" && tar -xzf "$WORK/t3rex.tar.gz" -C "$WORK/x"
  BIN=$(find "$WORK/x" -type f -name t3rex -perm -u+x | head -1)
  # A download can be quarantined; this one was asked for.
  [ -n "$BIN" ] && xattr -d com.apple.quarantine "$BIN" 2>/dev/null || true
fi
[ -n "$BIN" ] && [ -x "$BIN" ] || { no "Couldn't find t3rex in the download"; exit 1; }
ok "t3rex $("$BIN" --version | cut -d' ' -f2)"

# Copies itself to ~/.local/bin when it runs from a build or a download.
T3REX_REX="$REX" "$BIN" install
