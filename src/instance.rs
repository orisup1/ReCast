//! One ReCast at a time.
//!
//! Two copies running at once is not a harmless waste — it is broken. Both see
//! the same keystroke, both decide the same word needs correcting, and both
//! inject: the word is erased twice and retyped twice, over the top of itself.
//! On Linux there are also two `recast-injector` uinput devices, and on macOS
//! two event taps, so even the *un*corrected typing goes through twice as much
//! machinery as it should.
//!
//! Nothing prevented it. There was a pidfile, but only the Linux daemon wrote
//! one and nothing read it at startup; launching from a tray, a TUI, a second
//! terminal or a login item all just started another one. The failure looked
//! like ReCast being buggy rather than like ReCast running twice, which is the
//! worst kind of failure to have.
//!
//! So a new instance clears the way for itself: find the others, stop them,
//! wait for them to actually be gone, and only then carry on. The newest wins,
//! because the newest is the one the user just asked for.
//!
//! # The one case where it refuses instead
//!
//! A service manager told to keep ReCast alive will start it again the instant
//! it dies — and the copy it starts runs this same code and stops *us*. Two
//! processes taking turns killing each other is worse than either problem it
//! was meant to solve, so a supervised instance is never signalled. ReCast says
//! how to stop the service and exits, which is the only outcome that still ends
//! with one instance running.
//!
//! macOS is where this bites: the LaunchAgent written by "Start at login" sets
//! `KeepAlive=true` (see `prefs`), so it restarts on *any* exit. Linux's systemd
//! unit uses `Restart=on-failure`, and a SIGTERM is not a failure — it can be
//! stopped, it just will not come back by itself, which is worth saying out
//! loud and is what [`Outcome::EndedService`] is for.

use std::time::{Duration, Instant};

/// How long a previous instance gets to shut down after being asked politely.
///
/// A ceiling, not a cost: the wait ends as soon as the process is gone, which
/// for a program whose shutdown is "the OS reclaims everything" is immediate.
/// The generous ceiling is for the one thing that is not immediate — an
/// instance in the middle of injecting a correction, which should be allowed to
/// finish rather than leave half a word behind.
const GRACE: Duration = Duration::from_millis(2000);

/// How long a *forced* stop gets. Nothing can ignore it, so this only covers
/// the kernel getting round to it.
const FORCE_GRACE: Duration = Duration::from_millis(500);

/// How often either wait re-checks. Straight latency in front of startup, so
/// it is tight; the loop costs one cheap liveness check per tick.
const POLL: Duration = Duration::from_millis(20);

