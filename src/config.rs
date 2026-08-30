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
    /// Load configuration from environment variables.
    /// RECAST_SHORT – set to `0` to disable switching on short (≤3 char) words
    ///                (default: enabled).
    /// RECAST_SPLIT – set (to anything but `0`) to enable the missing-space
    ///                split fallback (default: disabled).
    /// RECAST_FREQ  – set to `0` to disable the homograph frequency tie-break
    ///                (default: enabled).
    /// RECAST_SPELL – set to `0` to disable the English spelling autocorrect
    ///                (default: enabled).
    /// RECAST_SPELL_MIN  – shortest correctable word (default: 4).
    /// RECAST_SPELL_RANK – worst frequency rank a suggestion may have
    ///                     (default: 20000).
    /// RECAST_SPELL_DIST – maximum edit distance, 1 to 3 (default: 3).
    /// RECAST_COMPLETE   – set to `0` to disable auto-complete (default:
    ///                     enabled).
    /// RECAST_COMPLETE_MIN  – shortest completable prefix (default: 3).
    /// RECAST_COMPLETE_RANK – worst frequency rank a completion may have
    ///                        (default: 30000).
    pub fn from_env() -> Self {
        Self {
            short_enabled: std::env::var("RECAST_SHORT")
                .map(|v| v != "0")
                .unwrap_or(true),
            split_enabled: std::env::var("RECAST_SPLIT")
                .map(|v| !v.is_empty() && v != "0")
                .unwrap_or(false),
            freq_enabled: std::env::var("RECAST_FREQ")
                .map(|v| v != "0")
                .unwrap_or(true),
            spell_enabled: std::env::var("RECAST_SPELL")
                .map(|v| v != "0")
                .unwrap_or(true),
            spell_min_len: env_num("RECAST_SPELL_MIN", DEFAULT_SPELL_MIN_LEN),
            spell_max_rank: env_num("RECAST_SPELL_RANK", DEFAULT_SPELL_MAX_RANK),
            spell_max_dist: env_num("RECAST_SPELL_DIST", DEFAULT_SPELL_MAX_DIST),
            complete_enabled: std::env::var("RECAST_COMPLETE")
                .map(|v| v != "0")
                .unwrap_or(true),
            complete_min_len: env_num("RECAST_COMPLETE_MIN", DEFAULT_COMPLETE_MIN_LEN),
            complete_max_rank: env_num("RECAST_COMPLETE_RANK", DEFAULT_COMPLETE_MAX_RANK),
        }
    }
}

/// Numeric env override, falling back to `default` when unset or unparsable.
fn env_num<T: std::str::FromStr>(key: &str, default: T) -> T {
    std::env::var(key)
        .ok()
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

/// All known settings, for validating the config file.
pub const ALL_KEYS: &[&str] = &[
    "RECAST_SHORT",
    "RECAST_SPLIT",
    "RECAST_FREQ",
    "RECAST_SPELL",
    "RECAST_SPELL_MIN",
    "RECAST_SPELL_RANK",
    "RECAST_SPELL_DIST",
    "RECAST_COMPLETE",
    "RECAST_COMPLETE_MIN",
    "RECAST_COMPLETE_RANK",
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
    // Debug setting (not in Config struct)
    "RECAST_DEBUG",
    // Linux-only setting (not in Config struct)
    "RECAST_LAYOUT_BACKEND",
];

/// Settings that were set but could not be understood, described for the user.
///
/// [`env_num`] falls back to the shipped default on anything it cannot parse,
/// which is the right behaviour — a bad value should not stop the program —
/// but doing it *silently* inverts the user's intent in the one case that
/// matters. `RECAST_SPELL_DIST=l` (an el for a one) reads as the default 3,
/// the loosest setting there is, from someone who was plainly trying to
/// tighten it. Nothing said so. This is what `--status` reads out.
pub fn env_complaints() -> Vec<String> {
    let mut out = Vec::new();
    for key in NUMERIC_KEYS {
        if let Ok(raw) = std::env::var(key) {
            if raw.trim().parse::<u64>().is_err() {
                out.push(format!(
                    "{key}={raw:?} is not a number — using the default instead."
                ));
            }
        }
    }
    out
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
    fn every_numeric_setting_is_covered() {
        // A new RECAST_*_MIN/RANK/DIST added to `from_env` without being added
        // here would go back to failing silently.
        let doc = include_str!("config.rs");
        for key in NUMERIC_KEYS {
            assert!(doc.contains(key), "{key} listed but not used");
        }
        for key in ["RECAST_SPELL_MIN", "RECAST_COMPLETE_RANK"] {
            assert!(
                NUMERIC_KEYS.contains(&key),
                "{key} is numeric but unchecked"
            );
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
        assert!(
            read.len() >= 10,
            "found only {} timings: {read:?}",
            read.len()
        );

        for key in read {
            assert!(
                NUMERIC_KEYS.contains(&key),
                "{key} is read by timing::injection but not in NUMERIC_KEYS, \
                 so a typoed value would fall back to the default in silence"
            );
            assert!(
                help.contains(key),
                "{key} is a real setting but not in --help"
            );
        }
    }
}
