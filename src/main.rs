// Release builds on Windows run as a GUI app (no console window) so launching
// from Explorer behaves like a normal menubar/tray app — parity with macOS,
// which already lives only in the menubar. Debug builds keep the console so
// `println!` diagnostics remain visible during development.
#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

// Startup ASCII-art banner: shown when launched from a terminal (tty), not
// when started by a background service / LaunchAgent whose stdout has no TTY.
mod banner;
#[cfg(test)]
mod benchmarks;
mod complete;
mod config;
mod daemon;
mod dictionary;
mod explain;
mod footprint;
#[cfg(target_os = "linux")]
mod gui;
mod instance;
mod keymap;
mod layout;
mod notify;
mod personal;
mod platform;
mod practice;
mod prefs;
mod settings;
mod spell;
mod status;
mod timing;
mod types;
// The terminal dashboard is Linux/Windows only: on macOS the event tap owns the
// main run loop, so `--gui` is refused there and the whole module would be dead
// code — which is what it was, warning about itself on every macOS build.
#[cfg(not(target_os = "macos"))]
mod tui;

use crate::dictionary::{en_dict, he_dict};
use std::process;
use std::sync::Arc;

const HELP: &str = "\
recast — automatic English/Hebrew layout correction + English autocorrect

Usage: recast [OPTIONS]

Options:
  -g, --gui         Run in the foreground with a terminal dashboard (TUI)
  -w, --window      Run in the foreground with a small control window
                    (Linux only)
  -s, --stop        Stop the running ReCast (Linux and macOS; on Windows,
                    quit it from the tray)
  -f, --foreground  Linux: don't daemonize (implied when run under systemd)
      --keep-others Don't stop instances that are already running (by default a
                    new ReCast replaces the old one — two at once correct every
                    word twice)
      --status      Print what is running and what is configured, then exit
      --explain WORD --layout en|he
                    Preview one visible word using an explicit layout, then exit
                    (no keyboard capture, layout changes, or typing)
      --write-config  Write a commented config.toml with every setting in it
                    (never overwrites an existing one), then exit
      --clear-personal-data  Delete locally learned word, correction, and
                    typing-timing files, then exit (stop ReCast first)
  -v, --version     Print the version and exit
  -h, --help        Show this help

Settings:
  Everything below can be set two ways: as the environment variable named
  here, or in <config dir>/recast/config.toml as `key = value` with the
  RECAST_ prefix dropped and the name lowercased — RECAST_SPELL_DIST is
  `spell_dist`. The environment wins where both are set. Run
  `recast --write-config` to get a commented file with all of them in it.

  A service manager starts ReCast on every platform and none of them pass
  the shell environment through, so the file is the one that works when
  ReCast is started the way it is meant to be.

  RECAST_DEBUG=1      Print every word check and switch decision
  RECAST_SPLIT=1      Enable the opt-in missing-space split fallback
  RECAST_SHORT=0      Restrict short (≤3 char) switches to very common words
  RECAST_FREQ=0       Disable the homograph frequency tie-break
  RECAST_SPELL=0      Disable the English spelling autocorrect
  RECAST_SPELL_MIN=n  Shortest word the autocorrect may fix (default 4)
  RECAST_SPELL_RANK=n Worst frequency rank a suggestion may have (default 20000)
  RECAST_SPELL_DIST=n Maximum edit distance, 0 to 3 (0 disables; default 3)
  RECAST_COMPLETE=0   Disable auto-complete (word completion + abbreviations)
  RECAST_COMPLETE_MIN=n  Shortest prefix that will be completed (default 3)
  RECAST_COMPLETE_RANK=n Worst frequency rank a completion may have (default 30000)
  RECAST_PERSONAL=1   Opt in to local word/correction/timing personalization;
                      off by default because its files can contain typed words
  RECAST_EXCLUDE_APPS=  Comma-separated exact application IDs to leave alone:
                      Linux app_id/WM_CLASS, macOS bundle ID, Windows exe name.
                      Case-insensitive; unknown apps are skipped when set.
  RECAST_LAYOUT_ONLY_APPS=  Exact app IDs for layout correction only; spelling,
                      abbreviations, completion, and personalization are off there.
  RECAST_UNDO_SHORTCUT=  none (default), left_ctrl, or right_ctrl to add a
                      single Ctrl tap for undo in addition to the action shortcut.
  RECAST_ACTION_SHORTCUT=  Double-tap key: ctrl (default), left_ctrl, right_ctrl,
                      left_shift, right_shift, or none.
  RECAST_COMPLETION_SHORTCUT=  Single-tap key: right_shift (default), left_shift,
                      left_ctrl, right_ctrl, or none. Must not share an action/undo key.
  RECAST_LAYOUT_BACKEND=  Linux: what drives the keyboard layout — hyprland,
                      sway, kde, gnome, x11 or none. Detected when unset;
                      --status prints what was chosen.

