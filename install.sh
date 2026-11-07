#!/bin/sh
# tr3x: your T3 Code threads, as tabs in Rex. One command:
#
#   curl -fsSL https://raw.githubusercontent.com/macnason/tr3x/master/install.sh | sh
#   sh install.sh                        # from a clone, building with cargo
#
# On a Mac or on Linux. Checks that Rex and T3 Code are installed and running, gets the tr3x
# binary (a build when run from a clone with Rust installed, else the latest release, checked
# against its SHA-256), and runs `tr3x install`. TR3X_URL (and TR3X_SHA256) pick another tarball. Nothing needs
# sudo. `tr3x uninstall` undoes it.
set -eu

REPO=macnason/tr3x
OS=$(uname -s)
case "$OS" in
  Darwin) NAME=tr3x-macos-universal ;;
  Linux)
    case "$(uname -m)" in
      x86_64|amd64) NAME=tr3x-linux-x86_64 ;;
      aarch64|arm64) NAME=tr3x-linux-aarch64 ;;
      *) printf '✗ tr3x has no build for %s\n' "$(uname -m)" >&2; exit 1 ;;
    esac ;;
  *) printf '✗ tr3x needs macOS or Linux\n' >&2; exit 1 ;;
esac
ASSET=$NAME.tar.gz

if [ -t 1 ]; then
  ok() { printf '\033[32m✓\033[0m %s\n' "$*"; }
  no() { printf '\033[31m✗\033[0m %s\n' "$*" >&2; }
else
  ok() { printf '✓ %s\n' "$*"; }
  no() { printf '✗ %s\n' "$*" >&2; }
fi
say() { printf '%s\n' "$*"; }

