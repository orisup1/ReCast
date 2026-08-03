use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::config::Config;

/// Supported layout languages.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Language {
    English,
    Hebrew,
}

impl Language {
    /// Opposite layout.
    pub fn other(self) -> Self {
        match self {
            Language::English => Language::Hebrew,
            Language::Hebrew => Language::English,
        }
    }
}

/// Global runtime configuration, set once at startup.
static GLOBAL_CONFIG: OnceLock<Config> = OnceLock::new();

impl Config {
    /// Access the global config, falling back to defaults if not yet set
    /// (matches the `from_env` defaults: short-word switching on, split off,
    /// spelling autocorrect on).
    pub fn global() -> &'static Config {
        GLOBAL_CONFIG.get_or_init(|| Config {
            short_enabled: true,
            split_enabled: false,
            freq_enabled: true,
            spell_enabled: true,
            spell_min_len: crate::config::DEFAULT_SPELL_MIN_LEN,
            spell_max_rank: crate::config::DEFAULT_SPELL_MAX_RANK,
            spell_max_dist: crate::config::DEFAULT_SPELL_MAX_DIST,
            complete_enabled: true,
            complete_min_len: crate::config::DEFAULT_COMPLETE_MIN_LEN,
            complete_max_rank: crate::config::DEFAULT_COMPLETE_MAX_RANK,
        })
    }
}

/// Which pipeline produced a correction — the one thing a user reading the
/// history needs that the two words alone don't tell them, since "this was a
/// layout switch" and "this was a guess about my spelling" are answered very
/// differently.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FixKind {
    /// The word was retyped in the other keyboard layout.
    Layout,
    /// The English speller rewrote it, or an abbreviation expanded.
    Spelling,
    /// The completion key finished a partial word.
    Complete,
}

impl FixKind {
    /// One-word tag for the history line.
    pub fn tag(self) -> &'static str {
        match self {
            FixKind::Layout => "layout",
            FixKind::Spelling => "spell",
            FixKind::Complete => "complete",
        }
    }
}

/// One correction as it happened, for the recent-corrections history.
#[derive(Clone, Debug)]
pub struct Correction {
    /// What was on screen before.
    pub from: String,
    /// What replaced it.
    pub to: String,
    pub kind: FixKind,
    /// Wall-clock time, for the log line. Read by the TUI, which macOS does
    /// not build (the event tap owns the main run loop there).
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    pub at: chrono::DateTime<chrono::Local>,
    /// Set when the undo gesture took this one back.
    pub undone: bool,
}

/// How many corrections are kept. Long enough to answer "what did it just
/// change?" after a few more words, short enough to stay a glance rather than a
/// log to read.
const HISTORY_LEN: usize = 20;

/// Shared runtime state between the keyboard listener and the optional GUI.
pub struct AppControl {
    enabled: AtomicBool,
    fixed_count: AtomicU64,
    undo_count: AtomicU64,
    /// Set while correction is paused for a stretch (the tray's "Pause for 30
    /// minutes"), cleared by any explicit enable/disable. A pause is not the
    /// same as being switched off: it expires by itself and it does not change
    /// what the user gets back when it does.
    paused_until: Mutex<Option<Instant>>,
    history: Mutex<VecDeque<Correction>>,
}

impl AppControl {
    /// Create a new control and register the provided config globally.
    pub fn new_with_config(cfg: Config) -> Self {
        let _ = GLOBAL_CONFIG.set(cfg);
        Self {
            enabled: AtomicBool::new(true),
            fixed_count: AtomicU64::new(0),
            undo_count: AtomicU64::new(0),
            paused_until: Mutex::new(None),
            history: Mutex::new(VecDeque::with_capacity(HISTORY_LEN)),
        }
    }

