use std::process::Command;

#[cfg(unix)]
#[test]
fn language_preference_supports_file_environment_status_and_preview() {
    let binary = isolated_binary("prefer");
    let dir = std::env::temp_dir().join(format!("recast-prefer-{}", std::process::id()));
    let config = if cfg!(target_os = "macos") {
        dir.join("Library/Application Support/recast")
    } else {
        dir.join("recast")
    };
    std::fs::create_dir_all(&config).unwrap();
    let invoke = |args: &[&str], preference: Option<&str>| {
        let mut command = Command::new(&binary);
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("RECAST_") {
                command.env_remove(key);
            }
        }
        command
            .args(args)
            .env("HOME", &dir)
            .env("XDG_CONFIG_HOME", &dir)
            .env("RECAST_LAYOUT_BACKEND", "none");
        if let Some(preference) = preference {
            command.env("RECAST_PREFER", preference);
        }
        let result = command.output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        result
    };
    for (mode, label, english_after, hebrew_after) in [
        ("balanced", "Balanced", "מגירה", "flute"),
        ("english", "Prefer English", "ndhrv", "flute"),
        ("hebrew", "Prefer Hebrew", "מגירה", "כךואק"),
    ] {
        std::fs::write(config.join("config.toml"), format!("prefer = \"{mode}\"\n")).unwrap();
        let status = invoke(&["--status"], None);
        let stdout = String::from_utf8_lossy(&status.stdout);
        assert!(
            stdout.contains(&format!("language preference  {label}")),
            "{stdout}"
        );
        for (word, layout, after) in [
            ("ndhrv", "en", english_after),
            ("כךואק", "he", hebrew_after),
            ("akuo", "en", "שלום"),
        ] {
            let preview = invoke(&["--explain", word, "--layout", layout], None);
            let stdout = String::from_utf8_lossy(&preview.stdout);
            assert!(
                stdout.contains(&format!("Replacement: {after:?}")),
                "{mode}: {stdout}"
            );
            assert!(
                stdout.contains(&format!("Language preference: {label}")),
                "{stdout}"
            );
        }
    }
    std::fs::write(config.join("config.toml"), "prefer = \"english\"\n").unwrap();
    let status = invoke(&["--status"], Some("hebrew"));
    assert!(String::from_utf8_lossy(&status.stdout).contains("language preference  Prefer Hebrew"));
    for invalid in ["engish", "", "balanced\nprefer=hebrew"] {
        let status = invoke(&["--status"], Some(invalid));
        assert!(String::from_utf8_lossy(&status.stdout).contains("language preference  Balanced"));
        assert!(String::from_utf8_lossy(&status.stderr)
            .contains("must be balanced, english, or hebrew"));
    }
    std::fs::remove_file(config.join("config.toml")).unwrap();
    let status = invoke(&["--status"], None);
    assert!(String::from_utf8_lossy(&status.stdout).contains("language preference  Balanced"));
    std::fs::remove_dir_all(dir).unwrap();
    std::fs::remove_dir_all(binary.parent().unwrap()).unwrap();
}

// A unique executable name keeps status tests from discovering a developer's
// actual ReCast instance or another CLI test running in parallel.
fn isolated_binary(name: &str) -> std::path::PathBuf {
    // Keep the fixture on the binary's filesystem. Linux needs a hard link to
    // avoid ETXTBSY while other tests spawn children of the source executable.
    // macOS derives proc_pidpath from the executable's vnode, so hard links can
    // all report the same name. A copy gives each fixture its own identity.
    let source = std::path::Path::new(env!("CARGO_BIN_EXE_recast"));
    let dir = source
        .parent()
        .unwrap()
        .join(format!("recast-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!(
        "rc-{}-{name}{}",
        std::process::id(),
        std::env::consts::EXE_SUFFIX
    ));
    #[cfg(target_os = "macos")]
    std::fs::copy(source, &path).unwrap();
    #[cfg(not(target_os = "macos"))]
    std::fs::hard_link(source, &path).unwrap();
    path
}

