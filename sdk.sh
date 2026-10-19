#!/bin/sh
# Runs a command inside the GNOME 50 SDK, the same runtime the Vinegar
# flatpak uses, so the resulting binaries link against identical libraries.
exec flatpak run --filesystem=home --share=network --die-with-parent \
	--env=CARGO_HOME="$HOME/.cargo-sdk" --command=sh org.gnome.Sdk//50 \
	-c 'export PATH=/usr/lib/sdk/rust-stable/bin:$PATH; cd "$0" && exec "$@"' "$PWD" "$@"
