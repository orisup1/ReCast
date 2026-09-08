use std::process::Command;

#[test]
fn cli_reports_version_help_and_bad_options() {
    for (arg, expected, code) in [
        (
            "--version",
            concat!("recast ", env!("CARGO_PKG_VERSION")),
            0,
        ),
        ("--help", "Usage: recast [OPTIONS]", 0),
        ("--not-an-option", "Unknown option:", 2),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_recast"))
            .arg(arg)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(code));
        let text = if code == 0 {
            output.stdout
        } else {
            output.stderr
        };
        assert!(String::from_utf8_lossy(&text).contains(expected));
    }
}

#[test]
fn status_reports_numeric_fallbacks_and_application_exclusions() {
    for value in ["4", "256", "-1", "l"] {
        let output = Command::new(env!("CARGO_BIN_EXE_recast"))
            .arg("--status")
            .env("RECAST_LAYOUT_BACKEND", "none")
            .env("RECAST_SPELL_DIST", value)
            .env("RECAST_COMPLETE_RANK", "4294967296")
            .env("RECAST_EXCLUDE_APPS", " Code.exe, com.apple.Terminal ")
            .output()
            .unwrap();
        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stdout.contains("not the running daemon"), "{stdout}");
        assert!(stdout.contains("max distance 3)"), "{stdout}");
        assert!(stdout.contains("max rank 30000)"), "{stdout}");
        assert!(stdout.contains("code.exe, com.apple.terminal"), "{stdout}");
        assert!(stderr.contains("RECAST_SPELL_DIST="), "{stderr}");
        assert!(stderr.contains("RECAST_COMPLETE_RANK="), "{stderr}");
    }
    let output = Command::new(env!("CARGO_BIN_EXE_recast"))
        .arg("--status")
        .env("RECAST_LAYOUT_BACKEND", "none")
        .env("RECAST_SPELL_DIST", "0")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("max distance 0)"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("RECAST_SPELL_DIST="));
}

#[cfg(target_os = "linux")]
#[test]
fn unreadable_config_stops_startup_and_status_but_not_help() {
    let dir = std::env::temp_dir().join(format!("recast-cli-config-read-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("recast/config.toml")).unwrap();
    for arg in ["--foreground", "--status", "--help"] {
        let output = Command::new(env!("CARGO_BIN_EXE_recast"))
            .env("XDG_CONFIG_HOME", &dir)
            .arg(arg)
            .output()
            .unwrap();
        if arg == "--help" {
            assert!(output.status.success());
        } else {
            assert_eq!(output.status.code(), Some(1));
            assert!(String::from_utf8_lossy(&output.stderr)
                .contains("refusing to discard configured settings"));
        }
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn write_config_preserves_existing_files_and_symlink_targets() {
    let dir = std::env::temp_dir().join(format!("recast-cli-{}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    let write = || {
        Command::new(env!("CARGO_BIN_EXE_recast"))
            .env("XDG_CONFIG_HOME", &dir)
            .arg("--write-config")
            .output()
            .unwrap()
    };
    assert!(write().status.success());
    let config = dir.join("recast/config.toml");
    assert!(std::fs::read_to_string(&config)
        .unwrap()
        .contains("#spell = true"));
    std::fs::write(&config, "spell = false\n").unwrap();
    assert!(!write().status.success());
    assert_eq!(std::fs::read_to_string(&config).unwrap(), "spell = false\n");
    std::fs::remove_file(&config).unwrap();
    let absent = dir.join("absent.toml");
    std::os::unix::fs::symlink(&absent, &config).unwrap();
    assert!(!write().status.success());
    assert!(!absent.exists());
    std::fs::remove_dir_all(dir).unwrap();
}
