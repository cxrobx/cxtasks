#!/usr/bin/env bash
# Install `cxtasks-server` as a user LaunchAgent, bound to this Mac's Tailscale
# address.
#
# Why an agent at all: Phase 1b built the server and nothing ran it. The iOS
# client has no way to be paired without a reachable node, so this is what the
# phone talks to until Phase 2 makes the NAS canonical.
#
# Why the TAILSCALE address and never 0.0.0.0: `/rpc` can call
# `queue_background`, which starts an unattended agent with whatever tool grant
# the task carries, in whatever repo it names. Binding wide has to be a decision
# somebody typed, and this is not that decision. `config.rs` defaults to
# loopback for the same reason.
#
# Idempotent: re-running regenerates nothing and re-bootstraps cleanly.
set -euo pipefail

LABEL=com.cxtasks.app.server
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$REPO/target/release/cxtasks-server"
CONF="$HOME/.config/cxtasks"
LOG="$HOME/Library/Logs/CXTasks"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"

# TZ is required by `config.rs` and deliberately not defaulted there: it decides
# what "today" means, and guessing it silently changes which tasks are due.
TZ_NAME="${CXTASKS_TZ:-America/Chicago}"

TS_BIN=/Applications/Tailscale.app/Contents/MacOS/Tailscale
BIND_IP="${CXTASKS_BIND_IP:-$("$TS_BIN" ip -4 2>/dev/null | head -1)}"
if [ -z "$BIND_IP" ]; then
  echo "error: no Tailscale IPv4 address. Is Tailscale running?" >&2
  exit 1
fi

if [ ! -x "$BIN" ]; then
  echo "error: $BIN not found — build it with" >&2
  echo "  source \"\$HOME/.cargo/env\" && cargo build --release --bin cxtasks-server" >&2
  exit 1
fi

mkdir -p "$CONF" "$LOG" "$HOME/Library/LaunchAgents"

# Two scopes, two files. `auth.rs` refuses a token under 32 chars or one that is
# group/other-readable, so generate at 0600 and never echo the value.
for t in full-token board-token; do
  if [ ! -s "$CONF/$t" ]; then
    (umask 077; openssl rand -hex 32 > "$CONF/$t")
    chmod 600 "$CONF/$t"
    echo "generated $CONF/$t"
  fi
done

cat > "$PLIST" <<PLIST_EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>$LABEL</string>
    <key>ProgramArguments</key>
    <array><string>$BIN</string></array>
    <key>EnvironmentVariables</key>
    <dict>
        <key>TZ</key><string>$TZ_NAME</string>
        <key>CXTASKS_BIND</key><string>$BIND_IP:8787</string>
        <key>CXTASKS_TOKEN_FILE</key><string>$CONF/full-token</string>
        <key>CXTASKS_BOARD_TOKEN_FILE</key><string>$CONF/board-token</string>
        <key>RUST_LOG</key><string>info</string>
    </dict>
    <key>RunAtLoad</key>
    <true/>
    <!-- Tailscale may not be up at login, so a failed bind is expected and has
         to be retried rather than treated as terminal. -->
    <key>KeepAlive</key>
    <dict><key>SuccessfulExit</key><false/></dict>
    <key>ThrottleInterval</key>
    <integer>30</integer>
    <key>StandardOutPath</key><string>$LOG/server.log</string>
    <key>StandardErrorPath</key><string>$LOG/server.err.log</string>
    <key>LimitLoadToSessionType</key><string>Aqua</string>
</dict>
</plist>
PLIST_EOF

plutil -lint "$PLIST" >/dev/null

# 36 = EALREADY (already loaded), which is success — same inherited detail as
# `launchd.rs`'s runner install.
launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
launchctl bootstrap "gui/$(id -u)" "$PLIST" 2>/dev/null || true

sleep 2
if curl -s -m 5 "http://$BIND_IP:8787/health" | grep -q '"ok":true'; then
  echo "cxtasks-server is up on http://$BIND_IP:8787"
  echo "pair the phone with the BOARD token: cat $CONF/board-token | pbcopy"
else
  echo "server did not answer — check $LOG/server.err.log" >&2
  exit 1
fi