# Linux: Rex's server as a systemd user service, on your tailnet as its own HTTPS device when
# Tailscale is here, so Rex on your other devices can follow it. Only when no server is
# answering; one that already is stays as it is.
rex_service() {
  "$REX" --autostart=false ls >/dev/null 2>&1 && return 0
  command -v systemctl >/dev/null 2>&1 || return 0
  unit=${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/rex-server.service
  listen=""
  if command -v tailscale >/dev/null 2>&1 && tailscale ip -4 >/dev/null 2>&1; then
    listen=" --tailscale --tailscale-hostname $(hostname -s 2>/dev/null || echo host)-rex"
  fi
  if [ ! -f "$unit" ]; then
    mkdir -p "$(dirname "$unit")"
    cat > "$unit" <<UNIT
[Unit]
Description=Rex server
After=network-online.target tailscaled.service

[Service]
ExecStart=$REX server run --log-format text$listen
Restart=on-failure
RestartSec=5s

[Install]
WantedBy=default.target
UNIT
    systemctl --user daemon-reload
  fi
  systemctl --user enable --now rex-server.service >/dev/null 2>&1 || true
  i=0; while ! "$REX" --autostart=false ls >/dev/null 2>&1; do i=$((i+1)); [ $i -gt 15 ] && break; sleep 1; done
  # A user service stops at logout unless the account is allowed to linger.
  if command -v loginctl >/dev/null 2>&1 \
    && [ "$(loginctl show-user "$(id -un)" -p Linger --value 2>/dev/null)" != yes ]; then
    say "  Rex stops when you log out. To keep it running: sudo loginctl enable-linger $(id -un)"
  fi
}

if [ "$OS" = Darwin ]; then
  CURL=/usr/bin/curl
  REX_APP=${TR3X_REX_APP:-"/Applications/Rex Beta.app"}
  REX="$REX_APP/Contents/Helpers/Rex Server.rex/Contents/MacOS/rex"
  T3_APP=${TR3X_T3_APP:-}
  if [ -z "$T3_APP" ]; then
    for app in "/Applications/T3 Code.app" "/Applications/T3 Code (Nightly).app" "/Applications/T3 Code (Beta).app"; do
      [ -d "$app" ] && { T3_APP=$app; break; }
    done
  fi
else
  CURL=curl T3_APP=
  REX=${TR3X_REX:-$(command -v rex || true)}
fi

# Rex: installed, and its server answering.
if [ -z "$REX" ] || [ ! -x "$REX" ]; then
  no "Rex isn't installed"
  if [ "$OS" = Darwin ]; then
    say "  Get Rex Beta from https://www.superlogical.com/rex, open it once, then run this again."
  else
    say "  Install it with: curl -fsSL https://pkgs.superlogical.com/rex/install.sh | sh"
    say "  (it asks for sudo), then run this again."
  fi
  exit 1
fi
if [ "$OS" = Darwin ]; then
  if ! "$REX" ls >/dev/null 2>&1; then
    open -a "$REX_APP"
    i=0; while ! "$REX" ls >/dev/null 2>&1; do i=$((i+1)); [ $i -gt 30 ] && break; sleep 1; done
  fi
  "$REX" ls >/dev/null 2>&1 || { no "Rex is installed but its server isn't answering"; say "  Open Rex Beta and finish its setup, then run this again."; exit 1; }
else
  # A machine that stays on keeps its Rex server as a service of its own, so it outlives
  # every login and every terminal.
  rex_service
  "$REX" --autostart=false ls >/dev/null 2>&1 || { no "Rex is installed but its server isn't answering"; say "  Try: systemctl --user status rex-server"; exit 1; }
fi
ok "Rex"

# T3 Code: installed, and its server written down in ~/.t3.
RUNTIME=${T3CODE_HOME:-$HOME/.t3}/userdata/server-runtime.json
t3_up() {
  [ -f "$RUNTIME" ] || return 1
  # No python3: a Mac without the Command Line Tools answers it with an install dialog.
  ORIGIN=$(sed -n 's/.*"origin"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$RUNTIME" | head -1)
  [ -n "$ORIGIN" ] || return 1
  $CURL -fsS --max-time 3 -o /dev/null "$ORIGIN/" 2>/dev/null
}
if ! t3_up; then
  if [ "$OS" = Linux ]; then
    no "T3's server isn't running"
    say "  Start it and keep it running with: t3 service install"
    say "  (or open the T3 Code app), then run this again."
    exit 1
  fi
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
if [ -z "${TR3X_URL:-}" ] && [ -f "$HERE/tr3x/Cargo.toml" ] && command -v cargo >/dev/null 2>&1; then
  say "Building tr3x (a minute or two the first time)…"
  cargo build --release --locked -p tr3x --manifest-path "$HERE/Cargo.toml"
  BIN="$HERE/target/release/tr3x"
else
  URL=${TR3X_URL:-https://github.com/$REPO/releases/latest/download/$ASSET}
  SUM=${TR3X_SHA256:-}
  if [ -z "${TR3X_URL:-}" ] && [ -z "$SUM" ]; then
    $CURL -fsSL -o "$WORK/sum" "$URL.sha256" || {
      no "Couldn't download tr3x from github.com/$REPO"
      say "  Check your connection, or run this from a clone with Rust installed (https://rustup.rs)."
      exit 1
    }
    SUM=$(cut -d' ' -f1 < "$WORK/sum")
  fi
  say "Downloading tr3x…"
  $CURL -fL --progress-bar -o "$WORK/tr3x.tar.gz" "$URL" || { no "Couldn't download $URL"; exit 1; }
  if [ -n "$SUM" ]; then
    if command -v sha256sum >/dev/null 2>&1; then
      GOT=$(sha256sum "$WORK/tr3x.tar.gz" | cut -d' ' -f1)
    else
      GOT=$(shasum -a 256 "$WORK/tr3x.tar.gz" | cut -d' ' -f1)
    fi
    [ "$GOT" = "$SUM" ] || { no "The download's SHA-256 doesn't match"; say "  Expected $SUM, got $GOT."; exit 1; }
  fi
  mkdir "$WORK/x" && tar -xzf "$WORK/tr3x.tar.gz" -C "$WORK/x"
  BIN=$(find "$WORK/x" -type f -name tr3x -perm -u+x | head -1)
  # A download can be quarantined; this one was asked for.
  if [ "$OS" = Darwin ] && [ -n "$BIN" ]; then xattr -d com.apple.quarantine "$BIN" 2>/dev/null || true; fi
fi
[ -n "$BIN" ] && [ -x "$BIN" ] || { no "Couldn't find tr3x in the download"; exit 1; }
ok "tr3x $("$BIN" --version | cut -d' ' -f2)"

# Copies itself to ~/.local/bin when it runs from a build or a download.
# Piped into sh, stdin is this script: install asks about other machines on the terminal.
if [ -r /dev/tty ] && [ -t 1 ]; then
  TR3X_REX="$REX" "$BIN" install </dev/tty
else
  TR3X_REX="$REX" "$BIN" install
fi

# A machine that stays on is the one other devices follow. Say how, once: Rex's own QR code
# for its tailnet address. The first time, Rex's tailnet device needs approving in a browser;
# its HTTPS name appears a minute after.
if [ "$OS" = Linux ] && "$REX" --autostart=false ls >/dev/null 2>&1; then
  i=0; URL=""; AUTH=""
  while [ $i -lt 12 ]; do
    URL=$("$REX" --autostart=false server tailscale pair --json 2>/dev/null | sed -n 's/^ *"url": *"\(https:[^"]*\)".*$/\1/p' | head -1)
    [ -n "$URL" ] && break
    STATUS=$("$REX" --autostart=false server status 2>/dev/null || true)
    # A Rex from before `server tailscale pair` lists the address in its status.
    URL=$(printf '%s\n' "$STATUS" | sed -n 's/^ *https: \(https:\/\/.*\)$/\1/p' | head -1)
    [ -n "$URL" ] && break
    AUTH=$(printf '%s\n' "$STATUS" | sed -n 's/^ *authenticate: //p' | head -1)
    [ -n "$AUTH" ] && break
    printf '%s\n' "$STATUS" | grep -q '^tailscale:' || break
    i=$((i+1)); sleep 2
  done
  if [ -n "$AUTH" ]; then
    say ""
    say "Approve this machine on your tailnet so Rex on your other devices can follow it:"
    say "  $AUTH"
    say "then run \`rex server tailscale pair\` and scan its code with Rex on each device."
  elif [ -n "$URL" ]; then
    say ""
    say "To follow this machine from Rex on another device, scan this with Pair via QR Code:"
    "$REX" --autostart=false server tailscale pair 2>/dev/null || say "  $URL"
    say "Or on a computer: rex hosts add $(hostname -s 2>/dev/null || echo host) $URL"
  fi
fi

