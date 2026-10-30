//! Replacement workers, guarded injection, replay, and retry context.

use super::correction_pipeline::completion_correction;
use super::word_buffer::{PendingLearning, MAX_UNDO_SPACES};
use super::{
    reading, AppState, Cycle, Engine, LastAction, LastFix, LastSkip, Platform, Typed,
    MAX_SELECTION_BYTES,
};
use crate::types::{FixKind, Language, ReplaceGuard};
use std::sync::{Arc, MutexGuard};
use std::thread;
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

/// Restore tracked text only when a rewrite failed before injection started.
pub(super) struct RetryContext<P: Platform> {
    pub(super) layout: Option<Language>,
    pub(super) finish_completion_on_commit: bool,
    pub(super) clear_phrase_on_commit: bool,
    pub(super) keys: Vec<Typed<P::Key>>,
    pub(super) deletion_tail: Option<Vec<Typed<P::Key>>>,
    pub(super) no_fix: bool,
    pub(super) last_action: Option<LastAction<P>>,
    pub(super) cycle: Option<Cycle<P>>,
    pub(super) pending_learning: Option<PendingLearning>,
}

impl<P: Platform> RetryContext<P> {
    pub(super) fn capture(st: &AppState<P>, layout: Option<Language>) -> Self {
        let last_action = st.last_action.as_ref().map(|action| match action {
            LastAction::Fixed(fix) => LastAction::Fixed(fix.clone()),
            LastAction::Selection { before, after } => LastAction::Selection {
                before: before.clone(),
                after: after.clone(),
            },
            LastAction::Skipped(skip) => LastAction::Skipped(LastSkip {
                keys: skip.keys.clone(),
                terminator: skip.terminator,
                trailing_spaces: skip.trailing_spaces.clone(),
                layout: skip.layout,
                word: skip.word.clone(),
            }),
            LastAction::Unchanged {
                keys,
                terminator,
                trailing_spaces,
                layout,
            } => LastAction::Unchanged {
                keys: keys.clone(),
                terminator: *terminator,
                trailing_spaces: trailing_spaces.clone(),
                layout: *layout,
            },
        });
        Self {
            layout,
            finish_completion_on_commit: false,
            clear_phrase_on_commit: false,
            keys: st.keys.to_vec(),
            deletion_tail: st.deletion_tail.clone(),
            no_fix: st.no_fix,
            last_action,
            cycle: st.cycle.clone(),
            pending_learning: st.pending_learning.clone(),
        }
    }
}

pub(super) enum Commit {
    /// Cycling may change language without counting another corrected word.
    Suggestion { layout: Language },
    Fix {
        from: String,
        to: String,
        kind: FixKind,
        rule: Option<&'static str>,
        details: String,
        deferred_layout: Option<Language>,
    },
    Undo {
        suppress: Option<String>,
        layout: Option<Language>,
        rule: Option<&'static str>,
        learning_pair: Option<(String, String)>,
    },
}

/// What injection is being asked to do.
pub struct Plan<P: Platform> {
    /// How many characters have to come off the screen, the terminator the user
    /// typed included.
    pub erase: usize,
    /// What goes in their place.
    pub retype: P::Retype,
    /// The key that ended the word, to press again afterwards. `None` for a
    /// completion, which is triggered by a key that types nothing and therefore
    /// has nothing to restore.
    pub terminator: Option<P::Key>,
}

impl<P: Platform> AppState<P> {
    /// Physical replay keys and modifiers must be up before a keycode batch.
    #[cfg(target_os = "linux")]
    pub(super) fn replay_keys_held(&self, keys: &[P::Key]) -> bool {
        keys.iter()
            .copied()
            .chain(self.buffered_keys.iter().map(|typed| typed.key))
            .any(|key| self.held_keys.contains(&key))
            || self
                .held_keys
                .iter()
                .any(|&key| P::is_modifier(key) && key != P::CAPS_LOCK)
    }
}

