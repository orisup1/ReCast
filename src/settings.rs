//! Where a setting's value comes from: the environment, then the config file,
//! then the shipped default.
//!
//! Every knob in the program used to be environment-only, and that made almost
//! all of them unreachable in practice. ReCast is started by a service manager
//! on all three platforms — a systemd user unit, a launchd LaunchAgent, a logon
//! Scheduled Task — and none of those inherit the shell environment a user
//! would type `RECAST_SPELL_DIST=1` into. `--help` documented twenty settings
//! that only worked if you ran the binary by hand from a terminal, which is the
//! one way it is not meant to be run.
//!
//! So there is a file as well: `<config dir>/recast/config.toml`, next to the
//! `abbrev.txt` and `ignore.txt` that are already there. The environment still
//! wins where both are set, because a variable is the more deliberate of the
//! two — someone who exports one for a single run is overriding the file on
//! purpose.
//!
//! # The format
//!
//! Flat `key = value` lines, `#` comments, blank lines ignored — a strict
//! subset of TOML, so an editor's TOML mode does the right thing and nobody has
//! to learn a format for ten scalars. There are no tables and no arrays: every
//! setting here is a bool, number, or string. Keys are the environment names without
//! their `RECAST_` prefix, lowercased, so `RECAST_SPELL_DIST` is `spell_dist`
//! and the two spellings of a setting can never drift apart — [`file_key`] is
//! the only place the mapping exists.
//!
//! Read once, at startup, into a `OnceLock`: the callers ([`crate::config`] and
//! [`crate::timing`]) both cache what they build from it, and one of them is
//! consulted inside the loop that paces individual keystrokes. Editing the file
//! takes a restart — unlike `abbrev.txt`/`ignore.txt`, which are watched.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;

/// Where the file lives. `None` when the OS has no config directory at all,
/// which is also how "there is no file" is spelled — neither is an error.
pub fn file_path() -> Option<PathBuf> {
    crate::complete::user_path("config.toml")
}

/// Save a menu setting before applying it, so a failed save cannot look successful.
pub fn set_live(control: &crate::types::AppControl, key: &str, value: &str) -> Result<(), String> {
    if std::env::var_os(format!("RECAST_{}", key.to_uppercase())).is_some() {
        return Err(format!("{key} is controlled by an environment variable. Remove that override and relaunch to change it here."));
    }
    match key {
        "spell" | "complete" if parse_flag(value).is_some() => (),
        "spell_dist" if parse_number("RECAST_SPELL_DIST", value).is_ok() => (),
        "exclude_apps" | "layout_only_apps" if !value.contains(['\n', '\r', '"', '#', '\\']) => (),
        "undo_shortcut" if crate::config::valid_undo_shortcut(value) => (),
        _ => return Err("Invalid setting value".into()),
    }
    let path = file_path().ok_or("No config directory available")?;
    save_value(&path, key, value).map_err(|e| format!("Could not save settings: {e}"))?;
    crate::config::Config::update_live(|cfg| match key {
        "spell" => cfg.spell_enabled = parse_flag(value).unwrap(),
        "complete" => cfg.complete_enabled = parse_flag(value).unwrap(),
        "spell_dist" => cfg.spell_max_dist = value.trim().parse().unwrap(),
        "exclude_apps" => cfg.excluded_apps = crate::config::parse_excluded_apps(value),
        "layout_only_apps" => cfg.layout_only_apps = crate::config::parse_excluded_apps(value),
        "undo_shortcut" => cfg.undo_shortcut = value.into(),
        _ => unreachable!(),
    });
    if key == "exclude_apps" {
        *crate::types::lock_forgiving(&control.excluded_apps) =
            crate::config::parse_excluded_apps(value);
    }
    if key == "layout_only_apps" {
        *crate::types::lock_forgiving(&control.layout_only_apps) =
            crate::config::parse_excluded_apps(value);
    }
    Ok(())
}

fn save_value(path: &std::path::Path, key: &str, value: &str) -> std::io::Result<()> {
    save_values(path, &[(key, value)])
}

