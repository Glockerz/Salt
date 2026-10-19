#!/bin/sh
# ---------------------------------------------------------------------------
# make_payload.sh — build a self-extracting mirror/files payload
# ---------------------------------------------------------------------------
# Produces the same payload shape the proven mirror uses:
#   <slug>-<NNN>.txt = POSIX sh stub + one line of base64(gzip(tar)).
#
# The stub:
#   - verifies sha256(archive) before writing anything,
#   - requires exactly one absolute destination and refuses an existing dir,
#   - extracts, then verifies sha256 of the sorted extracted file list,
#   - prints ARCHIVE_SHA256_GOT / PAYLOAD_SHA256_GOT / <TOKEN>_DONE=YES
#     plus an inverse line, and exits non-zero on every failure.
#
# Usage:
#   tools/make_payload.sh [--manifest FILE] [--out-dir DIR] <slug> <NNN>
#
# Manifest (default: mirror/payload.include) is one entry per line relative to
# the repository root; '#' starts a comment and blank lines are ignored.
#   src                      copy src into the payload at the same path
#   src => dest              copy src into the payload at dest
#
# Examples:
#   tools/make_payload.sh app 001
#   tools/make_payload.sh toolchain 007 --out-dir /tmp/payloads
#
# Payload numbers are never reused. Ship changes as a new number; the courier
# publishes every file under mirror/files/ on push.
# ---------------------------------------------------------------------------
set -eu

# macOS bsdtar can add AppleDouble ._ files for extended attributes; that would
# make the extracted file list differ from the staged one. Ask tar not to.
COPYFILE_DISABLE=1
export COPYFILE_DISABLE

usage() {
    cat <<'EOF'
usage: tools/make_payload.sh [--manifest FILE] [--out-dir DIR] <slug> <NNN>

  --manifest FILE   payload include list (default: mirror/payload.include)
  --out-dir DIR     output directory (default: mirror/files)
  -h, --help        show this help

Manifest entries are paths relative to the repo root:
  path                  include as-is
  path => dest          include at dest inside the payload
EOF
}

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
MANIFEST="$ROOT/mirror/payload.include"
OUT_DIR="$ROOT/mirror/files"
SLUG=""
NNN=""

while [ $# -gt 0 ]; do
    case "$1" in
        --manifest)
            [ $# -ge 2 ] || { echo "PACK_FAIL=missing-argument:--manifest" >&2; exit 2; }
            MANIFEST=$2; shift 2 ;;
        --out-dir)
            [ $# -ge 2 ] || { echo "PACK_FAIL=missing-argument:--out-dir" >&2; exit 2; }
            OUT_DIR=$2; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        --*) echo "PACK_FAIL=unknown-option:$1" >&2; exit 2 ;;
        *)
            if [ -z "$SLUG" ]; then SLUG=$1
            elif [ -z "$NNN" ]; then NNN=$1
            else echo "PACK_FAIL=unexpected-argument:$1" >&2; exit 2
            fi
            shift ;;
    esac
done

[ -n "$SLUG" ] && [ -n "$NNN" ] || { usage >&2; exit 2; }
case "$SLUG" in
    ''|*[!A-Za-z0-9._-]*|[._-]*) echo "PACK_FAIL=bad-slug:$SLUG (use letters, digits, dot, dash, underscore; not leading punctuation)" >&2; exit 2 ;;
esac
case "$NNN" in
    [0-9][0-9][0-9]) ;;
    *) echo "PACK_FAIL=bad-number:$NNN (must be exactly three digits)" >&2; exit 2 ;;
esac