impl<P: Platform> Engine<P> {
    /// Wait for every key in `keys` to be physically up, or for `ceiling` to
    /// pass. A ceiling, not a cost: it returns the moment the last one lifts.
    ///
    /// There is no injecting a way out of this. A press injected while the
    /// physical key is still down never reaches the focused window — the OS
    /// already has that key down and discards the second press as a duplicate —
    /// and sending a *release* first does nothing at all, because key state is
    /// tracked per input device and this device never pressed the key. Only the
    /// user's own finger clears it, which leaves waiting as the only thing that
    /// works.
    #[cfg(target_os = "linux")]
    pub fn wait_for_release(&self, keys: &[P::Key], ceiling: Duration) {
        if keys.is_empty() {
            return;
        }
        let poll = crate::timing::injection().held_poll;
        let start = Instant::now();
        loop {
            let held = {
                let st = self.lock();
                keys.iter().any(|k| st.held_keys.contains(k))
            };
            if !held || start.elapsed() >= ceiling {
                return;
            }
            crate::timing::pause(poll);
        }
    }

    /// Snapshot the replay suffix only after replay keys and modifiers are up.
    /// The suffix can grow during a release wait, so inspect it together with
    /// held keys under one lock. A timeout must leave the visible text intact.
    #[cfg(target_os = "linux")]
    pub fn replay_snapshot(
        &self,
        keys: &[P::Key],
        generation: u64,
        ceiling: Duration,
    ) -> Option<Vec<Typed<P::Key>>> {
        let poll = crate::timing::injection().held_poll;
        let start = Instant::now();
        loop {
            let st = self.lock();
            if st.generation != generation {
                return None;
            }
            let held = st.replay_keys_held(keys);
            if !held {
                return Some(st.buffered_keys.to_vec());
            }
            drop(st);
            if start.elapsed() >= ceiling {
                return None;
            }
            crate::timing::pause(poll);
        }
    }

    /// Revalidate a Linux batch between writes, including physical input that
    /// arrived after its replay suffix was frozen. Focus queries run without
    /// holding either the typing-state lock or the injector lock.
    #[cfg(target_os = "linux")]
    pub fn replay_valid(&self, keys: &[P::Key], generation: u64, buffered_len: usize) -> bool {
        if !self.replacement_valid(generation) {
            return false;
        }
        let st = self.lock();
        st.generation == generation
            && st.buffered_keys.len() == buffered_len
            && !st.replay_keys_held(keys)
    }

    /// Keys the user managed to type while a correction was landing. Read while
    /// holding the lock and returned by value: the injected keystrokes re-enter
    /// the listener, which needs the same lock, so holding it across an
    /// injection is a deadlock.
    #[cfg(any(target_os = "macos", target_os = "windows", test))]
    pub fn buffered(&self) -> Vec<Typed<P::Key>> {
        self.lock().buffered_keys.to_vec()
    }

    /// Called after waits and immediately before destructive injection.
    pub fn replacement_valid(&self, generation: u64) -> bool {
        let current_generation = self.lock().generation;
        if current_generation != generation
            || !self.control.is_enabled()
            || !P::replacement_allowed()
        {
            return false;
        }
        let (focus, mode) = self.read_focus();
        let completion_layout = self.lock().cycle.as_ref().map(|cycle| cycle.lang);
        let layout_matches = completion_layout.is_none_or(|lang| P::current_layout() == Some(lang));
        let st = self.lock();
        layout_matches
            && mode.is_some_and(|mode| mode != crate::config::AppMode::Off && mode == st.mode)
            && st.generation == generation
            && st.focus == focus
            && (!P::requires_focus() || focus.is_some())
    }

