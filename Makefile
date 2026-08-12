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

# macOS .app bundle. Nothing in it is committed any more: the executable is a
# build output, the Info.plist is generated from the version above, and the
# icon is a copy of assets/recast.icns. `make bundle` assembles all three, so
# exec/ is scratch space that can be deleted at any time.
APP_BUNDLE := exec/ReCast.app
APP_PLIST  := $(APP_BUNDLE)/Contents/Info.plist
APP_ICON   := $(APP_BUNDLE)/Contents/Resources/AppIcon.icns

# Code signing identity for the macOS bundle. `-` is an ad-hoc signature: no
# certificate, no Apple account, valid only in the sense that the code has not
# been altered since it was signed.
#
# It is not optional. An unsigned bundle runs fine on the machine that built it
# and then reports itself "damaged and can't be opened" the moment it arrives
# anywhere through a download, a zip or a USB stick — Gatekeeper evaluates the
# signature of anything carrying com.apple.quarantine, and *missing* comes back
# as damaged rather than as unsigned. The build machine never sees it because
# files it produced locally are not quarantined, which is why this failure only
# ever shows up on the other side of a transfer, including a transfer back to
# the machine the build came from.
#
# Override with a Developer ID to go one better and get a bundle that opens
# with no warning at all — see the notarize target below:
#   make bundle CODESIGN_ID="Developer ID Application: Name (TEAMID)"
CODESIGN_ID ?= -

BIN_NAME := recast
BIN_SRC  := target/release/$(BIN_NAME)
BIN_DST  := $(BINDIR)/$(BIN_NAME)

# Sources that should retrigger a build of $(BIN_SRC). Listed explicitly so
# the file-target dependency does the right thing — `cargo build` itself is
# fast on a no-op rebuild, but the explicit list lets `make install` skip
# the cargo invocation when nothing has changed.
SRC := Cargo.toml Cargo.lock en_dict.txt he_dict.txt assets/tray-icon.rgba \
        $(shell find src -type f -name '*.rs' 2>/dev/null)

assets/tray-icon.rgba: assets/recast-icon.svg  assets/recast.icns
	@echo "Regenerating 32x32 transparent tray-icon.rgba..."
	@magick "$<" -background none -flatten none -alpha remove -resize 32x32 RGBA:"$@" \
	  || (echo "ERROR: ImageMagick (magick) required. Install via: brew install imagemagick" && exit 1)

# `make tray-icon` was listed in .PHONY and in `help` without ever existing as a
# target, so asking for it by name was an error. It is the file rule above.
tray-icon: assets/tray-icon.rgba

.PHONY: all build clean rebuild install uninstall deploy run help \
	tray-icon version bundle bundle-plist app sign notarize dist \
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

# macOS .app: build, restage the bundle in exec/, install it to /Applications
# and reset the TCC grants (macOS keys Input Monitoring and Accessibility to a
# bundle's signature, so a replaced executable keeps a ticked box and receives
# nothing). The script refuses to run anywhere but Darwin.
app:
	@src/platform/deploy-macos.sh

# Assemble exec/ReCast.app from its three ingredients. The bundle used to be
# committed and only its executable restaged, which meant the layout itself —
# the Resources directory, the icon — existed nowhere but in git history. A
# fresh clone can now produce it, which is what makes untracking exec/ safe.
bundle: $(BIN_SRC) bundle-plist
	@mkdir -p $(APP_BUNDLE)/Contents/MacOS $(dir $(APP_ICON))
	@$(INSTALL) -m 755 $(BIN_SRC) $(APP_BUNDLE)/Contents/MacOS/recast
	@cp assets/recast.icns $(APP_ICON)
ifeq ($(OS_NAME),macOS)
	@$(MAKE) --no-print-directory sign
endif
	@echo "$(APP_BUNDLE) → $(VERSION)"

# Sign the assembled bundle. Runs last, after the executable, the plist and the
# icon are all in place: a signature covers the bundle's contents, so anything
# written into it afterwards invalidates the signature and puts the bundle back
# in the state that reads as "damaged" on another machine.
#
# `--force` because the binary Rust hands us already carries the linker's own
# ad-hoc signature on arm64 and re-signing over it is the point. `--timestamp`
# only means anything to a real certificate, so ad-hoc builds skip it rather
# than reaching out to Apple's timestamp server for nothing; hardened runtime
# likewise only matters as a precondition for notarization.
sign:
	@codesign --force --sign "$(CODESIGN_ID)" \
	  $(if $(filter -,$(CODESIGN_ID)),--timestamp=none,--options runtime --timestamp) \
	  $(APP_BUNDLE)
	@codesign --verify --strict $(APP_BUNDLE) \
	  && echo "signed: $(APP_BUNDLE) ($(if $(filter -,$(CODESIGN_ID)),ad-hoc,$(CODESIGN_ID)))"

