// The pidfile and the stop that reads it are shared by Linux and macOS: Linux
// daemonizes and writes one, and macOS runs in the foreground under a launch
// agent but writes one too, so `--stop` has something to act on there rather
// than telling the user to go and find the menubar icon. Windows has neither —
// its instance is ended through the tray or Task Manager — so on that target
// these would all be unused imports.
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::fs;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::fs::OpenOptions;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::io::Write;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::process;

#[cfg(any(target_os = "linux", target_os = "macos"))]
use nix::{
    sys::signal::{self, Signal},
    unistd::Pid,
};

// Forking is the Linux daemon's alone: macOS stays in the foreground because
// the event tap has to own the main run loop.
#[cfg(target_os = "linux")]
use nix::{
    sys::wait::waitpid,
    unistd::{chdir, fork, setsid, ForkResult},
};

/// Daemonize the current process on Linux (fork, setsid, chdir).
#[cfg(target_os = "linux")]
pub fn daemonize() {
    // First fork: parent exits, child continues.
    match unsafe { fork() } {
        Ok(ForkResult::Parent { child }) => {
            // Parent: wait for child to avoid zombies, then exit.
            let _ = waitpid(child, None);
            process::exit(0);
        }
        Ok(ForkResult::Child) => {
            // Child: continue.
        }
        Err(e) => {
            eprintln!("Fork failed: {e}");
            process::exit(1);
        }
    }

    // Create a new session and process group.
    if let Err(e) = setsid() {
        eprintln!("setsid failed: {e}");
        process::exit(1);
    }

    // Second fork: ensure we are not a session leader.
    match unsafe { fork() } {
        Ok(ForkResult::Parent { .. }) => {
            // Parent: exit immediately.
            process::exit(0);
        }
        Ok(ForkResult::Child) => {
            // Child: we are the daemon.
        }
        Err(e) => {
            eprintln!("Second fork failed: {e}");
            process::exit(1);
        }
    }

    // Change working directory to root to avoid holding any directory in use.
    if let Err(e) = chdir("/") {
        eprintln!("chdir failed: {e}");
        process::exit(1);
    }

    // Detach stdio from the launching terminal: redirect stdin/stdout/stderr
    // to /dev/null so the daemon doesn't write to (or block on) a closed tty.
    if let Ok(devnull) = OpenOptions::new().read(true).write(true).open("/dev/null") {
        use std::os::fd::AsRawFd;
        let fd = devnull.as_raw_fd();
        for target in 0..=2 {
            let _ = nix::unistd::dup2(fd, target);
        }
    }
}

/// Write the current process ID so `--status` and `--stop` can find it.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn write_pidfile() -> std::io::Result<()> {
    let mut dir = dirs::cache_dir().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Unable to locate cache directory",
        )
    })?;
    dir.push("recast");
    fs::create_dir_all(&dir)?;
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(dir.join("pid"))?;
    writeln!(file, "{}", process::id())?;
    Ok(())
}

/// Where the running instance records its PID.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn pidfile_path() -> Option<std::path::PathBuf> {
    Some(dirs::cache_dir()?.join("recast").join("pid"))
}

/// Whether `pid` is a live process that is *this* program.
///
/// The liveness half is obvious; the identity half is the one that matters.
/// PIDs are reused, so a pidfile left behind by a daemon that was killed (or
/// that crashed before it could clean up) eventually names somebody else's
/// process — and acting on that number is how a stop command turns into
/// killing an unrelated program. Read the kernel's executable link rather
/// than `comm`, which is mutable and truncated to 15 bytes.
///
/// Compared against our own executable name rather than a hardcoded "recast",
/// so renamed and replaced installations still recognise earlier copies.
#[cfg(target_os = "linux")]
pub fn is_our_process(pid: u32) -> bool {
    let Ok(executable) = fs::read_link(format!("/proc/{pid}/exe")) else {
        return false;
    };
    let Ok(current) = std::env::current_exe() else {
        return false;
    };
    linux_executable_matches(&executable, &current)
}

