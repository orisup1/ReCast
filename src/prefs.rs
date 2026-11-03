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
// back to a shell. The tray checkbox writes the same launchd agent or toggles
// the Windows installer's existing task. Windows copies installed without a
// task use the per-user Run entry instead.

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

#[cfg(any(target_os = "windows", test))]
fn update_windows_registration<T>(
    task_exists: bool,
    enabled: bool,
    previous: Option<&T>,
    desired: Option<&T>,
    mut write_run: impl FnMut(Option<&T>) -> bool,
    set_task_enabled: impl FnOnce(bool) -> bool,
) -> bool {
    if !write_run(if task_exists { None } else { desired }) {
        return false;
    }
    if !task_exists || set_task_enabled(enabled) {
        return true;
    }
    // A failed task update must not discard the previous login registration.
    let _ = write_run(previous);
    false
}

#[cfg(any(target_os = "macos", target_os = "windows", test))]
const AUTOSTART_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(5);

#[cfg(any(target_os = "macos", target_os = "windows", test))]
fn cached_autostart_enabled(
    cached: &mut Option<(std::time::Instant, bool)>,
    now: impl Fn() -> std::time::Instant,
    query: impl FnOnce() -> Result<Option<bool>, ()>,
) -> bool {
    if let Some((updated, enabled)) = *cached {
        if now().saturating_duration_since(updated) < AUTOSTART_CACHE_TTL {
            return enabled;
        }
    }
    let enabled = query().is_ok_and(|state| state == Some(true));
    // Timestamp completion so a slow query does not expire its own result.
    *cached = Some((now(), enabled));
    enabled
}

#[cfg(any(target_os = "macos", test))]
mod macos_autostart {
    use std::path::{Path, PathBuf};
    #[cfg(target_os = "macos")]
    use std::process::Command;

    /// Same label the Makefile's `service-macos` target uses, so the checkbox
    /// and the install script own one agent between them rather than two that
    /// would both launch a copy.
    const LABEL: &str = "org.recast";

    #[cfg(target_os = "macos")]
    fn plist_path() -> Option<PathBuf> {
        Some(
            dirs::home_dir()?
                .join("Library/LaunchAgents")
                .join(format!("{LABEL}.plist")),
        )
    }

