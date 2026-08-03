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
mod complete;
mod dictionary;
#[cfg(target_os = "linux")]
mod gui;
mod keymap;
mod layout;
mod notify;
mod platform;
mod prefs;
mod spell;
mod types;
mod config;
mod daemon;
// The terminal dashboard is Linux/Windows only: on macOS the event tap owns the
// main run loop, so `--gui` is refused there and the whole module would be dead
// code — which is what it was, warning about itself on every macOS build.
#[cfg(not(target_os = "macos"))]
mod tui;

use std::sync::Arc;
use std::process;
use crate::dictionary::{en_dict, he_dict};

const HELP: &str = "\
recast — automatic English/Hebrew layout correction + English autocorrect

Usage: recast [OPTIONS]

Options:
  -g, --gui         Run in the foreground with a terminal dashboard (TUI)
  -w, --window      Run in the foreground with a small control window
                    (Linux only)
  -s, --stop        Stop a running recast daemon (via its pidfile)
  -f, --foreground  Linux: don't daemonize (implied when run under systemd)
      --status      Print what is running and what is configured, then exit
  -v, --version     Print the version and exit
  -h, --help        Show this help

Environment:
  RECAST_DEBUG=1      Print every word check and switch decision
  RECAST_SPLIT=1      Enable the opt-in missing-space split fallback
  RECAST_SHORT=0      Never auto-switch on short (≤3 char) words
  RECAST_FREQ=0       Disable the homograph frequency tie-break
  RECAST_SPELL=0      Disable the English spelling autocorrect
  RECAST_SPELL_MIN=n  Shortest word the autocorrect may fix (default 4)
  RECAST_SPELL_RANK=n Worst frequency rank a suggestion may have (default 20000)
  RECAST_SPELL_DIST=n Maximum edit distance, 1 to 3 (default 3)
  RECAST_COMPLETE=0   Disable auto-complete (word completion + abbreviations)
  RECAST_COMPLETE_MIN=n  Shortest prefix that will be completed (default 3)
  RECAST_COMPLETE_RANK=n Worst frequency rank a completion may have (default 30000)

Auto-complete:
  Tap Right Shift mid-word to finish it; tap again to cycle through the
  next guesses, and once more to get back exactly what you typed.
  Abbreviations expand when a word is finished, and are offered by the
  first tap too; define them one per line as `abbr = expansion` in
  <config dir>/recast/abbrev.txt.

Undo (Ctrl tapped twice, quickly):
  After a correction, it puts back what you typed — the layout too, if the
  correction changed it — and leaves that word alone from then on.
  After a word that was left alone *because* you had listed it, the same
  gesture takes it off the list (ignore.txt included) and corrects it.
  Only the word the cursor is still sitting on: type anything else and the
  gesture has nothing to act on.

Your files (<config dir>/recast/):
  abbrev.txt  `abbr = expansion` per line
  ignore.txt  one word per line, never corrected
  Both are re-read within a couple of seconds of being edited — no restart.";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut with_gui = false;
    let mut with_window = false;
    let mut with_kill = false;
    let mut with_foreground = false;
    for arg in &args {
        match arg.as_str() {
            "-g" | "--gui" => with_gui = true,
            "-w" | "--window" => with_window = true,
            "-s" | "--stop" => with_kill = true,
            "-f" | "--foreground" => with_foreground = true,
            "--status" => {
                print_status();
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
        // Try to kill existing daemon using pidfile.
        if let Err(e) = daemon::stop_daemon() {
            eprintln!("Failed to stop daemon: {e}");
            process::exit(1);
        }
        println!("Stopped recast daemon.");
        return;
    }

// Windows release builds run as a GUI-subsystem app with no console, so
// reattach the launching terminal's console first — otherwise stdout is not a
// TTY and the banner never prints. No-op when launched without a parent console.
#[cfg(target_os = "windows")]
platform::windows::attach_parent_console();

if banner::ran_from_terminal() {
  banner::print_logo();
}

    let cfg = config::Config::from_env();
    let en = en_dict();
    let he = he_dict();
    // The switch is remembered across restarts: someone who turned correction
    // off should not have it turned back on for them by a reboot.
    let enabled = prefs::load_enabled();
    if !enabled {
        println!("Correction is switched off from last time — turn it back on from the tray or TUI.");
    }
    let control = Arc::new(types::AppControl::new_with_config_and_state(cfg, enabled));
    // Pick up edits to abbrev.txt / ignore.txt without a restart.
    complete::spawn_watcher();

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

/// Answer the two questions a user asks when something isn't happening: is it
/// running, and is it configured the way I think it is.
///
/// Deliberately readable without a running daemon — it reports the state on
/// disk, which is what the next launch will pick up.
fn print_status() {
    println!("recast {}", env!("CARGO_PKG_VERSION"));

    #[cfg(target_os = "linux")]
    match daemon::running_pid() {
        Some(pid) => println!("  running:        yes (pid {pid})"),
        None => println!("  running:        no"),
    }

    println!(
        "  correction:     {}",
        if prefs::load_enabled() { "enabled" } else { "disabled" }
    );
    match prefs::autostart_enabled() {
        Some(true) => println!("  start at login: yes"),
        Some(false) => println!("  start at login: no"),
        None => {}
    }
    match complete::config_dir() {
        Some(dir) => println!("  config dir:     {}", dir.display()),
        None => println!("  config dir:     (none — no OS config directory)"),
    }
    let (abbrevs, ignored) = complete::list_counts();
    println!("  abbrev.txt:     {abbrevs} abbreviation(s)");
    println!("  ignore.txt:     {ignored} word(s)");
}
