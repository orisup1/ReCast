<p align="center">
  <img src="assets/recast-icon.svg" width="128" height="128" alt="ReCast logo">
</p>

<h1 align="center">ReCast</h1>

ReCast is a small background helper that watches what you type, checks each finished word
against language dictionaries, and automatically switches the keyboard layout between
English and Hebrew when it looks like you are typing in the wrong layout — then retypes the
mistyped word in the correct layout. It also autocorrects English typos, so a word that is
merely misspelled gets fixed in place instead.

## How it works

- Captures global key events on every supported platform.
- Builds up the current word from key presses; resets the buffer on cursor / focus-shifting
  keys (Tab, Escape, arrows, Home/End, PgUp/PgDn, Insert, Delete) and on mouse clicks
  (macOS / Windows).
- When you press Space or Enter, it interprets the typed key sequence as both an English
  and a Hebrew word and looks each up in the matching dictionary.
- It **anchors on your live keyboard layout** (queried from the OS, with any English or
  Hebrew regional variant recognised). A sequence that already reads as a real word in your
  current layout is left untouched — including prefixed Hebrew forms (ו/ה/ל/ב/כ/מ/ש) and
  words whose other-layout reading happens to also be a dictionary word. It only switches
  when the *other* layout yields a confident word and the current one yields nothing real.
  This is what stops valid (and nested/prefixed) words from being mangled.
- On a switch it erases the mistyped word and puts the corrected one back **in one shot**,
  the way a paste lands rather than the way typing does — followed by the original
  Space/Enter. On macOS and Windows the word is inserted as text in a single event, so it
  appears at once and does not depend on the layout change having propagated; on Linux the
  whole erase + retype sequence goes to the virtual keyboard as one batch.
- If no layout switch applies and you are typing in English, a second pipeline compares the
  word *within* English and fixes near-miss typos in place (see
  [English autocorrect](#english-autocorrect)). Only one of the two ever acts on a word.
- Missing-space splitting (carving `helloעולם` into two words) is **opt-in** via
  `RECAST_SPLIT=1`; it is off by default because it cannot reliably tell a word we simply
  don't have in the dictionary from two run-together words.

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
files or wrapper scripts to install. They are baked in *sorted*, so a lookup is a binary
search over the embedded bytes: nothing is parsed at startup and the daemon idles at a
few MB of memory instead of ~100 MB.

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

reset app permision with 
```bash
tccutil reset All com.recast.app
```

## Windows

PowerShell:

```powershell
.\deploy.ps1 -Target service
```

Builds, installs to `%USERPROFILE%\.local\bin`, and registers a Scheduled Task that
runs ReCast at logon. `.\deploy.ps1 -Target help` lists every target.

## Running directly

If you don't want a service, just run the binary (or `cargo run --release`) from any
shell. On Linux it daemonizes into the background by default (`-f`/`--foreground`
keeps it attached; under systemd this is detected automatically); on macOS/Windows
it stays in the tray/menubar.

```bash
recast          # start (Linux: forks into the background and writes a pidfile)
recast -s       # stop a running daemon
recast -g       # foreground with a terminal dashboard (TUI): status, log, toggle
recast -w       # foreground with a small control window (Linux only)
recast -h       # full option list
```

The TUI (`-g`, Linux/Windows) shows the enabled state, fixed-word counter and a
live log; `e`/`Space` toggles correction on and off, `q` quits. The control
window (`-w`) offers the same toggle and counter in a tiny GUI window. On macOS
use the menubar menu instead.

Environment variables:

```bash
RECAST_DEBUG=1 recast   # print every word check and switch decision
RECAST_SPLIT=1 recast   # opt-in missing-space splitting (off by default)
RECAST_SHORT=0 recast   # never auto-switch on short (≤3 char) words
RECAST_FREQ=0  recast   # disable the homograph frequency tie-break (on by default)
RECAST_SPELL=0 recast   # disable the English spelling autocorrect (on by default)

RECAST_SPELL_MIN=5    recast  # shortest word the autocorrect may fix (default 4)
RECAST_SPELL_RANK=10000 recast  # how common a suggestion must be (default 20000)
RECAST_SPELL_DIST=2   recast  # also fix two-edit typos on longer words (default 1)
```

When a key sequence spells a real word in **both** layouts (a homograph
collision), ReCast normally keeps whatever layout you are in. The frequency
tie-break overrides that in the lopsided case: if the *other* layout's reading
is a genuinely common word (top ~2000 by usage) while your current reading is
rare or unlisted, it switches to the common one — so an accidental obscure
homograph still gets corrected. Set `RECAST_FREQ=0` to always keep the current
layout on homographs. Frequency ranks come from compact OpenSubtitles wordlists
embedded in the binary, consulted only for this tie-break (never as a switch
trigger on their own).

Short words are the most collision-prone (many 2–3 letter abbreviations are
valid in one dictionary while spelling a real word in the other layout), so
`RECAST_SHORT=0` is the knob to reach "never wrongly switch" at the cost of
not fixing short mistyped words.

## English autocorrect

Alongside the between-language comparison, ReCast also compares *within* English:
a word that is not a wrong-layout mistype but is a near-miss of a common English
word gets retyped as that word (`recieve` → `receive`, `helo` → `hello`,
`goverment` → `government`). It runs only when your live layout is English —
corrections are injected as keystrokes, so they only come out right there.

Because a wrong spelling fix silently rewrites your text, the bar is high. A
word is only corrected when all of this holds:

- it is not in the 370k-word English dictionary, and not a token the frequency
  corpus sees often (that is what protects names and handles — `sami`, `ori`,
  `github` are left alone),
- it is at least `RECAST_SPELL_MIN` characters (default 4), all letters — no
  digits, so identifiers survive,
- the correction is one edit away (`RECAST_SPELL_DIST=2` also allows two on
  words of 6+ characters, with a 5× stricter frequency bar),
- the correction is a common word, within `RECAST_SPELL_RANK` (default 20000),
- the correction keeps your first letter — except for a swap of the first two.

Among the survivors, finger slips (a swapped pair, a doubled letter typed once
or twice) beat plain misspellings, and the more common word breaks the tie.
Unavoidably, jargon absent from an everyday corpus can still be "fixed"
(`impl` → `imply`); `RECAST_SPELL=0` turns the whole pipeline off.

The two pipelines are mutually exclusive: each finished word gets **one**
correction or none. The layout switch is tried first, because it is exact — the
keystrokes literally spell a real word in the other language — and only if it
declines does the speller get a look. A word that the speller fixes is typed as
its corrected self and never re-examined, so it is not then flipped to the other
layout even if its keys happen to spell a Hebrew word too.
