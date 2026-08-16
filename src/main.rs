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
mod config;
mod daemon;
mod dictionary;
mod footprint;
#[cfg(target_os = "linux")]
mod gui;
mod instance;
mod keymap;
mod layout;
mod notify;
mod platform;
mod prefs;
mod settings;
mod spell;
mod timing;
mod types;
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
  -s, --stop        Stop the running ReCast (Linux and macOS; on Windows,
                    quit it from the tray)
  -f, --foreground  Linux: don't daemonize (implied when run under systemd)
      --keep-others Don't stop instances that are already running (by default a
                    new ReCast replaces the old one — two at once correct every
                    word twice)
      --status      Print what is running and what is configured, then exit
      --write-config  Write a commented config.toml with every setting in it
                    (never overwrites an existing one), then exit
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
  RECAST_SHORT=0      Never auto-switch on short (≤3 char) words
  RECAST_FREQ=0       Disable the homograph frequency tie-break
  RECAST_SPELL=0      Disable the English spelling autocorrect
  RECAST_SPELL_MIN=n  Shortest word the autocorrect may fix (default 4)
  RECAST_SPELL_RANK=n Worst frequency rank a suggestion may have (default 20000)
  RECAST_SPELL_DIST=n Maximum edit distance, 1 to 3 (default 3)
  RECAST_COMPLETE=0   Disable auto-complete (word completion + abbreviations)
  RECAST_COMPLETE_MIN=n  Shortest prefix that will be completed (default 3)
  RECAST_COMPLETE_RANK=n Worst frequency rank a completion may have (default 30000)
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
  config.toml `key = value` per line, everything under Settings above
  abbrev.txt  `abbr = expansion` per line
  ignore.txt  one word per line, never corrected
  The two lists are re-read within a couple of seconds of being edited;
  config.toml is read once, so a change to it takes a restart.";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
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
                print_status();
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

    // Windows release builds run as a GUI-subsystem app with no console, so
    // reattach the launching terminal's console first — otherwise stdout is not
    // a TTY and the banner never prints. No-op without a parent console.
    #[cfg(target_os = "windows")]
    platform::windows::attach_parent_console();

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
    for complaint in config::complaints() {
        eprintln!("Warning: {complaint}");
    }

    let cfg = config::Config::load();
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
    // …and layout changes made by hand, as they happen rather than within the
    // cache's 300 ms.
    layout::spawn_watcher();

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

/// Answer the two questions a user asks when something isn't happening: is it
/// running, and is it configured the way I think it is.
///
/// Deliberately readable without a running daemon — it reports the state on
/// disk, which is what the next launch will pick up.
fn print_status() {
    println!("recast {}", env!("CARGO_PKG_VERSION"));

    // Linux and macOS write a pidfile, so both can answer this from disk
    // without attaching to anything. Windows does not: its instance is found
    // through the process table instead, which `--status` does not walk.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    match daemon::running_pid() {
        Some(pid) => println!("  running:        yes (pid {pid})"),
        None => println!("  running:        no"),
    }

    println!(
        "  correction:     {}",
        if prefs::load_enabled() { "enabled" } else { "disabled" }
    );
    // The layout pipeline is the headline feature and the one that can be
    // silently unavailable: on a Linux session ReCast cannot drive, mistyped
    // words go through untouched and nothing else says why.
    println!("  layout switch:  {}", layout::describe_backend());
    match prefs::autostart_enabled() {
        Some(true) => println!("  start at login: yes"),
        Some(false) => println!("  start at login: no"),
        None => {}
    }
    match complete::config_dir() {
        Some(dir) => println!("  config dir:     {}", dir.display()),
        None => println!("  config dir:     (none — no OS config directory)"),
    }
    // Whether the file exists is the first thing to check when a setting in it
    // did nothing, and the most likely answer is that it is somewhere else.
    match settings::file_path() {
        Some(path) if path.exists() => println!("  config.toml:    {}", path.display()),
        Some(path) => println!("  config.toml:    none ({} — --write-config makes one)", path.display()),
        None => println!("  config.toml:    (none — no OS config directory)"),
    }
    let (abbrevs, ignored, learned) = complete::list_counts();
    println!("  abbrev.txt:     {abbrevs} abbreviation(s)");
    println!("  ignore.txt:     {ignored} word(s)");
    println!("  learned.txt:    {learned} word(s) retired by undo");
    // This process, not the daemon's — `--status` is a separate invocation and
    // cannot see the running one's figure. Still worth showing: it is the same
    // binary doing the same page-faulting, so it answers "roughly what does
    // this cost" without attaching to anything.
    if let Some(rss) = footprint::rss_human() {
        println!("  memory (this):  {rss}");
    }

    // The other half of "is it configured the way I think it is". Every one of
    // these can be overridden from the environment and none of them used to be
    // reported, so someone who set RECAST_SPELL_DIST had no way to confirm it
    // had been read — least of all when the value was a typo and had silently
    // fallen back to the default.
    let cfg = config::Config::load();
    println!("\n  settings (config.toml and RECAST_* applied):");
    println!("    short words          {}", on_off(cfg.short_enabled));
    println!("    missing-space split  {}", on_off(cfg.split_enabled));
    println!("    frequency tie-break  {}", on_off(cfg.freq_enabled));
    println!(
        "    spelling             {}  (min length {}, max rank {}, max distance {})",
        on_off(cfg.spell_enabled),
        cfg.spell_min_len,
        cfg.spell_max_rank,
        cfg.spell_max_dist,
    );
    println!(
        "    auto-complete        {}  (min prefix {}, max rank {})",
        on_off(cfg.complete_enabled),
        cfg.complete_min_len,
        cfg.complete_max_rank,
    );

    for complaint in config::complaints() {
        eprintln!("\n  ! {complaint}");
    }
}

fn on_off(value: bool) -> &'static str {
    if value {
        "on"
    } else {
        "off"
    }
}

#[cfg(test)]
mod tests {
    /// Every version string in the program is `env!("CARGO_PKG_VERSION")` — the
    /// help text, the tray's About box, the menubar title, the Windows exe
    /// resources — and the `.app` bundle's `Info.plist` is generated from
    /// Cargo.toml by `make bundle-plist`. The README is the one place left that
    /// has to *write* a version down, because its sample `--status` output is
    /// prose rather than something the program renders.
    ///
    /// So it is pinned here instead. A sample that quietly claims an old
    /// release is the kind of documentation error nobody notices for four
    /// versions — which is exactly what happened to the bundle plist, and why
    /// it is generated now.
    #[test]
    fn the_readme_states_the_version_the_binary_reports() {
        let readme = include_str!("../README.md");
        let quoted: Vec<&str> = readme
            .match_indices("recast ")
            .filter_map(|(at, matched)| {
                let token = readme[at + matched.len()..]
                    .split_whitespace()
                    .next()?;
                token.starts_with(|c: char| c.is_ascii_digit()).then_some(token)
            })
            .collect();
        assert!(
            !quoted.is_empty(),
            "the README no longer shows a version — drop this test or put the sample back"
        );
        for found in quoted {
            assert_eq!(
                found,
                env!("CARGO_PKG_VERSION"),
                "README says {found}, Cargo.toml says {}",
                env!("CARGO_PKG_VERSION")
            );
        }
    }
}
