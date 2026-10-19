#!/bin/sh
# Frozen mirror function: verify by default (exit 1 on drift or missing file); --repin re-records.
set -u
cd "$(dirname "$0")/.." || exit 1
MANIFEST="docs/frozen-tools.sha256"
MODE="${1:-}"
FROZEN=".github/workflows/mirror-upload.yml
scripts/frozen-tools.sh"
hash_of() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" 2>/dev/null | cut -d' ' -f1
  else shasum -a 256 "$1" 2>/dev/null | cut -d' ' -f1; fi
}
if [ "$MODE" = "--repin" ]; then
  : > "$MANIFEST"
  printf '%s\n' "$FROZEN" | while IFS= read -r f; do
    [ -n "$f" ] || continue
    [ -f "$f" ] || { echo "REPIN_FAIL=missing-$f"; exit 1; }
    echo "$(hash_of "$f")  $f" >> "$MANIFEST"
  done
  echo "FROZEN_REPIN=OK ($MANIFEST)"; cat "$MANIFEST"
  echo "Commit the manifest WITH the function change and state the reason in the message."
  exit 0
fi
[ -f "$MANIFEST" ] || { echo "VERIFY_FAIL=no-$MANIFEST"; exit 1; }
DRIFT=0
printf '%s\n' "$FROZEN" | while IFS= read -r f; do
  [ -n "$f" ] || continue
  want=$(awk -v p="$f" '$2 == p {print $1}' "$MANIFEST")
  if [ ! -f "$f" ]; then
    echo "FROZEN=$f ABSENT want=$want"; echo DRIFT >> /tmp/frozen.drift.$$; continue
  fi
  got=$(hash_of "$f")
  if [ "$got" = "$want" ]; then echo "FROZEN=$f OK"
  else echo "FROZEN=$f DRIFT got=$got want=$want"; echo DRIFT >> /tmp/frozen.drift.$$; fi
done
if [ -f /tmp/frozen.drift.$$ ]; then DRIFT=1; rm -f /tmp/frozen.drift.$$; fi
if [ "$DRIFT" = "1" ]; then
  echo "VERIFY_FAIL=function-drift - payload belongs in mirror/files/; function change needs human yes + re-proof + repin"
  exit 1
fi
echo "VERIFY_PASS=function-unchanged (payload files are free to change)"