case "$MANIFEST" in /*) ;; *) MANIFEST="$ROOT/$MANIFEST" ;; esac
case "$OUT_DIR" in /*) ;; *) OUT_DIR="$ROOT/$OUT_DIR" ;; esac

[ -f "$MANIFEST" ] || { echo "PACK_FAIL=no-manifest:$MANIFEST" >&2; exit 1; }

for c in base64 tar gzip mktemp find sort sed wc cut; do
    command -v "$c" >/dev/null 2>&1 || { echo "PACK_FAIL=missing-tool:$c" >&2; exit 1; }
done
if ! command -v sha256sum >/dev/null 2>&1 && ! command -v shasum >/dev/null 2>&1; then
    echo "PACK_FAIL=missing-tool:sha256sum-or-shasum" >&2
    exit 1
fi

hash_of() {
    if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
    else shasum -a 256 "$1" | cut -d' ' -f1; fi
}
hash_stream() {
    if command -v sha256sum >/dev/null 2>&1; then sha256sum | cut -d' ' -f1
    else shasum -a 256 | cut -d' ' -f1; fi
}

NAME="$SLUG-$NNN"
TOKEN=$(printf '%s' "$SLUG" | tr '[:lower:]' '[:upper:]' | tr -c 'A-Z0-9' '_')_$NNN
OUT="$OUT_DIR/$NAME.txt"
if [ -e "$OUT" ]; then
    echo "PACK_FAIL=payload-exists:$OUT (never reuse a payload number; ship a new NNN)" >&2
    exit 1
fi

WORK=$(mktemp -d "${TMPDIR:-/tmp}/mirror-payload.XXXXXX") || { echo "PACK_FAIL=mktemp" >&2; exit 1; }
trap 'rm -rf "$WORK"' EXIT INT TERM
STAGE="$WORK/stage"
mkdir -p "$STAGE"

ENTRIES=0
while IFS= read -r raw || [ -n "$raw" ]; do
    line=$(printf '%s' "$raw" | sed 's/^[[:space:]]*//; s/[[:space:]]*$//')
    case "$line" in ''|'#'*) continue ;; esac
    case "$line" in
        *" => "*) SRC=${line%% => *}; DEST=${line#* => } ;;
        *) SRC=$line; DEST=$line ;;
    esac
    SRC=$(printf '%s' "$SRC" | sed 's/^[[:space:]]*//; s/[[:space:]]*$//')
    DEST=$(printf '%s' "$DEST" | sed 's/^[[:space:]]*//; s/[[:space:]]*$//')
    [ -n "$SRC" ] && [ -n "$DEST" ] || { echo "PACK_FAIL=bad-manifest-line:$raw" >&2; exit 1; }
    case "$SRC" in /*|-*) echo "PACK_FAIL=bad-source:$SRC (relative, no leading dash)" >&2; exit 1 ;; esac
    case "$DEST" in /*|-*) echo "PACK_FAIL=bad-destination:$DEST (relative, no leading dash)" >&2; exit 1 ;; esac
    case "/$SRC/" in */../*) echo "PACK_FAIL=bad-source:$SRC (no .. segments)" >&2; exit 1 ;; esac
    case "/$DEST/" in */../*) echo "PACK_FAIL=bad-destination:$DEST (no .. segments)" >&2; exit 1 ;; esac

    if [ -d "$ROOT/$SRC" ]; then
        mkdir -p "$STAGE/$DEST" || { echo "PACK_FAIL=mkdir:$DEST" >&2; exit 1; }
        cp -a "$ROOT/$SRC/." "$STAGE/$DEST/" || { echo "PACK_FAIL=copy:$SRC" >&2; exit 1; }
    elif [ -f "$ROOT/$SRC" ]; then
        mkdir -p "$STAGE/$(dirname "$DEST")" || { echo "PACK_FAIL=mkdir:$DEST" >&2; exit 1; }
        cp -a "$ROOT/$SRC" "$STAGE/$DEST" || { echo "PACK_FAIL=copy:$SRC" >&2; exit 1; }
    else
        echo "PACK_FAIL=missing-source:$SRC (from $MANIFEST)" >&2
        exit 1
    fi
    ENTRIES=$((ENTRIES + 1))
done < "$MANIFEST"

[ "$ENTRIES" -gt 0 ] || { echo "PACK_FAIL=empty-manifest:$MANIFEST" >&2; exit 1; }

FILES=$(cd "$STAGE" && find . -type f | wc -l | tr -d ' ')
[ "$FILES" -gt 0 ] || { echo "PACK_FAIL=no-files-staged" >&2; exit 1; }
# File-list hash: sorted, C locale, same recipe the stub re-computes after
# extraction. LC_ALL=C keeps the ordering stable across machines.
PAYLOAD_SHA256=$(cd "$STAGE" && find . -type f | LC_ALL=C sort | hash_stream)

TARBALL="$WORK/payload.tar.gz"
tar -C "$STAGE" -czf "$TARBALL" . || { echo "PACK_FAIL=tar" >&2; exit 1; }
ARCHIVE_SHA256=$(hash_of "$TARBALL")
SOURCE_COMMIT=$(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)

mkdir -p "$OUT_DIR" || { echo "PACK_FAIL=mkdir:$OUT_DIR" >&2; exit 1; }
TMP_OUT="$WORK/$NAME.txt"
{
    sed -e "s|@@NAME@@|$NAME|g" \
        -e "s|@@TOKEN@@|$TOKEN|g" \
        -e "s|@@SOURCE_COMMIT@@|$SOURCE_COMMIT|g" \
        -e "s|@@ARCHIVE_SHA256@@|$ARCHIVE_SHA256|g" \
        -e "s|@@FILES@@|$FILES|g" \
        -e "s|@@PAYLOAD_SHA256@@|$PAYLOAD_SHA256|g" <<'STUB'
#!/bin/sh
# @@NAME@@ — self-extracting mirror payload
# Source commit: @@SOURCE_COMMIT@@
# Packed by tools/make_payload.sh — do not hand-edit; ship a new number instead.
set -u
NAME=@@NAME@@
SOURCE_COMMIT=@@SOURCE_COMMIT@@
ARCHIVE_SHA256=@@ARCHIVE_SHA256@@
FILES=@@FILES@@
PAYLOAD_SHA256=@@PAYLOAD_SHA256@@
[ -f "$0" ] || { echo "@@TOKEN@@_FAIL=run-from-file"; exit 1; }
[ $# -eq 1 ] || { echo "@@TOKEN@@_FAIL=usage: sh /path/to/@@NAME@@.txt /absolute/dir"; exit 1; }
DEST=$1
case "$DEST" in /*|[A-Za-z]:/*) ;; *) echo "@@TOKEN@@_FAIL=absolute required"; exit 1 ;; esac
if [ -e "$DEST" ]; then echo "@@TOKEN@@_FAIL=target-already-exists:$DEST — run: rm -rf $DEST"; exit 1; fi
for c in base64 tar mktemp find sort; do
    command -v "$c" >/dev/null 2>&1 || { echo "@@TOKEN@@_FAIL=missing:$c"; exit 1; }
done
command -v sha256sum >/dev/null 2>&1 || command -v shasum >/dev/null 2>&1 || { echo "@@TOKEN@@_FAIL=missing:sha256sum-or-shasum"; exit 1; }
hash_of() {
    if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
    else shasum -a 256 "$1" | cut -d' ' -f1; fi
}
hash_stream() {
    if command -v sha256sum >/dev/null 2>&1; then sha256sum | cut -d' ' -f1
    else shasum -a 256 | cut -d' ' -f1; fi
}
TMP=$(mktemp -d) || { echo "@@TOKEN@@_FAIL=mktemp"; exit 1; }
trap 'rm -rf "$TMP"' EXIT INT TERM
sed -n '/^__@@TOKEN@@_PAYLOAD__$/,$p' "$0" | tail -n +2 > "$TMP/b64.txt"
if ! base64 -d "$TMP/b64.txt" > "$TMP/src.tar.gz" 2>/dev/null; then
    base64 -D "$TMP/b64.txt" > "$TMP/src.tar.gz" 2>/dev/null || { echo "@@TOKEN@@_FAIL=base64"; exit 1; }
fi
GOT=$(hash_of "$TMP/src.tar.gz")
echo "ARCHIVE_SHA256_GOT=$GOT"
if [ "$GOT" != "$ARCHIVE_SHA256" ]; then echo "@@TOKEN@@_FAIL=archive-sha want=$ARCHIVE_SHA256 got=$GOT"; exit 1; fi
mkdir -p "$DEST" || { echo "@@TOKEN@@_FAIL=mkdir"; exit 1; }
tar -xzf "$TMP/src.tar.gz" -C "$DEST" || { echo "@@TOKEN@@_FAIL=tar"; exit 1; }
GOT2=$(cd "$DEST" && find . -type f | LC_ALL=C sort | hash_stream)
echo "PAYLOAD_SHA256_GOT=$GOT2"
if [ "$GOT2" != "$PAYLOAD_SHA256" ]; then echo "@@TOKEN@@_FAIL=payload-sha want=$PAYLOAD_SHA256 got=$GOT2"; exit 1; fi
echo "@@TOKEN@@_DONE=YES"
echo "FILES=$FILES DEST=$DEST"
echo "--- inverse ---"
echo "ARCHIVE_SHA256_WANT=$ARCHIVE_SHA256 ARCHIVE_SHA256_GOT=$GOT PAYLOAD_SHA256_WANT=$PAYLOAD_SHA256 PAYLOAD_SHA256_GOT=$GOT2 FILES=$FILES DEST=$DEST"
exit 0
__@@TOKEN@@_PAYLOAD__
STUB
    # Stream the base64 body instead of holding a multi-megabyte string in a
    # shell variable; the stub expects exactly one long line after the marker.
    base64 < "$TARBALL" | tr -d '\n'
    printf '\n'
} > "$TMP_OUT" || { echo "PACK_FAIL=write:$TMP_OUT" >&2; exit 1; }

# ---- round-trip proof: execute the payload before publishing it ------------
# The payload is still in $WORK; mirror/files/ only receives a file that has
# already extracted and passed both sha gates.
VERIFY="$WORK/verify"
if ! sh "$TMP_OUT" "$VERIFY" > "$WORK/verify.log" 2>&1 || ! grep -q "^${TOKEN}_DONE=YES$" "$WORK/verify.log"; then
    sed 's/^/  | /' "$WORK/verify.log" >&2
    echo "PACK_FAIL=round-trip (nothing written to $OUT_DIR)" >&2
    exit 1
fi
EXTRACTED=$(find "$VERIFY" -type f | wc -l | tr -d ' ')
[ "$EXTRACTED" = "$FILES" ] || {
    echo "PACK_FAIL=round-trip-file-count want=$FILES got=$EXTRACTED (nothing written to $OUT_DIR)" >&2
    exit 1
}
mv "$TMP_OUT" "$OUT" || { echo "PACK_FAIL=publish:$OUT" >&2; exit 1; }

echo "PACKED=$OUT"
echo "NAME=$NAME"
echo "TOKEN=$TOKEN"
echo "FILES=$FILES"
echo "ARCHIVE_SHA256=$ARCHIVE_SHA256"
echo "PAYLOAD_SHA256=$PAYLOAD_SHA256"
echo "ROUND_TRIP=PASS"