    /// As [`new_with_config`](Self::new_with_config), but starting from the
    /// enabled state the user left behind last time (see `crate::prefs`).
    pub fn new_with_config_and_state(cfg: Config, enabled: bool) -> Self {
        let control = Self::new_with_config(cfg);
        control.enabled.store(enabled, Ordering::Relaxed);
        control
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed) && self.pause_remaining().is_none()
    }

    /// The enabled switch on its own, ignoring any running pause — what the
    /// menu's Enable/Disable item reflects.
    pub fn is_switched_on(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Flip the switch, and remember it: an app that forgets it was turned off
    /// every time the machine reboots is one the user has to turn off twice.
    pub fn set_enabled(&self, value: bool) {
        self.enabled.store(value, Ordering::Relaxed);
        self.resume();
        crate::prefs::save_enabled(value);
    }

    /// Stop correcting for `how_long`, then carry on by itself.
    pub fn pause_for(&self, how_long: Duration) {
        if let Ok(mut until) = self.paused_until.lock() {
            *until = Some(Instant::now() + how_long);
        }
    }

    /// End a running pause early.
    pub fn resume(&self) {
        if let Ok(mut until) = self.paused_until.lock() {
            *until = None;
        }
    }

    /// How much of a pause is left, or `None` if none is running. Clears the
    /// pause once it has run out, so the state settles without a timer.
    pub fn pause_remaining(&self) -> Option<Duration> {
        let mut until = self.paused_until.lock().ok()?;
        let deadline = (*until)?;
        match deadline.checked_duration_since(Instant::now()) {
            Some(left) if !left.is_zero() => Some(left),
            _ => {
                *until = None;
                None
            }
        }
    }

    /// Corrections that stuck — undone ones are taken back out (see
    /// [`record_undo`](Self::record_undo)).
    pub fn fixed_count(&self) -> u64 {
        self.fixed_count.load(Ordering::Relaxed)
    }

    /// How many corrections the user has taken back. Kept apart from the fixed
    /// tally because it is the one number that says whether the thresholds are
    /// set where this user wants them: a fix nobody undoes is invisible, and a
    /// fix undone often is worse than no fix at all.
    pub fn undo_count(&self) -> u64 {
        self.undo_count.load(Ordering::Relaxed)
    }

    pub fn record_fix(&self, from: &str, to: &str, kind: FixKind) {
        self.fixed_count.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut log) = self.history.lock() {
            if log.len() == HISTORY_LEN {
                log.pop_back();
            }
            log.push_front(Correction {
                from: from.to_string(),
                to: to.to_string(),
                kind,
                at: chrono::Local::now(),
                undone: false,
            });
        }
        crate::notify::first_correction_hint();
    }

    /// A fix was taken back by the undo gesture, so it stops counting as one —
    /// and is marked in the history rather than dropped from it, since "it
    /// changed this and I put it back" is exactly what the user is looking for
    /// when they go looking. Saturates at zero rather than wrapping: the
    /// counter is a tally the user reads, not an audit trail.
    pub fn record_undo(&self) {
        let _ = self
            .fixed_count
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                Some(n.saturating_sub(1))
            });
        self.undo_count.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut log) = self.history.lock() {
            if let Some(last) = log.iter_mut().find(|c| !c.undone) {
                last.undone = true;
            }
        }
    }

    /// The recent corrections, newest first.
    pub fn history(&self) -> Vec<Correction> {
        self.history
            .lock()
            .map(|log| log.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// A nudge to tighten the speller, once enough of its work has been thrown
    /// away to say so. The ratio is the honest quality signal — a user undoing
    /// a third of what they get is being served badly by the default budget —
    /// and the floor keeps a single early undo from producing advice.
    pub fn tighten_hint(&self) -> Option<&'static str> {
        let undone = self.undo_count();
        let kept = self.fixed_count();
        (undone >= 5 && undone * 3 >= kept + undone)
            .then_some("Many corrections taken back — try RECAST_SPELL_DIST=1")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn control() -> AppControl {
        AppControl::new_with_config(Config {
            short_enabled: true,
            split_enabled: false,
            freq_enabled: true,
            spell_enabled: true,
            spell_min_len: crate::config::DEFAULT_SPELL_MIN_LEN,
            spell_max_rank: crate::config::DEFAULT_SPELL_MAX_RANK,
            spell_max_dist: crate::config::DEFAULT_SPELL_MAX_DIST,
            complete_enabled: true,
            complete_min_len: crate::config::DEFAULT_COMPLETE_MIN_LEN,
            complete_max_rank: crate::config::DEFAULT_COMPLETE_MAX_RANK,
        })
    }

    #[test]
    fn the_history_keeps_both_words_and_the_pipeline() {
        let c = control();
        c.record_fix("recieve", "receive", FixKind::Spelling);
        let log = c.history();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].from, "recieve");
        assert_eq!(log[0].to, "receive");
        assert_eq!(log[0].kind, FixKind::Spelling);
        assert!(!log[0].undone);
    }

    #[test]
    fn newest_first() {
        let c = control();
        c.record_fix("a", "one", FixKind::Layout);
        c.record_fix("b", "two", FixKind::Layout);
        let log = c.history();
        assert_eq!(log[0].from, "b");
        assert_eq!(log[1].from, "a");
    }

    #[test]
    fn undo_takes_the_newest_surviving_correction_back() {
        let c = control();
        c.record_fix("a", "one", FixKind::Layout);
        c.record_fix("b", "two", FixKind::Layout);
        c.record_undo();
        assert!(c.history()[0].undone, "the newest one is the one on screen");
        assert!(!c.history()[1].undone);
        // A second undo can only be about the one before it — the newest is
        // already back to what the user typed.
        c.record_undo();
        assert!(c.history()[1].undone);
        assert_eq!(c.fixed_count(), 0);
        assert_eq!(c.undo_count(), 2);
    }

    #[test]
    fn the_fixed_tally_never_goes_below_zero() {
        let c = control();
        // A completion cycled back to the user's own text records an undo
        // without a fix ever having been counted.
        c.record_undo();
        assert_eq!(c.fixed_count(), 0);
        assert_eq!(c.undo_count(), 1);
    }

    #[test]
    fn the_history_is_bounded() {
        let c = control();
        for i in 0..HISTORY_LEN + 5 {
            c.record_fix(&format!("w{i}"), "fixed", FixKind::Spelling);
        }
        let log = c.history();
        assert_eq!(log.len(), HISTORY_LEN);
        assert_eq!(log[0].from, format!("w{}", HISTORY_LEN + 4));
    }

    #[test]
    fn the_tighten_hint_needs_a_floor_and_a_ratio() {
        let c = control();
        for _ in 0..4 {
            c.record_fix("x", "y", FixKind::Spelling);
            c.record_undo();
        }
        assert!(c.tighten_hint().is_none(), "four undos is not a pattern yet");

        c.record_fix("x", "y", FixKind::Spelling);
        c.record_undo();
        assert!(c.tighten_hint().is_some(), "five undos, none kept");

        // Plenty of corrections the user was happy with drowns it out again.
        for _ in 0..20 {
            c.record_fix("x", "y", FixKind::Spelling);
        }
        assert!(c.tighten_hint().is_none());
    }

    #[test]
    fn a_pause_expires_and_is_not_the_same_as_being_switched_off() {
        let c = control();
        c.pause_for(Duration::from_millis(40));
        assert!(!c.is_enabled(), "paused");
        assert!(c.is_switched_on(), "but not switched off");
        std::thread::sleep(Duration::from_millis(60));
        assert!(c.is_enabled(), "the pause runs out by itself");
        assert!(c.pause_remaining().is_none());
    }

    #[test]
    fn resuming_ends_a_pause_early() {
        let c = control();
        c.pause_for(Duration::from_secs(1800));
        assert!(c.pause_remaining().is_some());
        c.resume();
        assert!(c.is_enabled());
    }
}
