# recast — build / install / deploy
#
# The binary is self-contained: dictionaries are embedded at compile time
# (`include_str!` in src/main.rs) so it can be invoked from any directory
# without a wrapper or environment variable.
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
    PERM_HINT_2 := "  sudo usermod -aG input \$$USER   # log out + back in"
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

BIN_NAME := recast
BIN_SRC  := target/release/$(BIN_NAME)
BIN_DST  := $(BINDIR)/$(BIN_NAME)

# Sources that should retrigger a build of $(BIN_SRC). Listed explicitly so
# the file-target dependency does the right thing — `cargo build` itself is
# fast on a no-op rebuild, but the explicit list lets `make install` skip
# the cargo invocation when nothing has changed.
SRC := Cargo.toml Cargo.lock en_dict.txt he_dict.txt \
       $(shell find src -type f -name '*.rs' 2>/dev/null)

.PHONY: all build clean rebuild install uninstall deploy run help \
        service service-uninstall \
        service-linux service-uninstall-linux \
        service-macos service-uninstall-macos \
        service-unsupported
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
service-linux: install
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
	@echo
	@echo "For Windows: use deploy.ps1 (PowerShell)."
