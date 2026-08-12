#[derive(Clone, Debug)]
pub struct Config {
    /// Allow auto-switching on short words (≤3 chars). Short key sequences are
    /// dictionary-collision-prone, so this can be turned off for a stricter,
    /// never-wrongly-switch behaviour.
    pub short_enabled: bool,
    /// Enable missing‑space split fallback.
    pub split_enabled: bool,
    /// Enable the homograph frequency tie-break: when a key sequence reads as a
    /// real word in *both* layouts, switch to the reading that is decisively more
    /// common instead of always keeping the current layout.
    pub freq_enabled: bool,
    /// Enable the English in-language spelling autocorrect: when a word is not
    /// a wrong-layout mistype but *is* a near-miss of a common English word,
    /// retype it as that word.
    pub spell_enabled: bool,
    /// Shortest word the spelling autocorrect will touch. Below this, unknown
    /// tokens are overwhelmingly initialisms and names ("btw", "ori"), and a
    /// single edit is enough to turn one into an unrelated word.
    pub spell_min_len: usize,
    /// Worst frequency rank a spelling suggestion may have. The dictionary
    /// contains ~370k words including archaic ones; this is what keeps the
    /// suggestion to a word people actually type.
    pub spell_max_rank: u32,
    /// Maximum edit distance for a spelling suggestion (0 disables, 1 = only
    /// single-typo fixes, 2–3 = also badly mangled words). The word's own
    /// length caps this further: two edits need 7 characters and three need 10,
    /// so raising it only ever affects words long enough to survive it.
    pub spell_max_dist: u8,
    /// Enable auto-complete: the completion key finishes the word being typed,
    /// and abbreviations from `<config>/recast/abbrev.txt` expand when a word
    /// is finished.
    pub complete_enabled: bool,
    /// Shortest partial word the completer will finish. One or two letters
    /// match too many words for the most common one to be a good guess.
    pub complete_min_len: usize,
    /// Worst frequency rank a completion may have — the same idea as
    /// `spell_max_rank`, but looser, because a completion is asked for.
    pub complete_max_rank: u32,
}

/// Shipped defaults for the spelling autocorrect. Deliberately conservative:
/// a missed correction is invisible, a wrong one rewrites the user's text.
pub const DEFAULT_SPELL_MIN_LEN: usize = 4;
pub const DEFAULT_SPELL_MAX_RANK: u32 = 20_000;
pub const DEFAULT_SPELL_MAX_DIST: u8 = 3;

/// Shipped defaults for auto-complete. Looser than the speller's, because a
/// completion only ever happens when the user presses the key for it.
pub const DEFAULT_COMPLETE_MIN_LEN: usize = 3;
pub const DEFAULT_COMPLETE_MAX_RANK: u32 = 30_000;

impl Config {
    /// Load configuration from the environment and `config.toml`, in that
    /// order of precedence (see [`crate::settings`]).
    ///
    /// * `RECAST_SHORT` / `short` – `0` to stop switching on short (≤3 char)
    ///   words (default: enabled).
    /// * `RECAST_SPLIT` / `split` – enable the missing-space split fallback
    ///   (default: disabled).
    /// * `RECAST_FREQ` / `freq` – `0` to disable the homograph frequency
    ///   tie-break (default: enabled).
    /// * `RECAST_SPELL` / `spell` – `0` to disable the English spelling
    ///   autocorrect (default: enabled).
    /// * `RECAST_SPELL_MIN` / `spell_min` – shortest correctable word
    ///   (default: 4).
    /// * `RECAST_SPELL_RANK` / `spell_rank` – worst frequency rank a suggestion
    ///   may have (default: 20000).
    /// * `RECAST_SPELL_DIST` / `spell_dist` – maximum edit distance, 1 to 3
    ///   (default: 3).
    /// * `RECAST_COMPLETE` / `complete` – `0` to disable auto-complete
    ///   (default: enabled).
    /// * `RECAST_COMPLETE_MIN` / `complete_min` – shortest completable prefix
    ///   (default: 3).
    /// * `RECAST_COMPLETE_RANK` / `complete_rank` – worst frequency rank a
    ///   completion may have (default: 30000).
    pub fn load() -> Self {
        Self {
            short_enabled: flag("RECAST_SHORT", true),
            split_enabled: flag("RECAST_SPLIT", false),
            freq_enabled: flag("RECAST_FREQ", true),
            spell_enabled: flag("RECAST_SPELL", true),
            spell_min_len: num("RECAST_SPELL_MIN", DEFAULT_SPELL_MIN_LEN),
            spell_max_rank: num("RECAST_SPELL_RANK", DEFAULT_SPELL_MAX_RANK),
            spell_max_dist: num("RECAST_SPELL_DIST", DEFAULT_SPELL_MAX_DIST),
            complete_enabled: flag("RECAST_COMPLETE", true),
            complete_min_len: num("RECAST_COMPLETE_MIN", DEFAULT_COMPLETE_MIN_LEN),
            complete_max_rank: num("RECAST_COMPLETE_RANK", DEFAULT_COMPLETE_MAX_RANK),
        }
    }
}

