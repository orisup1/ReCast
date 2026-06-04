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

use std::sync::Arc;
use crate::dictionary::{en_dict, he_dict};
use crate::types::AppControl;


fn main() {
    let with_gui = std::env::args().skip(1).any(|a| a == "-g" || a == "--gui");
    let cfg = config::Config::from_env();
    let en = en_dict();
    let he = he_dict();
    let control = Arc::new(AppControl::new_with_config(cfg.clone()));

    #[cfg(target_os = "linux")]
    {
        if with_gui {
            let listener_control = Arc::clone(&control);
            std::thread::spawn(move || {
                platform::linux::run(en.clone(), he.clone(), listener_control);
            });
            if let Err(e) = gui::run(control) {
                eprintln!("GUI error: {}", e);
            }
            return;
        }
        platform::linux::run(en.clone(), he.clone(), control);
    }

    #[cfg(target_os = "macos")]
    {
        let _ = with_gui; // GUI flag ignored on macOS.
        let _tap = platform::macos::setup_event_tap(en, he, Arc::clone(&control));
        platform::tray::run(control);
    }

    #[cfg(target_os = "windows")]
    {
        let listener_control = Arc::clone(&control);
        std::thread::spawn(move || {
            platform::windows::run(en, he, listener_control);
        });
        platform::tray::run(control);
    }
}

