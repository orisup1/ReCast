//! On-demand, authenticated loopback diagnostics. No typed text is transmitted.
use std::fmt::Write as _;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{atomic::Ordering, Arc};
use std::time::Duration;

use crate::{complete, config, footprint, layout, personal, prefs, settings, types::AppControl};

const TIMEOUT: Duration = Duration::from_secs(2);
const MAX_REPORT: u64 = 64 * 1024;
const HEADER: &str = "ReCast live status v1";

fn endpoint_path(pid: u32) -> Option<PathBuf> {
    Some(
        dirs::cache_dir()?
            .join("recast")
            .join(format!("status-{pid}")),
    )
}

/// Started after daemonization. A failed diagnostics server must not disable typing.
pub fn start(control: Arc<AppControl>) {
    let Some(path) = endpoint_path(std::process::id()) else {
        eprintln!("Live status unavailable: no cache directory");
        return;
    };
    let result = bind(&path);
    match result {
        Ok((listener, token)) => {
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { break };
                    let _ = respond(stream, &token, || {
                        let summary = crate::platform::status(&control);
                        let app = crate::platform::active_application().map(|(_, id)| id);
                        report(&control, &summary, app.as_deref())
                    });
                }
                let _ = std::fs::remove_file(path);
            });
        }
        Err(error) => eprintln!("Live status unavailable: {error}"),
    }
}

fn bind(path: &Path) -> io::Result<(TcpListener, [u8; 32])> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let mut token = [0; 32];
    getrandom::fill(&mut token).map_err(|error| io::Error::other(error.to_string()))?;
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("Missing cache directory"))?;
    std::fs::create_dir_all(parent)?;
    // A previous process may have used this PID. Unlink the entry itself, never
    // follow a stale symlink; create_new also protects the subsequent creation.
    match std::fs::remove_file(path) {
        Ok(()) => (),
        Err(error) if error.kind() == io::ErrorKind::NotFound => (),
        Err(error) => return Err(error),
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(&listener.local_addr()?.port().to_be_bytes())?;
    file.write_all(&token)?;
    Ok((listener, token))
}

fn respond(
    mut stream: TcpStream,
    token: &[u8; 32],
    report: impl FnOnce() -> String,
) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_millis(250)))?;
    stream.set_write_timeout(Some(TIMEOUT))?;
    let mut supplied = [0; 32];
    stream.read_exact(&mut supplied)?;
    if supplied != *token {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Invalid status token",
        ));
    }
    let text = report();
    if text.len() as u64 > MAX_REPORT {
        return Err(io::Error::other("Status response too large"));
    }
    stream.write_all(&(text.len() as u32).to_be_bytes())?;
    stream.write_all(text.as_bytes())
}

fn query(path: &Path, pid: u32) -> io::Result<String> {
    let mut endpoint = Vec::new();
    std::fs::File::open(path)?
        .take(35)
        .read_to_end(&mut endpoint)?;
    if endpoint.len() != 34 {
        return Err(io::Error::other("Invalid status endpoint"));
    }
    let port = u16::from_be_bytes([endpoint[0], endpoint[1]]);
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&address, TIMEOUT)?;
    stream.set_read_timeout(Some(TIMEOUT))?;
    stream.set_write_timeout(Some(TIMEOUT))?;
    stream.write_all(&endpoint[2..])?;
    let mut size = [0; 4];
    stream.read_exact(&mut size)?;
    let size = u32::from_be_bytes(size) as u64;
    if size > MAX_REPORT {
        return Err(io::Error::other("Status response too large"));
    }
    let mut bytes = vec![0; size as usize];
    stream.read_exact(&mut bytes)?;
    let text = String::from_utf8(bytes).map_err(io::Error::other)?;
    let header = format!("{HEADER} (pid {pid})\n");
    if !text.starts_with(&header) {
        return Err(io::Error::other("Invalid status response"));
    }
    Ok(text)
}

pub fn print() {
    let pids = crate::instance::running_pids();
    if pids.is_empty() {
        crate::require_readable_config();
        println!("recast {}\n  running:        no", env!("CARGO_PKG_VERSION"));
        println!("Offline diagnostics: this invocation's settings, not the running daemon.");
        print!("{}", diagnostics(None));
        for complaint in
            settings::complaints(config::NUMERIC_KEYS, config::BOOLEAN_KEYS, config::ALL_KEYS)
        {
            eprintln!("\n  ! {complaint}");
        }
        return;
    }
    if pids.len() > 1 {
        println!(
            "Warning: {} ReCast processes found; concurrent copies can correct text twice.",
            pids.len()
        );
    }
    for pid in pids {
        println!("  running:        yes (pid {pid})");
        let result = endpoint_path(pid)
            .ok_or_else(|| io::Error::other("No cache directory"))
            .and_then(|path| query(&path, pid));
        match result {
            Ok(text) => print!("{text}"),
            Err(error) => println!("  live status:    unavailable ({error}). The process may be starting, unresponsive, or an older build; reopen ReCast to enable live diagnostics."),
        }
    }
}

