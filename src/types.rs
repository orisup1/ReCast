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

/// Take a lock, accepting one poisoned by a panic elsewhere.
///
/// Poisoning is a warning that some other thread died partway through a write,
/// and for most programs `unwrap` is the right response: fail loudly rather
/// than read half-updated data. This one is a keyboard listener, and the
/// calculation is different in both directions.
///
/// The state behind these locks is a few keystrokes of a word in progress.
/// Nothing about it is worth a crash, and the recovery — a wrong correction on
/// the word being typed, at worst — is one the undo gesture already exists for.
/// Against that: every keystroke handler, on every device thread, takes this
/// lock. `unwrap` there means one panic anywhere does not stop one thread, it
/// stops every thread that touches the keyboard afterwards, one keystroke at a
/// time, while the process stays up and looks healthy. That is the failure this
/// avoids — see [`ReplaceGuard`], which handles the other half of it.
pub fn lock_forgiving<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The part of a platform's listener state that a replacement has to put back.
///
/// Each platform owns its own `AppState` (the key types differ: `evdev::KeyCode`,
/// `rdev::Key`), so this is the sliver they have in common — enough for
/// [`ReplaceGuard`] to be written once instead of three times.
pub trait Replaceable {
    /// The gate that makes the listener ignore keystrokes while a correction is
    /// being injected, so it does not read its own output as user input.
    fn set_replacing(&mut self, replacing: bool);
    /// Keys the user managed to type during the replacement. Dropped rather
    /// than replayed when a replacement fails: they belong after text that was
    /// never typed.
    fn clear_buffered(&mut self);
}

/// Clears the replacement gates however the replacement ends — including by
/// panic.
///
/// Injection runs on its own thread and sets two flags before it starts:
/// `is_replacing` in the listener state and, on macOS and Windows, an
/// `injecting` atomic. Both used to be cleared only by the last lines of
/// `replace_word`, which is fine right up until something in between panics.
/// Then they stay set: `is_replacing` makes every future correction return
/// early, and `injecting` makes the listener discard every key the user types.
/// The daemon keeps running, the tray still says "Enabled", and the keyboard
/// quietly stops working.
///
/// A guard makes the clearing structural rather than something the happy path
/// remembers to do. The normal path still writes back the corrected buffer
/// itself — only the gates are the guard's business, because they are the two
/// pieces of state whose failure mode is silence.
pub struct ReplaceGuard<'a, S: Replaceable> {
    state: &'a Mutex<S>,
    injecting: Option<&'a AtomicBool>,
}

impl<'a, S: Replaceable> ReplaceGuard<'a, S> {
    /// Arm the guard for a replacement that is about to start. `injecting` is
    /// `None` on Linux, which gates on `is_replacing` alone.
    pub fn new(state: &'a Mutex<S>, injecting: Option<&'a AtomicBool>) -> Self {
        Self { state, injecting }
    }
}

impl<S: Replaceable> Drop for ReplaceGuard<'_, S> {
    fn drop(&mut self) {
        // `lock_forgiving`, not `lock().unwrap()`: unwinding through a second
        // panic would abort the process, and this runs precisely when the first
        // one may already have poisoned this lock.
        {
            let mut st = lock_forgiving(self.state);
            st.set_replacing(false);
            st.clear_buffered();
        }
        if let Some(flag) = self.injecting {
            flag.store(false, Ordering::Relaxed);
        }
    }
}

/// Longest run of keys that can still be treated as one word.
///
/// Both pipelines already refuse anything much shorter than this — the speller
/// stops at 20 characters (`spell::MAX_LEN`) and the completer at 20
/// (`complete::MAX_PREFIX_LEN`) — so past this point the buffer is being filled
/// with something that is provably never going to be corrected: a base64 blob,
/// a URL, a path, a key held down. Held at 64 rather than 20 because the buffer
/// also has to hold the punctuation a word ends with and, once the split
/// fallback is on, two words run together.
pub const MAX_WORD_KEYS: usize = 64;

/// The keys of the word being typed, with a hard ceiling.
///
/// This used to be a bare `Vec` that only ever shrank when the user finished a
/// word, pressed a cursor key or clicked — so a long unbroken token grew it
/// without limit and then paid to fold the whole thing into two strings that
/// no dictionary was ever going to match.
///
/// The ceiling cannot simply drop the excess keys: the buffer has to keep
/// agreeing character-for-character with what is on screen, because that is
/// what the erase count is computed from, and a buffer holding the first 64
/// characters of a 200-character token would erase 64 of the wrong ones. Nor
/// can it keep the *last* 64, which stays consistent but lets a fragment of a
/// long token be "corrected" as if it were a word.
///
/// So an over-long run gives up on the word entirely: the buffer empties and
/// stays empty until something ends the word. Every existing reset already
/// calls [`clear`](Self::clear), and an emptied buffer already means "nothing
/// to check" at a terminator, so the give-up state needs no handling anywhere
/// else — it reads as a word that was never typed, which is exactly what a
/// 200-character token is.
pub struct WordBuffer<T> {
    keys: Vec<T>,
    /// Set when the run got too long. Cleared by [`clear`](Self::clear), i.e.
    /// at the next word boundary.
    given_up: bool,
}

