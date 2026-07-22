<p align="center">
  <img src="assets/icons/icon-256.png" width="128" height="128" alt="ReCast logo">
</p>

<h1 align="center">ReCast</h1>

<p align="center">
  Automatic English &harr; Hebrew keyboard-layout correction, as you type.
</p>

---

ReCast is a lightweight background daemon that watches what you type, checks each finished
word against embedded English and Hebrew dictionaries, and — when it detects that a word
was typed in the wrong layout — switches the keyboard layout and retypes the word
correctly. It runs quietly in the tray, menubar, or as a system service, and stays out of
your way until a mistyped word actually needs fixing.

- **Cross-platform** — Linux, macOS, and Windows share one correction engine.
- **Self-contained** — both dictionaries are compiled into the binary, so there are no data
  files to install and it runs identically from any directory.
- **Conservative by design** — it anchors on your live keyboard layout and only intervenes
  when it is confident, so valid words are never mangled.

## Contents

- [How it works](#how-it-works)
- [Supported platforms](#supported-platforms)
- [Requirements](#requirements)
- [Installation](#installation)
  - [Linux](#linux)
  - [macOS](#macos)
  - [Windows](#windows)
- [Usage](#usage)
  - [Command-line options](#command-line-options)
  - [Environment variables](#environment-variables)
- [Tuning correction behaviour](#tuning-correction-behaviour)

## How it works

1. **Capture** — ReCast reads global key events and builds up the current word from your
   key presses. The buffer resets on cursor- and focus-shifting keys (Tab, Escape, arrows,
   Home/End, PgUp/PgDn, Insert, Delete) and on mouse clicks (macOS / Windows).
2. **Evaluate** — when you press Space or Enter, ReCast interprets the typed key sequence as
   both an English word and a Hebrew word, and looks each up in the matching dictionary.
3. **Decide** — it **anchors on your live keyboard layout** (queried from the OS, with any
   English or Hebrew regional variant recognised). A sequence that already reads as a real
   word in your *current* layout is left untouched — including prefixed Hebrew forms
   (ו/ה/ל/ב/כ/מ/ש) and words whose other-layout reading also happens to be a dictionary
   word. ReCast switches only when the *other* layout yields a confident word and the
   current one yields nothing real. This is what keeps valid and prefixed words intact.
4. **Correct** — on a switch, ReCast erases the mistyped word and retypes it (followed by
   the original Space/Enter), waiting until the layout change has actually propagated first.

> **Missing-space splitting** (carving `helloעולם` into two words) is **opt-in** via
> `RECAST_SPLIT=1`. It is off by default because it cannot reliably distinguish a word that
> simply isn't in the dictionary from two run-together words.

## Supported platforms

| OS      | Capture | Layout switch                                       |
| ------- | ------- | --------------------------------------------------- |
| Linux   | `evdev` | `hyprctl switchxkblayout` (Hyprland only)           |
| macOS   | `rdev`  | Carbon `TISSelectInputSource`                       |
| Windows | `rdev`  | `LoadKeyboardLayoutW` + `WM_INPUTLANGCHANGEREQUEST` |

## Requirements

- **Rust** toolchain (`rustup`, `cargo`).
- Both **English and Hebrew layouts installed** in your OS keyboard settings. On
  Linux/Hyprland, the xkb config must list English as layout `0` and Hebrew as layout `1`.
- **Linux only:** membership in the `input` group (for `evdev` read access). ReCast creates
  a `uinput` virtual device named `recast-injector` to replay corrected words.

## Installation

### Linux

One-shot setup: adds your user to the `input` group, builds in release mode, installs the
binary to `~/.local/bin/recast`, and registers a `systemd --user` unit that starts ReCast
at login.

```bash
sudo usermod -aG input $USER && exec newgrp input <<< 'make service'
```

`newgrp` applies the new group to the current shell so you don't have to log out; omit it
and re-login instead if you prefer. Ensure `~/.local/bin` is on your `PATH`.

Manage the service:

```bash
systemctl --user status  recast    # health check
systemctl --user restart recast    # apply a rebuild
journalctl  --user -u    recast -f  # follow logs
make service-uninstall             # stop and remove the unit
```

Common Make targets:

| Target             | Action                                    |
| ------------------ | ----------------------------------------- |
| `make`             | `cargo build --release`                   |
| `make install`     | build + copy binary to `~/.local/bin`     |
| `make deploy`      | clean + build + install                   |
| `make uninstall`   | remove the installed binary               |
| `make run ARGS=-g` | `cargo run` with a flag (here, the TUI)   |
| `make help`        | full target list                          |

Override the install root with `PREFIX=`, e.g. `make install PREFIX=/opt/recast`.

### macOS

```bash
make service
```

This writes a launchd LaunchAgent at `~/Library/LaunchAgents/org.recast.plist` and starts
it. The first time it runs, grant the binary **Input Monitoring** and **Accessibility**
permissions in *System Settings → Privacy & Security*.

To reset ReCast's permissions:

```bash
tccutil reset All com.recast.app
```

Once installed, `recast` is on your `PATH` and supports the same flags as every other
platform (see [Usage](#usage)) — e.g. `recast -h`, `recast -g`, `recast -s`. Note that the
LaunchAgent runs with `KeepAlive`, so `recast -s` stops a manually-launched instance but the
*service* will be relaunched by launchd; to stop the service, run `make service-uninstall`.

### Windows

In PowerShell:

```powershell
.\deploy.ps1 -Target service
```

This builds, installs to `%USERPROFILE%\.local\bin`, and registers a Scheduled Task that
runs ReCast at logon. Run `.\deploy.ps1 -Target help` to list every target.

## Usage

You don't need the service — you can run the binary (or `cargo run --release`) directly
from any shell. On Linux it daemonizes into the background by default (under systemd this
is detected automatically); on macOS and Windows it lives in the tray/menubar.

### Command-line options

| Option              | Description                                                       |
| ------------------- | ---------------------------------------------------------------- |
| *(none)*            | Start ReCast (Linux: forks into the background and writes a pidfile) |
| `-s`, `--stop`      | Stop a running daemon (via its pidfile)                          |
| `-g`, `--gui`       | Foreground terminal dashboard (TUI): status, live log, toggle    |
| `-w`, `--window`    | Foreground control window (Linux only)                           |
| `-f`, `--foreground`| Don't daemonize (Linux; implied under systemd)                   |
| `-h`, `--help`      | Show the full option list                                        |

The **TUI** (`-g`, Linux/Windows) shows the enabled state, the fixed-word counter, and a
live log; press `e` or `Space` to toggle correction on and off, and `q` to quit. The
**control window** (`-w`) offers the same toggle and counter in a small GUI window. On
macOS, use the menubar menu instead.

### Environment variables

| Variable            | Effect                                                          |
| ------------------- | -------------------------------------------------------------- |
| `RECAST_DEBUG=1`    | Print every word check and switch decision                     |
| `RECAST_SPLIT=1`    | Enable the opt-in missing-space splitting fallback             |
| `RECAST_SHORT=0`    | Never auto-switch on short (≤3 char) words                      |

```bash
RECAST_DEBUG=1 recast   # verbose decision logging
```

## Tuning correction behaviour

Short words are the most collision-prone: many 2–3 letter abbreviations are valid in one
dictionary while also spelling a real word in the other layout. `RECAST_SHORT=0` is the
knob for "never switch incorrectly" — at the cost of no longer fixing short mistyped words.