/// What happened to one previously-running instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Asked to stop, and gone.
    Ended,
    /// Gone, and it was the OS service. Carries the command that starts the
    /// service again — the service manager will not, because it was asked to
    /// restart ReCast when it *fails* and being stopped on purpose is not that.
    EndedService(&'static str),
    /// Deliberately left running: a service manager would start it again the
    /// moment it died. Carries the command that really stops it.
    Supervised(&'static str),
    /// Asked to stop, forced to stop, and still there. Nothing more to try.
    Survived,
}

/// Stop every other running copy of ReCast, and report what happened to each.
///
/// An empty result is the ordinary case — nothing else was running.
///
/// Called before the platform's startup rather than after, so the old instance
/// has already released its devices by the time the new one goes looking for
/// them. The cost is that a startup which then fails its own preflight leaves
/// nothing running at all; the alternative — checking first, killing second —
/// costs a full device enumeration while a duplicate is still injecting, which
/// is the failure this module exists to prevent.
pub fn replace_running() -> Vec<(u32, Outcome)> {
    let others = imp::peers();
    if others.is_empty() {
        return Vec::new();
    }

    // Decided before a single signal is sent. Once the fight starts there is no
    // way to win it, and the answer for one supervised instance is the answer
    // for the whole launch: do not.
    let supervised: Vec<(u32, Outcome)> = others
        .iter()
        .filter(|&&pid| imp::restarts_on_any_exit(pid))
        .map(|&pid| (pid, Outcome::Supervised(imp::STOP_SERVICE)))
        .collect();
    if !supervised.is_empty() {
        return supervised;
    }

    // Asked before anything dies: on Linux the answer is read out of
    // `/proc/<pid>`, which disappears along with the process.
    let notes: Vec<Option<&'static str>> = others.iter().map(|&pid| imp::service_note(pid)).collect();

    for &pid in &others {
        imp::ask_to_stop(pid);
    }
    let mut left = wait_for_exit(&others, GRACE);

    if !left.is_empty() {
        for &pid in &left {
            imp::force_stop(pid);
        }
        left = wait_for_exit(&left, FORCE_GRACE);
    }

    others
        .iter()
        .zip(notes)
        .map(|(&pid, note)| {
            let outcome = if left.contains(&pid) {
                Outcome::Survived
            } else {
                // The pidfile named a process that no longer exists, and the
                // next `--status` would have believed it.
                forget_pidfile(pid);
                match note {
                    Some(how) => Outcome::EndedService(how),
                    None => Outcome::Ended,
                }
            };
            (pid, outcome)
        })
        .collect()
}

/// The subset of `pids` still alive when `grace` runs out.
fn wait_for_exit(pids: &[u32], grace: Duration) -> Vec<u32> {
    let deadline = Instant::now() + grace;
    let mut left = pids.to_vec();
    loop {
        left.retain(|&pid| imp::still_running(pid));
        if left.is_empty() || Instant::now() >= deadline {
            return left;
        }
        crate::timing::pause(POLL);
    }
}

/// Drop the daemon pidfile if it named the instance just stopped.
///
/// Only the Linux daemon writes one, and only the Linux daemon overwrites it at
/// startup — a new instance that goes on to run a TUI or a control window never
/// touches it, so without this the file would outlive its process and
/// `--status` would keep reporting a daemon that is not there.
fn forget_pidfile(pid: u32) {
    #[cfg(target_os = "linux")]
    crate::daemon::forget_pidfile(pid);
    #[cfg(not(target_os = "linux"))]
    let _ = pid;
}

// ─── Linux ────────────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
mod imp {
    use nix::sys::signal::{self, Signal};
    use nix::unistd::Pid;
    use std::os::unix::fs::MetadataExt;

    pub const STOP_SERVICE: &str = "systemctl --user stop recast";

    /// Every other live process that is this program, run by this user.
    ///
    /// `/proc` rather than the pidfile, because the pidfile only ever describes
    /// the daemon — an instance running a TUI or a control window writes none,
    /// and those are exactly the duplicates a user creates by accident.
    ///
    /// The owner check is not a formality. Identity here comes from
    /// `/proc/<pid>/comm`, which is a *name*, and names are not unique across
    /// users; matching on one alone is how a stop turns into signalling
    /// somebody else's process. It is also free: the directory is owned by the
    /// process's real UID.
    pub fn peers() -> Vec<u32> {
        let me = std::process::id();
        // Read the same way as everyone else's, out of `/proc`, so the two
        // sides of the comparison cannot come from different notions of "user".
        let Ok(mine) = std::fs::metadata("/proc/self").map(|m| m.uid()) else {
            return Vec::new();
        };
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return Vec::new();
        };
        entries
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let pid: u32 = entry.file_name().to_str()?.parse().ok()?;
                if pid == me {
                    return None;
                }
                if entry.metadata().ok()?.uid() != mine {
                    return None;
                }
                crate::daemon::is_our_process(pid).then_some(pid)
            })
            .collect()
    }

    /// systemd is configured with `Restart=on-failure` (see the Makefile), and
    /// a process that exits on SIGTERM has not failed — so it stays down, and
    /// there is no fight to avoid. Always false, and the interesting half of
    /// the answer is in [`service_note`].
    pub fn restarts_on_any_exit(_pid: u32) -> bool {
        false
    }

    /// How to start the service again, if the instance about to be stopped is
    /// the service.
    ///
    /// A systemd unit is stopped for good by a SIGTERM it did not ask for: the
    /// unit goes inactive and stays that way until the next login. Someone who
    /// runs `recast` in a terminal to try something out has no reason to expect
    /// that their login service is now off, so they get told.
    pub fn service_note(pid: u32) -> Option<&'static str> {
        let cgroup = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
        cgroup
            .contains("recast.service")
            .then_some("systemctl --user start recast")
    }

    pub fn ask_to_stop(pid: u32) {
        let _ = signal::kill(Pid::from_raw(pid as i32), Signal::SIGTERM);
    }

    pub fn force_stop(pid: u32) {
        let _ = signal::kill(Pid::from_raw(pid as i32), Signal::SIGKILL);
    }

    /// Liveness *and* identity, so a PID reused by an unrelated program in the
    /// couple of seconds we are waiting reads as "gone" rather than keeping the
    /// wait going — and, more to the point, never gets the SIGKILL that would
    /// follow.
    pub fn still_running(pid: u32) -> bool {
        crate::daemon::is_our_process(pid)
    }
}

