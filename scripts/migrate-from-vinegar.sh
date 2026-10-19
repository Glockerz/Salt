#!/bin/sh
# Moves your Roblox Studio setup (Wine prefix, Studio install, Wine build,
# settings and cache) from Vinegar to Salt, so nothing is downloaded or set
# up again. Works with the Vinegar flatpak and with native Vinegar installs.
# Afterwards, Vinegar can be uninstalled.
set -eu

SALT=io.github.glockerz.Salt
NEW="$HOME/.var/app/$SALT"
die() { echo "error: $*" >&2; exit 1; }

if [ -d "$HOME/.var/app/org.vinegarhq.Vinegar/data/vinegar" ]; then
	OLD="$HOME/.var/app/org.vinegarhq.Vinegar"
	OLD_DATA="$OLD/data/vinegar" OLD_CONFIG="$OLD/config/vinegar" OLD_CACHE="$OLD/cache/vinegar"
elif [ -d "${XDG_DATA_HOME:-$HOME/.local/share}/vinegar" ]; then
	OLD_DATA="${XDG_DATA_HOME:-$HOME/.local/share}/vinegar"
	OLD_CONFIG="${XDG_CONFIG_HOME:-$HOME/.config}/vinegar"
	OLD_CACHE="${XDG_CACHE_HOME:-$HOME/.cache}/vinegar"
else
	die "no Vinegar data found"
fi

flatpak info "$SALT" >/dev/null 2>&1 || die "install Salt first"
if flatpak ps --columns=application | grep -qxE 'org\.vinegarhq\.Vinegar|io\.github\.glockerz\.Salt' ||
	pgrep -f '[R]obloxStudioBeta' >/dev/null; then
	die "close Roblox Studio, Vinegar and Salt first"
fi
[ -e "$NEW/data/salt" ] && die "$NEW/data/salt already exists; Salt already has data"

echo "Moving $OLD_DATA"
mkdir -p "$NEW/data" "$NEW/config" "$NEW/cache"
mv "$OLD_DATA" "$NEW/data/salt"
if [ -d "$OLD_CONFIG" ] && [ ! -e "$NEW/config/salt" ]; then mv "$OLD_CONFIG" "$NEW/config/salt"; fi
if [ -d "$OLD_CACHE" ] && [ ! -e "$NEW/cache/salt" ]; then mv "$OLD_CACHE" "$NEW/cache/salt"; fi

echo "Updating stored paths"
flatpak run "$SALT" fix-paths "$OLD_DATA"

echo "Done. Your Studio setup now belongs to Salt."
echo "You can uninstall Vinegar:  flatpak uninstall --delete-data org.vinegarhq.Vinegar"
