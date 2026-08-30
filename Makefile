# recast — build / install / deploy
#
# The binary is self-contained: dictionaries are embedded at compile time
# (prepared by build.rs, embedded in src/dictionary.rs) so it can be invoked
# from any directory without a wrapper or environment variable.
#
# Common targets:
#   make              build (release)
#   make install      build + copy bin to $(BINDIR)
#   make deploy       clean + build + install
#   make service      install + register an OS autostart unit
#
# Branches on `uname -s`:
#   Linux  → systemd --user service
#   Darwin → launchd LaunchAgent plist
# For Windows, use deploy.ps1 next to this Makefile.

UNAME_S := $(shell uname -s)

ifeq ($(UNAME_S),Linux)
    OS_NAME := Linux
    SERVICE_TARGET := service-linux
    SERVICE_UNINSTALL_TARGET := service-uninstall-linux
    PERM_HINT_1 := Linux evdev access requires the 'input' group:
    PERM_HINT_2 := sudo usermod -aG input $$USER   # log out + back in
else ifeq ($(UNAME_S),Darwin)
    OS_NAME := macOS
    SERVICE_TARGET := service-macos
    SERVICE_UNINSTALL_TARGET := service-uninstall-macos
    PERM_HINT_1 := macOS needs the binary to be granted permissions:
    PERM_HINT_2 := "  System Settings → Privacy & Security → Input Monitoring + Accessibility"
else
    OS_NAME := $(UNAME_S)
    SERVICE_TARGET := service-unsupported
    SERVICE_UNINSTALL_TARGET := service-unsupported
    PERM_HINT_1 := Unsupported OS for the service target: $(UNAME_S)
    PERM_HINT_2 := "  (Windows users: run deploy.ps1 from PowerShell instead)"
endif

PREFIX ?= $(HOME)/.local
BINDIR := $(PREFIX)/bin

# Linux service path
SYSTEMD_DIR  := $(HOME)/.config/systemd/user
SYSTEMD_UNIT := $(SYSTEMD_DIR)/recast.service

# macOS service path
LAUNCHD_DIR   := $(HOME)/Library/LaunchAgents
LAUNCHD_LABEL := org.recast
LAUNCHD_PLIST := $(LAUNCHD_DIR)/$(LAUNCHD_LABEL).plist

CARGO   ?= cargo
INSTALL ?= install

# Cargo.toml is the single source of the version. Everything that has to state
# it either reads CARGO_PKG_VERSION at compile time (all of src/) or is
# generated from this — nothing is typed twice, because the copy that is typed
# twice is the one that goes stale (the .app bundle said 1.0 for four
# releases). Anchored at the line start so the dependency versions below the
# [package] table can't match.
VERSION := $(shell sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -1)

# macOS .app bundle: a committed artifact, but its Info.plist is generated so
# the version in it tracks the crate.
APP_BUNDLE := exec/ReCast.app
APP_PLIST  := $(APP_BUNDLE)/Contents/Info.plist
APP_ZIP    := exec/ReCast.app.zip
CODESIGN_ID ?= -
NOTARY_PROFILE ?= recast-notary

BIN_NAME := recast
BIN_SRC  := target/release/$(BIN_NAME)
BIN_DST  := $(BINDIR)/$(BIN_NAME)

# Sources that should retrigger a build of $(BIN_SRC). Listed explicitly so
# the file-target dependency does the right thing — `cargo build` itself is
# fast on a no-op rebuild, but the explicit list lets `make install` skip
# the cargo invocation when nothing has changed.
SRC := Cargo.toml Cargo.lock build.rs \
        en_dict.txt he_dict.txt en_freq.txt he_freq.txt \
        assets/tray-icon.rgba assets/recast.ico \
        $(shell find src -type f -name '*.rs' 2>/dev/null)

assets/tray-icon.rgba: assets/recast-icon.svg  assets/recast.icns
	@echo "Regenerating 32x32 transparent tray-icon.rgba..."
	@magick "$<" -background none -flatten none -alpha remove -resize 32x32 RGBA:"$@" \
	  || (echo "ERROR: ImageMagick (magick) required. Install via: brew install imagemagick" && exit 1)

# `make tray-icon` was listed in .PHONY and in `help` without ever existing as a
# target, so asking for it by name was an error. It is the file rule above.
tray-icon: assets/tray-icon.rgba

.PHONY: all build clean rebuild install uninstall deploy run bench help \
	tray-icon version bundle bundle-plist sign dist notarize app \
	service service-uninstall \
	service-linux service-uninstall-linux \
	service-macos service-uninstall-macos \
	service-unsupported setup-input-group
.DEFAULT_GOAL := build

all: build

build: $(BIN_SRC)

$(BIN_SRC): $(SRC)
	$(CARGO) build --release

clean:
	$(CARGO) clean

rebuild: clean build

