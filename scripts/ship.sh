#!/usr/bin/env bash
# Build, re-sign and install CXTasks into /Applications, then restart what runs
# from it. The steps are CLAUDE.md § "Shipping a build"; this file is that
# section made executable, so every check there is a hard failure here.
#
#   scripts/ship.sh            # build + install + relaunch
#   scripts/ship.sh --no-open  # install, don't relaunch the app
#
# Not covered: com.cxtasks.app.server runs target/release/cxtasks-server, which
# lives outside the bundle — rebuild + re-sign + kickstart it separately when
# the server changed (gotcha #43).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BUNDLE="$ROOT/target/release/bundle/macos/CXTasks.app"
DEST="/Applications/CXTasks.app"
IDENTITY="Developer ID Application: Christopher Robinson (CCYV5HQZCM)"
OPEN=1
[[ "${1:-}" == "--no-open" ]] && OPEN=0

step() { printf '\n==> %s\n' "$*"; }
die() { printf 'ship: %s\n' "$*" >&2; exit 1; }

cd "$ROOT"
source "$HOME/.cargo/env"

step "Stopping any tauri dev (two instances on one DB deadlock — gotcha #12)"
pkill -f "target/debug/cxtasks" || true
pkill -f "tauri dev" || true

step "Building (npm run tauri build)"
# Don't pipe through tee/grep: under pipefail that misreports (gotcha #37).
LOG="$(mktemp -t cxtasks-ship)"
if ! npm run tauri build >"$LOG" 2>&1; then
  tail -40 "$LOG"
  die "build failed — full log: $LOG"
fi
out="$(cat "$LOG")"
[[ "$out" == *"Finished 2 bundles"* ]] || die "build exited 0 but never said 'Finished 2 bundles' — log: $LOG"

step "Checking the bundle carries all three binaries (gotcha #5)"
for bin in cxtasks cxtasks-mcp cxtasks-runner; do
  [[ -x "$BUNDLE/Contents/MacOS/$bin" ]] \
    || die "$bin missing from the bundle — the helper STAGING failed (scripts/build-helpers.sh); do not copy it in by hand"
done

step "Re-signing to strip hardened runtime (gotcha #25)"
codesign --force --deep --sign "$IDENTITY" "$BUNDLE"
flags="$(codesign -dv --verbose=2 "$BUNDLE" 2>&1)"
[[ "$flags" == *"flags=0x0(none)"* ]] || die "signature flags are not 0x0(none) — Ghostty dispatch would die silently"

step "Quitting the running app"
osascript -e 'quit app "CXTasks"' >/dev/null 2>&1 || true
for _ in $(seq 1 20); do
  pgrep -f "$DEST/Contents/MacOS/cxtasks$" >/dev/null || break
  sleep 0.5
done
pgrep -f "$DEST/Contents/MacOS/cxtasks$" >/dev/null && die "CXTasks did not quit — close it and re-run"

step "Installing into /Applications"
rm -rf "$DEST"
cp -R "$BUNDLE" /Applications/
installed="$(codesign -dv --verbose=2 "$DEST" 2>&1)"
[[ "$installed" == *"flags=0x0(none)"* ]] || die "installed copy lost its 0x0 flags"

step "Restarting the runner on the new binary"
if launchctl print "gui/$(id -u)/com.cxtasks.app.runner" >/dev/null 2>&1; then
  launchctl kickstart -k "gui/$(id -u)/com.cxtasks.app.runner"
else
  echo "runner LaunchAgent not loaded — skipped"
fi

if (( OPEN )); then
  step "Relaunching CXTasks"
  open -a "$DEST"
fi

step "Runner health"
"$DEST/Contents/MacOS/cxtasks-runner" --health || die "runner health check failed"

printf '\nShipped %s (%s).\n' "$(git rev-parse --short HEAD)" \
  "$(git diff --quiet HEAD && echo clean tree || echo 'UNCOMMITTED changes included')"
echo "Open Claude sessions keep the previous cxtasks-mcp until they restart (gotcha #3)."