fn report(control: &AppControl, summary: &str, app: Option<&str>) -> String {
    let mut text = format!(
        "{HEADER} (pid {})\nrecast {}\n",
        std::process::id(),
        env!("CARGO_PKG_VERSION")
    );
    writeln!(text, "  status:         {summary}").unwrap();
    writeln!(
        text,
        "  application:    {}",
        app.unwrap_or("unknown or ReCast controls")
    )
    .unwrap();
    let mode = match control.app_mode(app) {
        Some(config::AppMode::Full) => "Full correction",
        Some(config::AppMode::LayoutOnly) => "Layout only",
        Some(config::AppMode::Off) => "Off",
        None => "unknown (processing suspended)",
    };
    writeln!(text, "  application mode: {mode}").unwrap();
    writeln!(
        text,
        "  listener:       {}",
        if control.listener_ready.load(Ordering::Relaxed) {
            "ready"
        } else {
            "unavailable"
        }
    )
    .unwrap();
    if let Some(left) = control.pause_remaining() {
        writeln!(
            text,
            "  pause:          {} seconds remaining",
            left.as_secs()
        )
        .unwrap();
    }
    if let Some(app) = control.paused_app() {
        writeln!(text, "  paused in:      {app}").unwrap();
    }
    writeln!(
        text,
        "  corrections:    {} ({} undone)",
        control.fixed_count(),
        control.undo_count()
    )
    .unwrap();
    text.push_str(&diagnostics(Some(control)));
    text
}