// ─── macOS ────────────────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
mod imp {
    use nix::sys::signal::{self, Signal};
    use nix::unistd::Pid;
    use std::ffi::c_void;
    use std::process::Command;

    pub const STOP_SERVICE: &str = "launchctl unload -w ~/Library/LaunchAgents/org.recast.plist";

    /// The label of the LaunchAgent written by "Start at login" (`prefs`) and
    /// by `make service`.
    const LABEL: &str = "org.recast";

    const PROC_ALL_PIDS: u32 = 1;
    /// `PROC_PIDPATHINFO_MAXSIZE` from `<sys/proc_info.h>`.
    const PATH_MAX: usize = 4 * 1024;

    // libproc, from libSystem — no crate needed, and no `ps` subprocess either.
    extern "C" {
        fn proc_listpids(kind: u32, typeinfo: u32, buffer: *mut c_void, buffersize: i32) -> i32;
        fn proc_pidpath(pid: i32, buffer: *mut c_void, buffersize: u32) -> i32;
    }

    /// The file name of our own executable, which is what a peer is recognised
    /// by. Taken from the running binary rather than hardcoded, so a renamed
    /// copy still recognises itself and an unrelated `recast` does not.
    fn our_name() -> String {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "recast".to_string())
    }

    /// The executable behind `pid`, or `None` if there is no such process or it
    /// belongs to another user — `proc_pidpath` refuses those, which is the
    /// ownership check the Linux side has to make explicitly.
    fn exe_of(pid: u32) -> Option<String> {
        let mut buf = vec![0u8; PATH_MAX];
        let written = unsafe {
            proc_pidpath(pid as i32, buf.as_mut_ptr() as *mut c_void, PATH_MAX as u32)
        };
        if written <= 0 {
            return None;
        }
        buf.truncate(written as usize);
        String::from_utf8(buf).ok()
    }

    fn is_ours(pid: u32) -> bool {
        exe_of(pid)
            .and_then(|path| {
                std::path::Path::new(&path)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
            })
            .is_some_and(|name| name == our_name())
    }

    pub fn peers() -> Vec<u32> {
        let me = std::process::id();
        // Asked for the size first: the process table changes between the two
        // calls, so the buffer is deliberately generous rather than exact.
        let bytes = unsafe { proc_listpids(PROC_ALL_PIDS, 0, std::ptr::null_mut(), 0) };
        if bytes <= 0 {
            return Vec::new();
        }
        let room = (bytes as usize / std::mem::size_of::<i32>()) + 64;
        let mut pids = vec![0i32; room];
        let filled = unsafe {
            proc_listpids(
                PROC_ALL_PIDS,
                0,
                pids.as_mut_ptr() as *mut c_void,
                (room * std::mem::size_of::<i32>()) as i32,
            )
        };
        if filled <= 0 {
            return Vec::new();
        }
        pids.truncate(filled as usize / std::mem::size_of::<i32>());
        pids.into_iter()
            .filter(|&pid| pid > 0)
            .map(|pid| pid as u32)
            .filter(|&pid| pid != me && is_ours(pid))
            .collect()
    }

    /// Whether launchd is holding this PID up.
    ///
    /// The agent sets `KeepAlive=true`, so it comes back from any exit at all —
    /// including a polite one. launchd is asked directly instead of the answer
    /// being guessed from the process tree: a ReCast started from Finder is
    /// also a child of launchd and is perfectly safe to replace, so the parent
    /// PID cannot tell these apart and the label can.
    pub fn restarts_on_any_exit(pid: u32) -> bool {
        let Ok(out) = Command::new("launchctl").arg("list").arg(LABEL).output() else {
            return false;
        };
        if !out.status.success() {
            return false; // no such job: nothing is supervising anything
        }
        let listing = String::from_utf8_lossy(&out.stdout);
        listing.lines().any(|line| {
            let line = line.trim();
            line.starts_with("\"PID\"")
                && line
                    .rsplit('=')
                    .next()
                    .is_some_and(|v| v.trim().trim_end_matches(';').trim() == pid.to_string())
        })
    }

    /// launchd is the only supervisor here, and a supervised instance is never
    /// stopped in the first place — so nothing that gets stopped needs a note.
    pub fn service_note(_pid: u32) -> Option<&'static str> {
        None
    }

    pub fn ask_to_stop(pid: u32) {
        let _ = signal::kill(Pid::from_raw(pid as i32), Signal::SIGTERM);
    }

    pub fn force_stop(pid: u32) {
        let _ = signal::kill(Pid::from_raw(pid as i32), Signal::SIGKILL);
    }

    pub fn still_running(pid: u32) -> bool {
        is_ours(pid)
    }
}