    pub(super) fn rescue_selection(self: &Arc<Self>, mut st: MutexGuard<'_, AppState<P>>) {
        let undo = st.last_action.take();
        let generation = st.generation;
        st.is_replacing = true;
        st.selection_replacing = true;
        drop(st);
        let engine = Arc::clone(self);
        thread::spawn(move || {
            let _gate = ReplaceGuard::new(&engine.state, P::injecting_flag(&engine.injector));
            let Some(focus) = P::focus() else {
                return;
            };
            if !engine.replacement_valid(generation) {
                return;
            }
            let Some(selected) = P::selection(&focus) else {
                return;
            };
            let text = match undo {
                Some(LastAction::Selection { before, after }) if after == selected => before,
                Some(LastAction::Selection { .. }) => return,
                _ => crate::keymap::convert_selection(&selected.text),
            };
            if text == selected.text
                || text.len() > MAX_SELECTION_BYTES
                || !engine.replacement_valid(generation)
            {
                return;
            }
            let Some(after) = P::replace_selection(&engine, &focus, &selected, &text, generation)
            else {
                return;
            };
            let mut st = engine.lock();
            if st.generation == generation {
                st.last_action = Some(LastAction::Selection {
                    before: selected.text,
                    after,
                });
                st.no_fix = true;
            }
        });
    }

    /// Gate the listener and hand the replacement to a thread of its own.
    ///
    /// The lock is taken by the caller and dropped here, before the thread
    /// starts: injection re-enters the listener, which needs the same lock.
    pub(super) fn start_replacement(
        self: &Arc<Self>,
        mut st: MutexGuard<'_, AppState<P>>,
        plan: Plan<P>,
        keep: Vec<Typed<P::Key>>,
        undo: Option<LastFix<P>>,
        commit: Option<Commit>,
        retry: Option<RetryContext<P>>,
    ) {
        let generation = st.generation;
        st.is_replacing = true;
        drop(st);
        let engine = Arc::clone(self);
        thread::spawn(move || engine.replace_word(plan, keep, undo, generation, commit, retry));
    }

