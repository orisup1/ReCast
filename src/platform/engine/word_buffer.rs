//! Tracked words, undo state, and text-context invalidation.

use super::{Platform, Selection};
use crate::dictionary::History;
use crate::types::{Language, Replaceable, WordBuffer};
#[cfg(target_os = "linux")]
use std::collections::HashMap;
use std::collections::HashSet;
use std::time::{Duration, Instant, SystemTime};

/// Keep correction undo bounded while the cursor advances through spaces.
pub(super) const MAX_UNDO_SPACES: usize = 32;

/// One key of the word being typed, with the shift state it was typed under.
/// The buffer holds key *positions*, which carry no case of their own, so the
/// shift has to be recorded here or the capitalization is lost by the time a
/// correction is typed back.
#[derive(Clone, Copy, Debug)]
pub struct Typed<K> {
    pub key: K,
    pub shift: bool,
}

/// Tracked text available to the conversion and optional undo gestures.
/// Corrections carry both visible keys and undo data; skipped words can be
/// unlisted by single-tap undo. Cursor movement invalidates every variant.
pub enum LastAction<P: Platform> {
    /// A correction landed and the cursor is still on it.
    Fixed(LastFix<P>),
    Selection {
        before: String,
        after: Selection,
    },
    /// A word was passed over only because it is on one of the user's lists.
    Skipped(LastSkip<P>),
    /// An unchanged word can be explicitly reconsidered, including ambiguous words.
    Unchanged {
        keys: Vec<Typed<P::Key>>,
        terminator: Option<P::Key>,
        trailing_spaces: Vec<Typed<P::Key>>,
        layout: Option<Language>,
    },
}

/// A correction that is on screen right now, with the cursor still sitting
/// immediately after it, with both visible keys for conversion and the original
/// text for the optional undo gesture.
///
/// It survives bounded trailing spaces. Other text or cursor movement drops it
/// because undo erases backwards from the cursor (see [`super::Engine::key_press`]).
pub struct LastFix<P: Platform> {
    /// Physical keys of the visible final word, including any unchanged prefix.
    pub(super) visible: Vec<Typed<P::Key>>,
    pub(super) visible_layout: Language,
    /// Only explicit layout conversions may be converted again by a double tap.
    pub(super) manual_conversion: bool,
    /// Characters our injection put on screen, terminator included — what has
    /// to come back off.
    pub(super) on_screen: usize,
    /// Original keys let undo reproduce the corrected suffix and trailing spaces.
    pub(super) restore: Vec<Typed<P::Key>>,
    pub(super) restore_layout: Language,
    pub(super) trailing_spaces: Vec<Typed<P::Key>>,
    /// Terminator to press again afterwards; `None` for a completion, which
    /// interrupted a word rather than finishing one.
    pub(super) terminator: Option<P::Key>,
    /// Layout to switch back to, when the correction was the one that changed
    /// it. Restoring the letters without restoring the layout would leave the
    /// user typing the wrong language into the word they just rescued.
    pub(super) layout: Option<Language>,
    /// Word buffer to leave behind: a completion's original prefix, empty for a
    /// word the terminator already finished.
    pub(super) keep: Vec<Typed<P::Key>>,
    /// The reading to stop correcting for the rest of the session. Undo that
    /// only rewrote the screen would be undone again by the next repetition of
    /// the same word (see `complete::suppress`).
    pub(super) suppress: Option<String>,
    /// Automatic planner rule, absent for manual conversion and completion.
    pub(super) rule: Option<&'static str>,
    pub(super) learning_pair: Option<(String, String)>,
}

/// A word the pipelines passed over because the user had already told us to
/// leave it alone, ready for single-tap unlisting or double-tap conversion.
///
/// The keys are kept rather than the decision, because there is no decision
/// yet: the word was never put through the pipelines with the list out of the
/// way. The gesture takes it off the list and runs them then.
pub struct LastSkip<P: Platform> {
    /// The word as typed, to run the pipelines over once it is off the list.
    pub(super) keys: Vec<Typed<P::Key>>,
    /// The terminator already on screen after it, erased with the word and
    /// pressed again afterwards exactly as on the normal path.
    pub(super) terminator: Option<P::Key>,
    pub(super) trailing_spaces: Vec<Typed<P::Key>>,
    pub(super) layout: Option<Language>,
    /// The reading that is on the list.
    pub(super) word: String,
}

