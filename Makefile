.POSIX:

# Salt runs inside the Vinegar flatpak, sharing its runtime, data and
# prefixes. Build inside the matching SDK and install into the flatpak's
# data directory:
#   ./sdk.sh make && ./sdk.sh make install
# Native (non-Flatpak) builds:
#   make native && make install-native PREFIX=/usr

FLATPAK = org.vinegarhq.Vinegar
APP     = io.github.glockerz.Salt
DEST   ?= $(HOME)/.var/app/$(FLATPAK)/data/salt
PREFIX ?= /usr/local
SDK    != [ -e /.flatpak-info ] || echo ./sdk.sh
BIN     = target/release/salt
LAYER   = target/release/libVkLayer_SALT_SaltLayer.so
LOCALES = de pt_BR ru zh_TW

all: locales
	$(SDK) env SALT_APP_ID=$(FLATPAK) cargo build --release -p salt -p salt-layer

locales:
	$(SDK) sh -c 'for l in $(LOCALES); do mkdir -p target/locale/$$l/LC_MESSAGES && msgfmt data/po/$$l.po -o target/locale/$$l/LC_MESSAGES/salt.mo; done'

test:
	$(SDK) cargo test --workspace

install:
	mkdir -p $(DEST)/bin $(DEST)/lib $(DEST)/share/vulkan/explicit_layer.d
	install -m755 $(BIN) $(DEST)/bin/salt
	install -m644 $(LAYER) $(DEST)/lib/
	sed 's#"library_path": ".*"#"library_path": "../../../lib/libVkLayer_SALT_SaltLayer.so"#' \
		data/VkLayer_SALT_SaltLayer.json > $(DEST)/share/vulkan/explicit_layer.d/VkLayer_SALT_SaltLayer.json
	cp -r target/locale $(DEST)/share/
	mkdir -p $(HOME)/.local/bin
	printf '#!/bin/sh\nexec flatpak run --command=%s %s "$$@"\n' "$(DEST)/bin/salt" $(FLATPAK) > $(HOME)/.local/bin/salt
	printf '#!/bin/sh\n# MCP stdio bridge to Roblox Studio running under Salt\nexec flatpak run --command=%s %s mcp\n' "$(DEST)/bin/salt" $(FLATPAK) > $(HOME)/.local/bin/salt-mcp
	chmod +x $(HOME)/.local/bin/salt $(HOME)/.local/bin/salt-mcp

native: locales
	cargo build --release -p salt -p salt-layer

install-native:
	install -Dm755 $(BIN) $(DESTDIR)$(PREFIX)/bin/salt
	install -Dm644 $(LAYER) -t $(DESTDIR)$(PREFIX)/lib
	install -Dm644 data/VkLayer_SALT_SaltLayer.json -t $(DESTDIR)$(PREFIX)/share/vulkan/explicit_layer.d
	install -Dm644 data/$(APP).desktop -t $(DESTDIR)$(PREFIX)/share/applications
	install -Dm644 data/$(APP).metainfo.xml -t $(DESTDIR)$(PREFIX)/share/metainfo
	install -Dm644 data/$(APP)-studio.xml -t $(DESTDIR)$(PREFIX)/share/mime/packages
	install -Dm644 data/icons/salt.svg $(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/$(APP).svg
	install -Dm644 data/icons/roblox-studio.svg $(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/$(APP).studio.svg
	for l in $(LOCALES); do install -Dm644 target/locale/$$l/LC_MESSAGES/salt.mo -t $(DESTDIR)$(PREFIX)/share/locale/$$l/LC_MESSAGES; done

.PHONY: all locales test install native install-native