fn save_values(path: &std::path::Path, values: &[(&str, &str)]) -> std::io::Result<()> {
    // Replacing a symlink would silently disconnect the user's managed config.
    if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(std::io::Error::other(
            "Settings is a symlink; edit its target instead",
        ));
    }
    let previous = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let mut text = String::new();
    for line in previous.split_inclusive('\n') {
        if !line.split_once('=').is_some_and(|(k, _)| {
            values
                .iter()
                .any(|(key, _)| k.trim().eq_ignore_ascii_case(key))
        }) {
            text.push_str(line);
        }
    }
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    for (key, value) in values {
        text.push_str(&format!("{key} = \"{value}\"\n"));
    }
    std::fs::create_dir_all(
        path.parent()
            .ok_or_else(|| std::io::Error::other("Missing settings directory"))?,
    )?;
    let temp = path.with_extension(format!("{}.tmp", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp)?;
    let result = (|| {
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// Save both lists together so changing modes cannot partially discard an exclusion.
pub fn set_app_mode(
    control: &crate::types::AppControl,
    id: &str,
    mode: crate::config::AppMode,
) -> Result<(), String> {
    use crate::config::AppMode;
    if id.trim().is_empty() || id.contains([',', '\n', '\r', '"', '#', '\\']) {
        return Err("This application identifier cannot be saved".into());
    }
    for key in ["RECAST_EXCLUDE_APPS", "RECAST_LAYOUT_ONLY_APPS"] {
        if std::env::var_os(key).is_some() {
            return Err(format!("{key} controls application modes. Remove the override and relaunch to change them here."));
        }
    }
    let id = id.trim().to_lowercase();
    let mut excluded = crate::types::lock_forgiving(&control.excluded_apps).clone();
    let mut layout = crate::types::lock_forgiving(&control.layout_only_apps).clone();
    excluded.retain(|app| app != &id);
    layout.retain(|app| app != &id);
    match mode {
        AppMode::Off => excluded.push(id),
        AppMode::LayoutOnly => layout.push(id),
        AppMode::Full => {}
    }
    let path = file_path().ok_or("No config directory available")?;
    save_values(
        &path,
        &[
            ("exclude_apps", &excluded.join(", ")),
            ("layout_only_apps", &layout.join(", ")),
        ],
    )
    .map_err(|e| format!("Could not save application modes: {e}"))?;
    crate::config::Config::update_live(|cfg| {
        cfg.excluded_apps = excluded.clone();
        cfg.layout_only_apps = layout.clone();
    });
    let mut live_excluded = crate::types::lock_forgiving(&control.excluded_apps);
    let mut live_layout = crate::types::lock_forgiving(&control.layout_only_apps);
    *live_excluded = excluded;
    *live_layout = layout;
    Ok(())
}

/// The file key for an environment name: `RECAST_SPELL_DIST` → `spell_dist`.
///
/// Mechanical on purpose. The alternative — a table pairing the two spellings —
/// is a table someone has to remember to add a line to, and the failure when
/// they forget is a setting that silently only works one of the two ways.
fn file_key(env_key: &str) -> String {
    env_key
        .strip_prefix("RECAST_")
        .unwrap_or(env_key)
        .to_lowercase()
}

/// The parsed file, or an empty table if there isn't one.
///
/// A missing file is optional. Read errors must stop startup because falling
/// back could silently discard application exclusions.
fn parsed() -> &'static Parsed {
    static PARSED: OnceLock<Parsed> = OnceLock::new();
    PARSED.get_or_init(|| file_path().map_or_else(Parsed::default, |path| load_file(&path)))
}

fn load_file(path: &std::path::Path) -> Parsed {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text),
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                && std::fs::symlink_metadata(path)
                    .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            Parsed::default()
        }
        Err(error) => Parsed {
            read_error: Some(format!("Cannot read {}: {error}", path.display())),
            ..Parsed::default()
        },
    }
}

