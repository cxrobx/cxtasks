#!/bin/bash
# Install the phone's direct Ask endpoint in the logged-in Aqua session.
# The runner still owns execution, grants and budgets; this serves its LOCAL
# replica. The NAS remains canonical and receives changes through normal sync.
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SOURCE="${CXTASKS_SERVER_BIN:-$REPO/target/release/cxtasks-server}"
CONF="$HOME/.config/cxtasks"
BIN="$HOME/.local/libexec/cxtasks-ask-server"
LOG="$HOME/Library/Logs/CXTasks"
LABEL=com.cxtasks.ask-server
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
TS="${CXTASKS_TAILSCALE_BIN:-/Applications/Tailscale.app/Contents/MacOS/Tailscale}"
BOARD="${CXTASKS_ASK_BOARD_TOKEN_FILE:-$CONF/ask-board-token}"
TZ_NAME="${CXTASKS_TZ:-America/New_York}"
[[ -x "$SOURCE" ]] || { echo "Build cxtasks-server --release first" >&2; exit 1; }
[[ -s "$BOARD" ]] || { echo "Install the NAS board-token at $BOARD (0600), file-to-file; never print it." >&2; exit 1; }
mkdir -p "$CONF" "$LOG" "$(dirname "$BIN")" "$(dirname "$PLIST")"
if [[ ! -s "$CONF/ask-full-token" ]]; then
  (umask 077; openssl rand -hex 32 > "$CONF/ask-full-token")
fi
chmod 600 "$CONF/ask-full-token"
# Stage and sign before replacing the executable. Cargo's linker-only ad-hoc
# signature is insufficient when launchd starts the binary (gotcha 43).
cp "$SOURCE" "$BIN.new"
codesign --force --sign "Developer ID Application: Christopher Robinson (CCYV5HQZCM)" "$BIN.new"
codesign --verify "$BIN.new"
mv "$BIN.new" "$BIN"
python3 - "$PLIST" "$BIN" "$CONF" "$BOARD" "$LOG" "$TZ_NAME" <<'PY'
import plistlib,sys
path,binary,conf,board,logs,tz=sys.argv[1:]
with open(path, 'wb') as f:
    plistlib.dump(dict(Label='com.cxtasks.ask-server', ProgramArguments=[binary],
        EnvironmentVariables=dict(TZ=tz, CXTASKS_BIND='127.0.0.1:8788',
          CXTASKS_ROLE='local', CXTASKS_TOKEN_FILE=conf+'/ask-full-token',
          CXTASKS_BOARD_TOKEN_FILE=board, RUST_LOG='info'),
        RunAtLoad=True, KeepAlive=True, ThrottleInterval=10,
        LimitLoadToSessionType='Aqua',
        StandardOutPath=logs+'/ask-server.log', StandardErrorPath=logs+'/ask-server.err.log'), f)
PY
plutil -lint "$PLIST" >/dev/null
launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
launchctl bootstrap "gui/$(id -u)" "$PLIST"
for attempt in {1..10}; do
  if curl -fsS --max-time 2 http://127.0.0.1:8788/health >/dev/null 2>&1; then break; fi
  sleep 1
done
curl -fsS --max-time 5 http://127.0.0.1:8788/health >/dev/null
"$TS" serve --bg --https=8788 8788
curl -fsS --max-time 10 https://cxs-mac-mini.tailbff47a.ts.net:8788/health >/dev/null
printf '%s\n' 'Ask endpoint ready: https://cxs-mac-mini.tailbff47a.ts.net:8788'