#[cfg(target_os = "linux")]
fn linux_executable_matches(executable: &std::path::Path, current: &std::path::Path) -> bool {
    use std::os::unix::ffi::OsStrExt;

    fn name(path: &std::path::Path) -> Option<&[u8]> {
        let bytes = path.file_name()?.as_bytes();
        // /proc keeps identifying a running binary after an upgrade unlinks it.
        Some(bytes.strip_suffix(b" (deleted)").unwrap_or(bytes))
    }
    matches!((name(executable), name(current)), (Some(a), Some(b)) if a == b)
}

/// The macOS half of the same question, asked of `libproc` because there is no
/// `/proc` to read.
///
/// `proc_pidpath` refuses a process belonging to another user, which is the
/// ownership check the Linux side has to make explicitly in `instance`.
///
/// This lives here rather than in `instance` so that both callers — the stop
/// path below and the duplicate-instance sweep — ask the same question of the
/// same code, which is already how Linux is arranged.
#[cfg(target_os = "macos")]
pub fn is_our_process(pid: u32) -> bool {
    use std::ffi::c_void;

    /// `PROC_PIDPATHINFO_MAXSIZE` from `<sys/proc_info.h>`.
    const PATH_MAX: usize = 4 * 1024;

    // libproc, from libSystem — no crate needed, and no `ps` subprocess either.
    extern "C" {
        fn proc_pidpath(pid: i32, buffer: *mut c_void, buffersize: u32) -> i32;
    }

    let mut buf = vec![0u8; PATH_MAX];
    let written =
        unsafe { proc_pidpath(pid as i32, buf.as_mut_ptr() as *mut c_void, PATH_MAX as u32) };
    if written <= 0 {
        return false; // no such process, or it is not ours to look at
    }
    buf.truncate(written as usize);
    let Ok(path) = String::from_utf8(buf) else {
        return false;
    };
    std::path::Path::new(&path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .is_some_and(|name| name == our_name())
}

/// The file name of our own executable, which is what a process is recognised
/// by. Taken from the running binary rather than hardcoded, so a renamed copy
/// still recognises earlier instances using that name.
#[cfg(any(target_os = "macos", all(target_os = "linux", test)))]
fn our_name() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "recast".to_string())
}

/// Remove the pidfile if — and only if — it names `pid`.
///
/// For whoever stopped that process to call afterwards. The daemon cannot clean
/// up after itself here: it is stopped by a signal it does not handle, so it
/// never gets the chance, leaving a stale entry for the next stop command.
///
/// The equality test is the whole point. A blind `remove_file` would delete the
/// *new* instance's pidfile in the ordinary case, because by the time an
/// instance is confirmed gone its replacement has often already written one.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn forget_pidfile(pid: u32) {
    let Some(path) = pidfile_path() else {
        return;
    };
    let named: Option<u32> = fs::read_to_string(&path)
        .ok()
        .and_then(|c| c.trim().parse().ok());
    if named == Some(pid) {
        let _ = fs::remove_file(&path);
    }
}

