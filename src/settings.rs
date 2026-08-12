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
//! setting here is a bool or a number. Keys are the environment names without
//! their `RECAST_` prefix, lowercased, so `RECAST_SPELL_DIST` is `spell_dist`
//! and the two spellings of a setting can never drift apart — [`file_key`] is
//! the only place the mapping exists.
//!
//! Read once, at startup, into a `OnceLock`: the callers ([`crate::config`] and
//! [`crate::timing`]) both cache what they build from it, and one of them is
//! consulted inside the loop that paces individual keystrokes. Editing the file
//! takes a restart — unlike `abbrev.txt`/`ignore.txt`, which are watched.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;

/// Where the file lives. `None` when the OS has no config directory at all,
/// which is also how "there is no file" is spelled — neither is an error.
pub fn file_path() -> Option<PathBuf> {
    crate::complete::user_path("config.toml")
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
/// A missing, unreadable or malformed file means "no settings from the file" —
/// never a startup failure. The same bargain the user lists make: these are
/// conveniences, and a daemon that refuses to start because of a stray line in
/// an optional file is worse than one that ignores the line and says so in
/// `--status` (see [`complaints`]).
fn parsed() -> &'static Parsed {
    static PARSED: OnceLock<Parsed> = OnceLock::new();
    PARSED.get_or_init(|| {
        let text = file_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .unwrap_or_default();
        parse(&text)
    })
}

fn table() -> &'static HashMap<String, String> {
    &parsed().settings
}

/// What one config file amounts to: the settings in it, and the lines that
/// were not settings.
#[derive(Default)]
struct Parsed {
    settings: HashMap<String, String>,
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
        let value = value.trim();
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
pub fn complaints(numeric_keys: &[&str], all_keys: &[&str]) -> Vec<String> {
    let mut out = Vec::new();

    for key in numeric_keys {
        if let Some((raw, source)) = lookup(key) {
            if raw.trim().parse::<u64>().is_err() {
                out.push(format!(
                    "{}={raw:?} is not a number — using the default instead.",
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
#short = true          # auto-switch on short (<= 3 char) words
#split = false         # missing-space split fallback (opt-in; can mis-split)
#freq = true           # homograph frequency tie-break
#spell = true          # English spelling autocorrect
#spell_min = {spell_min}           # shortest word the speller may fix
#spell_rank = {spell_rank}      # worst frequency rank a suggestion may have
#spell_dist = {spell_dist}          # maximum edit distance, 1 to 3
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
        return Err(format!("{} already exists — leaving it alone.", path.display()));
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(&path, sample()).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// The sample is the only documentation of the file format that the user
    /// can actually edit, so a key in it that the program does not read would
    /// be worse than no sample at all.
    #[test]
    fn every_key_in_the_sample_is_a_real_setting() {
        let sample = sample();
        let known: Vec<String> = crate::config::ALL_KEYS.iter().map(|k| file_key(k)).collect();
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
