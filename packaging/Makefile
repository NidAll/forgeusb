PREFIX ?= /usr
DESTDIR ?=
TARGET_DIR ?= target

.PHONY: build install
build:
	cargo build --release --locked

# Staging works without privilege: make -f packaging/Makefile install DESTDIR=/tmp/forgeusb-stage
# A real /usr installation is an explicit administrator action, never part of tests.
install:
	install -Dm755 $(TARGET_DIR)/release/forgeusb $(DESTDIR)$(PREFIX)/bin/forgeusb
	install -Dm755 $(TARGET_DIR)/release/forgeusb-gui $(DESTDIR)$(PREFIX)/bin/forgeusb-gui
	install -Dm755 $(TARGET_DIR)/release/forgeusb-helper $(DESTDIR)$(PREFIX)/lib/forgeusb/forgeusb-helper
	install -Dm644 packaging/forgeusb.desktop $(DESTDIR)$(PREFIX)/share/applications/forgeusb.desktop
	install -Dm644 packaging/icons/forgeusb.svg $(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/forgeusb.svg
	install -Dm644 packaging/polkit/io.forgeusb.write.policy $(DESTDIR)$(PREFIX)/share/polkit-1/actions/io.forgeusb.write.policy
	install -Dm644 LICENSE $(DESTDIR)$(PREFIX)/share/licenses/forgeusb/LICENSE
	install -Dm644 THIRD_PARTY_NOTICES.md $(DESTDIR)$(PREFIX)/share/licenses/forgeusb/THIRD_PARTY_NOTICES.md

	install -Dm644 docs/third-party-licenses.txt $(DESTDIR)$(PREFIX)/share/licenses/forgeusb/third-party-licenses.txt
	install -Dm644 docs/dependency-licenses.json $(DESTDIR)$(PREFIX)/share/licenses/forgeusb/dependency-licenses.json