// ─── Windows ──────────────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
mod imp {
    use std::mem::size_of;
    use winapi::shared::minwindef::FALSE;
    use winapi::shared::winerror::{ERROR_ACCESS_DENIED, WAIT_TIMEOUT};
    use winapi::um::errhandlingapi::GetLastError;
    use winapi::um::handleapi::{CloseHandle, INVALID_HANDLE_VALUE};
    use winapi::um::processthreadsapi::{
        GetCurrentProcessId, OpenProcess, ProcessIdToSessionId, TerminateProcess,
    };
    use winapi::um::synchapi::WaitForSingleObject;
    use winapi::um::tlhelp32::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use winapi::um::winnt::{PROCESS_TERMINATE, SYNCHRONIZE};

    /// Windows has no supervisor for ReCast — "start at login" is the per-user
    /// `Run` key, which launches it once and then forgets about it.
    pub const STOP_SERVICE: &str = "";

    /// Our own executable's file name, lowercased. Windows paths are
    /// case-insensitive and `PROCESSENTRY32W` reports whatever case the process
    /// was launched with, so `ReCast.exe` and `recast.exe` are the same program
    /// and must compare equal.
    fn our_name() -> String {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()))
            .unwrap_or_else(|| "recast.exe".to_string())
    }

    /// The Terminal Services session a process belongs to.
    ///
    /// The Toolhelp snapshot is machine-wide: with fast user switching, or a
    /// service account, it lists ReCasts belonging to people who are not at
    /// this keyboard. Stopping one of those would be someone else's program
    /// vanishing for no reason, and it would not fix anything here, because a
    /// different session's copy is not competing for this session's keystrokes.
    fn session_of(pid: u32) -> Option<u32> {
        let mut session = 0u32;
        (unsafe { ProcessIdToSessionId(pid, &mut session) } != FALSE).then_some(session)
    }

    /// Every live process running our executable, in this session, this one
    /// excluded.
    ///
    /// The snapshot is walked in full rather than stopping at the first match:
    /// the whole point is that there may be several.
    pub fn peers() -> Vec<u32> {
        let me = std::process::id();
        let ours = our_name();
        let mine = session_of(unsafe { GetCurrentProcessId() });
        let mut found = Vec::new();

        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return found;
        }
        let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
        entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;

        let mut more = unsafe { Process32FirstW(snapshot, &mut entry) };
        while more != FALSE {
            let end = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(&entry.szExeFile[..end]).to_lowercase();
            let pid = entry.th32ProcessID;
            if pid != me && pid != 0 && name == ours && session_of(pid) == mine {
                found.push(pid);
            }
            more = unsafe { Process32NextW(snapshot, &mut entry) };
        }
        unsafe { CloseHandle(snapshot) };
        found
    }

    pub fn restarts_on_any_exit(_pid: u32) -> bool {
        false
    }

    pub fn service_note(_pid: u32) -> Option<&'static str> {
        None
    }

    /// There is no polite version of this on Windows.
    ///
    /// A console process can be sent Ctrl-Break, but ReCast's release build has
    /// no console, and its tray lives on a message loop that only the tray's
    /// own Quit item drives. `TerminateProcess` is what Task Manager's "End
    /// task" does. The one thing lost by it is the tray icon's removal from the
    /// notification area, which Explorer clears the next time the user's mouse
    /// passes over it.
    pub fn ask_to_stop(pid: u32) {
        unsafe {
            let handle = OpenProcess(PROCESS_TERMINATE, FALSE, pid);
            if handle.is_null() {
                return;
            }
            TerminateProcess(handle, 1);
            CloseHandle(handle);
        }
    }

    /// Already as forceful as it gets.
    pub fn force_stop(_pid: u32) {}

    pub fn still_running(pid: u32) -> bool {
        unsafe {
            let handle = OpenProcess(SYNCHRONIZE, FALSE, pid);
            if handle.is_null() {
                // Two very different failures, and reporting the wrong one is
                // the difference between "replaced it" and "did not". A PID
                // that no longer exists is refused with ERROR_INVALID_PARAMETER
                // and really is gone; an elevated ReCast we cannot open is
                // refused with ERROR_ACCESS_DENIED and is very much still
                // there, typing over everything this instance does.
                return GetLastError() == ERROR_ACCESS_DENIED;
            }
            let state = WaitForSingleObject(handle, 0);
            CloseHandle(handle);
            state == WAIT_TIMEOUT
        }
    }
}

