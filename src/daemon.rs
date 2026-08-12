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

/// Write the current process ID to a pidfile in the user's cache directory.
///
/// Record our PID so `--stop` has something to act on.
///
/// Written by the Linux daemon and, since it runs in the foreground with no
/// fork, by the macOS tray process itself — `process::id()` is the process the
/// user means either way. Windows is the exception: its instance is ended from
/// the tray or Task Manager, so nothing there reads a pidfile and gating this
/// out avoids a dead-code warning.
#[cfg(any(target_os = "linux", target_os = "macos"))]
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
/// killing an unrelated program. `/proc/<pid>/comm` settles it for free.
///
/// Compared against our own executable name rather than a hardcoded "recast",
/// so a renamed binary still recognises itself; `comm` is truncated to 15
/// bytes by the kernel, which is what the shortened comparison is for.
#[cfg(target_os = "linux")]
pub fn is_our_process(pid: u32) -> bool {
    let Ok(comm) = fs::read_to_string(format!("/proc/{pid}/comm")) else {
        return false; // no such process
    };
    let comm = comm.trim();
    let truncated: String = our_name().chars().take(15).collect();
    comm == our_name() || comm == truncated
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
/// still recognises itself and an unrelated `recast` does not.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn our_name() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "recast".to_string())
}

/// The PID of a running instance, if there is one.
///
/// Linux and macOS, the two that write a pidfile; on Windows the OS is the
/// thing that knows. A stale pidfile left by a killed process reads as "not
/// running", which is what the user means by the question.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn running_pid() -> Option<u32> {
    let pid: u32 = fs::read_to_string(pidfile_path()?).ok()?.trim().parse().ok()?;
    is_our_process(pid).then_some(pid)
}

/// Remove the pidfile if — and only if — it names `pid`.
///
/// For whoever stopped that process to call afterwards. The daemon cannot clean
/// up after itself here: it is stopped by a signal it does not handle, so it
/// never gets the chance, and the file it leaves behind is what makes
/// `running_pid` (and so `--status`) claim a daemon that is not there.
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