/// Check the same cached read used by every setting, before stopping peers.
pub fn check_readable() -> Result<(), &'static str> {
    parsed().read_error.as_deref().map_or(Ok(()), Err)
}

fn table() -> &'static HashMap<String, String> {
    &parsed().settings
}

/// What one config file amounts to: the settings in it, and the lines that
/// were not settings.
#[derive(Default)]
struct Parsed {
    settings: HashMap<String, String>,
    read_error: Option<String>,
    /// 1-based line numbers with no `=` on them, for [`complaints`]. Kept as
    /// numbers rather than as the text: a config file can contain anything, and
    /// echoing a line back is how a diagnostic becomes the longest thing on the
    /// screen.
    malformed: Vec<usize>,
}

/// Split `key = value` lines into a table.
///
/// Deliberately forgiving about everything except the shape: a line without an
/// `=` is skipped rather than rejected, because the file is hand-edited and the
/// cost of one bad line should be that line, not the nine good ones under it.
/// What it is *not* forgiving about is silence — anything skipped or unknown is
/// named by [`complaints`].
fn parse(text: &str) -> Parsed {
    let mut out = Parsed::default();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            out.malformed.push(n + 1);
            continue;
        };
        // Values are scalars, so `#` can only begin the inline explanation
        // used by the generated sample file.
        let value = value
            .split_once('#')
            .map_or(value, |(value, _)| value)
            .trim();
        // Quotes are stripped so `spell = "0"` and `spell = 0` mean the same
        // thing. A TOML string is what an editor's autocomplete will offer, and
        // the difference between the two is not one worth having an opinion
        // about for a value that is always a bool or an integer.
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .unwrap_or(value);
        out.settings
            .insert(key.trim().to_lowercase(), value.to_string());
    }
    out
}

/// Where a value came from, so a complaint about it can say where to fix it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    Env,
    File,
}

impl Source {
    /// How to name this source to the user, given the environment spelling of
    /// the key.
    pub fn describe(self, env_key: &str) -> String {
        match self {
            Source::Env => env_key.to_string(),
            Source::File => format!("{} in config.toml", file_key(env_key)),
        }
    }
}

/// The raw value of a setting and where it came from, or `None` if neither
/// source sets it.
pub fn lookup(env_key: &str) -> Option<(String, Source)> {
    if let Ok(value) = std::env::var(env_key) {
        return Some((value, Source::Env));
    }
    table()
        .get(&file_key(env_key))
        .map(|v| (v.clone(), Source::File))
}

/// The value of a setting, whichever source set it.
pub fn get(env_key: &str) -> Option<String> {
    lookup(env_key).map(|(value, _)| value)
}

/// Loading and diagnostics must accept exactly the same values.
pub fn parse_number(key: &str, raw: &str) -> Result<u64, String> {
    let value = raw
        .trim()
        .parse::<u64>()
        .map_err(|_| "is not an unsigned integer".to_string())?;
    let max = match key {
        "RECAST_SPELL_DIST" => 3,
        "RECAST_SPELL_RANK" | "RECAST_COMPLETE_RANK" => u32::MAX as u64,
        "RECAST_SPELL_MIN" | "RECAST_COMPLETE_MIN" => usize::MAX as u64,
        _ => u64::MAX,
    };
    if value > max {
        return Err(format!("must be between 0 and {max}"));
    }
    Ok(value)
}