Injection timing (microseconds; only worth touching if corrections come out
scrambled, or if you want them faster and are willing to measure):
  RECAST_INJECT_PRESS_GAP=n     Key-down to key-up (macOS only)
  RECAST_INJECT_KEY_GAP=n       Between injected keys (macOS only)
  RECAST_INJECT_SETTLE=n        After the last event, before listening again
  RECAST_INJECT_HELD_TIMEOUT=n  Longest wait for you to lift a key being retyped
  RECAST_INJECT_TERM_TIMEOUT=n  Longest wait for you to lift space/enter (Linux);
                                the space after a correction cannot be typed
                                until you do
  RECAST_INJECT_HELD_POLL=n     How often those waits re-check
  RECAST_INJECT_DEVICE_SETTLE=n Injector device detection at startup (Linux)
  RECAST_INJECT_LAYOUT_CONFIRM=n  Longest wait for a layout switch to take
                                effect before the correction is given up on
  RECAST_INJECT_LAYOUT_POLL=n   How often that confirmation re-checks
  RECAST_INJECT_BATCH_GAP=n     Between writes of a correction (Linux); 0 sends
                                it as one write, which risks the kernel dropping
                                the end of long words

Auto-complete:
  Default shortcuts (change in Settings or config.toml):
  Tap Right Shift mid-word to finish it; tap again to cycle through the
  next guesses, and once more to get back exactly what you typed.
  Abbreviations expand when a word is finished, and are offered by the
  first tap too; define them one per line as `abbr = expansion` in
  <config dir>/recast/abbrev.txt.

Undo and manual correction (Ctrl tapped twice, quickly):
  After a correction, it puts back what you typed — the layout too, if the
  correction changed it — and leaves that word alone from then on.
  After a word that was left alone *because* you had listed it, the same
  gesture takes it off the list (ignore.txt included) and corrects it.
  For an unchanged word, a valid reading in the other layout takes priority,
  even when both readings are words. This works before or after Space/Enter.
  Undoing a manual conversion does not teach an exception.
  Typing anything else or moving the cursor ends the word gesture.
  On macOS, select text in an editable field and double-tap Ctrl to convert
  its layout. Repeat to restore the original. Requires accessibility selection
  editing; the clipboard is untouched.

Your files (<config dir>/recast/):
  config.toml `key = value` per line, everything under Settings above
  abbrev.txt  `abbr = expansion` per line
  ignore.txt  one word per line, never corrected
  personal/   created only with RECAST_PERSONAL=1; may contain typed words
  The two lists are re-read within a couple of seconds of being edited;
  config.toml is read once, so a change to it takes a restart.";

