#!/bin/sh
# t3rex: Rex as the interface to T3 Code. One command:
#
#   sh install.sh                        # from a checkout
#   T3REX_URL=https://…/t3rex-macos-arm64.tar.gz sh install.sh
#
# Gets the t3rex binary (a prebuilt one from $T3REX_URL, or `cargo build` when Rust is
# installed), checks that Rex and T3 Code are installed and running, and runs `t3rex install`.
# Nothing needs sudo. `t3rex uninstall` undoes it.
set -eu

say() { printf '%s\n' "$*"; }
ok() { printf '\033[32m✓\033[0m %s\n' "$*"; }
no() { printf '\033[31m✗\033[0m %s\n' "$*" >&2; }

[ "$(uname -s)" = Darwin ] || { no "t3rex needs macOS"; exit 1; }
ARCH=$(uname -m)

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

# The binary: prebuilt, else built here.
HERE=$(cd "$(dirname "$0")" && pwd)
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
BIN=""
if [ -n "${T3REX_URL:-}" ]; then
  say "Downloading t3rex…"
  /usr/bin/curl -fL --progress-bar -o "$WORK/t3rex.tar.gz" "$T3REX_URL"
  if [ -n "${T3REX_SHA256:-}" ]; then
    echo "$T3REX_SHA256  $WORK/t3rex.tar.gz" | /usr/bin/shasum -a 256 -c - >/dev/null || { no "The download doesn't match T3REX_SHA256"; exit 1; }
  fi
  mkdir "$WORK/x" && tar -xzf "$WORK/t3rex.tar.gz" -C "$WORK/x"
  BIN=$(find "$WORK/x" -type f -name t3rex -perm -u+x | head -1)
  # A download is quarantined; this one was asked for.
  [ -n "$BIN" ] && xattr -d com.apple.quarantine "$BIN" 2>/dev/null || true
elif [ -x "$HERE/dist/t3rex-macos-$ARCH/t3rex" ]; then
  BIN="$HERE/dist/t3rex-macos-$ARCH/t3rex"
elif command -v cargo >/dev/null 2>&1 && [ -f "$HERE/Cargo.toml" ]; then
  say "Building t3rex (a minute or two the first time)…"
  cargo build --release -p t3rex --manifest-path "$HERE/Cargo.toml"
  BIN="$HERE/target/release/t3rex"
else
  no "No t3rex binary"
  say "  Set T3REX_URL to a release tarball, or install Rust (https://rustup.rs) and run this from a checkout."
  exit 1
fi
[ -n "$BIN" ] && [ -x "$BIN" ] || { no "Couldn't find t3rex in the download"; exit 1; }
ok "t3rex $("$BIN" --version | cut -d' ' -f2)"

T3REX_REX="$REX" "$BIN" install
