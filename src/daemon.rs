// Everything that touches the filesystem here is part of the Linux daemon
// path — daemonize, the pidfile, and the stop that reads it. macOS and Windows
// run in the foreground under a launch agent or the `Run` key and never write
// one, so on those targets these would all be unused imports.
#[cfg(target_os = "linux")]
use std::fs;
#[cfg(target_os = "linux")]
use std::fs::OpenOptions;
#[cfg(target_os = "linux")]
use std::io::Write;
#[cfg(target_os = "linux")]
use std::process;

#[cfg(target_os = "linux")]
use nix::{
    unistd::{fork, ForkResult, chdir, setsid},
    sys::wait::waitpid,
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

/// Write the current process ID to a pidfile in the user's cache directory.
///
/// Only the Linux daemon writes a pidfile (macOS/Windows run in the foreground
/// under a launch agent / scheduled task), so this is Linux-only; gating it
/// avoids a dead-code warning on the other targets.
#[cfg(target_os = "linux")]
pub fn write_pidfile() -> std::io::Result<()> {
    let mut dir = dirs::cache_dir().ok_or_else(|| std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "Unable to locate cache directory",
    ))?;
    dir.push("recast");
    fs::create_dir_all(&dir)?;
    let mut file = OpenOptions::new().create(true).write(true).truncate(true).open(dir.join("pid"))?;
    let pid = process::id();
    writeln!(file, "{pid}")?;
    Ok(())
}

/// Where the Linux daemon records its PID.
#[cfg(target_os = "linux")]
fn pidfile_path() -> Option<std::path::PathBuf> {
    Some(dirs::cache_dir()?.join("recast").join("pid"))
}

/// Whether `pid` is a live process that is *this* program.
///
/// The liveness half is obvious; the identity half is the one that matters.
/// PIDs are reused, so a pidfile left behind by a daemon that was killed (or
/// that crashed before it could clean up) eventually names somebody else's
/// process — and acting on that number is how a stop command turns into
/// killing an unrelated program. `/proc/<pid>/comm` settles it for free.
///
/// Compared against our own executable name rather than a hardcoded "recast",
/// so a renamed binary still recognises itself; `comm` is truncated to 15
/// bytes by the kernel, which is what the shortened comparison is for.
#[cfg(target_os = "linux")]
fn is_our_process(pid: u32) -> bool {
    let Ok(comm) = fs::read_to_string(format!("/proc/{pid}/comm")) else {
        return false; // no such process
    };
    let comm = comm.trim();
    let ours = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "recast".to_string());
    let truncated: String = ours.chars().take(15).collect();
    comm == ours || comm == truncated
}

/// The PID of a running daemon, if there is one.
///
/// Linux-only, because it is the only platform that daemonizes and writes a
/// pidfile — macOS and Windows run in the foreground under a launch agent or
/// scheduled task, where the OS is the thing that knows. A stale pidfile left
/// by a killed process reads as "not running", which is what the user means by
/// the question.
#[cfg(target_os = "linux")]
pub fn running_pid() -> Option<u32> {
    let pid: u32 = fs::read_to_string(pidfile_path()?).ok()?.trim().parse().ok()?;
    is_our_process(pid).then_some(pid)
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
    /// SIGTERM was sent to a live daemon.
    Signalled(u32),
    /// The pidfile named a process that is gone; the stale file was removed.
    Stale,
    /// There was no pidfile.
    NotRunning,
    /// This platform never writes one, so this is not how ReCast is stopped
    /// here. Carries the way that it is.
    Unsupported(&'static str),
}

/// Stop a running daemon, if this platform has one and it is really there.
#[cfg(target_os = "linux")]
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
    // SIGTERM via the `kill` utility, which avoids pulling nix's signal
    // feature in for this one call.
    use std::process::Command;
    let _ = Command::new("kill").arg(pid.to_string()).status();
    let _ = fs::remove_file(&pidfile);
    Ok(Stopped::Signalled(pid))
}

/// macOS and Windows run ReCast in the foreground under a launch agent or the
/// per-user `Run` key, so there is no pidfile and never was one — `--stop` has
/// nothing to read and must say so rather than claim a stop it did not make.
#[cfg(not(target_os = "linux"))]
pub fn stop_daemon() -> std::io::Result<Stopped> {
    #[cfg(target_os = "macos")]
    let how = "quit it from the menubar icon, or: launchctl unload -w ~/Library/LaunchAgents/org.recast.plist";
    #[cfg(target_os = "windows")]
    let how = "quit it from the tray icon, or end the `recast` task in Task Manager";
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let how = "this platform has no daemon mode";
    Ok(Stopped::Unsupported(how))
}