fn diagnostics(control: Option<&AppControl>) -> String {
    let mut out = String::new();
    writeln!(
        out,
        "  correction:     {}",
        if control.map_or_else(prefs::load_enabled, AppControl::is_switched_on) {
            "enabled"
        } else {
            "disabled"
        }
    )
    .unwrap();
    // The layout pipeline is the headline feature and the one that can be
    // silently unavailable: on a Linux session ReCast cannot drive, mistyped
    // words go through untouched and nothing else says why.
    writeln!(out, "  layout switch:  {}", layout::describe_backend()).unwrap();
    match prefs::autostart_enabled() {
        Some(true) => writeln!(out, "  start at login: yes").unwrap(),
        Some(false) => writeln!(out, "  start at login: no").unwrap(),
        None => {}
    }
    match complete::config_dir() {
        Some(dir) => writeln!(out, "  config dir:     {}", dir.display()).unwrap(),
        None => writeln!(out, "  config dir:     (none — no OS config directory)").unwrap(),
    }
    // Whether the file exists is the first thing to check when a setting in it
    // did nothing, and the most likely answer is that it is somewhere else.
    match settings::file_path() {
        Some(path) if path.exists() => {
            writeln!(out, "  config.toml:    {}", path.display()).unwrap()
        }
        Some(path) => writeln!(
            out,
            "  config.toml:    none ({} — --write-config makes one)",
            path.display()
        )
        .unwrap(),
        None => writeln!(out, "  config.toml:    (none — no OS config directory)").unwrap(),
    }
    let (abbrevs, ignored, learned) = complete::list_counts();
    writeln!(out, "  abbrev.txt:     {abbrevs} abbreviation(s)").unwrap();
    writeln!(out, "  ignore.txt:     {ignored} word(s)").unwrap();
    writeln!(out, "  learned.txt:    {learned} word(s) retired by undo").unwrap();
    writeln!(
        out,
        "  memory:         {}",
        footprint::rss_human().unwrap_or_else(|| "unavailable on this platform".into())
    )
    .unwrap();

    // The other half of "is it configured the way I think it is". Every one of
    // these can be overridden from the environment and none of them used to be
    // reported, so someone who set RECAST_SPELL_DIST had no way to confirm it
    // had been read — least of all when the value was a typo and had silently
    // fallen back to the default.
    let cfg = control.map_or_else(config::Config::from_env, |_| config::Config::global());
    writeln!(out, "\n  settings:").unwrap();
    writeln!(
        out,
        "    excluded apps        {}",
        if cfg.excluded_apps.is_empty() {
            "none".to_string()
        } else {
            cfg.excluded_apps.join(", ")
        }
    )
    .unwrap();
    writeln!(
        out,
        "    layout-only apps     {}",
        if cfg.layout_only_apps.is_empty() {
            "none".into()
        } else {
            cfg.layout_only_apps.join(", ")
        }
    )
    .unwrap();
    writeln!(out, "    action shortcut      {}", cfg.action_gesture()).unwrap();
    writeln!(out, "    completion shortcut  {}", cfg.completion_gesture()).unwrap();
    writeln!(
        out,
        "    extra undo shortcut  {}",
        crate::practice::shortcut_label(&cfg.undo_shortcut)
    )
    .unwrap();
    writeln!(
        out,
        "    short words          {}",
        on_off(cfg.short_enabled)
    )
    .unwrap();
    writeln!(
        out,
        "    missing-space split  {}",
        on_off(cfg.split_enabled)
    )
    .unwrap();
    writeln!(out, "    frequency tie-break  {}", on_off(cfg.freq_enabled)).unwrap();
    writeln!(
        out,
        "    spelling             {}  (min length {}, max rank {}, max distance {})",
        on_off(cfg.spell_enabled),
        cfg.spell_min_len,
        cfg.spell_max_rank,
        cfg.spell_max_dist,
    )
    .unwrap();
    writeln!(
        out,
        "    auto-complete        {}  (min prefix {}, max rank {})",
        on_off(cfg.complete_enabled),
        cfg.complete_min_len,
        cfg.complete_max_rank,
    )
    .unwrap();
    writeln!(
        out,
        "    personalization      {}{}",
        on_off(cfg.personal_enabled),
        personal::data_dir()
            .map(|path| format!("  ({})", path.display()))
            .unwrap_or_default(),
    )
    .unwrap();

    for complaint in
        settings::complaints(config::NUMERIC_KEYS, config::BOOLEAN_KEYS, config::ALL_KEYS)
    {
        if control.is_some() {
            writeln!(out, "\n  ! {complaint}").unwrap();
        }
    }
    out
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
    use super::*;

    #[test]
    fn live_report_uses_runtime_settings_and_control_state() {
        const CHILD: &str = "RECAST_STATUS_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "status::tests::live_report_uses_runtime_settings_and_control_state",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .env("RECAST_LAYOUT_BACKEND", "none")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let control = AppControl::new_for_test();
        config::Config::update_live(|cfg| cfg.spell_enabled = false);
        control
            .layout_only_apps
            .lock()
            .unwrap()
            .push("editor.exe".into());
        control.listener_ready.store(true, Ordering::Relaxed);
        control.pause_for(Duration::from_secs(120));
        control.pause_in_app("editor.exe");
        let path = complete::config_dir().unwrap().join("live-report-test");
        let (listener, token) = bind(&path).unwrap();
        let pid = std::process::id();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            respond(stream, &token, || {
                report(&control, "Paused", Some("editor.exe"))
            })
            .unwrap();
            control.set_enabled(false);
            control.listener_ready.store(false, Ordering::Relaxed);
            let (stream, _) = listener.accept().unwrap();
            respond(stream, &token, || {
                report(&control, "Disabled", Some("editor.exe"))
            })
            .unwrap();
        });
        let text = query(&path, pid).unwrap();
        for expected in [
            "status:         Paused",
            "application:    editor.exe",
            "application mode: Layout only",
            "listener:       ready",
            "seconds remaining",
            "paused in:      editor.exe",
            "spelling             off",
            "memory:",
        ] {
            assert!(text.contains(expected), "{text}");
        }
        let text = query(&path, pid).unwrap();
        for expected in [
            "status:         Disabled",
            "correction:     disabled",
            "listener:       unavailable",
        ] {
            assert!(text.contains(expected), "{text}");
        }
        assert!(!text.contains("seconds remaining"));
        server.join().unwrap();
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn status_transport_authenticates_bounds_and_rejects_stale_endpoints() {
        let path = crate::complete::config_dir().unwrap().join("status-test");
        let (listener, token) = bind(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let pid = std::process::id();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            assert!(respond(stream, &token, || panic!("unauthenticated request")).is_err());
            for _ in 0..2 {
                let (stream, _) = listener.accept().unwrap();
                respond(stream, &token, || {
                    format!("{HEADER} (pid {pid})\n  status: Paused\n")
                })
                .unwrap();
            }
            for size in [MAX_REPORT as u32 + 1, 10] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut supplied = [0; 32];
                stream.read_exact(&mut supplied).unwrap();
                stream.write_all(&size.to_be_bytes()).unwrap();
                // The second response ends before the declared length.
                if size == 10 {
                    stream.write_all(b"bad").unwrap();
                }
            }
        });
        let mut wrong = TcpStream::connect({
            let bytes = std::fs::read(&path).unwrap();
            (
                Ipv4Addr::LOCALHOST,
                u16::from_be_bytes([bytes[0], bytes[1]]),
            )
        })
        .unwrap();
        wrong.write_all(&[0; 32]).unwrap();
        assert!(query(&path, pid).unwrap().contains("Paused"));
        assert!(query(&path, pid + 1).is_err());
        assert!(
            query(&path, pid).is_err(),
            "oversized response must be rejected"
        );
        assert!(
            query(&path, pid).is_err(),
            "truncated response must be rejected"
        );
        server.join().unwrap();
        assert!(
            query(&path, pid).is_err(),
            "dead endpoint must not look live"
        );
        std::fs::write(&path, b"truncated").unwrap();
        assert!(query(&path, pid).is_err());
        std::fs::remove_file(path).unwrap();
    }
}
