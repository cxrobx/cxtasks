#!/bin/bash
# Copy the CXTasks workflow (`t`) into Alfred's preferences. Alfred does not
# follow symlinks into a workflow folder, so the files are copied; re-run after
# editing them.
#
# Usage: install.sh [<Alfred.alfredpreferences dir>]
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/../.." && pwd)"

prefs="${1:-}"
if [ -z "$prefs" ]; then
  sync="$(defaults read com.runningwithcrayons.Alfred-Preferences syncfolder 2>/dev/null || true)"
  sync="${sync/#\~/$HOME}"
  prefs="${sync:-$HOME/Library/Application Support/Alfred}/Alfred.alfredpreferences"
fi
[ -d "$prefs/workflows" ] || { echo "no Alfred workflows folder at $prefs/workflows" >&2; exit 1; }

# A fixed folder name, so a re-install replaces the workflow instead of adding
# a second `t` keyword beside it.
dest="$prefs/workflows/user.workflow.5A7C2E91-0D3B-4C6F-8E12-C7A5B3D9F104"
mkdir -p "$dest"
cp "$here/info.plist" "$here/cxtasks_search.py" "$dest/"
cp "$repo/src-tauri/icons/128x128@2x.png" "$dest/icon.png"
echo "installed to $dest"