# Notarize and staple — the step that makes a download open with no warning at
# all, rather than with the one-time "Apple could not verify it" detour that an
# ad-hoc signature still earns. Needs a paid Apple Developer account: a
# Developer ID certificate in the keychain and credentials stored once with
#   xcrun notarytool store-credentials recast-notary \
#     --apple-id <you> --team-id <TEAMID> --password <app-specific-password>
#
# Stapling writes Apple's ticket into the bundle, so the check passes on a
# machine that is offline or has never seen the app. The zip is rebuilt after
# stapling because the ticket has to be inside the copy that ships.
NOTARY_PROFILE ?= recast-notary
APP_ZIP        := exec/ReCast.app.zip

notarize: bundle
	@test "$(CODESIGN_ID)" != "-" || { \
	  echo "notarize needs a real certificate: make notarize CODESIGN_ID=\"Developer ID Application: ...\"" >&2; \
	  exit 1; }
	ditto -c -k --sequesterRsrc --keepParent $(APP_BUNDLE) $(APP_ZIP)
	xcrun notarytool submit $(APP_ZIP) --keychain-profile "$(NOTARY_PROFILE)" --wait
	xcrun stapler staple $(APP_BUNDLE)
	ditto -c -k --sequesterRsrc --keepParent $(APP_BUNDLE) $(APP_ZIP)
	@spctl --assess --type execute --verbose=2 $(APP_BUNDLE)
	@echo "notarized + stapled: $(APP_ZIP)"

# Package the signed bundle for transfer to another machine. Use this rather
# than dragging exec/ReCast.app into a Finder zip or onto a USB stick: a .app is
# a directory, and the signature lives partly in Contents/_CodeSignature and
# partly in file modes. Anything that flattens or re-creates that structure —
# most zip tools, most filesystems that are not HFS+/APFS — arrives with a
# signature that no longer matches, which macOS reports as a damaged app in
# exactly the same words as no signature at all.
#
# `ditto -c -k --sequesterRsrc --keepParent` is the one archiver that preserves
# it, and it is what Finder's own "Compress" uses. The verify runs first so a
# bundle that is already broken is caught here, not after the transfer.
dist: bundle
	@codesign --verify --strict --verbose=2 $(APP_BUNDLE)
	@rm -f $(APP_ZIP)
	@ditto -c -k --sequesterRsrc --keepParent $(APP_BUNDLE) $(APP_ZIP)
	@echo
	@echo "Transfer this file, not the bundle: $(APP_ZIP)"
	@echo "On the far machine: unzip it, then right-click ReCast.app → Open"
	@echo "(or: xattr -dr com.apple.quarantine ReCast.app)"

# Rewrite the .app bundle's Info.plist from Cargo.toml. Run it after a version
# bump — or before packaging the macOS artifact — so the bundle reports the
# version the binary inside it was built from.
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
	@echo "  version            print the version from Cargo.toml"
	@echo "  bundle             assemble exec/ReCast.app (binary + plist + icon, signed)"
	@echo "  bundle-plist       regenerate the .app Info.plist from that version"
	@echo "  sign               (re)sign the bundle — ad-hoc unless CODESIGN_ID is set"
	@echo "  dist               package the signed bundle into a transfer-safe .zip"
	@echo "  notarize           sign + notarize + staple (needs a Developer ID)"
	@echo "  app                macOS: build + install ReCast.app to /Applications"
	@echo "  tray-icon          regenerate assets/tray-icon.rgba from the SVG"
	@echo
	@echo "Variables:"
	@echo "  PREFIX             install root (default: \$$HOME/.local)"
	@echo "  CARGO              cargo command (default: cargo)"
	@echo "  INSTALL            install command (default: install)"
	@echo "  CODESIGN_ID        macOS signing identity (default: - , ad-hoc)"
	@echo "  NOTARY_PROFILE     notarytool keychain profile (default: recast-notary)"
	@echo
	@echo "Current values:"
	@echo "  PREFIX  = $(PREFIX)"
	@echo "  BINDIR  = $(BINDIR)"
	@echo "  BIN_DST = $(BIN_DST)"
	@echo
	@echo "The binary is self-contained — dictionaries are embedded at"
	@echo "compile time, so it runs identically from any working directory."
	@echo "For Windows: use deploy.ps1 (PowerShell)."
