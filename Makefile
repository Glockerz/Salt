.POSIX:

# Salt is distributed as a flatpak:
#   make flatpak          build and install it for the current user
#   make bundle           also write salt.flatpak, a single-file installer
# Development (inside the GNOME 50 SDK, see sdk.sh):
#   ./sdk.sh make test
# Native (non-Flatpak) builds, which need a glibc system for Kombucha:
#   make native && make install-native PREFIX=/usr

APP     = io.github.glockerz.Salt
PREFIX ?= /usr/local
SDK    != [ -e /.flatpak-info ] || echo ./sdk.sh
BIN     = target/release/salt
LAYER   = target/release/libVkLayer_SALT_SaltLayer.so
LOCALES = de pt_BR ru zh_TW
BUILDER = flatpak run org.flatpak.Builder --disable-rofiles-fuse

flatpak:
	$(BUILDER) --user --install --force-clean build-dir $(APP).yml

bundle:
	$(BUILDER) --user --install --force-clean --repo=repo build-dir $(APP).yml
	flatpak build-bundle --runtime-repo=https://dl.flathub.org/repo/flathub.flatpakrepo repo salt.flatpak $(APP)

locales:
	sh -c 'for l in $(LOCALES); do mkdir -p target/locale/$$l/LC_MESSAGES && msgfmt data/po/$$l.po -o target/locale/$$l/LC_MESSAGES/salt.mo; done'

test:
	$(SDK) cargo test --workspace

native: locales
	cargo build --release -p salt -p salt-layer

install-native:
	install -Dm755 $(BIN) $(DESTDIR)$(PREFIX)/bin/salt
	install -Dm644 $(LAYER) -t $(DESTDIR)$(PREFIX)/lib
	mkdir -p $(DESTDIR)$(PREFIX)/share/vulkan/explicit_layer.d
	sed 's#"library_path": ".*"#"library_path": "$(PREFIX)/lib/libVkLayer_SALT_SaltLayer.so"#' \
		data/VkLayer_SALT_SaltLayer.json > $(DESTDIR)$(PREFIX)/share/vulkan/explicit_layer.d/VkLayer_SALT_SaltLayer.json
	install -Dm644 data/$(APP).desktop -t $(DESTDIR)$(PREFIX)/share/applications
	install -Dm644 data/$(APP).metainfo.xml -t $(DESTDIR)$(PREFIX)/share/metainfo
	install -Dm644 data/$(APP)-studio.xml -t $(DESTDIR)$(PREFIX)/share/mime/packages
	install -Dm644 data/icons/salt.svg $(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/$(APP).svg
	install -Dm644 data/icons/roblox-studio.svg $(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/$(APP).studio.svg
	for l in $(LOCALES); do install -Dm644 target/locale/$$l/LC_MESSAGES/salt.mo -t $(DESTDIR)$(PREFIX)/share/locale/$$l/LC_MESSAGES; done

.PHONY: flatpak bundle locales test native install-native
