# mirror — payload hosting and delivery

This directory is the payload side of the repository's delivery mirror. The
mechanism is frozen: the courier workflow (`.github/workflows/mirror-upload.yml`)
publishes every file in `mirror/files/` to anonymous paste hosts and records the
live URL in a commit status; `scripts/frozen-tools.sh` refuses any edit to the
courier or to itself.

## Files

- `files/` — payloads. One self-contained file per update, named
  `<slug>-<NNN>.txt` (for example `toolchain-007.txt`). Add a new name for every
  update; never overwrite or reuse a number.
- `payload.include` — what `tools/make_payload.sh` packs into a payload.
- `current-<name>` (optional) — a one-line pointer for humans naming the payload
  currently served for that stream. The authoritative URL is always the current
  `mirror/state.json` on the `mirror-state` branch, or the commit status on the
  head commit; a pointer file never carries a URL.

## What happens on push

1. A push touching `mirror/files/**` starts `.github/workflows/mirror-upload.yml`
   (or run it manually with `workflow_dispatch`).
2. The runner uploads each payload through the host ladder
   `pasters -> dpaste -> cnet -> tmpfiles -> 0x0 -> x0`, two passes per file.
   The first host that returns a URL whose sha256 equals the repo file wins.
3. It re-verifies every URL immediately, again after 40 seconds, and once more
   before reporting. A file that dies is re-uploaded; a file that cannot be
   verified is reported as a failure.
4. Each file gets a commit status `mirror/<file>` with `target_url` set to the
   paste URL. The run also force-pushes `mirror/state.json` to the
   `mirror-state` branch.

## Read the live URL (never from memory or chat history)

```bash
gh run list -w mirror-upload -L 1 --json databaseId,headSha,status,conclusion
gh api "repos/<owner>/<repo>/contents/mirror/state.json?ref=mirror-state" --jq .content | base64 -d
# macOS: replace `base64 -d` with `base64 -D`
gh api "repos/<owner>/<repo>/commits/<head-sha>/statuses" \
  --jq '.[] | select(.context|startswith("mirror/")) | {context,state,description,target_url}'
```

`state.json` is line-oriented:

```
run_id=<id> run_no=<n>
FILE <name> bytes=<n> sha=<full64>
UP <name> host=<h> url=<u>
RECHECK_FAIL <name> host=<h> - re-uploading
RESULT <name> host=<h> url=<u> sha=<first12> E2E=PASS
RESULT <name> E2E=FAIL
STATUS_POST=OK <name>
STATUS_POST=FAIL <name>
```

## Fetch and verify on the target

Ask which OS the target runs before serving a command, then use that row:

Void Linux:

```
xbps-fetch -o /tmp/<short> <url-from-state>
sha256sum /tmp/<short>
sh /tmp/<short> /absolute/extract-dir
```

Windows (cmd; the printed hash must equal the `FILE ... sha=` line before running):

```
curl.exe -fL -o "%USERPROFILE%\<short>" <url-from-state>
certutil -hashfile "%USERPROFILE%\<short>" SHA256
sh ~/<short> /absolute/extract-dir
```

Other POSIX:

```
curl -fL -o /tmp/<short> <url-from-state>
sha256sum /tmp/<short>        # macOS: shasum -a 256 /tmp/<short>
sh /tmp/<short> /absolute/extract-dir
```

Mismatch = stop; quote both hashes. The payload itself re-checks the archive sha
and the extracted file-list sha, refuses an existing destination, and prints
`<TOKEN>_DONE=YES` only after both gates pass.

## Build a payload

```bash
# 1. list what goes in mirror/payload.include, one path per line
#    path              -> same path inside the payload
#    path => dest      -> different path inside the payload
# 2. pack it (new number every time)
tools/make_payload.sh app 001
# 3. commit + push; the courier does the rest
```

The packer round-trips the payload it just wrote before returning `ROUND_TRIP=PASS`.

## Repository settings

- Actions must be enabled.
- `mirror-state` must be an unprotected branch (the workflow force-pushes it).
- The workflow's `permissions:` block supplies `contents: write` and
  `statuses: write`; no personal token is stored.
- Paste-host bytes are public-unlisted. Never put secrets, tokens, personal
  data, or inbox URLs in a payload.
- Keep LF line endings for `scripts/frozen-tools.sh` and the workflow (a Windows
  checkout with `core.autocrlf=true` can break the shell script; use
  `core.autocrlf=input` or a `.gitattributes` entry).
- In a private repository anonymous raw URLs return 404. The paste-host URL is
  the delivery path; do not build one that assumes anonymous raw access.

## Frozen mechanism

Do not edit `.github/workflows/mirror-upload.yml` or `scripts/frozen-tools.sh`
after the first successful receipt. Verify with:

```bash
sh scripts/frozen-tools.sh
```

Payload changes are free: add a new file under `mirror/files/` and push.