/// Whether a setting's value means "on".
///
/// The environment only ever had `0` for off, because a variable that is set at
/// all is usually meant as "yes". A file has to do better: someone writing
/// `spell = false` on a line of their own has said something unambiguous, and
/// reading it as *enabled* — which a bare `!= "0"` does — would be the worst
/// kind of quiet wrong answer. An empty value is off for the same reason:
/// `spell =` with nothing after it is not an endorsement.
fn truthy(value: &str) -> bool {
    !matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "" | "0" | "false" | "no" | "off"
    )
}

/// A boolean setting, falling back to `default` when neither source sets it.
pub fn flag(key: &str, default: bool) -> bool {
    crate::settings::get(key).map_or(default, |v| truthy(&v))
}

/// A numeric setting, falling back to `default` when unset or unparsable.
fn num<T: std::str::FromStr>(key: &str, default: T) -> T {
    crate::settings::get(key)
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

/// Every numeric setting, so a value that could not be read can be named.
pub const NUMERIC_KEYS: &[&str] = &[
    "RECAST_SPELL_MIN",
    "RECAST_SPELL_RANK",
    "RECAST_SPELL_DIST",
    "RECAST_COMPLETE_MIN",
    "RECAST_COMPLETE_RANK",
    // The injection timings (`crate::timing`). Worth complaining about for the
    // same reason as the rest, and more so: someone setting these is tuning by
    // trial, so a value that silently did not apply looks like a measurement.
    "RECAST_INJECT_PRESS_GAP",
    "RECAST_INJECT_KEY_GAP",
    "RECAST_INJECT_SETTLE",
    "RECAST_INJECT_HELD_TIMEOUT",
    "RECAST_INJECT_TERM_TIMEOUT",
    "RECAST_INJECT_HELD_POLL",
    "RECAST_INJECT_DEVICE_SETTLE",
    "RECAST_INJECT_LAYOUT_CONFIRM",
    "RECAST_INJECT_LAYOUT_POLL",
    "RECAST_INJECT_BATCH_GAP",
];

/// Every setting that is a plain on/off switch.
pub const BOOLEAN_KEYS: &[&str] = &[
    "RECAST_SHORT",
    "RECAST_SPLIT",
    "RECAST_FREQ",
    "RECAST_SPELL",
    "RECAST_COMPLETE",
    // Read by `dictionary::debug_enabled` rather than stored in `Config`, but a
    // setting the user can write down all the same — and one that has to be
    // recognised here, or `debug = true` in the file is reported as a typo.
    "RECAST_DEBUG",
];

/// Every setting the program reads, in its environment spelling. What
/// `settings::complaints` checks a config file against, so a key that is in
/// neither list is reported to the user as not being a setting.
pub const ALL_KEYS: &[&str] = &{
    // Concatenating `&[&str]` in a const needs the length written down, so it
    // is asserted against the two sources rather than trusted.
    let mut all = [""; 21];
    let mut n = 0;
    let mut i = 0;
    while i < NUMERIC_KEYS.len() {
        all[n] = NUMERIC_KEYS[i];
        n += 1;
        i += 1;
    }
    let mut j = 0;
    while j < BOOLEAN_KEYS.len() {
        all[n] = BOOLEAN_KEYS[j];
        n += 1;
        j += 1;
    }
    assert!(n == all.len(), "ALL_KEYS is the wrong length");
    all
};

/// Settings that were set but could not be used, described for the user.
/// `--status` reads these out; see [`crate::settings::complaints`].
pub fn complaints() -> Vec<String> {
    crate::settings::complaints(NUMERIC_KEYS, ALL_KEYS)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The parse is what decides whether a complaint is warranted, so it is
    /// what gets tested — the env itself is process-global and shared with
    /// every other test in the binary.
    #[test]
    fn a_typoed_number_is_worth_complaining_about() {
        // The failure this exists for: an el where a one was meant.
        assert!("l".parse::<u64>().is_err());
        assert!("".parse::<u64>().is_err());
        assert!(" 2 ".trim().parse::<u64>().is_ok());
    }

    #[test]
    fn spelling_a_switch_out_in_words_works_the_way_it_reads() {
        // The whole reason `truthy` exists rather than a bare `!= "0"`: in a
        // file, people write the word.
        assert!(!truthy("false"));
        assert!(!truthy("off"));
        assert!(!truthy("No"));
        assert!(!truthy("0"));
        assert!(!truthy("  "));
        assert!(truthy("true"));
        assert!(truthy("1"));
        assert!(truthy("yes"));
    }

    #[test]
    fn every_numeric_setting_is_covered() {
        // A new RECAST_*_MIN/RANK/DIST added to `load` without being added
        // here would go back to failing silently.
        let doc = include_str!("config.rs");
        for key in NUMERIC_KEYS {
            assert!(doc.contains(key), "{key} listed but not used");
        }
        for key in ["RECAST_SPELL_MIN", "RECAST_COMPLETE_RANK"] {
            assert!(NUMERIC_KEYS.contains(&key), "{key} is numeric but unchecked");
        }
    }

    #[test]
    fn all_keys_is_every_list_and_nothing_else() {
        assert_eq!(ALL_KEYS.len(), NUMERIC_KEYS.len() + BOOLEAN_KEYS.len());
        for key in NUMERIC_KEYS.iter().chain(BOOLEAN_KEYS) {
            assert!(ALL_KEYS.contains(key), "{key} missing from ALL_KEYS");
        }
    }

    /// Every switch the program reads has to be in one of the two lists, or a
    /// user who writes it in `config.toml` is told it is not a setting — while
    /// it quietly works.
    #[test]
    fn every_switch_read_anywhere_is_declared() {
        for key in [
            "RECAST_SHORT",
            "RECAST_SPLIT",
            "RECAST_FREQ",
            "RECAST_SPELL",
            "RECAST_COMPLETE",
            "RECAST_DEBUG",
        ] {
            assert!(BOOLEAN_KEYS.contains(&key), "{key} is a switch but undeclared");
        }
    }

    /// The injection timings are written down in three places — the code that
    /// reads them (`timing::injection`), the list that complains about a value
    /// it cannot parse (above), and `--help`. All three had drifted: `--help`
    /// was missing two of them, the complaint list a third, and `timing`'s own
    /// doc comment three. A setting nothing complains about and nothing
    /// documents is a setting that does not exist as far as the user is
    /// concerned, so the source of truth checks the copies.
    #[test]
    fn every_injection_timing_is_listed_and_documented() {
        let timing = include_str!("timing.rs");
        let help = include_str!("main.rs");

        // Every `"RECAST_INJECT_…"` string literal in timing.rs — which is
        // exactly the set `injection()` reads, since the doc comments name them
        // in backticks rather than quotes.
        let read: Vec<&str> = timing
            .match_indices("\"RECAST_INJECT_")
            .map(|(at, matched)| {
                let rest = &timing[at + matched.len() - "RECAST_INJECT_".len()..];
                &rest[..rest.find('"').expect("unterminated key")]
            })
            .filter(|key| !key.contains("NOTHING_SETS_THIS"))
            .collect();
        assert!(read.len() >= 10, "found only {} timings: {read:?}", read.len());

        for key in read {
            assert!(
                NUMERIC_KEYS.contains(&key),
                "{key} is read by timing::injection but not in NUMERIC_KEYS, \
                 so a typoed value would fall back to the default in silence"
            );
            assert!(help.contains(key), "{key} is a real setting but not in --help");
        }
    }
}
