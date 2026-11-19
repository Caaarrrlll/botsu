#!/bin/sh
# Starts as root only long enough to create the data directory and give it to PUID:PGID
# (Unraid's nobody:users, 99:100, by default), then runs the bot as that user. So a fresh
# host folder (Unraid creates appdata paths as root) works without any manual chown.
# Started with --user instead? Then nothing is changed and the bot just runs.
set -eu

DATA="${BOTSU_DATA_DIR:-/data}"

if [ "$(id -u)" = "0" ]; then
    PUID="${PUID:-99}"
    PGID="${PGID:-100}"
    mkdir -p "$DATA"
    chown "$PUID:$PGID" "$DATA"
    # Files copied in by hand (for example an older botsu.db) must be writable too.
    find "$DATA" -maxdepth 1 -type f ! -user "$PUID" -exec chown "$PUID:$PGID" {} +
    exec setpriv --reuid="$PUID" --regid="$PGID" --clear-groups -- /usr/local/bin/botsu "$@"
fi

exec /usr/local/bin/botsu "$@"