// ─── Anywhere else ────────────────────────────────────────────────────────

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
mod imp {
    pub const STOP_SERVICE: &str = "";
    pub fn peers() -> Vec<u32> {
        Vec::new()
    }
    pub fn restarts_on_any_exit(_pid: u32) -> bool {
        false
    }
    pub fn service_note(_pid: u32) -> Option<&'static str> {
        None
    }
    pub fn ask_to_stop(_pid: u32) {}
    pub fn force_stop(_pid: u32) {}
    pub fn still_running(_pid: u32) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one mistake this module could make that nothing else would catch.
    /// Everything downstream sends a signal to whatever comes back from here.
    #[test]
    fn we_are_never_our_own_peer() {
        assert!(!imp::peers().contains(&std::process::id()));
    }

    /// A liveness check that says "yes" about a process that is gone turns the
    /// grace period into a full two-second stall on every launch.
    #[test]
    fn a_pid_that_cannot_exist_is_not_running() {
        // Above every /proc/sys/kernel/pid_max in use, and not a valid handle
        // on Windows either.
        assert!(!imp::still_running(u32::MAX - 1));
    }

    /// `wait_for_exit` must return promptly when there is nothing to wait for,
    /// rather than sleeping out the grace period it was given.
    #[test]
    fn waiting_on_nothing_takes_no_time() {
        let started = Instant::now();
        assert!(wait_for_exit(&[], Duration::from_secs(30)).is_empty());
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    /// And must give up when the deadline passes, on a PID it can never see
    /// exit — otherwise a process we have no permission to signal hangs
    /// startup forever.
    #[test]
    fn a_deadline_that_passes_ends_the_wait() {
        let started = Instant::now();
        // Our own PID: alive for the whole test by definition on Linux/macOS.
        let _ = wait_for_exit(&[std::process::id()], Duration::from_millis(60));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    /// Both halves of the supervised story have to be present: the outcome
    /// carries a command, and the command is not empty on the platform that
    /// can produce it.
    #[test]
    fn a_refusal_says_what_to_do_about_it() {
        if cfg!(target_os = "macos") {
            assert!(!imp::STOP_SERVICE.is_empty());
        }
        let refused = Outcome::Supervised("launchctl unload -w ...");
        assert_ne!(refused, Outcome::Ended);
    }
}