#[cfg(unix)]
#[test]
fn personal_data_cannot_be_cleared_while_another_instance_can_rewrite_it() {
    use std::os::unix::fs::OpenOptionsExt;
    use std::time::{Duration, Instant};

    struct HeldChild(std::process::Child);
    impl Drop for HeldChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    let binary = isolated_binary("clear");
    let dir = std::env::temp_dir().join(format!("recast-clear-{}", std::process::id()));
    let config = if cfg!(target_os = "macos") {
        dir.join("Library/Application Support/recast")
    } else {
        dir.join("recast")
    };
    let personal = config.join("personal");
    std::fs::create_dir_all(&personal).unwrap();
    let frequency = personal.join("freq.txt");
    std::fs::write(&frequency, "retainedprivateword\t7\n").unwrap();
    // Keep a same-name peer alive without starting keyboard capture or
    // touching a developer's running daemon. Its config read blocks on a FIFO.
    let config_file = config.join("config.toml");
    nix::unistd::mkfifo(
        &config_file,
        nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
    )
    .unwrap();
    let mut held = HeldChild(
        Command::new(&binary)
            .arg("--status")
            .env("HOME", &dir)
            .env("XDG_CONFIG_HOME", &dir)
            .env("RECAST_LAYOUT_BACKEND", "none")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    let writer = loop {
        match std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(nix::libc::O_NONBLOCK)
            .open(&config_file)
        {
            Ok(writer) => break writer,
            Err(error) if error.raw_os_error() == Some(nix::libc::ENXIO) => {
                assert!(held.0.try_wait().unwrap().is_none());
                assert!(Instant::now() < deadline, "fixture did not read config");
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("opening config FIFO: {error}"),
        }
    };
    let clear = || {
        Command::new(&binary)
            .arg("--clear-personal-data")
            .env("HOME", &dir)
            .env("XDG_CONFIG_HOME", &dir)
            .output()
            .unwrap()
    };
    let refused = clear();
    assert!(
        !refused.status.success(),
        "a live peer can restore old data"
    );
    let error = String::from_utf8_lossy(&refused.stderr);
    assert!(error.contains("Stop ReCast first"), "{error}");
    assert_eq!(
        std::fs::read_to_string(&frequency).unwrap(),
        "retainedprivateword\t7\n"
    );
    assert!(held.0.try_wait().unwrap().is_none());
    drop(held);
    drop(writer);
    let cleared = clear();
    assert!(
        cleared.status.success(),
        "{}",
        String::from_utf8_lossy(&cleared.stderr)
    );
    assert!(!personal.exists());
    std::fs::remove_dir_all(dir).unwrap();
    std::fs::remove_dir_all(binary.parent().unwrap()).unwrap();
}

#[cfg(unix)]
#[test]
fn status_does_not_discover_another_isolated_cli_binary() {
    use std::os::unix::fs::OpenOptionsExt;
    use std::time::{Duration, Instant};

    struct HeldChild(std::process::Child);
    impl Drop for HeldChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    let held_binary = isolated_binary("held");
    let viewer_binary = isolated_binary("view");
    let dir = std::env::temp_dir().join(format!("recast-cli-peers-{}", std::process::id()));
    let held_home = dir.join("held");
    let viewer_home = dir.join("viewer");
    let config = if cfg!(target_os = "macos") {
        held_home.join("Library/Application Support/recast/config.toml")
    } else {
        held_home.join("recast/config.toml")
    };
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::create_dir_all(&viewer_home).unwrap();
    nix::unistd::mkfifo(
        &config,
        nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
    )
    .unwrap();
    let mut held = HeldChild(
        Command::new(&held_binary)
            .arg("--status")
            .env("HOME", &held_home)
            .env("XDG_CONFIG_HOME", &held_home)
            .env("RECAST_LAYOUT_BACKEND", "none")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );

    // Opening the FIFO writer proves the child is reading its configuration.
    // Hold it open without sending data so the child stays alive throughout
    // the second status query, without starting either keyboard listener.
    let deadline = Instant::now() + Duration::from_secs(5);
    let writer = loop {
        match std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(nix::libc::O_NONBLOCK)
            .open(&config)
        {
            Ok(writer) => break writer,
            Err(error) if error.raw_os_error() == Some(nix::libc::ENXIO) => {
                assert!(
                    held.0.try_wait().unwrap().is_none(),
                    "fixture exited before reading config"
                );
                assert!(Instant::now() < deadline, "fixture did not read config");
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("opening config FIFO: {error}"),
        }
    };
    let output = Command::new(&viewer_binary)
        .arg("--status")
        .env("HOME", &viewer_home)
        .env("XDG_CONFIG_HOME", &viewer_home)
        .env("RECAST_LAYOUT_BACKEND", "none")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("not the running daemon"), "{stdout}");
    assert!(held.0.try_wait().unwrap().is_none());
    drop(held);
    drop(writer);
    std::fs::remove_dir_all(dir).unwrap();
    std::fs::remove_dir_all(held_binary.parent().unwrap()).unwrap();
    std::fs::remove_dir_all(viewer_binary.parent().unwrap()).unwrap();
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
        ("hellorecieve", "en", "receive", "Merged-token"),
        ("helloRecieve!", "en", "Receive!", "Merged-token"),
        ("helloakuo", "en", "שלום", "Merged-token"),
        ("helloakuo,", "en", "שלום,", "Merged-token"),
        ("(hellorecieve)", "en", "(receive)", "Merged-token"),
        ("שלוםיקךךם", "he", "hello", "Merged-token"),
        ("שלוםרקבןקהק", "he", "receive", "Merged-token"),
        ("שלוםמקלדתת", "he", "מקלדת", "Merged-token"),
        ("helloworld", "en", "helloworld", "Protected word"),
        ("hellokeyboad", "en", "hellokeyboad", "Protected word"),
        ("enviroment", "en", "environment", "English spelling"),
        ("priviledge", "en", "privilege", "English spelling"),
        (
            "ines",
            "en",
            "ines",
            "Protected word or no confident correction",
        ),
        (
            "nour",
            "en",
            "nour",
            "Protected word or no confident correction",
        ),
        (
            "soren",
            "en",
            "soren",
            "Protected word or no confident correction",
        ),
        (
            "wasmtime",
            "en",
            "wasmtime",
            "Protected word or no confident correction",
        ),
        ("מקלדתת", "he", "מקלדת", "Hebrew spelling"),
        ("וכשמקלדתת", "he", "וכשמקלדת", "Hebrew spelling"),
        (
            "מחשבש",
            "he",
            "מחשבש",
            "Protected word or no confident correction",
        ),
        ("thos", "en", "this", "English spelling"),
        ("xocmputer", "en", "computer", "English spelling"),
        ("computerx", "en", "computer", "English spelling"),
        (
            "סםבצפואקר",
            "he",
            "computer",
            "Keyboard-layout correction with spelling",
        ),
        ("גםמ,א", "he", "don't", "Keyboard-layout correction"),
        ("ן,צ", "he", "i'm", "Keyboard-layout correction"),
        (
            "לקטנםשרג,ד",
            "he",
            "keyboard's",
            "Keyboard-layout correction",
        ),
        (
            "איםד",
            "he",
            "this",
            "Keyboard-layout correction with spelling",
        ),
        ("Recieve!", "en", "Receive!", "English spelling"),
        ("akuo", "en", "שלום", "Keyboard-layout correction"),
        (
            "thbyrbhy.",
            "en",
            "אינטרנט.",
            "Keyboard-layout correction with spelling",
        ),
        ("יקךךם", "he", "hello", "Keyboard-layout correction"),
        (
            "תודדה.",
            "he",
            "תודה.",
            "Hebrew spelling or learned replacement",
        ),
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
        if cfg!(windows) && matches!(word, "keyboad" | "hellokeyboad" | "zztest") {
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
        assert!(text.contains("English reading:"), "{text}");
        assert!(text.contains("Hebrew reading:"), "{text}");
        if word == "recieve" {
            assert!(text.contains("receive: score"), "{text}");
            assert!(text.contains("frequency rank"), "{text}");
        }
        if word == "keyboad" {
            assert!(text.contains("ignored word"), "{text}");
        }
        if word == "hello" {
            assert!(text.contains("Protected dictionary word"), "{text}");
        }
        if matches!(word, "מקלדתת" | "וכשמקלדתת" | "מחשבש") {
            assert!(text.contains("Hebrew spelling candidates"), "{text}");
            assert!(text.contains("Keep original:"), "{text}");
        }
        if word == "מחשבש" {
            assert!(text.contains("Score gap:"), "{text}");
            assert!(
                text.contains("leading Hebrew candidates are too close"),
                "{text}"
            );
        }
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
        vec!["--context", "data"],
        vec!["--context", "data", "--layout", "en"],
        vec!["--explain", "baye", "--layout", "en", "--context"],
        vec!["--explain", "baye", "--context", "--layout", "en"],
        vec![
            "--explain",
            "baye",
            "--layout",
            "en",
            "--context",
            "data",
            "--context",
            "other",
        ],
    ] {
        assert_eq!(preview(&args).status.code(), Some(2), "{args:?}");
    }
    for (context, expected) in [
        ("data", "base"),
        ("data\t", "base"),
        ("the data", "base"),
        ("we store all the data", "base"),
        ("data\n", "baye"),
        ("data\r\n", "baye"),
        ("data.", "baye"),
        ("data,", "baye"),
        ("data .", "baye"),
        ("data\nhello", "baye"),
        ("hello\ndata", "base"),
        ("", "baye"),
    ] {
        let output = preview(&["--explain", "baye", "--layout", "en", "--context", context]);
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            text.contains(&format!("Replacement: {expected:?}")),
            "context {context:?}: {text}"
        );
        assert!(text.contains(&format!("Context: {context:?}")), "{text}");
        assert!(text.contains("without live app checks"), "{text}");
        assert!(text.contains("No text is injected or learned"), "{text}");
    }
    for (word, layout, expected, context) in [
        ("akuo", "en", "שלום", "please keep writing in english"),
        ("יקךךם", "he", "hello", "אני רוצה להמשיך לכתוב בעברית"),
        (
            "thbyrbhy.",
            "en",
            "אינטרנט.",
            "please keep writing in english",
        ),
        ("רקבןקהק", "he", "receive", "אני רוצה להמשיך לכתוב בעברית"),
    ] {
        let output = preview(&["--explain", word, "--layout", layout, "--context", context]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(
            text.contains(&format!("Replacement: {expected:?}")),
            "context {context:?}: {text}"
        );
        assert!(
            text.contains("takes precedence over contextual intent"),
            "{text}"
        );
    }
    let output = preview(&["--explain", "baye", "--layout", "en"]);
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success());
    assert!(text.contains("Replacement: \"baye\""), "{text}");
    assert!(
        text.contains("without prior words or live app checks"),
        "{text}"
    );
    assert!(!text.contains("Context:"), "{text}");
    let too_many = "data ".repeat(17);
    assert_eq!(
        preview(&[
            "--explain",
            "baye",
            "--layout",
            "en",
            "--context",
            &too_many
        ])
        .status
        .code(),
        Some(2)
    );
    #[cfg(unix)]
    {
        std::fs::write(config.join("config.toml"), "split = false\n").unwrap();
        for (word, layout) in [
            ("hellorecieve", "en"),
            ("helloakuo", "en"),
            ("שלוםמקלדתת", "he"),
        ] {
            let output = preview(&["--explain", word, "--layout", layout]);
            assert!(output.status.success());
            let text = String::from_utf8_lossy(&output.stdout);
            assert!(text.contains(&format!("Replacement: {word:?}")), "{text}");
        }
        std::fs::write(config.join("config.toml"), "spell = false\n").unwrap();
        for word in ["teh", "recieve", "xocmputer", "computerx"] {
            let output = preview(&["--explain", word, "--layout", "en"]);
            assert!(output.status.success());
            assert!(
                String::from_utf8_lossy(&output.stdout).contains(&format!("Replacement: {word:?}"))
            );
            assert!(
                String::from_utf8_lossy(&output.stdout).contains("English spelling is disabled")
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
        assert!(stdout.contains("max rank 100000)"), "{stdout}");
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
    for (action, completion, expected_action, expected_completion, complaint) in [
        (
            "left_shift",
            "right_ctrl",
            "double-tap Left Shift",
            "tap Right Ctrl",
            "",
        ),
        (
            "none",
            "none",
            "double-tap action disabled",
            "completion shortcut disabled",
            "",
        ),
        (
            "ctrl",
            "left_ctrl",
            "double-tap Either Ctrl",
            "completion shortcut disabled",
            "conflicts",
        ),
        (
            "invalid",
            "tab",
            "double-tap Either Ctrl",
            "tap Right Shift",
            "is invalid",
        ),
    ] {
        let output = Command::new(&binary)
            .arg("--status")
            .env("RECAST_LAYOUT_BACKEND", "none")
            .env("RECAST_ACTION_SHORTCUT", action)
            .env("RECAST_COMPLETION_SHORTCUT", completion)
            .env("RECAST_UNDO_SHORTCUT", "none")
            .output()
            .unwrap();
        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stdout.contains(expected_action), "{stdout}");
        assert!(stdout.contains(expected_completion), "{stdout}");
        if !complaint.is_empty() {
            assert!(stderr.contains(complaint), "{stderr}");
        }
    }
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
