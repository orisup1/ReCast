//! Small pieces of state ReCast keeps between runs, and the OS hooks for
//! starting itself at login.
//!
//! Everything here is best-effort by design: a missing config directory, a
//! read-only home, a `launchctl` that refuses — none of it is worth failing
//! startup over, so every function degrades to "the shipped default" rather
//! than propagating an error nobody could act on. What it stores is
//! deliberately tiny (a switch, a marker); the *user's* files, `abbrev.txt` and
//! `ignore.txt`, stay in `crate::complete` where they are read.

use crate::complete::user_path;

/// The enabled/disabled switch, remembered across restarts. Turning the app off
/// is a decision about how the machine should behave, not about this run of the
/// process — before this, a reboot (or a `systemctl restart`) silently undid it
/// and text started being rewritten again with no one having asked.
const STATE_FILE: &str = "state.txt";

/// Marker for "this user has seen a correction happen at least once", written
/// the first time one lands. Its existence is the whole content — see
/// [`crate::notify::first_correction_hint`].
const WELCOME_FILE: &str = "welcomed";

/// Read the remembered enabled state, defaulting to on for a first run or an
/// unreadable file.
pub fn load_enabled() -> bool {
    user_path(STATE_FILE)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .as_deref()
        .map(parse_enabled)
        .unwrap_or(true)
}

/// The switch as written in the state file. Anything unrecognised reads as
/// enabled: a file we can't make sense of should leave the app working, not
/// silently switched off with no way to tell why.
fn parse_enabled(text: &str) -> bool {
    for line in text.lines() {
        if let Some(value) = line.trim().strip_prefix("enabled=") {
            return value.trim() != "0";
        }
    }
    true
}

/// Remember the enabled state. Silently does nothing if the config directory
/// can't be created — the app still works, it just forgets.
pub fn save_enabled(enabled: bool) {
    write_user_file(
        STATE_FILE,
        &format!(
            "# Written by ReCast — the state of the Enable/Disable switch.\nenabled={}\n",
            u8::from(enabled)
        ),
    );
}

/// Whether the user has already been told, once, that corrections can be taken
/// back.
pub fn welcomed() -> bool {
    user_path(WELCOME_FILE).is_some_and(|p| p.exists())
}

/// Record that the one-time hint has been shown.
pub fn mark_welcomed() {
    write_user_file(
        WELCOME_FILE,
        "ReCast has shown its one-time hint about the undo gesture.\n\
         Delete this file to see it again.\n",
    );
}

/// Write one of our own small files under the config directory, creating the
/// directory if it isn't there yet.
fn write_user_file(name: &str, contents: &str) {
    let Some(path) = user_path(name) else {
        return;
    };
    if let Some(dir) = path.parent() {
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
    }
    let _ = std::fs::write(path, contents);
}

// ---------------------------------------------------------------------------
// Start at login
// ---------------------------------------------------------------------------
//
// `make service` / `deploy.ps1 -Target service` already register ReCast at
// login, but they are the install path — someone who was handed the binary, or
// who ran it once to try it, has no way to say "keep doing this" without going
// back to a shell. That is the gap the tray checkbox fills, so it writes the
// same launchd agent / Run entry those scripts do.

/// Whether ReCast is registered to start at login. `None` on platforms where
/// this isn't wired up, which is how the tray knows to leave the item out.
pub fn autostart_enabled() -> Option<bool> {
    #[cfg(target_os = "macos")]
    {
        Some(macos_autostart::is_enabled())
    }
    #[cfg(target_os = "windows")]
    {
        Some(windows_autostart::is_enabled())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        None
    }
}

/// Register or unregister ReCast at login. Returns whether the change stuck, so
/// the caller can leave the menu item alone rather than lie about it.
///
/// Only the tray checkbox calls it; Linux registers its autostart through
/// `make service` (a systemd user unit), which is not something to toggle from
/// under it.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn set_autostart(enable: bool) -> bool {
    #[cfg(target_os = "macos")]
    {
        macos_autostart::set(enable)
    }
    #[cfg(target_os = "windows")]
    {
        windows_autostart::set(enable)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        false
    }
}

#[cfg(target_os = "macos")]
mod macos_autostart {
    use std::path::PathBuf;
    use std::process::Command;

    /// Same label the Makefile's `service-macos` target uses, so the checkbox
    /// and the install script own one agent between them rather than two that
    /// would both launch a copy.
    const LABEL: &str = "org.recast";

    fn plist_path() -> Option<PathBuf> {
        Some(
            dirs::home_dir()?
                .join("Library/LaunchAgents")
                .join(format!("{LABEL}.plist")),
        )
    }

