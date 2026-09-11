<p align="center">
  <img src="assets/recast-icon.svg" width="128" height="128" alt="ReCast logo">
</p>

<h1 align="center">ReCast</h1>

ReCast runs in the background and fixes English/Hebrew keyboard-layout mistakes
as you type. It switches layouts and replaces the mistyped word, corrects English
spelling (`recieve` → `receive`), and offers word completion and custom abbreviations.
Everything runs locally, with dictionaries embedded in the executable. Your
clipboard is never touched.

## How it works

ReCast checks words when you finish them with Space, Enter, or punctuation. It
uses the current keyboard layout, dictionary matches, and word frequency to decide
whether to rewrite them. Each word gets one rewrite or none; capitalization and
the terminator are preserved. A confident English spelling fix can also correct a
wrong-layout word in the same replacement.

Spelling correction protects dictionary words, ALL CAPS, and tokens containing
digits or internal punctuation. It uses word frequency and weighted typo costs,
without surrounding-sentence context, so unfamiliar names or jargon can still get
unwanted fixes. Undo, ignored words, and Conservative spelling give you control.
Navigation, mouse clicks, and focus changes cancel stale corrections where detected.

## Install

Enable **English and Hebrew keyboards** in your OS settings. Their order does not matter.

The release workflow produces these self-contained downloads; check
[GitHub Releases](https://github.com/orisup1/recast/releases) for available builds:

| File | Platform |
| --- | --- |
| `recastLinux` | Linux x86-64 |
| `ReCast.exe` | Windows x86-64, Windows 10+ |
| `recastMac` | macOS, Intel and Apple Silicon |
| `ReCast.app.zip` | macOS app bundle, Intel and Apple Silicon |
| `SHA256SUMS` | Download checksums |

For a source build, install Rust/Cargo, clone this repository, and run the commands
below from its root. No separate dictionary installation is needed.

### macOS

For a downloaded app, extract `ReCast.app.zip`, move the app to `/Applications`,
and open it from Finder. To build and install the app from source:

```bash
make app
```

Setup opens on first launch or whenever a requirement is missing. It checks
**Accessibility** and both keyboards, links to System Settings, and offers
**Check again**. A separate Input Monitoring entry is not required by setup.
If ReCast is missing from Accessibility, click **+**, then **Cmd+Shift+G**, and enter
the exact path shown in setup. **Show ReCast in Finder** reveals that copy.
Quit and reopen ReCast if permission changes require a relaunch.

Use **Start at login** in the menubar menu for autostart. Alternatively,
`make service` installs a bare binary with a launchd LaunchAgent;
`make service-uninstall` removes that service. Terminal-launched binaries may have
permissions attributed to the terminal rather than ReCast.

### Linux

ReCast reads keyboards through `evdev` and writes corrections through `uinput`.
Your user needs access to both devices. Add yourself to the `input` group, then
log out and back in before installing the user service:

```bash
sudo usermod -aG input "$USER"
# After logging back in, from the repository:
make service
```

This builds and installs `~/.local/bin/recast` and starts a systemd user service.
Ensure `~/.local/bin` is on your `PATH`.

```bash
systemctl --user status recast
journalctl --user -u recast -f
systemctl --user restart recast
make service-uninstall
```

Layout backends support Hyprland, Sway, KDE, GNOME, and X11. Native GNOME/KDE
Wayland cannot identify the focused application; see [Application exclusions](#application-exclusions).
Disconnected keyboards are detected automatically; ReCast waits for reconnection.

### Windows

Run `ReCast.exe` for the tray app. **Start at login** registers it in the per-user
Run key. To build, install, and register a logon Scheduled Task from source:

```powershell
.\deploy.ps1 -Target service
```

The default install directory is `%USERPROFILE%\.local\bin`.
Use `-Target service-uninstall` to remove the task, or `-Target help` for all targets.

## Controls

| Action | Gesture or control |
| --- | --- |
| Complete an English word | Tap **Right Shift** mid-word; tap again to cycle through suggestions and back to your prefix |
| Undo the latest correction | Tap **Ctrl twice within half a second**, immediately after the correction |
| Allow an ignored word again | Type that word and its space, then immediately double-tap Ctrl |
| Enable/disable or pause | Tray/menubar, Linux control window, or terminal dashboard |
| Ignore a correction permanently | Click it in the tray's **Recent** menu, or add it to `ignore.txt` |
| Review gestures | **Typing shortcuts** in the tray or Linux control window |

Holding Shift for capitals or Ctrl for shortcuts does not trigger these gestures.
Further typing or cursor movement ends the undo opportunity. Undo restores the
original text and, when changed, the previous layout. One undo suppresses that word
for the session; undo counts are saved in `learned.txt`, and two occasions make the
exception survive restarts. Allowing the word again clears its saved exception.

The macOS/Windows tray and Linux control window offer live **Settings** for spelling,
completion/abbreviations, and **Conservative spelling**. Conservative spelling caps
fixes at one edit; turning it off restores the default ceiling of three, with
stricter limits for shorter words. The tray counter offers this setting when many
corrections have been undone. The enabled/disabled switch survives restarts.

### Command line

```bash
recast                 # Linux: background daemon; macOS/Windows: tray app
recast -f              # Linux: stay in the foreground
recast -g              # Terminal dashboard (Linux/Windows)
recast -w              # Control window (Linux only)
recast --stop          # Linux/macOS; on Windows, quit from the tray
recast --status        # Running state and configuration diagnostics
recast --write-config  # Create a commented config without overwriting one
recast --help          # All options and environment settings
```

In the terminal dashboard, `e`/Space toggles correction, `p` pauses for 30 minutes,
`r` reloads lists, and `q` quits. Closing the control window or quitting the dashboard
ends ReCast. On macOS, use the menubar instead of the dashboard.

Starting ReCast normally replaces an existing instance. If a service manager would
immediately restart the old copy, ReCast explains how to stop that service first.
`--keep-others` bypasses replacement, but concurrent copies can correct text twice.
`--status` reports settings and memory for its own invocation, not the running
process's live configuration or memory.

## Configuration and files

| OS | Configuration directory |
| --- | --- |
| Linux | `~/.config/recast/` (or `$XDG_CONFIG_HOME/recast/`) |
| macOS | `~/Library/Application Support/recast/` |
| Windows | `%APPDATA%\recast\` |

Live UI settings save to `config.toml` and apply immediately. Manual edits to that
file require a restart. Environment variables override the file; the UI explains
when an override prevents a change. Run `recast --write-config` for every supported
setting, including layout backends and injection timings.

The file accepts flat `key = value` lines and `#` comments, without tables or arrays.
Keys are environment names lowercased without `RECAST_`; for example,
`RECAST_SPELL_DIST=1` is `spell_dist = 1`.

```toml
# Example overrides; everything else uses its default.
spell_dist = 1      # Single-edit spelling fixes (default ceiling: 3)
complete = true    # Completion and abbreviations (default: true)
split = false      # Missing-space splitting (default: false)
personal = false   # Persistent personalization (default: false)
```

Spelling defaults to words of at least 4 characters and suggestions ranked within
20,000; completion defaults to prefixes of at least 3 characters and rank 30,000.
`short = false` restricts short layout switches to very common words;
`freq = false` disables the frequency tie-break between valid readings in both layouts.
An absent config uses defaults. Invalid values produce diagnostics and fall back;
an unreadable config stops startup so exclusions are not silently discarded.

| File | Purpose |
| --- | --- |
| `config.toml` | Settings; optional until created or saved from the UI |
| `abbrev.txt` | Custom `abbr = expansion` rules; `#` comments allowed |
| `ignore.txt` | Words to leave alone, one per line; `#` comments allowed |
| `learned.txt` | Undo counts used for persistent exceptions |
| `state.txt` | Saved enabled/disabled state |
| `welcomed` | Marker for the one-time correction hint |
| `setup-complete` | macOS setup marker; missing requirements still reopen setup |
| `personal/` | Opt-in word counts, correction pairs, and aggregate typing timings |

`abbrev.txt` and `ignore.txt` reload within about two seconds of edits. The tray's
**Advanced settings** and **Open ignored words** open files in your editor, creating
missing files without overwriting existing content.

For abbreviations, add rules to `abbrev.txt`:

```text
btw = by the way
addr = 1 Main Street, Tel Aviv
```

Rules expand when you finish the word and take priority over automatic corrections.
Right Shift also offers an expansion. Capitalization follows your input:
`Btw` becomes `By the way`. No abbreviations ship enabled; `complete = false`
disables both abbreviations and word completion.

### Application exclusions

Use **Excluded applications** in the tray or Linux window to exclude the last
detected active app, or choose **Allow** to remove an exclusion. Changes save and
apply immediately. You can also set `exclude_apps` in the config and restart:

```toml
# Exact IDs, comma-separated and case-insensitive; no wildcards.
exclude_apps = "Alacritty, org.keepassxc.KeePassXC"
# macOS uses bundle IDs; Windows uses executable filenames including .exe.
```

Excluded apps receive no corrections, expansions, completion, undo rewrites,
word logging, or learning. Global events still track key releases. With any
exclusions configured, an unknown active application also suspends processing.
On **native GNOME/KDE Wayland**, this means correction pauses throughout the session;
the Linux window disables adding exclusions when detection is unavailable.
After returning to an allowed app, finish the current word with Space/Enter to resume.

## Privacy

ReCast processes global keyboard events locally: no telemetry, remote dictionaries,
or update checks. Recent corrections stay in memory. Undo counts and explicitly
ignored words are saved locally; debug logging and personalization are off by default.
Synthetic input replaces text without using the clipboard.

On macOS, ReCast suspends processing while the OS reports **Secure Input**.
Linux and Windows have no equivalent check: exclusions can protect a whole app,
but cannot identify individual password fields in an allowed browser or application.

`RECAST_DEBUG=1` prints checked words and may expose sensitive text in service logs.
`RECAST_PERSONAL=1` saves word/correction data and aggregate key timings under
`personal/`; on Linux/Windows, this may include password-field text. Personal files
are user-only on Unix. To clear them, **stop ReCast first**, then run:

```bash
recast --clear-personal-data
```

This removes ReCast's personal-data files, not your ignored words or undo exceptions.

## Troubleshooting

- **No corrections:** check the enabled state, installed keyboards, permissions,
  exclusions, and `recast --status`. On Linux, verify `input` membership and `uinput`
  access, then inspect the service journal.
- **Wrong Linux backend:** set `layout_backend = "hyprland"` (or `sway`, `kde`,
  `gnome`, `x11`) and restart. Hyprland needs the current
  `HYPRLAND_INSTANCE_SIGNATURE` and `XDG_RUNTIME_DIR` in the process environment.
- **macOS permissions broken after reinstall:** remove the old Accessibility entry
  and add the running copy shown in setup. `make app` resets privacy grants;
  for a manual reinstall, `tccutil reset All com.recast.app` resets them before you
  grant access again. Relaunch afterward if needed.
- **Unwanted spelling fixes:** undo immediately, ignore the word, enable
  Conservative spelling, or disable spelling in Settings.

## Development

```bash
cargo build --locked --release
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --release --test cli
cargo test correction_accuracy_corpus -- --nocapture
make bench
```

The shared correction planner is in [src/dictionary.rs](src/dictionary.rs), with
spelling in [src/spell.rs](src/spell.rs) and completion/lists in
[src/complete.rs](src/complete.rs). [src/platform/engine.rs](src/platform/engine.rs)
owns typing, cancellation, completion cycling, and undo across all platforms.
Native adapters handle capture/injection; [src/layout](src/layout) handles layout backends.

Add real correction reports to [tests/data/corrections.tsv](tests/data/corrections.tsv).
The corpus separates unwanted, missed, and wrong corrections; it is a regression
check, not an estimate of accuracy for all typing. Engine tests use a simulated
screen; focus benchmarks require a desktop session.

[CI](.github/workflows/ci.yml) checks formatting, tests, Clippy, release builds, and
release CLI behavior on Linux, macOS, and Windows. The manual
[Binaries workflow](.github/workflows/binaries.yml) publishes release assets and
checksums, with signing/notarization when repository credentials are configured.
`make help` and `.\deploy.ps1 -Target help` list build, install, and service targets.

## License

Apache-2.0 — see [LICENSE](LICENSE).
