<p align="center">
  <img src="assets/icons/icon-256.png" width="128" height="128" alt="ReCast logo">
</p>

<h1 align="center">ReCast</h1>

ReCast is a small background helper that watches what you type, checks each finished word
against language dictionaries, and automatically switches the keyboard layout between
English and Hebrew when it looks like you are typing in the wrong layout — then retypes the
mistyped word in the correct layout.

## How it works

- Captures global key events on every supported platform.
- Builds up the current word from key presses; resets the buffer on cursor / focus-shifting
  keys (Tab, Escape, arrows, Home/End, PgUp/PgDn, Insert, Delete) and on mouse clicks
  (macOS / Windows).
- When you press Space or Enter, it interprets the typed key sequence as both an English
  and a Hebrew word and looks each up in the matching dictionary.
- If exactly one dictionary contains the word, it switches the OS keyboard layout to that
  language, erases the mistyped word, and retypes it (followed by the original Space/Enter).

## Supported platforms

| OS      | Capture       | Layout switch                                    |
| ------- | ------------- | ------------------------------------------------ |
| Linux   | `evdev`       | `hyprctl switchxkblayout` (Hyprland only)        |
| macOS   | `rdev`        | Carbon `TISSelectInputSource`                    |
| Windows | `rdev`        | `LoadKeyboardLayoutW` + `WM_INPUTLANGCHANGEREQUEST` |

Linux additionally requires the user to be in the `input` group (for `evdev` read access)
and creates a `uinput` virtual device named `recast-injector` to replay corrected words.

## Setup

1. Install Rust (`rustup`, `cargo`).
2. Make sure both English and Hebrew layouts are installed in your OS keyboard settings.
   On Linux/Hyprland the xkb config must list English as layout 0 and Hebrew as layout 1.

The English and Hebrew dictionaries are baked into the binary at compile time, so the
executable is self-contained and runs identically from any working directory — no data
files or wrapper scripts to install.

## Linux: full install + autostart

One-shot setup. Adds your user to the `input` group (required for `evdev` access),
builds in release mode, installs the binary to `~/.local/bin/recast`, and registers a
`systemd --user` unit that starts ReCast at login:

```bash
sudo usermod -aG input $USER && exec newgrp input <<< 'make service'
```

`newgrp` applies the new group to the current shell so you don't have to log out;
omit the `newgrp` part and re-login instead if you prefer. Make sure `~/.local/bin`
is on your `PATH`.

Manage the service:

```bash
systemctl --user status  recast       # health check
systemctl --user restart recast       # apply a rebuild
journalctl  --user -u    recast -f    # logs
make service-uninstall                  # stop + remove the unit
```

### Other Make targets

```bash
make              # cargo build --release
make install      # build + copy bin to ~/.local/bin
make deploy       # clean + build + install
make uninstall    # remove the installed bin
make run ARGS=-g  # cargo run with the GUI flag
make help         # full target list
```

Override the install root with `PREFIX=`, e.g. `make install PREFIX=/opt/recast`.

## macOS

```bash
make service
```

Writes a launchd LaunchAgent at `~/Library/LaunchAgents/org.recast.plist` and starts it.
You will need to grant the binary **Input Monitoring** and **Accessibility** permissions
in System Settings → Privacy & Security the first time it runs.

## Windows

PowerShell:

```powershell
.\deploy.ps1 -Target service
```

Builds, installs to `%USERPROFILE%\.local\bin`, and registers a Scheduled Task that
runs ReCast at logon. `.\deploy.ps1 -Target help` lists every target.

## Running directly

If you don't want a service, just run the binary (or `cargo run --release`) from any
shell and leave it in the background.

Pass `-g` (Linux only) to launch a small control window with an enable/disable
switch and a counter of words fixed since startup:

```bash
recast -g
```

Set `RECAST_DEBUG=1` to print every word check and switch decision:

```bash
RECAST_DEBUG=1 recast
```