fn main() {
    // Windows release builds run as a GUI-subsystem app with no console, so
    // reattach the launching terminal's console first — otherwise stdout is not
    // a TTY and the banner never prints. No-op without a parent console.
    #[cfg(target_os = "windows")]
    platform::windows::attach_parent_console();

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args
        .iter()
        .any(|arg| matches!(arg.as_str(), "--explain" | "--layout"))
    {
        if let Err(error) = explain::run(&args) {
            eprintln!("{error}");
            process::exit(2);
        }
        return;
    }
    let mut with_gui = false;
    let mut with_window = false;
    let mut with_kill = false;
    let mut with_foreground = false;
    let mut keep_others = false;
    for arg in &args {
        match arg.as_str() {
            "-g" | "--gui" => with_gui = true,
            "-w" | "--window" => with_window = true,
            "-s" | "--stop" => with_kill = true,
            "-f" | "--foreground" => with_foreground = true,
            "--keep-others" => keep_others = true,
            "--status" => {
                status::print();
                return;
            }
            "--write-config" => {
                match settings::write_sample() {
                    Ok(path) => {
                        println!("Wrote {}", path.display());
                        println!("Every setting is in there, commented out, showing its default.");
                    }
                    Err(why) => {
                        eprintln!("{why}");
                        process::exit(1);
                    }
                }
                return;
            }
            "--clear-personal-data" => {
                match personal::clear_data() {
                    Ok(Some(path)) => println!("Cleared personal data from {}.", path.display()),
                    Ok(None) => println!("No personal data directory is available on this OS."),
                    Err(why) => {
                        eprintln!("Failed to clear personal data: {why}");
                        process::exit(1);
                    }
                }
                return;
            }
            "-v" | "-V" | "--version" => {
                println!("recast {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            "-h" | "--help" => {
                println!("{HELP}");
                return;
            }
            other => {
                eprintln!("Unknown option: {other}\n\n{HELP}");
                process::exit(2);
            }
        }
    }

    if with_kill {
        match daemon::stop_daemon() {
            Ok(daemon::Stopped::Signalled(pid)) => {
                println!("Stopped ReCast (pid {pid}).");
            }
            Ok(daemon::Stopped::Stale) => {
                println!("ReCast is not running — cleared a stale pidfile.");
            }
            Ok(daemon::Stopped::NotRunning) => {
                println!("ReCast is not running.");
            }
            // Not an error the user made, but not a stop either: say which it
            // is and how to actually do it here.
            Ok(daemon::Stopped::Unsupported(how)) => {
                eprintln!("--stop is not supported on this platform.");
                eprintln!("To stop it: {how}");
                process::exit(1);
            }
            Err(e) => {
                eprintln!("Failed to stop ReCast: {e}");
                process::exit(1);
            }
        }
        return;
    }

    require_readable_config();

    if banner::ran_from_terminal() {
        banner::print_logo();
    }

    // Before anything is opened or listened on: an old instance still holding
    // its injector and its device threads would correct every word alongside
    // this one. `--keep-others` is for the rare deliberate second copy.
    if !keep_others {
        clear_the_way();
    }

    // Every complaint the settings raise is about something the user wrote and
    // ReCast could not use. Printed here rather than only under `--status`,
    // because a daemon launched at login is one nobody runs `--status` on until
    // they have already spent a while wondering why their setting did nothing.
    for complaint in
        settings::complaints(config::NUMERIC_KEYS, config::BOOLEAN_KEYS, config::ALL_KEYS)
    {
        eprintln!("Warning: {complaint}");
    }

    let cfg = config::Config::from_env();
    let en = en_dict();
    let he = he_dict();
    // The switch is remembered across restarts: someone who turned correction
    // off should not have it turned back on for them by a reboot.
    let enabled = prefs::load_enabled();
    if !enabled {
        println!(
            "Correction is switched off from last time — turn it back on from the tray or TUI."
        );
    }
    let control = Arc::new(types::AppControl::new_with_config_and_state(cfg, enabled));

    #[cfg(not(target_os = "linux"))]
    if with_window {
        eprintln!("--window is Linux-only; ignoring (use the tray menu instead).");
    }
    #[cfg(not(target_os = "linux"))]
    let _ = with_foreground;

    // Each platform owns its entire startup sequence in its own module, so a
    // change to one OS's launch path can't reach into another's. `main` only
    // parses arguments and dispatches; the per-OS `start` functions live in
    // `platform/{linux,macos,windows}.rs`.
    #[cfg(target_os = "linux")]
    platform::linux::start(en, he, control, with_gui, with_window, with_foreground);

    #[cfg(target_os = "macos")]
    platform::macos::start(en, he, control, with_gui);

    #[cfg(target_os = "windows")]
    platform::windows::start(en, he, control, with_gui);
}

/// Make sure this is the only ReCast running, and say what that took.
///
/// Silent when nothing else was running, which is almost always. Everything it
/// does print is something the user would otherwise have to work out from
/// symptoms: a service that is now off, or a duplicate that could not be
/// stopped and is about to double every correction.
fn clear_the_way() {
    let mut supervised = None;
    for (pid, outcome) in instance::replace_running() {
        match outcome {
            instance::Outcome::Ended => {
                println!("Replaced the running ReCast (pid {pid}).");
            }
            instance::Outcome::EndedService(restart) => {
                println!("Replaced the running ReCast service (pid {pid}).");
                println!("  The service stays stopped until you start it again: {restart}");
            }
            // Nothing was signalled — see `instance`. Collected rather than
            // printed in the loop so the exit happens after every one of them
            // has been named.
            instance::Outcome::Supervised(stop) => {
                eprintln!("ReCast is already running under a service manager (pid {pid}).");
                supervised = Some(stop);
            }
            instance::Outcome::Survived => {
                eprintln!(
                    "Warning: could not stop the ReCast already running (pid {pid}) — \
                     with two running, every correction happens twice."
                );
            }
        }
    }
    if let Some(stop) = supervised {
        eprintln!(
            "Stopping it here would only make the service manager start it again.\n\
             To stop it: {stop}"
        );
        process::exit(1);
    }
}

fn require_readable_config() {
    if let Err(error) = settings::check_readable() {
        eprintln!("{error}; refusing to discard configured settings.");
        process::exit(1);
    }
}
