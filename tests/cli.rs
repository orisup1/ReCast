use std::process::Command;

// A unique executable name keeps status tests from discovering a developer's
// actual ReCast instance or another CLI test running in parallel.
fn isolated_binary(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("recast-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!(
        "rc-{}-{name}{}",
        std::process::id(),
        std::env::consts::EXE_SUFFIX
    ));
    std::fs::copy(env!("CARGO_BIN_EXE_recast"), &path).unwrap();
    path
}

#[test]
fn explain_previews_both_layouts_and_rejects_invalid_arguments() {
    let dir = std::env::temp_dir().join(format!("recast-explain-{}", std::process::id()));
    let config = if cfg!(target_os = "macos") {
        dir.join("Library/Application Support/recast")
    } else {
        dir.join("recast")
    };
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("abbrev.txt"), "zztest = preview expansion\n").unwrap();
    std::fs::write(config.join("ignore.txt"), "keyboad\n").unwrap();
    let preview = |args: &[&str]| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_recast"));
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("RECAST_") {
                command.env_remove(key);
            }
        }
        command
            .args(args)
            .env("HOME", &dir)
            .env("XDG_CONFIG_HOME", &dir)
            .env("APPDATA", &dir)
            .env("RECAST_LAYOUT_BACKEND", "none")
            .output()
            .unwrap()
    };
    for (word, layout, expected, reason) in [
        ("recieve", "en", "receive", "English spelling"),
        ("Recieve!", "en", "Receive!", "English spelling"),
        ("akuo", "en", "שלום", "Keyboard-layout correction"),
        ("יקךךם", "he", "hello", "Keyboard-layout correction"),
        (
            "רקבןקהק",
            "he",
            "receive",
            "Keyboard-layout correction with spelling",
        ),
        (
            "שלום",
            "he",
            "שלום",
            "Protected word or no confident correction",
        ),
        (
            "hello",
            "en",
            "hello",
            "Protected word or no confident correction",
        ),
        (
            "keyboad",
            "en",
            "keyboad",
            "Protected word or no confident correction",
        ),
        (
            "zztest",
            "en",
            "preview expansion",
            "Configured abbreviation",
        ),
    ] {
        // Windows resolves config through Known Folders, ignoring APPDATA.
        // Fixture-dependent checks run where HOME isolates the files.
        if cfg!(windows) && matches!(word, "keyboad" | "zztest") {
            continue;
        }
        let output = preview(&["--explain", word, "--layout", layout]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(
            text.contains(&format!("Replacement: {expected:?}")),
            "{text}"
        );
        assert!(text.contains(reason), "{text}");
    }
    for args in [
        vec!["--explain"],
        vec!["--explain", "hello"],
        vec!["--layout", "en"],
        vec!["--explain", "hello", "--layout", "fr"],
        vec!["--explain", "", "--layout", "en"],
        vec!["--explain", "two words", "--layout", "en"],
        vec!["--explain", "🙂", "--layout", "en"],
        vec!["--explain", "hello", "--layout", "en", "--stop"],
        vec!["--explain", "hello", "--layout", "en", "--layout", "he"],
    ] {
        assert_eq!(preview(&args).status.code(), Some(2), "{args:?}");
    }
    #[cfg(unix)]
    {
        std::fs::write(config.join("config.toml"), "spell = false\n").unwrap();
        for word in ["teh", "recieve"] {
            let output = preview(&["--explain", word, "--layout", "en"]);
            assert!(output.status.success());
            assert!(
                String::from_utf8_lossy(&output.stdout).contains(&format!("Replacement: {word:?}"))
            );
        }
        std::fs::remove_file(config.join("config.toml")).unwrap();
    }
    assert_eq!(
        preview(&["--explain", &"a".repeat(65), "--layout", "en"])
            .status
            .code(),
        Some(2)
    );
    assert_eq!(
        std::fs::read_dir(&config).unwrap().count(),
        2,
        "preview must not create state or personal data"
    );
    assert_eq!(
        std::fs::read_to_string(config.join("ignore.txt")).unwrap(),
        "keyboad\n"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

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
    let binary = isolated_binary("status");
    for value in ["4", "256", "-1", "l"] {
        let output = Command::new(&binary)
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
    let output = Command::new(&binary)
        .arg("--status")
        .env("RECAST_LAYOUT_BACKEND", "none")
        .env("RECAST_SPELL_DIST", "0")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("max distance 0)"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("RECAST_SPELL_DIST="));
    std::fs::remove_dir_all(binary.parent().unwrap()).unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn unreadable_config_stops_startup_and_status_but_not_help() {
    let binary = isolated_binary("config");
    let dir = std::env::temp_dir().join(format!("recast-cli-config-read-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("recast/config.toml")).unwrap();
    for arg in ["--foreground", "--status", "--help"] {
        let output = Command::new(&binary)
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
    std::fs::remove_dir_all(binary.parent().unwrap()).unwrap();
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