/// A completion cycle: the guesses on offer for the word being typed, and which
/// one is currently on screen.
///
/// `index == candidates.len()` is the entry past the end — what the user typed
/// — so tapping through the whole list always arrives back at their own text
/// rather than stranding them on the last guess. **That wrap-around is the
/// feature's safety property**: a wrong guess costs a keypress, not a deletion,
/// which is what lets the completer guess at all.
pub struct Cycle<P: Platform> {
    /// The word buffer as the user typed it, before any completion.
    pub(super) typed: Vec<Typed<P::Key>>,
    /// Separators already following a finished word, preserved on every offer.
    pub(super) trailing_spaces: Vec<Typed<P::Key>>,
    pub(super) candidates: Vec<String>,
    pub(super) candidate_langs: Vec<Language>,
    pub(super) index: usize,
    /// The next page starts here after the original-text checkpoint.
    pub(super) resume_index: usize,
    /// Characters the current offer put on screen, to erase for the next one.
    pub(super) on_screen: usize,
    /// Layout currently displayed, checked before every injection.
    pub(super) lang: Language,
    pub(super) original_lang: Language,
    pub(super) correction: bool,
}

impl<P: Platform> LastAction<P> {
    pub(super) fn trailing_spaces_mut(&mut self) -> Option<&mut Vec<Typed<P::Key>>> {
        match self {
            Self::Fixed(fix) => Some(&mut fix.trailing_spaces),
            Self::Skipped(skip) => Some(&mut skip.trailing_spaces),
            Self::Unchanged {
                trailing_spaces, ..
            } => Some(trailing_spaces),
            Self::Selection { .. } => None,
        }
    }
}

impl<P: Platform> Clone for Cycle<P> {
    fn clone(&self) -> Self {
        Self {
            typed: self.typed.clone(),
            trailing_spaces: self.trailing_spaces.clone(),
            candidates: self.candidates.clone(),
            candidate_langs: self.candidate_langs.clone(),
            index: self.index,
            resume_index: self.resume_index,
            on_screen: self.on_screen,
            lang: self.lang,
            original_lang: self.original_lang,
            correction: self.correction,
        }
    }
}

impl<P: Platform> Clone for LastFix<P> {
    fn clone(&self) -> Self {
        Self {
            visible: self.visible.clone(),
            visible_layout: self.visible_layout,
            manual_conversion: self.manual_conversion,
            on_screen: self.on_screen,
            restore: self.restore.clone(),
            restore_layout: self.restore_layout,
            trailing_spaces: self.trailing_spaces.clone(),
            terminator: self.terminator,
            layout: self.layout,
            keep: self.keep.clone(),
            suppress: self.suppress.clone(),
            rule: self.rule,
            learning_pair: self.learning_pair.clone(),
        }
    }
}

/// A rewrite is not a retained choice until subsequent trusted typing.
#[derive(Clone)]
pub(super) struct PendingLearning {
    /// A correction's original text; None for an unaccepted completion offer.
    pub(super) from: Option<String>,
    pub(super) to: String,
    pub(super) phrase_boundary: bool,
    pub(super) manual_pair: Option<(String, String)>,
}

