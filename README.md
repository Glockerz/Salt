# Salt

<img src="data/icons/salt.svg" width="96" align="right" alt="">

Salt runs Roblox Studio on Linux. It is a Rust rewrite of
[Vinegar](https://github.com/vinegarhq/vinegar) by VinegarHQ, including the Go
libraries Vinegar depends on (`sewnie/wine`, `sewnie/rbxbin`, `sewnie/rbxweb`)
and its Vulkan layer. Salt is not affiliated with or endorsed by VinegarHQ.

Salt reads and writes the same `config.toml`, prefix, versions and cache
directories as Vinegar 1.x, so it can be used in place of Vinegar on existing
data. It also uses Vinegar's [Kombucha](https://github.com/vinegarhq/kombucha)
Wine builds.

| Crate | Ports | |
|---|---|---|
| `crates/wine` | `sewnie/wine` | prefixes, commands, registry parser/exporter, DXVK, WebView2 |
| `crates/rbxweb` | `sewnie/rbxweb` | the Roblox web API endpoints Vinegar uses |
| `crates/rbxbin` | `sewnie/rbxbin` | deployments, mirrors, package install |
| `crates/salt` | `cmd/vinegar`, `internal/*` | the app: GTK UI, bootstrapper, MCP bridge |
| `crates/layer` | `layer/vinegar_layer.cpp` | the Vulkan swapchain layer |

## Building

Salt currently runs inside the Vinegar flatpak (`org.vinegarhq.Vinegar`, from
Flathub), using its runtime, sandbox and data. Builds run inside the matching
GNOME 50 SDK, so the binary links against the same libraries it runs with:

```sh
flatpak install --user flathub org.gnome.Sdk//50 org.freedesktop.Sdk.Extension.rust-stable//25.08
./sdk.sh make          # release build
./sdk.sh make test     # unit tests
./sdk.sh make install  # into ~/.var/app/org.vinegarhq.Vinegar/data/salt
```

`make install` also creates two launchers in `~/.local/bin`:

- `salt [run|manage] [args]` runs Salt inside the Vinegar flatpak.
- `salt-mcp` is a stdio MCP server for Roblox Studio (see below).

`make native && make install-native PREFIX=/usr` builds and installs for the
host instead, as `io.github.glockerz.Salt` (Kombucha needs glibc).

Inside the flatpak, Salt uses the app ID `org.vinegarhq.Vinegar`: the sandbox
only lets it own D-Bus names under that ID, and it routes Studio's browser
login callbacks (opened through Vinegar's desktop file) to Salt.

## Changes from Vinegar

### Stability and responsiveness

- **Roblox logs are read from Studio's own log files** (`studio.log_source =
  "file"`, the default). The Go version enabled Wine's `warn+seh` channel, so
  every Studio log call in every thread became a synchronous write to Wine's
  stderr pipe (one measured session wrote 248,000 lines). Set
  `log_source = "debugger"` for the old behaviour.
- **Nothing slow runs in the Wine output reader.** Discord Rich Presence did
  two HTTPS requests inside it on every place open; a full pipe stalls any
  Wine thread that writes debug output, including ones holding loader or heap
  locks. Rich Presence now has its own thread, output pipes are 1 MiB, and log
  file writes are buffered.
- **WINEDEBUG ordering fix.** `fixme-all` came after `warn+seh`, which left
  `fixme:seh` enabled (the `WerRegisterCustomMetadata` spam).
- **5-second DNS stalls fixed** (`RES_OPTIONS=single-request-reopen`, also
  passed to Wine): glibc's parallel A/AAAA lookups intermittently hit the
  resolver timeout on some routers. Measured about 1 in 60 lookups taking
  5.0 s before, none after.
- `Prefix::running` checks wineserver's lock instead of the socket file, so a
  crashed server is not mistaken for a running one.
- Kombucha's `wineserver` crashes when run directly; the prefix is now started
  through `wineboot`, which starts the server the way Wine does.
- No unbounded line length in the registry parser (Go's 64 KiB `bufio.Scanner`
  limit could make large Studio layouts unreadable), atomic config and
  settings-backup writes, and atomic Wine build extraction (an interrupted
  download no longer leaves a broken build that counts as installed).

### Speed

- Setup runs Wine prefix preparation **concurrently** with the Studio,
  WebView2 and DXVK downloads instead of after them.
- Packages download 8 at a time, largest first, hashed while streaming
  (no second read to verify). Mirrors are probed concurrently; the package
  manifest and the installer are fetched in parallel.
- The Wine theme is only re-imported when it actually changed (it used to
  spawn `regedit` on every launch).
- WebView2's version override check reads the offline registry instead of
  spawning `wine reg query`.
- The PCI ID database is scanned for the needed entries only.
- The registry of a real prefix (2.4 MB) parses in about 20 ms.
- Binary size: 4.0 MB versus 11.5 MB.

### Vulkan layer

- Read-write lock for dispatch tables (the per-frame `vkAcquireNextImageKHR`
  path only reads) and an atomic flag, instead of one global mutex.
- `vkCreateInstance`/`vkCreateDevice` failures are returned instead of being
  ignored, and destroy calls are passed down the chain.

## MCP bridge

`salt mcp` (the `salt-mcp` launcher) is a stdio MCP server that bridges
to Studio's `StudioMCP.exe`.

Running `StudioMCP.exe` directly from a wrapper started the prefix's
wineserver inside the wrapper's own Flatpak sandbox whenever Studio wasn't
already running. Studio then attached to that wineserver, and restarting the
MCP server tore the sandbox down along with the wineserver, hanging Studio.

The bridge never starts Wine itself. Until Studio is running, it answers the
MCP handshake with an empty tool list. When Studio's wineserver appears, it
starts `StudioMCP.exe` from the installed Studio version, replays the
handshake, proxies all traffic and sends `tools/list_changed`. When Studio
exits it goes back to waiting. It also stops a stale `StudioMCP.exe` that
still holds port 13469 before starting a new one.

For Claude Code:

```sh
claude mcp add --scope user Roblox_Studio -- ~/.local/bin/salt-mcp
```

## License

GPL-3.0-only, like Vinegar and the libraries it ports. Salt is derived from
[vinegarhq/vinegar](https://github.com/vinegarhq/vinegar) and
[sewnie/wine](https://github.com/sewnie/wine),
[sewnie/rbxbin](https://github.com/sewnie/rbxbin) and
[sewnie/rbxweb](https://github.com/sewnie/rbxweb); its UI definitions and
translations come from Vinegar. The Vulkan layer is based on Vinegar's layer
(by tunis and V3L0C1T13S) and baldurk/sample_layer (BSD-2-Clause).

Discord Rich Presence uses Vinegar's Discord application ID for its images.