    pub fn is_enabled() -> bool {
        plist_path().is_some_and(|p| p.exists())
    }

    pub fn set(enable: bool) -> bool {
        let Some(path) = plist_path() else {
            return false;
        };
        if !enable {
            let _ = Command::new("launchctl").arg("unload").arg("-w").arg(&path).status();
            return std::fs::remove_file(&path).is_ok();
        }
        let Ok(exe) = std::env::current_exe() else {
            return false;
        };
        if let Some(dir) = path.parent() {
            if std::fs::create_dir_all(dir).is_err() {
                return false;
            }
        }
        // KeepAlive matches the Makefile's agent: the point of asking for this
        // is that ReCast is running, and a tap the OS tore down should come
        // back rather than quietly stay dead until the next login.
        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
    </array>
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key><true/>
    <key>StandardOutPath</key><string>/tmp/recast.out.log</string>
    <key>StandardErrorPath</key><string>/tmp/recast.err.log</string>
</dict>
</plist>
"#,
            exe.display()
        );
        if std::fs::write(&path, plist).is_err() {
            return false;
        }
        // Already-loaded is the normal case when the running copy *is* the
        // agent, so an unload failure says nothing and is ignored.
        let _ = Command::new("launchctl").arg("unload").arg(&path).status();
        Command::new("launchctl")
            .arg("load")
            .arg("-w")
            .arg(&path)
            .status()
            .is_ok_and(|s| s.success())
    }
}

#[cfg(test)]
mod tests {
    use super::parse_enabled;

    #[test]
    fn reads_the_switch_back() {
        assert!(!parse_enabled("enabled=0\n"));
        assert!(parse_enabled("enabled=1\n"));
        assert!(!parse_enabled("# a comment\nenabled=0\n"));
    }

    #[test]
    fn an_unreadable_state_file_leaves_recast_working() {
        assert!(parse_enabled(""));
        assert!(parse_enabled("nonsense\n"));
        // Not the key we write, so not an answer to the question.
        assert!(parse_enabled("disabled=1\n"));
    }
}

#[cfg(target_os = "windows")]
mod windows_autostart {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use std::ptr;

    use winapi::shared::winerror::ERROR_SUCCESS;
    use winapi::um::winnt::{KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SZ};
    use winapi::um::winreg::{
        RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
        HKEY_CURRENT_USER,
    };

    /// The per-user Run key: entries here start at logon without needing
    /// administrator rights or a scheduled task, which is what makes this
    /// something a menu checkbox can do at all.
    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const VALUE_NAME: &str = "ReCast";

    fn wide(s: &str) -> Vec<u16> {
        OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
    }

    pub fn is_enabled() -> bool {
        let subkey = wide(RUN_KEY);
        let name = wide(VALUE_NAME);
        unsafe {
            let mut hkey = ptr::null_mut();
            if RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, KEY_QUERY_VALUE, &mut hkey)
                != ERROR_SUCCESS as i32
            {
                return false;
            }
            let mut size = 0u32;
            let found = RegQueryValueExW(
                hkey,
                name.as_ptr(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                &mut size,
            ) == ERROR_SUCCESS as i32;
            RegCloseKey(hkey);
            found
        }
    }

    pub fn set(enable: bool) -> bool {
        let subkey = wide(RUN_KEY);
        let name = wide(VALUE_NAME);
        unsafe {
            let mut hkey = ptr::null_mut();
            if RegOpenKeyExW(
                HKEY_CURRENT_USER,
                subkey.as_ptr(),
                0,
                KEY_SET_VALUE | KEY_QUERY_VALUE,
                &mut hkey,
            ) != ERROR_SUCCESS as i32
            {
                return false;
            }
            let ok = if enable {
                // Quoted: a path with spaces ("C:\Program Files\...") is one
                // argument, and the Run key hands the value to the shell.
                match std::env::current_exe() {
                    Ok(exe) => {
                        let value = wide(&format!("\"{}\"", exe.display()));
                        RegSetValueExW(
                            hkey,
                            name.as_ptr(),
                            0,
                            REG_SZ,
                            value.as_ptr() as *const u8,
                            (value.len() * std::mem::size_of::<u16>()) as u32,
                        ) == ERROR_SUCCESS as i32
                    }
                    Err(_) => false,
                }
            } else {
                RegDeleteValueW(hkey, name.as_ptr()) == ERROR_SUCCESS as i32
            };
            RegCloseKey(hkey);
            ok
        }
    }
}
