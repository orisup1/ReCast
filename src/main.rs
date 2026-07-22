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
mod dictionary;
#[cfg(target_os = "linux")]
mod gui;
mod keymap;
mod layout;
mod platform;
mod types;
mod config;
mod daemon;
mod tui;

use std::sync::Arc;
use std::process;
use crate::dictionary::{en_dict, he_dict};

const HELP: &str = "\
recast — automatic English/Hebrew keyboard-layout correction

Usage: recast [OPTIONS]

Options:
  -g, --gui         Run in the foreground with a terminal dashboard (TUI)
  -w, --window      Run in the foreground with a small control window
                    (Linux only)
  -s, --stop        Stop a running recast daemon (via its pidfile)
  -f, --foreground  Linux: don't daemonize (implied when run under systemd)
  -h, --help        Show this help

Environment:
  RECAST_DEBUG=1    Print every word check and switch decision
  RECAST_SPLIT=1    Enable the opt-in missing-space split fallback
  RECAST_SHORT=0    Never auto-switch on short (≤3 char) words";

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

if banner::ran_from_terminal() {
  banner::print_logo();
}

    let cfg = config::Config::from_env();
    let en = en_dict();
    let he = he_dict();
    let control = Arc::new(types::AppControl::new_with_config(cfg));

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
