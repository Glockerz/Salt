.POSIX:

# Builds inside the GNOME 50 SDK (the Vinegar flatpak's runtime) and
# installs into the flatpak's data directory, where the sandbox can run it:
#   ./sdk.sh make && ./sdk.sh make install
# Native (non-Flatpak) builds: make native && make install-native PREFIX=/usr

APP     = org.vinegarhq.Vinegar
DEST   ?= $(HOME)/.var/app/$(APP)/data/vinegar-rs
PREFIX ?= /usr/local
SDK    != [ -e /.flatpak-info ] || echo ./sdk.sh
BIN     = target/release/vinegar
LAYER   = target/release/libVkLayer_VINEGAR_VinegarLayer.so
LOCALES = de pt_BR ru zh_TW

all:
	$(SDK) cargo build --release -p vinegar -p vinegar-layer
	$(SDK) sh -c 'for l in $(LOCALES); do mkdir -p target/locale/$$l/LC_MESSAGES && msgfmt data/po/$$l.po -o target/locale/$$l/LC_MESSAGES/vinegar.mo; done'

test:
	$(SDK) cargo test --workspace

install:
	mkdir -p $(DEST)/bin $(DEST)/lib $(DEST)/share/vulkan/explicit_layer.d
	install -m755 $(BIN) $(DEST)/bin/vinegar
	install -m644 $(LAYER) $(DEST)/lib/
	sed 's#"library_path": ".*"#"library_path": "../../../lib/libVkLayer_VINEGAR_VinegarLayer.so"#' \
		data/VkLayer_VINEGAR_VinegarLayer.json > $(DEST)/share/vulkan/explicit_layer.d/VkLayer_VINEGAR_VinegarLayer.json
	cp -r target/locale $(DEST)/share/
	mkdir -p $(HOME)/.local/bin
	printf '#!/bin/sh\nexec flatpak run --command=%s %s "$$@"\n' "$(DEST)/bin/vinegar" $(APP) > $(HOME)/.local/bin/vinegar-rs
	printf '#!/bin/sh\n# MCP stdio bridge to Roblox Studio under Vinegar (see vinegar-rs README)\nexec flatpak run --command=%s %s mcp\n' "$(DEST)/bin/vinegar" $(APP) > $(HOME)/.local/bin/vinegar-mcp
	chmod +x $(HOME)/.local/bin/vinegar-rs $(HOME)/.local/bin/vinegar-mcp

native:
	cargo build --release -p vinegar -p vinegar-layer

install-native: native
	install -Dm755 $(BIN) $(DESTDIR)$(PREFIX)/bin/vinegar
	install -Dm644 $(LAYER) -t $(DESTDIR)$(PREFIX)/lib
	install -Dm644 data/VkLayer_VINEGAR_VinegarLayer.json -t $(DESTDIR)$(PREFIX)/share/vulkan/explicit_layer.d
	install -Dm644 data/$(APP).desktop -t $(DESTDIR)$(PREFIX)/share/applications
	install -Dm644 data/$(APP).metainfo.xml -t $(DESTDIR)$(PREFIX)/share/metainfo
	install -Dm644 data/$(APP)-studio.xml -t $(DESTDIR)$(PREFIX)/share/mime/packages
	install -Dm644 data/icons/vinegar.svg $(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/$(APP).svg
	install -Dm644 data/icons/roblox-studio.svg $(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/$(APP).studio.svg

.PHONY: all test install native install-native