impl<T> WordBuffer<T> {
    pub fn new() -> Self {
        Self {
            keys: Vec::new(),
            given_up: false,
        }
    }

    /// Add a key to the word, unless the word has already grown past being one.
    pub fn push(&mut self, key: T) {
        if self.given_up {
            return;
        }
        if self.keys.len() >= MAX_WORD_KEYS {
            self.give_up();
            return;
        }
        self.keys.push(key);
    }

    /// Backspace. A word already given up on has nothing to take back.
    pub fn pop(&mut self) {
        self.keys.pop();
    }

    /// End the word: drop the keys and start listening again.
    ///
    /// Also releases the buffer's capacity, so one long token does not leave a
    /// 64-key allocation behind for the rest of the process's life.
    pub fn clear(&mut self) {
        self.keys.clear();
        self.keys.shrink_to_fit();
        self.given_up = false;
    }

    /// Give up on the current word without ending it — the keys still being
    /// typed belong to a token that is not a word, and must not be checked as
    /// the tail of one.
    fn give_up(&mut self) {
        self.keys.clear();
        self.keys.shrink_to_fit();
        self.given_up = true;
    }

    /// Start the word again from `keys` — what is on screen after a correction
    /// has landed. Never more than the ceiling.
    pub fn replace_with(&mut self, keys: impl IntoIterator<Item = T>) {
        self.keys.clear();
        self.given_up = false;
        self.extend(keys);
    }

    /// Append, respecting the ceiling — the keys typed while a correction was
    /// being injected, replayed after it.
    pub fn extend(&mut self, keys: impl IntoIterator<Item = T>) {
        for key in keys {
            self.push(key);
        }
    }
}

impl<T> Default for WordBuffer<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// So `&buffer` still reads as `&[T]` everywhere it is passed to the pipelines.
impl<T> std::ops::Deref for WordBuffer<T> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        &self.keys
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

    /// A control wired to the shipped defaults, for tests that need one
    /// without caring how it is configured.
    ///
    /// `GLOBAL_CONFIG` is a `OnceLock` shared by every test in the binary, so
    /// the first caller wins and the rest are no-ops — which is why this uses
    /// the defaults rather than anything a single test would want to vary.
    #[cfg(test)]
    pub fn new_for_test() -> Self {
        Self::new_with_config(Config {
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
        AppControl::new_for_test()
    }

    /// Stands in for the three platforms' `AppState`, which cannot be built in
    /// a test — they hold `evdev` and `rdev` key types and are only compiled on
    /// their own OS. The two fields below are the only ones the guard touches.
    #[derive(Default)]
    struct FakeState {
        replacing: bool,
        buffered: usize,
    }

    impl Replaceable for FakeState {
        fn set_replacing(&mut self, replacing: bool) {
            self.replacing = replacing;
        }
        fn clear_buffered(&mut self) {
            self.buffered = 0;
        }
    }

    #[test]
    fn a_panicking_replacement_still_opens_the_gates() {
        let state = Mutex::new(FakeState {
            replacing: true,
            buffered: 3,
        });
        let injecting = AtomicBool::new(true);

        // Exactly what the injection thread does, up to and including dying
        // partway through.
        let died = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _gate = ReplaceGuard::new(&state, Some(&injecting));
            panic!("injection blew up halfway through");
        }));
        assert!(died.is_err(), "the panic is the point of this test");

        // Before the guard, both of these stayed set: every later correction
        // returned early, and the listener discarded every key as its own.
        let st = lock_forgiving(&state);
        assert!(!st.replacing, "is_replacing left set — corrections wedged");
        assert_eq!(st.buffered, 0, "keys typed during a failed replacement kept");
        assert!(
            !injecting.load(Ordering::Relaxed),
            "injecting left set — the keyboard would stop working entirely"
        );
    }

    #[test]
    fn the_gates_open_on_the_ordinary_path_too() {
        let state = Mutex::new(FakeState {
            replacing: true,
            buffered: 2,
        });
        let injecting = AtomicBool::new(true);
        {
            let _gate = ReplaceGuard::new(&state, Some(&injecting));
        }
        assert!(!lock_forgiving(&state).replacing);
        assert!(!injecting.load(Ordering::Relaxed));
    }

    #[test]
    fn a_poisoned_lock_is_still_usable() {
        let state = Mutex::new(FakeState::default());
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut st = state.lock().unwrap();
            st.buffered = 9;
            panic!("die while holding the lock");
        }));
        assert!(state.is_poisoned(), "the panic should have poisoned it");
        // `lock().unwrap()` here is what used to take every keyboard thread
        // down, one keystroke at a time, after any single panic.
        assert_eq!(lock_forgiving(&state).buffered, 9);
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
