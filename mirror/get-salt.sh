#!/bin/sh
# get-salt.sh — install Salt from the mirror in one step.
#
#   curl -fsSL https://raw.githubusercontent.com/Glockerz/Salt/main/mirror/get-salt.sh | sh
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

command -v curl >/dev/null 2>&1 || { echo "GET_SALT_FAIL=curl-not-found" >&2; exit 1; }

url=${1:-}
if [ -z "$url" ]; then
    # Payloads are numbered salt-001.txt, salt-002.txt, ...; take the newest
    # one that passed its end-to-end check.
    url=$(curl -fsSL "$STATE_URL" \
        | awk '$1 == "RESULT" && $2 ~ /^salt-[0-9]+\.txt$/ && / E2E=PASS/ {
                   n = $2; gsub(/[^0-9]/, "", n); n += 0
                   for (i = 3; i <= NF; i++) if ($i ~ /^url=/ && n >= best) { best = n; u = substr($i, 5) }
               } END { print u }')
fi
[ -n "$url" ] || { echo "GET_SALT_FAIL=no-mirror-url (read it from $STATE_URL)" >&2; exit 1; }
echo "GET_SALT=mirror $url"

WORK=$(mktemp -d "${TMPDIR:-/tmp}/get-salt.XXXXXX")
trap 'rm -rf "$WORK"' EXIT INT TERM

curl -fsSL "$url" -o "$WORK/salt-001.txt"
sh "$WORK/salt-001.txt" "$WORK/salt"
sh "$WORK/salt/install.sh"
