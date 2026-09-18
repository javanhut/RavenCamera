# Raven Camera. `make`, `make run`, `make test`, `sudo make install`.
#
# Builds against the RavenGUI checkout beside this one (see
# .cargo/config.toml): the encoders are RavenGUI crates.

PREFIX ?= /usr/local
APP_ID := com.ravencamera.Raven
PC_DEPS := gtk4 >= 4.12, libadwaita-1 >= 1.5

.PHONY: build run test check probe install uninstall deps

build: deps
	cargo build --release

run: deps
	cargo run --release

test: deps
	cargo test

check: deps
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings
	cargo test

probe: deps
	cargo run --release -- --probe

deps:
	@pkg-config --exists "$(PC_DEPS)" || rvn install --repo-only -y gtk4 libadwaita
	@test -d ../RavenGUI/crates/raven-mp4 || { \
	  echo "Raven Camera builds against ../RavenGUI (raven-h264, raven-rec, raven-aac, raven-mp4)."; \
	  echo "Clone RavenGUI next to this repository, or delete .cargo/config.toml to use the git versions."; \
	  exit 1; }

install: build
	install -Dm755 target/release/raven-camera $(DESTDIR)$(PREFIX)/bin/raven-camera
	install -Dm644 data/$(APP_ID).desktop $(DESTDIR)$(PREFIX)/share/applications/$(APP_ID).desktop
	install -Dm644 data/$(APP_ID).metainfo.xml $(DESTDIR)$(PREFIX)/share/metainfo/$(APP_ID).metainfo.xml
	install -Dm644 data/icons/hicolor/scalable/apps/$(APP_ID).svg $(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/$(APP_ID).svg
	-command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database -q $(DESTDIR)$(PREFIX)/share/applications
	-command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -qtf $(DESTDIR)$(PREFIX)/share/icons/hicolor

uninstall:
	rm -f $(DESTDIR)$(PREFIX)/bin/raven-camera \
	      $(DESTDIR)$(PREFIX)/share/applications/$(APP_ID).desktop \
	      $(DESTDIR)$(PREFIX)/share/metainfo/$(APP_ID).metainfo.xml \
	      $(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/$(APP_ID).svg