/// Listener state, shared by every capture thread of a platform.
pub struct AppState<P: Platform> {
    pub(super) mode: crate::config::AppMode,
    pub(super) practice: bool,
    pub keys: WordBuffer<Typed<P::Key>>,
    pub is_replacing: bool,
    pub(super) selection_replacing: bool,
    pub buffered_keys: WordBuffer<Typed<P::Key>>,
    /// Physical keys currently held down. Tracked from press/release events so
    /// injection can wait for the user to lift the keys it is about to retype —
    /// otherwise the OS sees the synthetic press as a duplicate of the
    /// still-held physical key and drops it.
    pub held_keys: HashSet<P::Key>,
    /// Per-reader ownership: several keyboards may hold the same key.
    #[cfg(target_os = "linux")]
    pub(super) device_keys: HashMap<usize, HashSet<P::Key>>,
    /// Caps Lock latch. Together with the held shifts it is what decides
    /// whether a letter came out capitalized.
    pub caps_lock: bool,
    /// When the configured completion modifier went down alone.
    pub completion_down: Option<Instant>,
    /// When an action modifier went down with nothing pressed since. `None` once
    /// another key joins it, because that makes it a shortcut rather than a
    /// tap.
    pub action_down: Option<Instant>,
    /// When the last completed action tap happened; a second one inside
    /// [`super::DOUBLE_TAP_WINDOW`] is the undo gesture.
    pub last_action_tap: Option<Instant>,
    /// An action completed while the current correction was landing.
    pub(super) pending_undo: bool,
    /// Whether the queued action uses double-tap undo/conversion semantics.
    pub(super) pending_conversion: bool,
    /// Coalesce rapid suggestion taps while the previous offer is landing.
    pub(super) pending_suggestion: Option<bool>,
    pub(super) shortcut_bindings: Option<(String, String, String, String)>,
    /// A key combination shaped like a layout-switch hotkey has been pressed and
    /// the modifiers holding it have not all come back up yet. When they do, the
    /// cached layout is dropped — see [`crate::layout::invalidate`].
    pub layout_hotkey: bool,
    /// What the action double-tap would do to the word the cursor is sitting on,
    /// if it would do anything.
    pub last_action: Option<LastAction<P>>,
    /// The completion cycle in progress, if the user is tapping through guesses.
    pub cycle: Option<Cycle<P>>,
    pub(super) pending_learning: Option<PendingLearning>,
    pub(super) phrase: crate::complete::PhraseContext,
    pub(super) completion_use: Option<bool>,
    /// What language the last few finished words were in — the context every
    /// ambiguous decision is missing when it looks at one word on its own.
    ///
    /// Kept here because this is the only place that knows when a word is
    /// finished, and cleared alongside the gestures whenever the cursor moves
    /// somewhere else: the run is a claim about the text being written *here*.
    pub history: History,
    /// The last key seen and its source timestamp, for multi-node deduplication.
    /// Only read where [`Platform::DEDUP_WINDOW`] is set, which is Linux.
    pub(super) last_key: Option<P::Key>,
    pub(super) last_key_at: SystemTime,
    /// Incremented whenever the cursor or text can no longer be trusted.
    pub(super) generation: u64,
    /// Every input event, including releases, invalidates an in-flight query.
    pub(super) revision: u64,
    pub(super) focus: Option<P::Focus>,
    pub(super) no_fix: bool,
    /// Known finished text immediately before the cursor, retained for deletion.
    /// Some(empty) means the tracked word was fully erased; repeated Backspace
    /// must not suppress the next freshly captured word. None means unknown text.
    pub(super) deletion_tail: Option<Vec<Typed<P::Key>>>,
}

impl<P: Platform> AppState<P> {
    pub(super) fn new() -> Self {
        Self {
            mode: crate::config::AppMode::Full,
            practice: false,
            keys: WordBuffer::new(),
            is_replacing: false,
            selection_replacing: false,
            buffered_keys: WordBuffer::new(),
            held_keys: HashSet::new(),
            #[cfg(target_os = "linux")]
            device_keys: HashMap::new(),
            caps_lock: false,
            completion_down: None,
            action_down: None,
            last_action_tap: None,
            pending_undo: false,
            pending_conversion: false,
            pending_suggestion: None,
            shortcut_bindings: None,
            layout_hotkey: false,
            last_action: None,
            cycle: None,
            pending_learning: None,
            phrase: crate::complete::PhraseContext::default(),
            completion_use: None,
            history: History::default(),
            last_key: None,
            last_key_at: SystemTime::now(),
            generation: 0,
            revision: 0,
            focus: None,
            no_fix: false,
            deletion_tail: None,
        }
    }

    /// Ignore mirrored device reports using their event timestamps. Delivery
    /// can be delayed or reordered between readers; processing time would make
    /// genuine repeated letters and spaces in a queued batch look duplicated.
    pub(super) fn duplicate_press(
        &mut self,
        key: P::Key,
        repeat: bool,
        timestamp: SystemTime,
        window: Option<Duration>,
    ) -> bool {
        if let Some(window) = window.filter(|_| !repeat) {
            let separation = timestamp
                .duration_since(self.last_key_at)
                .unwrap_or_else(|error| error.duration());
            if self.last_key == Some(key) && separation < window {
                // A delayed mirror can arrive after another reader's release.
                // Deduplicate text without losing this source's physical hold.
                self.held_keys.insert(key);
                return true;
            }
        }
        self.last_key_at = timestamp;
        self.last_key = Some(key);
        false
    }