/// What `--stop` was actually able to do.
///
/// It used to return `Ok(())` for every one of these, and the caller printed
/// "Stopped recast daemon." on all of them — including on macOS and Windows,
/// where no pidfile is ever written and so nothing could possibly have been
/// stopped. A stop command that reports success without stopping anything is
/// worse than one that fails, because it sends the user looking somewhere else.
///
/// Which variants can occur is decided by the target — Linux produces the
/// first three and never the last, the others produce only the last — so all
/// four are dead code somewhere, and `main` matches on the whole enum
/// regardless.
#[derive(Debug, PartialEq)]
#[allow(dead_code)]
pub enum Stopped {
    /// A live daemon was asked to stop, directly or through its supervisor.
    Signalled(u32),
    /// The pidfile named a process that is gone; the stale file was removed.
    Stale,
    /// There was no pidfile.
    NotRunning,
    /// This platform never writes one, so this is not how ReCast is stopped
    /// here. Carries the way that it is.
    Unsupported(&'static str),
}

/// Stop a matching LaunchAgent for this login session, preserving its plist
/// and enabled state so it can start again at the next login.
#[cfg(target_os = "macos")]
pub fn stop_launch_agent(pid: u32) -> std::io::Result<bool> {
    stop_launch_agent_with(pid, unsafe { nix::libc::getuid() }, |args| {
        let output = std::process::Command::new("/bin/launchctl")
            .args(args)
            .output()?;
        Ok(output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).into_owned()))
    })
}

#[cfg(any(target_os = "macos", test))]
fn stop_launch_agent_with(
    pid: u32,
    uid: u32,
    mut run: impl FnMut(&[&str]) -> std::io::Result<Option<String>>,
) -> std::io::Result<bool> {
    let target = format!("gui/{uid}/org.recast");
    let Some(listing) = run(&["print", &target])? else {
        return Ok(false);
    };
    if launch_agent_pid(&listing) != Some(pid) {
        return Ok(false);
    }
    if run(&["bootout", &target])?.is_none() {
        return Err(std::io::Error::other(format!(
            "could not stop LaunchAgent {target}; ReCast was left running"
        )));
    }
    Ok(true)
}

#[cfg(any(target_os = "macos", test))]
fn launch_agent_pid(listing: &str) -> Option<u32> {
    let mut depth = 0usize;
    for line in listing.lines().map(str::trim) {
        if depth == 1 {
            if let Some(("pid", value)) =
                line.split_once('=').map(|(key, value)| (key.trim(), value))
            {
                return value.trim().parse().ok();
            }
        }
        if line.ends_with("= {") || line == "{" {
            depth += 1;
        } else if line == "}" {
            depth = depth.saturating_sub(1);
        }
    }
    None
}

/// Stop a running daemon, if this platform has one and it is really there.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn stop_daemon() -> std::io::Result<Stopped> {
    let pidfile = pidfile_path().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Unable to locate cache directory",
        )
    })?;
    let Ok(contents) = fs::read_to_string(&pidfile) else {
        return Ok(Stopped::NotRunning);
    };
    let pid: u32 = contents.trim().parse().map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "Invalid PID in pidfile")
    })?;
    // Checked before the signal is sent, not after: this is the whole
    // difference between stopping our daemon and killing whatever inherited
    // its PID.
    if !is_our_process(pid) {
        let _ = fs::remove_file(&pidfile);
        return Ok(Stopped::Stale);
    }
    #[cfg(target_os = "macos")]
    if stop_launch_agent(pid)? {
        forget_pidfile(pid);
        return Ok(Stopped::Signalled(pid));
    }
    // SIGTERM directly. This used to fork and exec `/bin/kill` — a whole
    // process, a PATH lookup and a wait, to make one syscall we can make here.
    // Worse than wasteful: it depended on `kill` being on the PATH of whatever
    // shell was in use, and reported success whether or not the signal landed,
    // because it never looked at the exit status.
    match signal::kill(Pid::from_raw(pid as i32), Signal::SIGTERM) {
        Ok(()) => {
            let _ = fs::remove_file(&pidfile);
            Ok(Stopped::Signalled(pid))
        }
        // The identity check above passed, so the process was ours a moment
        // ago; ESRCH here means it exited in between. Nothing to stop, and the
        // pidfile is now stale.
        Err(nix::errno::Errno::ESRCH) => {
            let _ = fs::remove_file(&pidfile);
            Ok(Stopped::Stale)
        }
        Err(e) => Err(std::io::Error::other(format!(
            "could not signal pid {pid}: {e}"
        ))),
    }
}

