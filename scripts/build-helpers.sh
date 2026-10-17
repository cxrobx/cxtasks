#!/usr/bin/env bash
# Build cxtasks-mcp / cxtasks-runner and stage them where Tauri's `externalBin`
# expects to find them.
#
# WHY THIS EXISTS
#
# Tauri CLI 2.10.1 used to bundle these two into CXTasks.app by itself, purely
# because they were extra `[[bin]]` targets in the same package as the app
# (gotcha #5 — undocumented, never requested). Moving them to `cxtasks-core` so
# they could build for Linux silently stopped that: the bundle came out with
# `cxtasks` alone and nothing said a word. `externalBin` is the supported
# mechanism and does not depend on the two crates staying married.
#
# `externalBin` resolves each entry as `<path>-<target-triple>`, and Tauri
# strips the triple back off when it copies into Contents/MacOS/.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"

TRIPLE="$(rustc -vV | awk '/^host:/{print $2}')"
OUT="$REPO/src-tauri/binaries"
mkdir -p "$OUT"

cargo build --release -p cxtasks-core --bin cxtasks-mcp --bin cxtasks-runner

for b in cxtasks-mcp cxtasks-runner; do
  src="$REPO/target/release/$b"
  [ -f "$src" ] || { echo "missing $src" >&2; exit 1; }

  # Guard gotcha #35 rather than trusting it stays fixed. These binaries are
  # clean today only because `cxtasks-core` has no tauri-build; if anything ever
  # gives it one, an embedded __info_plist makes any notification abort the
  # process, and the bundle would ship looking perfectly fine.
  if otool -l "$src" | grep -q __info_plist; then
    echo "REFUSING: $b carries an embedded __info_plist (gotcha #35)" >&2
    exit 1
  fi

  cp -f "$src" "$OUT/$b-$TRIPLE"
done

echo "staged cxtasks-mcp / cxtasks-runner for $TRIPLE"