# Install the binary directly to $(BINDIR). No data dir, no wrapper:
# dictionaries are baked into the binary, so it runs identically no matter
# what cwd it is launched from.
install: $(BIN_SRC)
	@mkdir -p $(BINDIR)
	$(INSTALL) -m 755 $(BIN_SRC) $(BIN_DST)
	@echo
	@echo "Installed for $(OS_NAME):"
	@echo "  $(BIN_DST)"
	@echo
	@echo "Make sure $(BINDIR) is on your PATH, then run: $(BIN_NAME)"
	@echo "$(PERM_HINT_1)"
	@echo $(PERM_HINT_2)

uninstall:
	@rm -f $(BIN_DST)
	@echo "Removed $(BIN_DST)"

deploy: clean install

run: build
	$(CARGO) run --release -- $(ARGS)

# Stable, opt-in microbenchmarks. They print latency/throughput without making
# timing-sensitive assertions that would be unreliable on shared CI runners.
bench:
	$(CARGO) test benchmark_ --release -- --ignored --nocapture --test-threads=1

# ─── service: dispatch to the OS-specific target ───────────────────────────
service: $(SERVICE_TARGET)
service-uninstall: $(SERVICE_UNINSTALL_TARGET)

# ─── Linux: systemd --user ────────────────────────────────────────────────
service-linux: install setup-input-group
	@mkdir -p $(SYSTEMD_DIR)
	@printf '%s\n' \
	  '[Unit]' \
	  'Description=recast keyboard layout corrector' \
	  'After=graphical-session.target' \
	  'PartOf=graphical-session.target' \
	  '' \
	  '[Service]' \
	  'Type=simple' \
	  'ExecStart=$(BIN_DST)' \
	  'Restart=on-failure' \
	  'RestartSec=2' \
	  '' \
	  '[Install]' \
	  'WantedBy=graphical-session.target' \
	  > $(SYSTEMD_UNIT)
	systemctl --user daemon-reload
	systemctl --user enable --now recast.service
	@echo
	@echo "systemd --user service installed and started."
	@echo "  status: systemctl --user status recast"
	@echo "  logs:   journalctl --user -u recast -f"

setup-input-group:
	@echo "Checking for 'input' group membership..."
	@if id -nG $$USER | grep -qw input; then \
		echo "User $$USER is already in the 'input' group."; \
	else \
		echo "User $$USER is NOT in the 'input' group."; \
		echo "Attempting to add $$USER to the 'input' group (requires sudo)..."; \
		sudo usermod -aG input $$USER && \
		echo "Added $$USER to 'input' group. Please log out and back in for changes to take effect."; \
	fi

service-uninstall-linux:
	-systemctl --user disable --now recast.service
	@rm -f $(SYSTEMD_UNIT)
	systemctl --user daemon-reload
	@echo "systemd --user service stopped and removed"

# ─── macOS: launchd LaunchAgent ───────────────────────────────────────────
service-macos: install
	@mkdir -p $(LAUNCHD_DIR)
	@printf '%s\n' \
	  '<?xml version="1.0" encoding="UTF-8"?>' \
	  '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">' \
	  '<plist version="1.0">' \
	  '<dict>' \
	  '    <key>Label</key><string>$(LAUNCHD_LABEL)</string>' \
	  '    <key>ProgramArguments</key>' \
	  '    <array>' \
	  '        <string>$(BIN_DST)</string>' \
	  '    </array>' \
	  '    <key>RunAtLoad</key><true/>' \
	  '    <key>KeepAlive</key><true/>' \
	  '    <key>StandardOutPath</key><string>/tmp/recast.out.log</string>' \
	  '    <key>StandardErrorPath</key><string>/tmp/recast.err.log</string>' \
	  '</dict>' \
	  '</plist>' \
	  > $(LAUNCHD_PLIST)
	-launchctl unload "$(LAUNCHD_PLIST)" 2>/dev/null
	launchctl load -w "$(LAUNCHD_PLIST)"
	@echo
	@echo "launchd LaunchAgent installed and started."
	@echo "  plist:  $(LAUNCHD_PLIST)"
	@echo "  status: launchctl list | grep $(LAUNCHD_LABEL)"
	@echo "  logs:   tail -f /tmp/recast.err.log"

service-uninstall-macos:
	-launchctl unload "$(LAUNCHD_PLIST)" 2>/dev/null
	@rm -f $(LAUNCHD_PLIST)
	@echo "launchd LaunchAgent stopped and removed"

service-unsupported:
	@echo "Service target is not supported on $(OS_NAME)." >&2
	@echo "Windows users: run deploy.ps1 -Target service from PowerShell." >&2
	@exit 1

version:
	@echo $(VERSION)

# Assemble a fresh .app from source. `CODESIGN_ID=-` is an ad-hoc local
# signature; release automation supplies a Developer ID identity instead.
bundle: build bundle-plist
	@mkdir -p $(APP_BUNDLE)/Contents/MacOS $(APP_BUNDLE)/Contents/Resources
	$(INSTALL) -m 755 $(BIN_SRC) $(APP_BUNDLE)/Contents/MacOS/recast
	$(INSTALL) -m 644 assets/recast.icns $(APP_BUNDLE)/Contents/Resources/AppIcon.icns
	$(MAKE) sign