/// Windows runs ReCast in the foreground under the per-user `Run` key and
/// writes no pidfile — `--stop` has nothing to read and must say so rather than
/// claim a stop it did not make.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn stop_daemon() -> std::io::Result<Stopped> {
    #[cfg(target_os = "windows")]
    let how = "quit it from the tray icon, or end the `recast` task in Task Manager";
    #[cfg(not(target_os = "windows"))]
    let how = "this platform has no daemon mode";
    Ok(Stopped::Unsupported(how))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn matching_process_names_do_not_identify_unrelated_executables() {
        use std::os::unix::fs::symlink;

        let dir = std::env::temp_dir().join(format!("recast-process-identity-{}", process::id()));
        fs::create_dir_all(&dir).unwrap();
        let executable = dir.join(our_name());
        symlink("/bin/sleep", &executable).unwrap();
        let mut child = process::Command::new(&executable)
            .arg("30")
            .spawn()
            .unwrap();
        let recognized = is_our_process(child.id());
        let _ = child.kill();
        child.wait().unwrap();
        fs::remove_dir_all(dir).unwrap();
        assert!(
            !recognized,
            "a matching comm must not identify another executable"
        );
        assert!(is_our_process(process::id()));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn executable_identity_preserves_upgrades_and_full_names() {
        use std::path::Path;
        let current = Path::new("/new/recast-long-binary-name");
        for (executable, expected) in [
            ("/old/recast-long-binary-name", true),
            ("/old/recast-long-binary-name (deleted)", true),
            ("/old/recast-long-bin", false),
            ("/old/recast-long-binary-other", false),
            ("/usr/bin/python3", false),
            ("/", false),
        ] {
            assert_eq!(
                linux_executable_matches(Path::new(executable), current),
                expected
            );
        }
        assert!(linux_executable_matches(
            Path::new("/old/recast"),
            Path::new("/new/recast (deleted)")
        ));
    }

    const AGENT: &str = "gui/501/org.recast = {\n\tstate = running\n\tenvironment = {\n\t\tpid = 99\n\t}\n\tpid = 123\n}\n";

    #[test]
    fn intentional_stop_removes_only_the_matching_login_job() {
        let mut calls = Vec::new();
        let stopped = stop_launch_agent_with(123, 501, |args| {
            calls.push(args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>());
            Ok(Some(if args[0] == "print" {
                AGENT.to_string()
            } else {
                String::new()
            }))
        });
        assert!(stopped.unwrap());
        assert_eq!(
            calls,
            [
                ["print", "gui/501/org.recast"],
                ["bootout", "gui/501/org.recast"],
            ]
        );
    }

    #[test]
    fn manual_instances_and_missing_jobs_do_not_change_launchd() {
        for listing in [None, Some(AGENT.to_string()), Some("pid = 99".to_string())] {
            let mut calls = 0;
            let stopped = stop_launch_agent_with(99, 501, |args| {
                calls += 1;
                assert_eq!(args, ["print", "gui/501/org.recast"]);
                Ok(listing.clone())
            });
            assert!(!stopped.unwrap());
            assert_eq!(calls, 1);
        }
    }

    #[test]
    fn failed_bootout_does_not_report_a_successful_stop() {
        let result = stop_launch_agent_with(123, 501, |args| {
            Ok((args[0] == "print").then(|| AGENT.to_string()))
        });
        assert!(result.unwrap_err().to_string().contains("left running"));
    }

    #[test]
    fn launchctl_execution_errors_are_reported() {
        for failing_command in ["print", "bootout"] {
            let result = stop_launch_agent_with(123, 501, |args| {
                if args[0] == failing_command {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "launchctl denied",
                    ))
                } else {
                    Ok(Some(AGENT.to_string()))
                }
            });
            assert_eq!(
                result.unwrap_err().kind(),
                std::io::ErrorKind::PermissionDenied
            );
        }
    }
}
