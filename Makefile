# Pitwall — build and install to a personal (per-user) prefix.
#
# No sudo: this is a one-user, one-machine app (see CLAUDE.md), so everything
# lands under ~/.local, which is already on PATH and XDG_DATA_DIRS. Override
# PREFIX for a system install (make PREFIX=/usr/local install, with sudo).

PREFIX  ?= $(HOME)/.local
BINDIR   = $(PREFIX)/bin
DATADIR  = $(PREFIX)/share
APPID    = dev.miguelrincon.Pitwall

ICON_SIZES = 16 32 48 64 128 256 512

.PHONY: all build run test check install dev-install uninstall clean

all: build

build:
	cargo build --release

run:
	cargo run

test:
	cargo test

# The bar from CLAUDE.md. --all-targets so tests are linted too.
check:
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings
	cargo test

install: build dev-install
	install -Dm755 target/release/pitwall $(BINDIR)/pitwall
	@echo "Installed to $(PREFIX). Launch 'Pitwall' from the app grid, or run 'pitwall'."

# Everything except the release binary: the .desktop entry and the icons.
# Not a way to get a dev-mode icon — on Wayland only the fully installed app
# shows one (see main.rs, once written).
dev-install:
	install -Dm644 data/$(APPID).desktop $(DATADIR)/applications/$(APPID).desktop
	install -Dm644 data/icons/hicolor/scalable/apps/$(APPID).svg \
		$(DATADIR)/icons/hicolor/scalable/apps/$(APPID).svg
	@for sz in $(ICON_SIZES); do \
		install -Dm644 data/icons/hicolor/$${sz}x$${sz}/apps/$(APPID).png \
			$(DATADIR)/icons/hicolor/$${sz}x$${sz}/apps/$(APPID).png; \
	done
	@if [ -f $(DATADIR)/icons/hicolor/index.theme ]; then \
		touch $(DATADIR)/icons/hicolor; \
		gtk-update-icon-cache -q -t -f $(DATADIR)/icons/hicolor; \
	fi
	-update-desktop-database -q $(DATADIR)/applications

uninstall:
	rm -f $(BINDIR)/pitwall
	rm -f $(DATADIR)/applications/$(APPID).desktop
	rm -f $(DATADIR)/icons/hicolor/scalable/apps/$(APPID).svg
	@for sz in $(ICON_SIZES); do \
		rm -f $(DATADIR)/icons/hicolor/$${sz}x$${sz}/apps/$(APPID).png; \
	done
	@if [ -f $(DATADIR)/icons/hicolor/index.theme ]; then \
		gtk-update-icon-cache -q -t -f $(DATADIR)/icons/hicolor; \
	fi
	-update-desktop-database -q $(DATADIR)/applications
	@echo "Uninstalled from $(PREFIX)."

clean:
	cargo clean
