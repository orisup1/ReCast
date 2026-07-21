// Release builds on Windows run as a GUI app (no console window) so launching
// from Explorer behaves like a normal menubar/tray app — parity with macOS,
// which already lives only in the menubar. Debug builds keep the console so
// `println!` diagnostics remain visible during development.
#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

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
#[cfg(any(target_os = "linux", target_os = "windows"))]
use std::thread;
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

    // Greet interactive users with the logo when launched directly from a
    // terminal — not when started by the macOS LaunchAgent / Windows Scheduled
    // Task, whose stdout is not a TTY.
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

    #[cfg(target_os = "linux")]
    {
        if with_window {
            // Control window: eframe owns the main thread, listener runs in
            // the background.
            let listener_control = Arc::clone(&control);
            thread::spawn(move || {
                platform::linux::run(en, he, listener_control);
            });
            if let Err(e) = gui::run(control) {
                eprintln!("GUI error: {e}");
            }
            return;
        }
        if with_gui {
            let listener_control = Arc::clone(&control);
            thread::spawn(move || {
                platform::linux::run(en, he, listener_control);
            });
            if let Err(e) = tui::run_tui(control) {
                eprintln!("TUI error: {e}");
            }
            return;
        }
        // Daemonize (fork, setsid, detach stdio) unless asked to stay in the
        // foreground. Under systemd (Type=simple) the service manager tracks
        // our PID, so forking would make the unit see the service as dead —
        // INVOCATION_ID is set by systemd and disables the fork automatically.
        let under_systemd = std::env::var_os("INVOCATION_ID").is_some();
        if !with_foreground && !under_systemd {
            daemon::daemonize();
        }
        if let Err(e) = daemon::write_pidfile() {
            eprintln!("Failed to write pidfile: {e}");
            process::exit(1);
        }
        platform::linux::run(en, he, control);
    }

    #[cfg(target_os = "macos")]
    {
        // The event tap must live on the main run loop (see macos.rs), so a
        // main-thread TUI can't coexist with it — the tray is the UI here.
        if with_gui {
            eprintln!("--gui is not supported on macOS; running with the menubar tray instead.");
        }
        let _tap = platform::macos::setup_event_tap(en, he, Arc::clone(&control));
        platform::tray::run(control);
    }

    #[cfg(target_os = "windows")]
    {
        if with_gui {
            let listener_control = Arc::clone(&control);
            thread::spawn(move || {
                platform::windows::run(en, he, listener_control);
            });
            if let Err(e) = tui::run_tui(control) {
                eprintln!("TUI error: {e}");
            }
            return;
        }
        // Windows: run listener thread and tray; no daemonization.
        let listener_control = Arc::clone(&control);
        thread::spawn(move || {
            platform::windows::run(en, he, listener_control);
        });
        platform::tray::run(control);
    }
}