    /// Erase what the user typed and put the replacement in its place.
    ///
    /// `keep` is the word buffer to leave behind — what is now on screen for
    /// the word still in progress, so a completion the user keeps typing over
    /// is checked as the word they can see rather than as the tail they added.
    /// `undo` carries visible keys and the original text, kept through bounded spaces.
    fn replace_word(
        self: &Arc<Self>,
        plan: Plan<P>,
        keep: Vec<Typed<P::Key>>,
        mut undo: Option<LastFix<P>>,
        generation: u64,
        commit: Option<Commit>,
        retry: Option<RetryContext<P>>,
    ) {
        // Armed for the whole replacement: whatever happens below — including a
        // panic — `is_replacing` and the injecting gate are cleared, rather than
        // leaving the listener shut for the rest of the session.
        let gate = ReplaceGuard::new(&self.state, P::injecting_flag(&self.injector));

        if !self.replacement_valid(generation) {
            self.abort_before_injection(generation, retry);
            return;
        }
        if !self.confirm_replacement_layout(commit.as_ref(), generation) {
            self.abort_before_injection(generation, retry);
            return;
        }
        let phrase_boundary = plan.terminator.is_some_and(P::is_line_break);
        let Some(buffered) = P::inject(self, plan, generation) else {
            let mut st = self.lock();
            if st.generation == generation {
                st.invalidate_text();
            }
            return;
        };
        let (practice, mode) = {
            let mut st = self.lock();
            if st.generation != generation {
                return;
            }
            if st.buffered_keys.len() != buffered.len() {
                // Input arriving after the native replay snapshot moves the
                // cursor beyond our undo payload. Never restore stale text state.
                st.invalidate_text();
                return;
            }
            if st.generation == generation {
                if let Some(retry) = &retry {
                    if retry.finish_completion_on_commit {
                        st.finish_completion_use(false);
                    }
                    if retry.clear_phrase_on_commit {
                        st.phrase.clear();
                    }
                }
            }
            (st.practice, st.mode)
        };
        let mut learn = None;
        let mut pending_learning = None;
        match commit {
            Some(Commit::Fix {
                from,
                to,
                kind,
                rule,
                details,
                ..
            }) => {
                if practice {
                    crate::practice::fixed(&self.control, &from, &to, kind);
                }
                if !practice {
                    self.control
                        .record_fix_with_details(&from, &to, kind, details);
                    if let Some(rule) = rule {
                        crate::personal::record_rule(rule, false);
                    }
                    if mode == crate::config::AppMode::Full
                        && kind != FixKind::Complete
                        && rule.is_some()
                    {
                        pending_learning = Some(PendingLearning {
                            from: Some(from),
                            to,
                            phrase_boundary,
                            manual_pair: None,
                        });
                    }
                }
            }
            Some(Commit::Undo {
                suppress,
                rule,
                learning_pair,
                ..
            }) => {
                if !practice {
                    if let Some((from, to)) = learning_pair {
                        crate::personal::reject_confusion(&from, &to);
                    }
                }
                if practice && suppress.as_deref() == Some("akuo") {
                    let _ = self.control.practice_stage.compare_exchange(
                        1,
                        2,
                        std::sync::atomic::Ordering::Relaxed,
                        std::sync::atomic::Ordering::Relaxed,
                    );
                }
                if let Some(word) = suppress.filter(|_| !practice) {
                    crate::complete::suppress(&word);
                    learn = Some(word);
                }
                if !practice {
                    self.control.record_undo();
                    if let Some(rule) = rule {
                        crate::personal::record_rule(rule, true);
                    }
                }
            }
            Some(Commit::Suggestion { .. }) | None => {}
        }

        let mut st = self.lock();
        if st.generation == generation && st.buffered_keys.len() != buffered.len() {
            // Bookkeeping above can block while physical input keeps arriving.
            // The replay snapshot must still describe the cursor at commit time.
            st.invalidate_text();
        }
        if st.generation == generation {
            if st.cycle.is_some() {
                // An explicit successful edit establishes trusted text at the
                // cursor, even if capture began after a click into an unknown field.
                st.no_fix = false;
            }
            if st
                .cycle
                .as_ref()
                .is_some_and(|cycle| !cycle.trailing_spaces.is_empty())
            {
                // The last recorded language described the word we just replaced.
                st.history.clear();
            }
            if !practice && crate::config::Config::global().rule_stats_enabled {
                if let Some(cycle) = &st.cycle {
                    let back = cycle.index >= cycle.candidates.len();
                    use crate::personal::{record_use, UseEvent};
                    if st.completion_use.is_some() {
                        record_use(UseEvent::CycleTap);
                        st.completion_use = Some(false);
                    } else if !back {
                        record_use(UseEvent::Session);
                        st.completion_use = Some(true);
                    }
                    if back {
                        st.finish_completion_use(false);
                    }
                }
            }
            st.pending_learning = if let Some(cycle) = &st.cycle {
                cycle.candidates.get(cycle.index).map(|to| PendingLearning {
                    from: None,
                    to: to.clone(),
                    phrase_boundary: false,
                    manual_pair: {
                        let from = reading::<P>(&cycle.typed, cycle.original_lang);
                        if cycle.correction && self.en_dict.contains(&from.to_lowercase()) {
                            None
                        } else {
                            completion_correction(
                                &from,
                                to,
                                cycle.original_lang,
                                cycle.candidate_langs[cycle.index],
                            )
                        }
                    },
                })
            } else {
                pending_learning
            };
            let tracked_spaces = undo.as_ref().map_or(0, |fix| fix.trailing_spaces.len());
            let only_spaces = tracked_spaces + buffered.len() <= MAX_UNDO_SPACES
                && buffered
                    .iter()
                    .all(|typed| P::is_terminator(typed.key) && !P::is_line_break(typed.key));
            if only_spaces && !buffered.is_empty() {
                st.keys.clear();
                if let Some(fix) = &mut undo {
                    fix.trailing_spaces.extend(buffered.iter().copied());
                    fix.keep.clear();
                }
            } else if let Some(boundary) = buffered
                .iter()
                .rposition(|typed| P::is_terminator(typed.key))
            {
                st.keys
                    .replace_with(buffered[boundary + 1..].iter().copied());
                st.no_fix = false;
                st.cycle = None;
                st.pending_learning = None;
                st.phrase.clear();
                st.history.clear();
            } else {
                st.keys.replace_with(keep);
                st.keys.extend(buffered.iter().copied());
            }
            // Only tracked spaces may follow a correction that can still be undone.
            st.last_action = if only_spaces {
                undo.map(LastAction::Fixed).or_else(|| {
                    let cycle = st.cycle.as_ref()?;
                    if cycle.index < cycle.candidates.len() || cycle.trailing_spaces.is_empty() {
                        return None;
                    }
                    let mut trailing_spaces: Vec<_> =
                        cycle.trailing_spaces.iter().skip(1).copied().collect();
                    trailing_spaces.extend_from_slice(&buffered);
                    Some(LastAction::Unchanged {
                        keys: cycle.typed.clone(),
                        terminator: cycle.trailing_spaces.first().map(|typed| typed.key),
                        trailing_spaces,
                        layout: Some(cycle.original_lang),
                    })
                })
            } else {
                None
            };
            st.deletion_tail = match &st.last_action {
                Some(LastAction::Fixed(fix))
                    if fix.terminator.is_some() && !fix.visible.is_empty() =>
                {
                    let mut tail = fix.visible.clone();
                    tail.extend(fix.terminator.map(|key| Typed { key, shift: false }));
                    tail.extend_from_slice(&fix.trailing_spaces);
                    Some(tail)
                }
                _ => None,
            };
            st.last_key = None;
        }
        gate.finish(&mut st);
        drop(st);

        let mut st = self.lock();
        let retry = RetryContext::capture(&st, None);
        let pending = (!st.is_replacing && std::mem::take(&mut st.pending_undo))
            .then(|| st.last_action.take())
            .flatten();
        let suggestion = if st.generation == generation && !st.is_replacing && st.cycle.is_some() {
            st.pending_suggestion.take()
        } else {
            None
        };
        if let Some(LastAction::Fixed(fix)) = pending {
            if std::mem::take(&mut st.pending_conversion) {
                self.double_tap_fixed(st, fix, retry);
            } else {
                self.undo_fix(st, fix, retry);
            }
        } else if let Some(correction) = suggestion {
            self.request_suggestion(st, correction);
        } else {
            drop(st);
        }
        // Persistence must not extend the injection window: real typing during
        // a slow disk write used to be treated as an interrupted replacement.
        if let Some(word) = learn {
            crate::complete::learn(&word);
        }
    }

    /// A refused switch did not edit text. Preserve retry context only while
    /// focus and tracked input still match; never use this after injection.
    fn abort_before_injection(&self, generation: u64, retry: Option<RetryContext<P>>) {
        let (focus, mode) = self.read_focus();
        let layout = P::current_layout();
        let mut st = self.lock();
        if st.generation != generation {
            return;
        }
        if let Some(retry) = retry.filter(|retry| {
            st.buffered_keys.is_empty()
                && (layout.is_none() || retry.clear_phrase_on_commit || retry.layout == layout)
                && st.focus == focus
                && (!P::requires_focus() || focus.is_some())
                && mode == Some(st.mode)
                && mode != Some(crate::config::AppMode::Off)
                && self.control.is_enabled()
        }) {
            st.keys.replace_with(retry.keys);
            st.deletion_tail = retry.deletion_tail;
            st.no_fix = retry.no_fix;
            st.last_action = retry.last_action;
            st.cycle = retry.cycle;
            st.pending_learning = retry.pending_learning;
            st.pending_undo = false;
            st.pending_conversion = false;
            st.pending_suggestion = None;
        } else {
            st.invalidate_text();
        }
    }
}
