// Release builds on Windows run as a GUI app (no console window) so launching
// from Explorer behaves like a normal menubar/tray app — parity with macOS,
// which already lives only in the menubar. Debug builds keep the console so
// `println!` diagnostics remain visible during development.
#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

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
use std::thread;
use std::time::Duration;
use crate::dictionary::{en_dict, he_dict};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let with_gui = args.iter().any(|a| a == "-g" || a == "--gui");
    let with_kill = args.iter().any(|a| a == "-s" || a == "--stop");

    let cfg = config::Config::from_env();
    let en = en_dict();
    let he = he_dict();
    let control = Arc::new(types::AppControl::new_with_config(cfg.clone()));

    if with_kill {
        // Try to kill existing daemon using pidfile
        if let Err(e) = daemon::stop_daemon() {
            eprintln!("Failed to stop daemon: {e}");
            process::exit(1);
        }
        println!("Stopped recast daemon.");
        return;
    }

    #[cfg(target_os = "linux")]
    {
        if with_gui {
            let listener_control = Arc::clone(&control);
            thread::spawn(move || {
                platform::linux::run(en.clone(), he.clone(), listener_control);
            });
            if let Err(e) = tui::run_tui(control) {
                eprintln!("TUI error: {e}");
            }
            return;
        }
        // Daemonize: fork and let parent exit, child continues
        daemon::daemonize();
        // Daemon mode: write pidfile then run listener
        if let Err(e) = daemon::write_pidfile() {
            eprintln!("Failed to write pidfile: {e}");
            process::exit(1);
        }
        platform::linux::run(en.clone(), he.clone(), control);
    }

    #[cfg(target_os = "macos")]
    {
        if with_gui {
            let listener_control = Arc::clone(&control);
            thread::spawn(move || {
                platform::macos::run(en, he, listener_control);
            });
            if let Err(e) = tui::run_tui(control) {
                eprintln!("TUI error: {e}");
            }
            return;
        }
        // On macOS we don't daemonize; just run with tray (or GUI ignored)
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
        // Windows: run listener thread and tray; no daemonization
        let listener_control = Arc::clone(&control);
        thread::spawn(move || {
            platform::windows::run(en, he, listener_control);
        });
        platform::tray::run(control);
    }
}