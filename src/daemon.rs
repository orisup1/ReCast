use std::process;
use std::fs::{self, OpenOptions};
use std::io::Write;

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

/// Read the pidfile and attempt to kill that process.
pub fn stop_daemon() -> std::io::Result<()> {
    let mut dir = dirs::cache_dir().ok_or_else(|| std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "Unable to locate cache directory",
    ))?;
    dir.push("recast");
    let pidfile = dir.join("pid");
    // If no pidfile, assume daemon not running.
    let pid_str = match fs::read_to_string(&pidfile) {
        Ok(s) => s,
        Err(_) => return Ok(()),
    };
    let pid: u32 = pid_str.trim().parse().map_err(|_| std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        "Invalid PID in pidfile",
    ))?;
    // Send SIGTERM via the `kill` utility (avoids pulling nix's signal feature
    // in for one call and works on macOS too).
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::process::Command;
        let _ = Command::new("kill").arg(pid.to_string()).status();
    }
    #[cfg(target_os = "windows")]
    {
        use std::process::Command;
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .status();
    }
    // Remove pidfile
    let _ = fs::remove_file(pidfile);
    Ok(())
}