/// Settings that were set but could not be used, described for the user.
///
/// Falling back to the shipped default on a value that cannot be parsed is the
/// right behaviour — a bad value should not stop the program — but doing it
/// *silently* inverts the user's intent in the one case that matters.
/// `spell_dist = l` (an el for a one) reads as the default 3, the loosest
/// setting there is, from someone who was plainly trying to tighten it.
///
/// Three kinds of mistake are caught: a numeric setting that is not a number, a
/// key in the file that is not a setting at all (a typo, or a name from an
/// older version), and a line in the file with no `=` on it. `--status` reads
/// these out.
pub fn complaints(numeric_keys: &[&str], boolean_keys: &[&str], all_keys: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(value) = get("RECAST_UNDO_SHORTCUT") {
        if !crate::config::valid_undo_shortcut(&value) {
            out.push("undo_shortcut must be none, left_ctrl, or right_ctrl — using none.".into());
        }
    }

    for key in numeric_keys {
        if let Some((raw, source)) = lookup(key) {
            if let Err(reason) = parse_number(key, &raw) {
                out.push(format!(
                    "{}={raw:?} {reason} — using the default instead.",
                    source.describe(key)
                ));
            }
        }
    }

    for key in boolean_keys {
        if let Some((raw, source)) = lookup(key) {
            if parse_flag(&raw).is_none() {
                out.push(format!(
                    "{}={raw:?} is not true/false or 1/0 — using the default instead.",
                    source.describe(key)
                ));
            }
        }
    }

    // A key the program does not know is the quietest failure of the lot: the
    // file parses, the daemon starts, and the setting the user came to change
    // is exactly as it was.
    let known: Vec<String> = all_keys.iter().map(|k| file_key(k)).collect();
    let mut unknown: Vec<&String> = table().keys().filter(|k| !known.contains(k)).collect();
    unknown.sort();
    for key in unknown {
        out.push(format!("{key} in config.toml is not a ReCast setting."));
    }

    for line in &parsed().malformed {
        out.push(format!(
            "config.toml line {line} has no `=` on it — skipped."
        ));
    }

    out
}

/// The commented sample file, written on demand so there is something to edit.
///
/// Every line is commented out, so writing it changes no behaviour: it is a
/// list of what can be set and what the shipped value is, which is the thing
/// `--help` could never be — you cannot edit `--help`.
pub fn sample() -> String {
    let d = crate::timing::DEFAULTS;
    format!(
        "\
# ReCast settings. Every line here is commented out and shows the shipped
# default — uncomment one to change it. The matching RECAST_* environment
# variable overrides this file when both are set.
#
# Read once at startup, so changes take effect on the next launch.

# Correction pipelines
#exclude_apps = \"\"    # exact app IDs where correction is Off
#layout_only_apps = \"\" # exact app IDs where only layout correction is allowed
#undo_shortcut = \"none\" # none, left_ctrl, or right_ctrl; double-tap Ctrl stays active
#personal = false      # persist local word/correction/timing data (privacy-sensitive)
#short = true          # short switches: rank <= 20000; false restricts to <= 500
#split = false         # missing-space split fallback (opt-in; can mis-split)
#freq = true           # homograph frequency tie-break
#spell = true          # English spelling autocorrect
#spell_min = {spell_min}           # shortest word the speller may fix
#spell_rank = {spell_rank}      # worst frequency rank a suggestion may have
#spell_dist = {spell_dist}          # maximum edit distance, 0 to 3 (0 disables)
#complete = true       # word completion + abbreviation expansion
#complete_min = {complete_min}        # shortest prefix that will be completed
#complete_rank = {complete_rank}   # worst frequency rank a completion may have
#debug = false         # log every word check and switch decision

# Linux only: what drives the keyboard layout. Detected from the session when
# unset — set it if the guess is wrong. One of hyprland, sway, kde, gnome,
# x11, none. `recast --status` prints what was chosen and the layouts it found.
#layout_backend = x11

# Injection timings, in microseconds. Only worth touching if corrections come
# out scrambled, or if you want them faster and are willing to measure.
#inject_press_gap = {press_gap}       # key-down to key-up (macOS)
#inject_key_gap = {key_gap}         # between injected keys (macOS)
#inject_settle = {settle}         # after the last event, before listening again
#inject_held_timeout = {held}   # longest wait for you to lift a key being retyped
#inject_term_timeout = {term}  # longest wait for you to lift space/enter (Linux)
#inject_held_poll = {held_poll}         # how often those waits re-check
#inject_device_settle = {device}  # injector device detection at startup (Linux)
#inject_layout_confirm = {confirm} # longest wait for a layout switch to take effect
#inject_layout_poll = {layout_poll}      # how often that confirmation re-checks
#inject_batch_gap = {batch}        # between writes of a correction (Linux)
",
        spell_min = crate::config::DEFAULT_SPELL_MIN_LEN,
        spell_rank = crate::config::DEFAULT_SPELL_MAX_RANK,
        spell_dist = crate::config::DEFAULT_SPELL_MAX_DIST,
        complete_min = crate::config::DEFAULT_COMPLETE_MIN_LEN,
        complete_rank = crate::config::DEFAULT_COMPLETE_MAX_RANK,
        press_gap = d.press_gap.as_micros(),
        key_gap = d.inter_key_gap.as_micros(),
        settle = d.settle.as_micros(),
        held = d.held_release_timeout.as_micros(),
        term = d.terminator_release_timeout.as_micros(),
        held_poll = d.held_poll.as_micros(),
        device = d.device_settle.as_micros(),
        confirm = d.layout_confirm.as_micros(),
        layout_poll = d.layout_poll.as_micros(),
        batch = d.batch_gap.as_micros(),
    )
}