    /// A successful offer is resolved once, independently of the visual cycle
    /// (which ordinary typing clears before the word-ending handler runs).
    pub(super) fn finish_completion_use(&mut self, accepted: bool) {
        if let Some(first) = self.completion_use.take() {
            use crate::personal::{record_use, UseEvent};
            record_use(if accepted {
                UseEvent::Accepted
            } else {
                UseEvent::Abandoned
            });
            if accepted && first {
                record_use(UseEvent::FirstChoice);
            }
        }
    }

    pub(super) fn retain_phrase_word(&mut self, word: &str) {
        if !self.practice && self.mode == crate::config::AppMode::Full {
            self.phrase
                .observe(word, crate::config::Config::global().personal_enabled);
        } else {
            self.phrase.clear();
        }
    }

    /// Whether a letter pressed right now would come out capitalized.
    pub(super) fn shift_active(&self) -> bool {
        let held =
            self.held_keys.contains(&P::SHIFT_LEFT) || self.held_keys.contains(&P::SHIFT_RIGHT);
        held != self.caps_lock
    }

    /// Add a key to whichever buffer is live: the word being typed, or the one
    /// holding what the user got in while a correction was landing.
    pub(super) fn push_key(&mut self, typed: Typed<P::Key>) {
        if self.is_replacing {
            self.buffered_keys.push(typed);
        } else {
            self.keys.push(typed);
        }
    }

    pub(super) fn invalidate_text(&mut self) {
        self.finish_completion_use(false);
        self.revision = self.revision.wrapping_add(1);
        self.generation = self.generation.wrapping_add(1);
        self.keys.clear();
        self.buffered_keys.clear();
        self.forget_gestures();
        self.pending_learning = None;
        self.deletion_tail = None;
        self.phrase.clear();
        self.history.clear();
        self.no_fix = true;
    }

    /// Cancel everything tied to the old cursor while accepting newly typed
    /// text immediately. No pre-existing text is included in the new buffer.
    pub(super) fn start_new_context(&mut self) {
        self.invalidate_text();
        self.action_down = None;
        self.completion_down = None;
        self.no_fix = false;
    }
}

impl<P: Platform> Replaceable for AppState<P> {
    fn set_replacing(&mut self, replacing: bool) {
        self.is_replacing = replacing;
        if !replacing {
            self.selection_replacing = false;
            if P::injection_finished() {
                self.invalidate_text();
            }
        }
    }
    fn clear_buffered(&mut self) {
        self.buffered_keys.clear();
    }
}

pub(super) fn hebrew_char<P: Platform>(typed: Typed<P::Key>) -> Option<char> {
    crate::keymap::hebrew_char_shifted(
        P::english_char(typed.key, typed.shift),
        P::hebrew_char(typed.key),
        typed.shift,
    )
}

/// The text a key sequence spells under `lang`, capitals included — what was on
/// screen before a correction rewrote it.
///
/// The capitals matter on macOS and Windows, where this *is* what gets typed
/// back; on Linux the restore path replays the user's keys instead and this is
/// only read for the history and the suppression list. Shared anyway, because a
/// history that shows `Shalom` on two platforms and `shalom` on the third is a
/// difference nobody chose.
pub fn reading<P: Platform>(keys: &[Typed<P::Key>], lang: Language) -> String {
    keys.iter()
        .filter_map(|t| match lang {
            _ if P::is_terminator(t.key) => Some(if P::is_line_break(t.key) { '\n' } else { ' ' }),
            Language::English => P::english_char(t.key, t.shift).map(|c| {
                if t.shift {
                    c.to_ascii_uppercase()
                } else {
                    c
                }
            }),
            // Hebrew has no case, so the shift the user held says nothing.
            Language::Hebrew => hebrew_char::<P>(*t),
        })
        .collect()
}

pub(super) fn non_empty(s: String) -> Option<String> {
    (!s.is_empty()).then_some(s)
}