    #[cfg(target_os = "macos")]
    fn cached_state() -> &'static std::sync::Mutex<Option<(std::time::Instant, bool)>> {
        static STATE: std::sync::Mutex<Option<(std::time::Instant, bool)>> =
            std::sync::Mutex::new(None);
        &STATE
    }

    #[cfg(target_os = "macos")]
    pub fn is_enabled() -> bool {
        if !plist_path().is_some_and(|path| path.exists()) {
            return false;
        }
        let Ok(mut cached) = cached_state().lock() else {
            return true;
        };
        super::cached_autostart_enabled(&mut cached, std::time::Instant::now, || {
            let domain = format!("gui/{}", unsafe { nix::libc::getuid() });
            let disabled = Command::new("/bin/launchctl")
                .args(["print-disabled", &domain])
                .output()
                .ok()
                .filter(|out| out.status.success())
                .and_then(|out| launch_agent_disabled(&String::from_utf8_lossy(&out.stdout)));
            // Keep the previous file-based answer if launchd cannot be queried.
            Ok(Some(disabled != Some(true)))
        })
    }

    fn launch_agent_disabled(listing: &str) -> Option<bool> {
        let mut lines = listing.lines().map(str::trim);
        if lines.next()? != "disabled services = {" {
            return None;
        }
        for line in lines {
            if line == "}" {
                return Some(false);
            }
            if let Some((key, value)) = line.split_once("=>") {
                if key.trim() == format!("\"{LABEL}\"") {
                    return match value.trim().trim_end_matches([',', ';']) {
                        "true" | "disabled" => Some(true),
                        "false" | "enabled" => Some(false),
                        _ => None,
                    };
                }
            }
        }
        None
    }

    #[cfg(target_os = "macos")]
    pub fn set(enable: bool) -> bool {
        let Some(path) = plist_path() else {
            return false;
        };
        let Ok(exe) = std::env::current_exe() else {
            return false;
        };
        let changed = set_with(
            &path,
            &exe,
            unsafe { nix::libc::getuid() },
            enable,
            |args| {
                Command::new("/bin/launchctl")
                    .args(args)
                    .status()
                    .is_ok_and(|status| status.success())
            },
        );
        if let Ok(mut cached) = cached_state().lock() {
            *cached = changed.then(|| (std::time::Instant::now(), enable));
        }
        changed
    }

    fn set_with(
        path: &Path,
        exe: &Path,
        uid: u32,
        enable: bool,
        mut run: impl FnMut(&[&str]) -> bool,
    ) -> bool {
        if std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            return false;
        }
        let previous = match std::fs::read_to_string(path) {
            Ok(text) => Some(text),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return false,
        };
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
            exe.to_string_lossy()
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
        );
        let changed = if enable {
            crate::settings::write_atomic(path, &plist)
        } else {
            match std::fs::remove_file(path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                result => result,
            }
        };
        if changed.is_err() {
            return false;
        }
        // Change future login registration without stopping this process or
        // starting another copy. Loading/unloading the agent would do both.
        let target = format!("gui/{uid}/{LABEL}");
        if run(&[if enable { "enable" } else { "disable" }, &target]) {
            return true;
        }
        // A failed launchd update must not replace or discard the old plist.
        if let Some(previous) = previous {
            let _ = crate::settings::write_atomic(path, &previous);
        } else {
            let _ = std::fs::remove_file(path);
        }
        false
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn test_dir(name: &str) -> PathBuf {
            let dir = std::env::temp_dir().join(format!(
                "recast-macos-autostart-{name}-{}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            dir
        }

        #[test]
        fn disabled_overrides_match_only_the_recast_service() {
            assert_eq!(
                launch_agent_disabled("disabled services = {\n \"org.recast\" => disabled\n}\n"),
                Some(true)
            );
            assert_eq!(
                launch_agent_disabled("disabled services = {\n \"org.recast\" => enabled\n}\n"),
                Some(false)
            );
            assert_eq!(
                launch_agent_disabled("disabled services = {\n \"org.recast\" => true\n}\n"),
                Some(true)
            );
            assert_eq!(
                launch_agent_disabled("disabled services = {\n \"org.recast\" => false\n}\n"),
                Some(false)
            );
            assert_eq!(
                launch_agent_disabled("disabled services = {\n \"org.recast.other\" => true\n}\n"),
                Some(false)
            );
            assert_eq!(launch_agent_disabled("unrecognized response"), None);
            assert_eq!(
                launch_agent_disabled("disabled services = {\n \"org.recast\" => invalid\n}\n"),
                None
            );
        }

        #[test]
        fn toggling_registration_preserves_the_running_launch_agent() {
            let dir = test_dir("toggle");
            let path = dir.join("org.recast.plist");
            let exe = Path::new("/Applications/ReCast.app/Contents/MacOS/recast");
            for enable in [true, true, false, false] {
                let mut commands = Vec::new();
                assert!(set_with(&path, exe, 501, enable, |args| {
                    commands.push(args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>());
                    true
                }));
                assert_eq!(
                    commands,
                    [vec![
                        if enable { "enable" } else { "disable" }.to_string(),
                        "gui/501/org.recast".into()
                    ]]
                );
                assert_eq!(path.exists(), enable);
                if enable {
                    assert!(std::fs::read_to_string(&path)
                        .unwrap()
                        .contains("<key>KeepAlive</key><true/>"));
                }
            }
            std::fs::remove_dir_all(dir).unwrap();
        }

        #[test]
        fn failed_registration_commands_preserve_the_previous_plist() {
            let dir = test_dir("failure");
            let path = dir.join("org.recast.plist");
            let exe = Path::new("/Applications/new-recast");
            for previous in [None, Some("previous registration\n")] {
                for enable in [false, true] {
                    match previous {
                        Some(text) => std::fs::write(&path, text).unwrap(),
                        None => {
                            let _ = std::fs::remove_file(&path);
                        }
                    }
                    assert!(!set_with(&path, exe, 501, enable, |_| false));
                    assert_eq!(std::fs::read_to_string(&path).ok().as_deref(), previous);
                }
            }
            std::fs::remove_dir_all(dir).unwrap();
        }

        #[test]
        fn failed_file_updates_do_not_change_launchd_registration() {
            let dir = test_dir("file-failure");
            let path = dir.join("org.recast.plist");
            let exe = Path::new("/Applications/recast");
            std::fs::create_dir(&path).unwrap();
            for enable in [false, true] {
                assert!(!set_with(&path, exe, 501, enable, |_| {
                    panic!("unreadable registration must leave launchd untouched")
                }));
            }
            std::fs::remove_dir(&path).unwrap();
            std::fs::write(&path, "previous registration\n").unwrap();
            let temp = path.with_extension(format!("{}.tmp", std::process::id()));
            std::fs::write(&temp, "occupied").unwrap();
            assert!(!set_with(&path, exe, 501, true, |_| {
                panic!("failed save must leave launchd untouched")
            }));
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                "previous registration\n"
            );
            std::fs::remove_dir_all(dir).unwrap();
        }

        #[test]
        fn launch_agent_paths_escape_xml_metacharacters() {
            let dir = test_dir("xml");
            let path = dir.join("org.recast.plist");
            let exe = Path::new("/Applications/Work & <Play>/ReCast.app/Contents/MacOS/recast");
            assert!(set_with(&path, exe, 501, true, |_| true));
            let plist = std::fs::read_to_string(path).unwrap();
            assert!(plist.contains(
                "/Applications/Work &amp; &lt;Play&gt;/ReCast.app/Contents/MacOS/recast"
            ));
            std::fs::remove_dir_all(dir).unwrap();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        cached_autostart_enabled, parse_enabled, update_windows_registration, AUTOSTART_CACHE_TTL,
    };

    #[test]
    fn windows_task_cache_refreshes_external_changes_and_recovers_from_errors() {
        use std::{cell::Cell, time::Instant};

        let clock = Cell::new(Instant::now());
        let task = Cell::new(Ok(None));
        let queries = Cell::new(0);
        let read = |cache: &mut Option<(Instant, bool)>| {
            cached_autostart_enabled(
                cache,
                || clock.get(),
                || {
                    queries.set(queries.get() + 1);
                    task.get()
                },
            )
        };
        let mut cache = None;
        assert!(!read(&mut cache));
        assert_eq!(queries.get(), 1);

        for (state, expected) in [(Ok(Some(true)), true), (Ok(Some(false)), false)] {
            let previous = !expected;
            task.set(state);
            let before = queries.get();
            clock.set(clock.get() + AUTOSTART_CACHE_TTL - std::time::Duration::from_millis(1));
            assert_eq!(read(&mut cache), previous);
            assert_eq!(queries.get(), before, "rendering must reuse a fresh result");
            clock.set(clock.get() + std::time::Duration::from_millis(1));
            assert_eq!(read(&mut cache), expected);
            assert_eq!(queries.get(), before + 1);
        }

        task.set(Err(()));
        clock.set(clock.get() + AUTOSTART_CACHE_TTL);
        assert!(!read(&mut cache));
        let before = queries.get();
        task.set(Ok(Some(true)));
        assert!(!read(&mut cache));
        assert_eq!(queries.get(), before);
        clock.set(clock.get() + AUTOSTART_CACHE_TTL);
        assert!(read(&mut cache), "a query failure must expire too");
        assert_eq!(queries.get(), before + 1);
    }

    #[test]
    fn failed_task_updates_restore_the_previous_run_value() {
        for enabled in [false, true] {
            let previous = Some("previous executable");
            let mut saved = previous;
            let mut writes = Vec::new();
            assert!(!update_windows_registration(
                true,
                enabled,
                previous.as_ref(),
                Some(&"new executable"),
                |value| {
                    saved = value.copied();
                    writes.push(saved);
                    true
                },
                |_| false,
            ));
            assert_eq!(saved, previous);
            assert_eq!(writes, [None, previous]);
        }
    }

    #[test]
    fn a_failed_run_update_preserves_the_scheduled_registration() {
        assert!(!update_windows_registration(
            true,
            false,
            Some(&"previous executable"),
            Some(&"new executable"),
            |_| false,
            |_| panic!("the scheduled task must survive a failed registry write"),
        ));
    }

    #[test]
    fn an_existing_task_keeps_its_definition_and_running_instance() {
        for initial_enabled in [false, true] {
            for enabled in [false, true] {
                let definition = ("installed executable", "logon", 3, 60);
                let mut task = (initial_enabled, definition, true);
                let previous = Some("duplicate Run entry");
                let mut saved = previous;
                assert!(update_windows_registration(
                    true,
                    enabled,
                    previous.as_ref(),
                    Some(&"new executable"),
                    |value| {
                        saved = value.copied();
                        true
                    },
                    |enabled| {
                        task.0 = enabled;
                        true
                    },
                ));
                assert_eq!(saved, None);
                assert_eq!(task, (enabled, definition, true));
            }
        }
    }

    #[test]
    fn an_install_without_a_task_keeps_using_the_run_entry() {
        for enabled in [false, true] {
            let previous = Some("previous executable");
            let desired = enabled.then_some("new executable");
            let mut saved = previous;
            assert!(update_windows_registration(
                false,
                enabled,
                previous.as_ref(),
                desired.as_ref(),
                |value| {
                    saved = value.copied();
                    true
                },
                |_| panic!("the UI must not create a scheduled task"),
            ));
            assert_eq!(saved, desired);
        }
    }

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
    use std::os::windows::process::CommandExt;
    use std::ptr;
    use std::sync::Mutex;
    use std::time::Instant;

    use winapi::shared::winerror::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use winapi::um::winbase::CREATE_NO_WINDOW;
    use winapi::um::winnt::{KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SZ};
    use winapi::um::winreg::{
        RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW,
        RegSetValueExW, HKEY_CURRENT_USER,
    };

    /// Copies installed without a Scheduled Task use the per-user Run key.
    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const VALUE_NAME: &str = "ReCast";
    static TASK_ENABLED: Mutex<Option<(Instant, bool)>> = Mutex::new(None);

    // Enumerate instead of treating every failed GetTask as "not found". Access
    // errors must fail a change rather than leave a second active registration.
    const SCHEDULED_TASK: &str = r#"
$ErrorActionPreference = 'Stop'
try {
    $service = New-Object -ComObject 'Schedule.Service'
    $service.Connect()
    $folder = $service.GetFolder('\')
    $task = $folder.GetTasks(1) | Where-Object { $_.Name -eq 'recast' } | Select-Object -First 1
    if ($null -eq $task) {
        if ($env:RECAST_TASK_ACTION -ne 'query') { exit 1 }
        Write-Output 'absent'; exit 0
    }
    $principal = $task.Definition.Principal.UserId
    $current = [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
    if ($principal -match '^S-\d-') {
        $owner = $principal
    } else {
        $owner = ([System.Security.Principal.NTAccount]::new($principal)).Translate([System.Security.Principal.SecurityIdentifier]).Value
    }
    if ($owner -ne $current) {
        if ($env:RECAST_TASK_ACTION -ne 'query') { exit 1 }
        Write-Output 'absent'; exit 0
    }
    if ($env:RECAST_TASK_ACTION -eq 'enable') { $task.Enabled = $true }
    elseif ($env:RECAST_TASK_ACTION -eq 'disable') { $task.Enabled = $false }
    if ($task.Enabled) {
        Write-Output 'enabled'
    } else {
        Write-Output 'disabled'
    }
    exit 0
} catch { exit 1 }
"#;

    struct RunValue {
        kind: u32,
        bytes: Vec<u8>,
    }

    fn wide(s: &str) -> Vec<u16> {
        OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    fn read_run() -> Result<Option<RunValue>, ()> {
        let subkey = wide(RUN_KEY);
        let name = wide(VALUE_NAME);
        unsafe {
            let mut hkey = ptr::null_mut();
            let opened = RegOpenKeyExW(
                HKEY_CURRENT_USER,
                subkey.as_ptr(),
                0,
                KEY_QUERY_VALUE,
                &mut hkey,
            );
            if opened == ERROR_FILE_NOT_FOUND as i32 {
                return Ok(None);
            }
            if opened != ERROR_SUCCESS as i32 {
                return Err(());
            }
            let result = (|| {
                let mut size = 0u32;
                let mut kind = 0u32;
                let found = RegQueryValueExW(
                    hkey,
                    name.as_ptr(),
                    ptr::null_mut(),
                    &mut kind,
                    ptr::null_mut(),
                    &mut size,
                );
                if found == ERROR_FILE_NOT_FOUND as i32 {
                    return Ok(None);
                }
                if found != ERROR_SUCCESS as i32 || size > 1024 * 1024 {
                    return Err(());
                }
                let mut bytes = vec![0; size as usize];
                if RegQueryValueExW(
                    hkey,
                    name.as_ptr(),
                    ptr::null_mut(),
                    &mut kind,
                    bytes.as_mut_ptr(),
                    &mut size,
                ) != ERROR_SUCCESS as i32
                {
                    return Err(());
                }
                bytes.truncate(size as usize);
                Ok(Some(RunValue { kind, bytes }))
            })();
            RegCloseKey(hkey);
            result
        }
    }

    fn write_run(value: Option<&RunValue>) -> bool {
        let subkey = wide(RUN_KEY);
        let name = wide(VALUE_NAME);
        unsafe {
            let mut hkey = ptr::null_mut();
            let opened = if value.is_some() {
                RegCreateKeyExW(
                    HKEY_CURRENT_USER,
                    subkey.as_ptr(),
                    0,
                    ptr::null_mut(),
                    0,
                    KEY_SET_VALUE,
                    ptr::null_mut(),
                    &mut hkey,
                    ptr::null_mut(),
                )
            } else {
                RegOpenKeyExW(
                    HKEY_CURRENT_USER,
                    subkey.as_ptr(),
                    0,
                    KEY_SET_VALUE,
                    &mut hkey,
                )
            };
            if opened == ERROR_FILE_NOT_FOUND as i32 && value.is_none() {
                return true;
            }
            if opened != ERROR_SUCCESS as i32 {
                return false;
            }
            let ok = if let Some(value) = value {
                RegSetValueExW(
                    hkey,
                    name.as_ptr(),
                    0,
                    value.kind,
                    value.bytes.as_ptr(),
                    value.bytes.len() as u32,
                ) == ERROR_SUCCESS as i32
            } else {
                matches!(
                    RegDeleteValueW(hkey, name.as_ptr()) as u32,
                    ERROR_SUCCESS | ERROR_FILE_NOT_FOUND
                )
            };
            RegCloseKey(hkey);
            ok
        }
    }

    fn scheduled_task(enabled: Option<bool>) -> Result<Option<bool>, ()> {
        let output = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", SCHEDULED_TASK])
            .env(
                "RECAST_TASK_ACTION",
                match enabled {
                    Some(true) => "enable",
                    Some(false) => "disable",
                    None => "query",
                },
            )
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|_| ())?;
        if !output.status.success() {
            return Err(());
        }
        match String::from_utf8_lossy(&output.stdout).trim() {
            "enabled" if enabled != Some(false) => Ok(Some(true)),
            "disabled" if enabled != Some(true) => Ok(Some(false)),
            "absent" if enabled.is_none() => Ok(None),
            _ => Err(()),
        }
    }

    pub fn is_enabled() -> bool {
        if read_run().is_ok_and(|value| value.is_some()) {
            return true;
        }
        // Limit per-frame queries while still noticing external task changes.
        let mut cached = crate::types::lock_forgiving(&TASK_ENABLED);
        super::cached_autostart_enabled(&mut cached, Instant::now, || scheduled_task(None))
    }

    pub fn set(enable: bool) -> bool {
        let Ok(task) = scheduled_task(None) else {
            return false;
        };
        let Ok(previous) = read_run() else {
            return false;
        };
        let desired = if enable && task.is_none() {
            let Ok(exe) = std::env::current_exe() else {
                return false;
            };
            Some(RunValue {
                kind: REG_SZ,
                bytes: wide(&format!("\"{}\"", exe.display()))
                    .into_iter()
                    .flat_map(u16::to_le_bytes)
                    .collect(),
            })
        } else {
            None
        };
        let saved = super::update_windows_registration(
            task.is_some(),
            enable,
            previous.as_ref(),
            desired.as_ref(),
            write_run,
            |enabled| scheduled_task(Some(enabled)).is_ok(),
        );
        *crate::types::lock_forgiving(&TASK_ENABLED) =
            saved.then(|| (Instant::now(), task.is_some() && enable));
        saved
    }
}
