#!/bin/sh
# get-salt.sh — install Salt from the mirror in one step.
#
#   curl -fsSL https://raw.githubusercontent.com/Glockerz/Salt/main/mirror/get-salt.sh | sh
#   wget -qO- https://raw.githubusercontent.com/Glockerz/Salt/main/mirror/get-salt.sh | sh
#
# On a fresh Void Linux install, which has neither curl nor wget:
#
#   xbps-fetch -o get-salt.sh https://raw.githubusercontent.com/Glockerz/Salt/main/mirror/get-salt.sh
#   sh get-salt.sh
#
# It looks up the current payload URL in mirror/state.json on the mirror-state
# branch (the URL rotates on every update), downloads the payload, lets it
# verify its own SHA-256 and extract, then runs the bundled install.sh.
# Pass a payload URL as the first argument to skip the lookup:
#
#   sh get-salt.sh https://paste.c-net.org/...
#
# Plain POSIX sh, so it runs the same whatever your login shell is.
set -eu

STATE_URL="https://raw.githubusercontent.com/Glockerz/Salt/mirror-state/mirror/state.json"

# fetch URL [FILE]: download with curl, or wget, or xbps-fetch (the only one
# a fresh Void Linux install has); to stdout without FILE.
if command -v curl >/dev/null 2>&1; then
    fetch() { curl -fsSL "$1" -o "${2:--}"; }
elif command -v wget >/dev/null 2>&1; then
    fetch() { wget -qO "${2:--}" "$1"; }
elif command -v xbps-fetch >/dev/null 2>&1; then
    # xbps-fetch can't write to stdout ("-o -" makes a file named "-"), and
    # prints its progress there.
    fetch() {
        if [ -n "${2:-}" ]; then
            xbps-fetch -o "$2" "$1" >/dev/null
        else
            _t=$(mktemp "${TMPDIR:-/tmp}/get-salt-fetch.XXXXXX")
            xbps-fetch -o "$_t" "$1" >/dev/null && cat "$_t"
            _rc=$?
            rm -f "$_t"
            return "$_rc"
        fi
    }
else
    echo "GET_SALT_FAIL=need curl, wget or xbps-fetch" >&2
    exit 1
fi

url=${1:-}
if [ -z "$url" ]; then
    # Payloads are numbered salt-001.txt, salt-002.txt, ...; take the newest
    # one that passed its end-to-end check.
    url=$(fetch "$STATE_URL" \
        | awk '$1 == "RESULT" && $2 ~ /^salt-[0-9]+\.txt$/ && / E2E=PASS/ {
                   n = $2; gsub(/[^0-9]/, "", n); n += 0
                   for (i = 3; i <= NF; i++) if ($i ~ /^url=/ && n >= best) { best = n; u = substr($i, 5) }
               } END { print u }')
fi
[ -n "$url" ] || { echo "GET_SALT_FAIL=no-mirror-url (read it from $STATE_URL)" >&2; exit 1; }
echo "GET_SALT=mirror $url"

WORK=$(mktemp -d "${TMPDIR:-/tmp}/get-salt.XXXXXX")
trap 'rm -rf "$WORK"' EXIT INT TERM

fetch "$url" "$WORK/payload.txt"
sh "$WORK/payload.txt" "$WORK/salt"
sh "$WORK/salt/install.sh"