/// Write the sample file, unless one is already there.
///
/// Returns the path on success. Refuses to overwrite: the file is the user's,
/// and the one thing worse than not having a config file is having the one you
/// wrote replaced by a comment block.
pub fn write_sample() -> Result<PathBuf, String> {
    let path = file_path().ok_or_else(|| "no OS config directory to write to".to_string())?;
    if path.exists() {
        return Err(format!(
            "{} already exists — leaving it alone.",
            path.display()
        ));
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .and_then(|mut file| file.write_all(sample().as_bytes()))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

/// Get a boolean setting from the environment, falling back to `default` when
/// unset or unparsable.
pub fn flag(key: &str, default: bool) -> bool {
    get(key).and_then(|v| parse_flag(&v)).unwrap_or(default)
}

fn parse_flag(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" => Some(true),
        "0" | "false" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_menu_changes_apply_and_persist() {
        // Isolate process-global config from the parallel dictionary tests.
        if std::env::var_os("RECAST_MENU_TEST_CHILD").is_none() {
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "settings::tests::live_menu_changes_apply_and_persist",
                ])
                .env("RECAST_MENU_TEST_CHILD", "1")
                .env_remove("RECAST_SPELL")
                .env_remove("RECAST_COMPLETE")
                .env_remove("RECAST_SPELL_DIST")
                .env_remove("RECAST_EXCLUDE_APPS")
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
            return;
        }
        let control = crate::types::AppControl::new_for_test();
        let dict = crate::dictionary::Dict::of(&["hello"]);
        let freq = crate::dictionary::Freq::of(&[("hello", 1)]);
        assert_eq!(
            crate::spell::correct("helo", dict, freq).as_deref(),
            Some("hello")
        );
        set_live(&control, "spell", "false").unwrap();
        assert!(crate::spell::correct("helo", dict, freq).is_none());
        set_live(&control, "spell", "true").unwrap();
        assert_eq!(
            crate::spell::correct("helo", dict, freq).as_deref(),
            Some("hello")
        );
        set_live(&control, "complete", "false").unwrap();
        assert!(crate::complete::completions("hel", dict, freq).is_empty());
        set_live(&control, "complete", "true").unwrap();
        assert_eq!(crate::complete::completions("hel", dict, freq), ["hello"]);
        set_live(&control, "spell_dist", "1").unwrap();
        assert_eq!(crate::config::Config::global().spell_max_dist, 1);
        assert!(set_live(&control, "spell_dist", "4").is_err());
        set_app_mode(&control, "Editor", crate::config::AppMode::Off).unwrap();
        assert_eq!(
            *crate::types::lock_forgiving(&control.excluded_apps),
            ["editor"]
        );
        assert_eq!(
            load_file(&file_path().unwrap()).settings["exclude_apps"],
            "editor"
        );
        set_app_mode(&control, "EDITOR", crate::config::AppMode::Full).unwrap();
        assert!(crate::types::lock_forgiving(&control.excluded_apps).is_empty());
        use crate::config::AppMode;
        set_app_mode(&control, "Editor", AppMode::LayoutOnly).unwrap();
        assert_eq!(control.app_mode(Some("EDITOR")), Some(AppMode::LayoutOnly));
        assert_eq!(
            load_file(&file_path().unwrap()).settings["layout_only_apps"],
            "editor"
        );
        assert_eq!(control.app_mode(None), None);
        assert!(set_app_mode(&control, "bad,id", AppMode::Off).is_err());
        set_app_mode(&control, "Editor", AppMode::Off).unwrap();
        assert!(crate::types::lock_forgiving(&control.layout_only_apps).is_empty());
        assert_eq!(
            load_file(&file_path().unwrap()).settings["exclude_apps"],
            "editor"
        );
        set_live(&control, "undo_shortcut", "right_ctrl").unwrap();
        assert_eq!(crate::config::Config::global().undo_shortcut, "right_ctrl");
        assert!(set_live(&control, "undo_shortcut", "ctrl+z").is_err());
        std::fs::write(file_path().unwrap(), [0xff]).unwrap();
        assert!(set_app_mode(&control, "Editor", AppMode::Full).is_err());
        assert_eq!(control.app_mode(Some("Editor")), Some(AppMode::Off));
        assert!(set_live(&control, "spell", "false").is_err());
        assert!(crate::config::Config::global().spell_enabled);
        std::fs::remove_file(file_path().unwrap()).unwrap();
    }

    #[test]
    fn menu_settings_preserve_other_settings_and_refuse_unreadable_files() {
        let dir = std::env::temp_dir().join(format!("recast-menu-settings-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "# My settings\nspell = true\nSPELL = true\nexclude_apps = \"Code.exe\"",
        )
        .unwrap();
        save_value(&path, "spell", "false").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            text,
            "# My settings\nexclude_apps = \"Code.exe\"\nspell = \"false\"\n"
        );
        save_value(&path, "exclude_apps", "Code.exe, Terminal.exe").unwrap();
        assert_eq!(
            load_file(&path).settings["exclude_apps"],
            "Code.exe, Terminal.exe"
        );
        std::fs::write(&path, [0xff]).unwrap();
        assert!(save_value(&path, "spell", "true").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), [0xff]);
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn missing_config_is_optional_but_unreadable_config_is_an_error() {
        let dir = std::env::temp_dir().join(format!("recast-config-read-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("config.toml");
        assert!(load_file(&path).read_error.is_none());
        std::fs::write(&path, b"exclude_apps = \"Editor\"\n").unwrap();
        assert_eq!(load_file(&path).settings["exclude_apps"], "Editor");
        std::fs::write(&path, [0xff]).unwrap();
        assert!(load_file(&path).read_error.is_some());
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(load_file(&path).read_error.is_some());
        std::fs::remove_dir(&path).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.join("missing"), &path).unwrap();
            assert!(load_file(&path).read_error.is_some());
            std::fs::remove_file(&path).unwrap();
        }
        std::fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn the_file_key_is_the_env_name_without_its_prefix() {
        assert_eq!(file_key("RECAST_SPELL_DIST"), "spell_dist");
        assert_eq!(file_key("RECAST_INJECT_BATCH_GAP"), "inject_batch_gap");
        // Already a file key, or something that never had the prefix: left
        // alone rather than mangled.
        assert_eq!(file_key("spell_dist"), "spell_dist");
    }

    #[test]
    fn comments_and_blank_lines_are_not_settings() {
        let t = parse(
            "\
# a comment

spell_dist = 1

  # an indented comment
spell_min = 5
",
        );
        assert_eq!(t.settings.get("spell_dist").map(String::as_str), Some("1"));
        assert_eq!(t.settings.get("spell_min").map(String::as_str), Some("5"));
        assert_eq!(t.settings.len(), 2);
        assert!(t.malformed.is_empty(), "a comment is not a broken line");
    }

    #[test]
    fn quotes_and_whitespace_are_stripped_from_both_sides() {
        let t = parse("  SPELL  =  \"0\"  \nsplit=1\n");
        // Keys fold to lowercase, so a user shouting at the file still gets the
        // setting they asked for.
        assert_eq!(t.settings.get("spell").map(String::as_str), Some("0"));
        assert_eq!(t.settings.get("split").map(String::as_str), Some("1"));
    }

    #[test]
    fn uncommented_sample_values_parse_as_written() {
        let t = parse("personal = false # privacy-sensitive\nspell_min = 4 # shortest word\n");
        assert_eq!(
            t.settings.get("personal").map(String::as_str),
            Some("false")
        );
        assert_eq!(t.settings.get("spell_min").map(String::as_str), Some("4"));
        assert_eq!(parse_flag(t.settings["personal"].as_str()), Some(false));
        assert_eq!(parse_flag("TRUE"), Some(true));
        assert_eq!(parse_flag("yes"), None);
    }

    #[test]
    fn a_line_without_an_equals_costs_only_that_line_and_is_reported() {
        let t = parse("# fine\nthis is not a setting\nspell = 0\n");
        assert_eq!(t.settings.len(), 1);
        assert_eq!(t.settings.get("spell").map(String::as_str), Some("0"));
        // The line number is what makes the complaint actionable, and it counts
        // every line including the comments — the user is looking at the file.
        assert_eq!(t.malformed, vec![2]);
    }

    #[test]
    fn a_value_containing_an_equals_keeps_it() {
        // `split_once`, not `split`: nothing here needs it today, but a value
        // truncated at its second `=` is the kind of bug that only shows up
        // once some future setting takes a string.
        let t = parse("key = a=b\n");
        assert_eq!(t.settings.get("key").map(String::as_str), Some("a=b"));
    }

    #[test]
    fn numeric_loading_and_diagnostics_share_type_and_range_limits() {
        for value in ["4", "256", "-1", "l", "18446744073709551616"] {
            assert!(parse_number("RECAST_SPELL_DIST", value).is_err(), "{value}");
        }
        for value in ["0", "1", " 3 "] {
            assert!(parse_number("RECAST_SPELL_DIST", value).is_ok());
        }
        assert_eq!(
            parse_number("RECAST_SPELL_RANK", "4294967295"),
            Ok(u32::MAX as u64)
        );
        assert!(parse_number("RECAST_COMPLETE_RANK", "4294967296").is_err());
        assert_eq!(parse_number("RECAST_INJECT_BATCH_GAP", "0"), Ok(0));
        assert_eq!(
            parse_number("RECAST_INJECT_SETTLE", "18446744073709551615"),
            Ok(u64::MAX)
        );
        let parsed = parse("exclude_apps = \"Code.exe, com.apple.Terminal\" # exact IDs\n");
        assert_eq!(
            crate::config::parse_excluded_apps(&parsed.settings["exclude_apps"]),
            ["code.exe", "com.apple.terminal"]
        );
    }

    /// The sample is the only documentation of the file format that the user
    /// can actually edit, so a key in it that the program does not read would
    /// be worse than no sample at all.
    #[test]
    fn every_key_in_the_sample_is_a_real_setting() {
        let sample = sample();
        let known: Vec<String> = crate::config::ALL_KEYS
            .iter()
            .map(|k| file_key(k))
            .collect();
        let mut found = 0;
        for line in sample.lines() {
            let Some(line) = line.strip_prefix('#') else {
                continue;
            };
            // Only the `#key = value` lines; prose comments have no `=` before
            // any whitespace-free head, and are filtered by the key test below.
            let Some((key, _)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim();
            if key.is_empty() || key.contains(char::is_whitespace) {
                continue;
            }
            assert!(
                known.contains(&key.to_string()),
                "the sample offers `{key}`, which nothing reads"
            );
            found += 1;
        }
        assert_eq!(
            found,
            crate::config::ALL_KEYS.len(),
            "the sample shows {found} settings but there are {}",
            crate::config::ALL_KEYS.len()
        );
    }
}
