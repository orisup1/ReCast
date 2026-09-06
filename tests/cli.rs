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