sign:
	codesign --force --deep --sign "$(CODESIGN_ID)" \
		$(if $(filter -,$(CODESIGN_ID)),--timestamp=none,--options runtime --timestamp) \
		$(APP_BUNDLE)
	codesign --verify --deep --strict $(APP_BUNDLE)

# `ditto` preserves the signature, executable bit, and macOS metadata.
dist: bundle
	@mkdir -p exec
	ditto -c -k --sequesterRsrc --keepParent $(APP_BUNDLE) $(APP_ZIP)

# Store credentials first with:
# xcrun notarytool store-credentials $(NOTARY_PROFILE) ...
notarize: dist
	@if [ "$(CODESIGN_ID)" = "-" ]; then \
		echo "CODESIGN_ID must be a Developer ID Application identity" >&2; exit 1; \
	fi
	xcrun notarytool submit $(APP_ZIP) --keychain-profile "$(NOTARY_PROFILE)" --wait
	xcrun stapler staple $(APP_BUNDLE)
	ditto -c -k --sequesterRsrc --keepParent $(APP_BUNDLE) $(APP_ZIP).notarized
	mv $(APP_ZIP).notarized $(APP_ZIP)

# macOS .app: build, restage the bundle in exec/, install it to /Applications
# and reset the TCC grants (macOS keys Input Monitoring and Accessibility to a
# bundle's signature, so a replaced executable keeps a ticked box and receives
# nothing). The script refuses to run anywhere but Darwin.
app:
	@src/platform/deploy-macos.sh

# Rewrite the .app bundle's Info.plist from Cargo.toml. Run it after a version
# bump — or before refreshing the committed macOS artifact — so the bundle
# reports the version the binary inside it was built from.
bundle-plist:
	@mkdir -p $(dir $(APP_PLIST))
	@printf '%s\n' \
	  '<?xml version="1.0" encoding="UTF-8"?>' \
	  '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">' \
	  '<plist version="1.0">' \
	  '<dict>' \
	  '    <key>CFBundleName</key><string>ReCast</string>' \
	  '    <key>CFBundleDisplayName</key><string>ReCast</string>' \
	  '    <key>CFBundleIdentifier</key><string>com.recast.app</string>' \
	  '    <key>CFBundleExecutable</key><string>recast</string>' \
	  '    <key>CFBundleIconFile</key><string>AppIcon</string>' \
	  '    <key>CFBundlePackageType</key><string>APPL</string>' \
	  '    <key>CFBundleVersion</key><string>$(VERSION)</string>' \
	  '    <key>CFBundleShortVersionString</key><string>$(VERSION)</string>' \
	  '    <key>LSMinimumSystemVersion</key><string>11.0</string>' \
	  '    <key>LSUIElement</key><true/>' \
	  '    <key>NSHighResolutionCapable</key><true/>' \
	  '</dict>' \
	  '</plist>' \
	  > $(APP_PLIST)
	@echo "$(APP_PLIST) → $(VERSION)"

help:
	@echo "recast Makefile (host OS detected as: $(OS_NAME))"
	@echo
	@echo "Targets:"
	@echo "  build              cargo build --release (default)"
	@echo "  clean              cargo clean"
	@echo "  rebuild            clean + build"
	@echo "  install            build + copy bin to \$$BINDIR"
	@echo "  uninstall          remove installed bin"
	@echo "  deploy             clean + build + install"
	@echo "  service            install + register OS autostart unit"
	@echo "  service-uninstall  remove autostart unit"
	@echo "  run                cargo run --release (use ARGS=... for flags)"
	@echo "  bench              run ignored correction/completion microbenchmarks"
	@echo "  version            print the version from Cargo.toml"
	@echo "  bundle-plist       regenerate the .app Info.plist from that version"
	@echo "  bundle             assemble and sign exec/ReCast.app"
	@echo "  sign               sign the assembled app (CODESIGN_ID=- by default)"
	@echo "  dist               package the app with ditto"
	@echo "  notarize           notarize and staple dist (requires real signing)"
	@echo "  app                macOS: build + install ReCast.app to /Applications"
	@echo "  tray-icon          regenerate assets/tray-icon.rgba from the SVG"
	@echo
	@echo "Variables:"
	@echo "  PREFIX             install root (default: \$$HOME/.local)"
	@echo "  CARGO              cargo command (default: cargo)"
	@echo "  INSTALL            install command (default: install)"
	@echo
	@echo "Current values:"
	@echo "  PREFIX  = $(PREFIX)"
	@echo "  BINDIR  = $(BINDIR)"
	@echo "  BIN_DST = $(BIN_DST)"
	@echo
	@echo "The binary is self-contained — dictionaries are embedded at"
	@echo "compile time, so it runs identically from any working directory."
	@echo "For Windows: use deploy.ps1 (PowerShell)."
