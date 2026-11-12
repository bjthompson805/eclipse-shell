#!/bin/sh
# Runs eclipse-shell in place of Ryoku's bar and frame. Ctrl+C (or closing the
# terminal) stops it and puts Ryoku's shell.json back as it was, so avoid
# changing Ryoku settings while this runs.
set -eu

conf="${XDG_CONFIG_HOME:-$HOME/.config}/ryoku/shell.json"
backup=$(mktemp)
cp -p "$conf" "$backup"
restore() {
    cp -p "$backup" "$conf"
    rm -f "$backup"
}
trap restore EXIT
trap 'exit 130' INT TERM HUP

# Hide the bar and frame, and zero the frame opacity too: with the frame off,
# Ryoku still draws a hairline and corner outline above other layers.
sed -i "" -E 's/"barEnabled": true/"barEnabled": false/; s/"frameEnabled": true/"frameEnabled": false/; s/"frameOpacity": [0-9.]+/"frameOpacity": 0/' "$conf"
cd "$(dirname "$0")"
cargo run --quiet
