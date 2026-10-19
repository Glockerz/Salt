#!/bin/sh
# install-salt.sh — install the Salt flatpak that ships beside this script.
#
# This is delivered inside Salt's mirror payload (see mirror/README.md). After
# you extract the payload to a directory, run:
#
#   sh install.sh
#
# It installs salt.flatpak for the current user, pulling the GNOME runtime from
# Flathub if it isn't present. Salt then appears in your application menu, or
# run it with: flatpak run io.github.glockerz.Salt
set -eu

DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
FLATPAK="$DIR/salt.flatpak"

[ -f "$FLATPAK" ] || { echo "SALT_INSTALL_FAIL=missing:$FLATPAK" >&2; exit 1; }
command -v flatpak >/dev/null 2>&1 || {
    echo "SALT_INSTALL_FAIL=flatpak-not-found — install Flatpak first: https://flatpak.org/setup/" >&2
    exit 1
}

# Flathub provides the GNOME runtime Salt depends on; adding it per-user is
# harmless if it is already configured.
flatpak remote-add --user --if-not-exists flathub \
    https://dl.flathub.org/repo/flathub.flatpakrepo || true

echo "SALT_INSTALL=installing $FLATPAK"
flatpak install --user --assumeyes "$FLATPAK"

echo "SALT_INSTALL_DONE=YES"
echo "Run Salt from your application menu, or: flatpak run io.github.glockerz.Salt"
