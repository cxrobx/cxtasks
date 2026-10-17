#!/bin/sh
set -eu
cx_herdr_source_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
cx_herdr_destination="$HOME/.local/bin/cx-open-in-herdr"
mkdir -p "$HOME/.local/bin"
cp "$cx_herdr_source_dir/herdr_open.py" "$cx_herdr_destination.staging"
chmod 755 "$cx_herdr_destination.staging"
mv -f "$cx_herdr_destination.staging" "$cx_herdr_destination"
printf 'Installed %s\n' "$cx_herdr_destination"
