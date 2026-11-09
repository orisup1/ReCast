//! Automatic correction, explicit suggestions, and replacement planning.

use super::injection::{Commit, RetryContext};
use super::word_buffer::{hebrew_char, non_empty, MAX_UNDO_SPACES, MAX_UNDO_SUFFIX};
use super::{
    reading, AppState, Cycle, Engine, LastAction, LastFix, LastSkip, Plan, Platform, Typed,
};
use crate::dictionary::{
    all_completion_candidates, check_and_correct_in_context, correction_candidates,
    declined_by_list, Fix,
};
use crate::types::{FixKind, Language};
use std::sync::{Arc, MutexGuard};

pub(super) struct WordCheck<P: Platform> {
    keys: Vec<Typed<P::Key>>,
    key: P::Key,
    shift: bool,
    focus: Option<Arc<P::Focus>>,
    run: crate::dictionary::Run,
    mode: crate::config::AppMode,
    phrase: crate::complete::PhraseContext,
    layout: Option<Language>,
    generation: u64,
    presses: u64,
}

impl<P: Platform> AppState<P> {
    fn enqueue_word_check(&mut self, check: WordCheck<P>) {
        if self.word_checks.len() >= MAX_UNDO_SUFFIX {
            self.word_checks.pop_front();
        }
        self.word_checks.push_back(check);
    }

    /// Replayed text still needs the ordinary checks its Space presses requested.
    pub(super) fn recheck_replayed_words(
        &mut self,
        text: &[Typed<P::Key>],
        first_complete: bool,
        focus: Option<Arc<P::Focus>>,
        layout: Option<Language>,
    ) {
        if self.mode == crate::config::AppMode::ManualOnly
            || self.focus.as_ref() != focus.as_deref()
        {
            return;
        }
        let mut start = 0;
        for (end, typed) in text.iter().enumerate() {
            if !P::is_terminator(typed.key) {
                continue;
            }
            if P::is_line_break(typed.key) {
                return;
            }
            let following = text.len() - end - 1;
            if (first_complete || start > 0)
                && end > start
                && end - start <= crate::types::MAX_WORD_KEYS
                && following <= MAX_UNDO_SUFFIX
            {
                self.enqueue_word_check(WordCheck {
                    keys: text[start..end].to_vec(),
                    key: typed.key,
                    shift: typed.shift,
                    focus: focus.clone(),
                    run: self.history.run(),
                    mode: self.mode,
                    phrase: self.phrase.clone(),
                    layout,
                    generation: self.generation,
                    presses: self.press_revision.wrapping_sub(following as u64),
                });
            }
            start = end + 1;
        }
    }
}

pub(super) fn completion_correction(
    from: &str,
    to: &str,
    original: Language,
    candidate: Language,
) -> Option<(String, String)> {
    (original == Language::English
        && candidate == Language::English
        && to.len() <= from.len()
        && !to.eq_ignore_ascii_case(from)
        && from.bytes().all(|c| c.is_ascii_alphabetic())
        && to.bytes().all(|c| c.is_ascii_alphabetic()))
    .then(|| (from.to_lowercase(), to.to_lowercase()))
}

/// Undo exceptions use the same word boundaries as automatic correction.
fn undo_word(text: &str, language: Language) -> Option<String> {
    let (start, end) = crate::dictionary::frame_bounds_for_layout(text, language);
    let mut word: String = text.chars().skip(start).take(end - start).collect();
    let word_end = word
        .char_indices()
        .filter(|(_, c)| {
            c.is_alphanumeric()
                || (language == Language::Hebrew && crate::spell::hebrew::is_mark(*c))
        })
        .map(|(index, c)| index + c.len_utf8())
        .next_back()
        .unwrap_or(0);
    // Punctuation-only rules may own their brackets, so protect their full key.
    if word_end == 0 {
        return non_empty(text.to_owned());
    }
    word.truncate(word_end);
    non_empty(word)
}

impl<P: Platform> Engine<P> {
    /// Space checks a finished word; Enter accepts an offer and clears context.
    pub(super) fn word_finished(
        self: &Arc<Self>,
        mut st: MutexGuard<'_, AppState<P>>,
        key: P::Key,
        shift: bool,
        focus: Option<P::Focus>,
    ) {
        if st.is_replacing {
            st.buffered_keys.push(Typed { key, shift });
            if st.buffered_keys.is_empty() {
                // Overflow discarded the visible suffix, so its erase count is unknown.
                st.invalidate_text();
            }
            return;
        }
        // A correction stays undoable while the user types into the next
        // word; the terminator joins the tracked suffix.
        if P::is_terminator(key) && !P::is_line_break(key) {
            st.push_fix_suffix_key(Typed { key, shift });
        }
        if st.keys.is_empty() {
            // An overlong token empties the buffer but keeps its give-up flag.
            st.keys.clear();
            // A manual mid-word conversion leaves no word to recheck, but
            // spaces still belong to its undo payload.
            st.no_fix = false;
            if P::is_line_break(key) {
                st.invalidate_text();
                st.no_fix = false;
            } else if st.last_action.is_some() {
                if st.focus != focus || !P::input_allowed() {
                    st.invalidate_text();
                    st.no_fix = false;
                } else if let Some(spaces) = st
                    .last_action
                    .as_mut()
                    .and_then(LastAction::trailing_spaces_mut)
                {
                    spaces.push(Typed { key, shift });
                    if let Some(LastAction::Fixed(fix)) = &mut st.last_action {
                        fix.keep.clear();
                    }
                }
            }
            if let Some(tail) = &mut st.deletion_tail {
                if tail.len() < crate::types::MAX_WORD_KEYS + MAX_UNDO_SPACES {
                    tail.push(Typed { key, shift });
                } else {
                    st.deletion_tail = None;
                }
            }
            return;
        }
        if std::mem::take(&mut st.no_fix) || !self.control.is_enabled() {
            // Suppressing an automatic edit must not discard the user's next
            // explicit opportunity to convert the freshly tracked word.
            if !P::is_line_break(key)
                && self.control.is_enabled()
                && P::input_allowed()
                && st.focus == focus
                && (!P::requires_focus() || focus.is_some())
            {
                st.last_action = Some(LastAction::Unchanged {
                    keys: st.keys.to_vec(),
                    terminator: Some(key),
                    trailing_spaces: Vec::new(),
                    layout: P::current_layout(),
                });
            }
            st.keys.clear();
            st.deletion_tail = None;
            st.phrase.clear();
            if P::is_line_break(key) {
                st.invalidate_text();
                st.no_fix = false;
            }
            return;
        }
        if !P::input_allowed() || (P::requires_focus() && st.focus.is_none()) || st.focus != focus {
            st.invalidate_text();
            // This terminator already ended the untrusted word.
            st.no_fix = false;
            return;
        }

        if let Some(pending) = st.pending_learning.take() {
            if pending.from.is_none() {
                if let Some(lang) = P::current_layout() {
                    let chosen = pending
                        .to
                        .rsplit(char::is_whitespace)
                        .next()
                        .unwrap_or_default();
                    if chosen == reading::<P>(&st.keys, lang) {
                        // An explicitly chosen word must not be autocorrected on acceptance.
                        st.history.push(lang);
                        if !st.practice && st.mode == crate::config::AppMode::Full {
                            crate::personal::record_word(chosen);
                            if let Some((from, to)) = &pending.manual_pair {
                                crate::personal::record_explicit_confusion(from, to);
                            }
                        }
                        st.finish_completion_use(true);
                        st.retain_phrase_word(chosen);
                        let mut tail = st.keys.to_vec();
                        tail.push(Typed { key, shift });
                        st.deletion_tail = Some(tail);
                        if P::is_line_break(key) {
                            st.invalidate_text();
                            st.no_fix = false;
                        } else if let Some(LastAction::Fixed(fix)) = &mut st.last_action {
                            fix.trailing_spaces.push(Typed { key, shift });
                            fix.keep.clear();
                        }
                        st.keys.clear();
                        return;
                    }
                }
            }
        }

        // The physical Enter already reached the app; never erase or replay it.
        if P::is_line_break(key) {
            st.invalidate_text();
            st.no_fix = false;
            return;
        }

        st.finish_completion_use(false);
        let mut tail = st.keys.to_vec();
        tail.push(Typed { key, shift });
        st.deletion_tail = Some(tail);
        if st.mode == crate::config::AppMode::ManualOnly {
            st.last_action = Some(LastAction::Unchanged {
                keys: st.keys.to_vec(),
                terminator: Some(key),
                trailing_spaces: Vec::new(),
                layout: P::current_layout(),
            });
            st.keys.clear();
            st.phrase.clear();
            return;
        }
        let layout = P::current_layout();
        if st.last_action.is_none() {
            st.last_action = Some(LastAction::Unchanged {
                keys: st.keys.to_vec(),
                terminator: Some(key),
                trailing_spaces: Vec::new(),
                layout,
            });
        }
        let check = WordCheck {
            keys: st.keys.to_vec(),
            key,
            shift,
            focus: focus.map(Arc::new),
            run: st.history.run(),
            mode: st.mode,
            phrase: st.phrase.clone(),
            layout,
            generation: st.generation,
            presses: st.press_revision,
        };
        st.enqueue_word_check(check);
        st.keys.clear();
        drop(st);
        self.ensure_word_check_worker();
    }

    pub(crate) fn ensure_word_check_worker(self: &Arc<Self>) {
        let mut st = self.lock();
        if st.word_check_worker || st.word_checks.is_empty() || st.is_replacing {
            return;
        }
        st.word_check_worker = true;
        drop(st);
        let engine = Arc::clone(self);
        std::thread::spawn(move || loop {
            let mut st = engine.lock();
            if st.is_replacing {
                st.word_check_worker = false;
                return;
            }
            let Some(mut check) = st.word_checks.pop_front() else {
                st.word_check_worker = false;
                return;
            };
            // Expired checks cannot land; do not delay newer words with their searches.
            if st.generation != check.generation
                || st.press_revision.wrapping_sub(check.presses) > MAX_UNDO_SUFFIX as u64
            {
                continue;
            }
            // Earlier queued words have now supplied the context that fast typing missed.
            check.run = st.history.run();
            check.phrase = st.phrase.clone();
            drop(st);
            engine.check_word(check);
        });
    }

    fn check_word(self: &Arc<Self>, check: WordCheck<P>) {
        let WordCheck {
            keys,
            key,
            shift,
            focus,
            run,
            mode,
            phrase,
            layout,
            generation,
            presses,
        } = check;
        #[cfg(test)]
        super::tests::pause_word_check();
        // Pure search must not block releases or cancellation on the state lock.
        let outcome = check_and_correct_in_context(
            &keys,
            |t: Typed<P::Key>| P::english_char(t.key, t.shift),
            hebrew_char::<P>,
            |t: Typed<P::Key>| t.shift,
            run,
            self.en_dict,
            self.he_dict,
            layout,
            mode == crate::config::AppMode::LayoutOnly,
            Some(&phrase),
            |_| crate::layout::LayoutSwitch::Switched,
        );
        let Some(mut st) =
            self.captured_state(generation, crate::timing::injection().held_release_timeout)
        else {
            return;
        };
        // Following text is everything typed after the checked word while
        // search ran, reconstructed from tracked presses and verified
        // against the checked word itself. That covers extra spaces, an
        // unfinished next word, and whole finished words typed in between.
        // Words reconstructed by editing fall back to the deletion tail.
        // Verified edits below decline shapes they cannot match natively.
        let following_of = |st: &AppState<P>| -> Option<(Vec<Typed<P::Key>>, bool)> {
            let delta = st.press_revision.wrapping_sub(presses) as usize;
            // Fallback for words whose on-screen keys were reconstructed
            // (backspace recovery, kept words after a correction): the
            // deletion tail still describes them exactly.
            let fallback = |st: &AppState<P>| -> Option<(Vec<Typed<P::Key>>, bool)> {
                let tail = st.deletion_tail.as_ref()?;
                if tail.len() < keys.len() + 1 {
                    return None;
                }
                let spaces = tail.len() - (keys.len() + 1);
                if delta != spaces + st.keys.len()
                    || spaces + st.keys.len() > MAX_UNDO_SUFFIX
                    || !tail[keys.len() + 1..]
                        .iter()
                        .all(|typed| P::is_terminator(typed.key) && !P::is_line_break(typed.key))
                {
                    return None;
                }
                let mut following = tail[keys.len() + 1..].to_vec();
                following.extend_from_slice(&st.keys);
                Some((following, false))
            };
            if delta > MAX_UNDO_SUFFIX || st.tracked_text.len() < delta + keys.len() + 1 {
                return fallback(st);
            }
            let start = st.tracked_text.len() - delta;
            let Some(checked_start) = start.checked_sub(keys.len() + 1) else {
                return fallback(st);
            };
            let checked: Vec<_> = st
                .tracked_text
                .iter()
                .skip(checked_start)
                .take(keys.len())
                .collect();
            if checked
                .iter()
                .zip(&keys)
                .any(|(a, b)| a.key != b.key || a.shift != b.shift)
            {
                return fallback(st);
            }
            let Some(terminator) = st.tracked_text.get(checked_start + keys.len()) else {
                return fallback(st);
            };
            if terminator.key != key {
                return fallback(st);
            }
            let following: Vec<_> = st.tracked_text.iter().skip(start).copied().collect();
            // A finished word inside the following text needs its own check
            // after this one; spaces-only and unfinished-word tails do not.
            let preserved = following.iter().enumerate().any(|(index, typed)| {
                !P::is_terminator(typed.key)
                    && following[index + 1..]
                        .iter()
                        .any(|next| P::is_terminator(next.key))
            });
            Some((following, preserved))
        };
        let current = |st: &AppState<P>| {
            st.generation == generation && following_of(st).is_some() && !st.is_replacing
        };
        if !current(&st) {
            if st.is_replacing && st.generation == generation {
                // An earlier correction may preserve several queued words verbatim.
                // Defer checks whose captured text or reconstructed tail still matches.
                let preserved = st.preserved_word_tail.as_ref().map(|tail| {
                    let end = tail
                        .iter()
                        .rposition(|typed| !P::is_terminator(typed.key))
                        .map_or(0, |index| index + 1);
                    end == keys.len()
                        && tail[..end]
                            .iter()
                            .zip(&keys)
                            .all(|(a, b)| a.key == b.key && a.shift == b.shift)
                });
                if preserved == Some(true) || following_of(&st).is_some() {
                    st.word_checks.push_front(WordCheck {
                        keys,
                        key,
                        shift,
                        focus,
                        run,
                        mode,
                        phrase,
                        layout,
                        generation,
                        presses,
                    });
                }
            }
            return;
        }
        let revision = st.revision;
        let focus_presses = st.press_revision;
        drop(st);
        // An obsolete search must not invalidate text captured during its focus query.
        let (current_focus, current_mode) = self.read_focus();
        let current_layout = P::current_layout();
        let Some(mut st) =
            self.captured_state(generation, crate::timing::injection().held_release_timeout)
        else {
            return;
        };
        if !current(&st)
            || st.revision.wrapping_sub(revision) != st.press_revision.wrapping_sub(focus_presses)
        {
            return;
        }
        if current_mode != Some(mode) {
            st.invalidate_text();
            return;
        }
        if st.focus != current_focus
            || current_focus.as_ref() != focus.as_deref()
            || st.mode != mode
            || current_layout != layout
            || st.is_replacing
            || !self.control.is_enabled()
        {
            return;
        }
        st.practice = self
            .control
            .practice_open
            .load(std::sync::atomic::Ordering::Relaxed)
            && current_focus.as_ref().is_some_and(P::is_own_focus);
        if st.practice || st.mode != crate::config::AppMode::Full {
            st.phrase.clear();
        }

        let keep = st.keys.to_vec();
        let Some((following, preserved_word)) = following_of(&st) else {
            return;
        };
        let leading_spaces = following
            .iter()
            .take_while(|typed| P::is_terminator(typed.key) && !P::is_line_break(typed.key))
            .count();
        let extended = following.iter().any(|typed| !P::is_terminator(typed.key));
        let note = outcome.fix.as_ref().map(|fix| note_of::<P>(&keys, fix));
        let rep = replacement::<P>(&keys, outcome.fix);
        if extended
            && rep
                .as_ref()
                .is_some_and(|rep| !P::VERIFIED_REPLACEMENT || rep.previous_layout.is_some())
        {
            // Later keys may span a layout switch; do not guess their visible text.
            return;
        }

        // Record what this word turned out to be before anything else happens
        // to it: the next word is decided with this one behind it. A word whose
        // language could not be told is not recorded at all — see
        // `dictionary::observed`.
        if let Some(lang) = outcome.lang {
            st.history.push(lang);
        }
        let details = format!("Reason: {}\n{}", outcome.reason, outcome.details);
        let rule = outcome.rule;
        let abstain = outcome.abstain;
        if let Some(mut rep) = rep {
            let mut undo = undo_of::<P>(&keys, &rep, Some(key));
            undo.rule = rule;
            undo.learning_pair = note
                .as_ref()
                .map(|(from, to, _)| (from.clone(), to.clone()));
            // +1 for the terminator the user physically typed, which is erased
            // along with the word and pressed again afterwards.
            let erase = rep.erase + 1 + following.len();
            let deferred_layout = rep.previous_layout.map(|_| rep.target_layout);
            let mut retry =
                RetryContext::capture(&st, Some(rep.previous_layout.unwrap_or(rep.target_layout)));
            if following.is_empty() {
                retry.keys.clear();
                retry.last_action = Some(LastAction::Unchanged {
                    keys: keys.clone(),
                    terminator: Some(key),
                    trailing_spaces: Vec::new(),
                    layout: Some(rep.previous_layout.unwrap_or(rep.target_layout)),
                });
            }
            let source = reading::<P>(&keys, rep.previous_layout.unwrap_or(rep.target_layout));
            let suffix = reading::<P>(&following, rep.target_layout);
            let expected = format!(
                "{} {suffix}",
                source
                    .chars()
                    .skip(keys.len() - rep.erase)
                    .collect::<String>()
            );
            let continued = !following.is_empty();
            if continued {
                let mut tail = vec![Typed { key, shift }];
                tail.extend_from_slice(&following);
                P::append_retype(
                    &mut rep.retype,
                    P::retype_original(&tail, rep.target_layout),
                );
                undo.suffix = suffix;
                undo.trailing_spaces = following[..leading_spaces].to_vec();
            }
            if preserved_word {
                // The intervening finished word stays on screen verbatim;
                // keep its tail so its own check can land after this one.
                st.preserved_word_tail = st.deletion_tail.clone();
            }
            st.keys.clear();
            self.start_replacement(
                st,
                Plan {
                    verify: extended.then_some(rep.target_layout),
                    expected,
                    erase,
                    retype: rep.retype,
                    terminator: (!continued).then_some(key),
                },
                keep,
                Some(undo),
                note.map(|(from, to, kind)| Commit::Fix {
                    from,
                    to,
                    kind,
                    rule,
                    details,
                    deferred_layout,
                }),
                Some(retry),
            );
            return;
        }

        if !st.practice && st.mode == crate::config::AppMode::Full {
            if let Some(abstain) = abstain {
                crate::personal::record_abstain(abstain);
            }
            if let Some(lang) = outcome.lang {
                crate::personal::record_word(&reading::<P>(&keys, lang));
            }
            // Phrase context describes the visible text, even for words whose
            // keystrokes happen to be valid in both languages (for example "my").
            if let Some(lang) = current_layout {
                let word = reading::<P>(&keys, lang);
                st.retain_phrase_word(&word);
            } else {
                st.phrase.clear();
            }
        }
        if P::is_line_break(key) {
            st.phrase.clear();
        }

        if !following.is_empty() {
            return;
        }

        if let Some(word) = (!st.practice)
            .then(|| {
                declined_by_list(
                    &keys,
                    |t: Typed<P::Key>| P::english_char(t.key, t.shift),
                    hebrew_char::<P>,
                    |t: Typed<P::Key>| t.shift,
                    current_layout,
                )
            })
            .flatten()
        {
            // Nothing happened to this word, and the only reason is that the
            // user has it listed. Keep it available for explicit conversion.
            st.last_action = Some(LastAction::Skipped(LastSkip {
                keys: keys.clone(),
                terminator: Some(key),
                trailing_spaces: Vec::new(),
                layout: current_layout,
                word,
            }));
        }
        if st.last_action.is_none() {
            st.last_action = Some(LastAction::Unchanged {
                keys,
                terminator: Some(key),
                trailing_spaces: Vec::new(),
                layout: current_layout,
            });
        }
        st.keys.clear();
    }

    pub(super) fn request_suggestion<'a>(
        self: &'a Arc<Self>,
        st: MutexGuard<'a, AppState<P>>,
        correction: bool,
    ) {
        if !self.control.is_enabled() {
            return;
        }
        if st.is_replacing {
            let mut st = st;
            if !st.selection_replacing && st.cycle.is_some() {
                st.pending_suggestion = Some(correction);
            }
            return;
        }
        let (mut st, focus) = {
            // Bare rapid taps may change event revision while focus is queried.
            // Text/cursor changes cancel this worker through generation instead.
            let generation = st.generation;
            drop(st);
            let (focus, mode) = self.read_focus();
            let mut st = self.lock();
            if st.generation != generation {
                return;
            }
            if mode.is_none_or(|mode| mode == crate::config::AppMode::Off || mode != st.mode) {
                st.invalidate_text();
                return;
            }
            (st, focus)
        };
        if (P::requires_focus() && st.focus.is_none()) || st.focus != focus || !P::input_allowed() {
            st.invalidate_text();
            return;
        }
        if st.is_replacing {
            return;
        }
        self.suggestion_tap(st, correction, false);
    }

    pub(super) fn accept_preview<'a>(self: &'a Arc<Self>, st: MutexGuard<'a, AppState<P>>) {
        P::hide_preview();
        let Some((mut st, focus)) = self.refresh_focus(st) else {
            return;
        };
        if st.focus != focus || !self.control.is_enabled() || !P::input_allowed() {
            st.invalidate_text();
            return;
        }
        if st
            .cycle
            .as_ref()
            .is_some_and(|cycle| cycle.preview && cycle.preview_ready)
        {
            let correction = st.cycle.as_ref().unwrap().correction;
            self.suggestion_tap(st, correction, true);
        } else {
            st.cycle = None;
        }
    }

    /// Completion and explicit correction share cycling and undo, but search
    /// separately so an exact completion cannot hide spelling alternatives.
    fn suggestion_tap<'a>(
        self: &'a Arc<Self>,
        mut st: MutexGuard<'a, AppState<P>>,
        correction: bool,
        accept: bool,
    ) {
        if !matches!(
            st.mode,
            crate::config::AppMode::Full | crate::config::AppMode::ManualOnly
        ) {
            st.cycle = None;
            return;
        }
        if !correction && !crate::config::Config::global().complete_enabled {
            if !st.cycle.as_ref().is_some_and(|cycle| cycle.correction) {
                st.cycle = None;
            }
            return;
        }
        let Some(active_lang) = P::current_layout() else {
            // No text changed. Keep the prefix and cycle so another tap can retry.
            return;
        };
        if st
            .cycle
            .as_ref()
            .is_some_and(|cycle| cycle.lang != active_lang)
        {
            st.invalidate_text();
            return;
        }
        // A word boundary clears the typing buffer, not the tracked word at
        // the cursor. Explicit requests may reconsider that word and its layout.
        let (source_keys, separators, lang) = if !st.keys.is_empty() {
            (st.keys.to_vec(), Vec::new(), active_lang)
        } else if matches!(&st.last_action, Some(LastAction::Fixed(fix))
            if fix.suffix.chars().any(|ch| ch != ' '))
        {
            // Undo may still refer to an earlier correction; suggestions follow the caret.
            let Some(tail) = &st.deletion_tail else {
                return;
            };
            let text = reading::<P>(tail, active_lang);
            let Some(LastAction::Fixed(fix)) = &st.last_action else {
                unreachable!();
            };
            if text.is_empty() || !fix.suffix.ends_with(&text) {
                return;
            }
            let end = tail
                .iter()
                .rposition(|typed| !P::is_terminator(typed.key))
                .map_or(0, |index| index + 1);
            (tail[..end].to_vec(), tail[end..].to_vec(), active_lang)
        } else {
            let (keys, terminator, spaces, layout) = match &st.last_action {
                Some(LastAction::Fixed(fix)) => (
                    &fix.visible,
                    fix.terminator,
                    &fix.trailing_spaces,
                    Some(fix.visible_layout),
                ),
                Some(LastAction::Skipped(skip)) => (
                    &skip.keys,
                    skip.terminator,
                    &skip.trailing_spaces,
                    skip.layout,
                ),
                Some(LastAction::Unchanged {
                    keys,
                    terminator,
                    trailing_spaces,
                    layout,
                }) => (keys, *terminator, trailing_spaces, *layout),
                _ => return,
            };
            let mut separators = Vec::with_capacity(spaces.len() + 1);
            if let Some(key) = terminator {
                separators.push(Typed { key, shift: false });
            }
            separators.extend_from_slice(spaces);
            (keys.clone(), separators, layout.unwrap_or(active_lang))
        };
        let mut retry = RetryContext::capture(&st, Some(active_lang));
        let switched = if st
            .cycle
            .as_ref()
            .is_some_and(|cycle| cycle.correction != correction)
        {
            st.cycle.take()
        } else {
            None
        };
        let current = match switched.as_ref().or(st.cycle.as_ref()) {
            Some(cycle) if !cycle.preview => cycle
                .candidates
                .get(cycle.index)
                .cloned()
                .unwrap_or_else(|| reading::<P>(&cycle.typed, cycle.original_lang)),
            _ => reading::<P>(&source_keys, lang),
        };
        let (
            typed,
            candidates,
            candidate_langs,
            index,
            resume_index,
            erase,
            original_lang,
            trailing_spaces,
        ) = match st.cycle.take() {
            Some(cycle) => {
                let (next, resume) = if accept {
                    (cycle.index, cycle.resume_index)
                } else if cycle.index >= cycle.candidates.len() {
                    (cycle.resume_index, cycle.resume_index)
                } else if cycle.index + 1 == cycle.candidates.len()
                    || (cycle.index + 1) % crate::complete::MAX_CANDIDATES == 0
                {
                    (
                        cycle.candidates.len(),
                        (cycle.index + 1) % cycle.candidates.len(),
                    )
                } else {
                    (cycle.index + 1, cycle.resume_index)
                };
                (
                    cycle.typed,
                    cycle.candidates,
                    cycle.candidate_langs,
                    next,
                    resume,
                    cycle.on_screen,
                    cycle.original_lang,
                    cycle.trailing_spaces,
                )
            }
            None => {
                if source_keys.is_empty() {
                    st.cycle = switched;
                    return;
                }
                let visible = reading::<P>(&source_keys, lang);
                let (frame_start, frame_end) =
                    crate::dictionary::frame_bounds_for_layout(&visible, lang);
                let framed_keys: Vec<_> = source_keys
                    .iter()
                    .copied()
                    .enumerate()
                    .map(|(index, typed)| (typed, index < frame_start || index >= frame_end))
                    .collect();
                let candidate_char = |(typed, wrapper): (Typed<P::Key>, bool), candidate_lang| {
                    match if wrapper { lang } else { candidate_lang } {
                        Language::English => P::english_char(typed.key, typed.shift),
                        Language::Hebrew => (!typed.shift
                            || P::english_char_plain(typed.key)
                                .is_some_and(|c| !c.is_ascii_alphabetic()))
                        .then(|| hebrew_char::<P>(typed))
                        .flatten(),
                    }
                };
                // Candidate search can be expensive. Snapshot bounded context so
                // capture can keep canceling/replaying input without waiting for it.
                let generation = st.generation;
                let phrase = (!st.practice).then(|| st.phrase.clone());
                drop(st);
                let offers = |candidate_lang| {
                    let candidates = if correction {
                        correction_candidates
                    } else {
                        all_completion_candidates
                    };
                    candidates(
                        &framed_keys,
                        |typed| candidate_char(typed, candidate_lang),
                        |(typed, _): (Typed<P::Key>, bool)| typed.shift,
                        if candidate_lang == Language::English {
                            self.en_dict
                        } else {
                            self.he_dict
                        },
                        Some(candidate_lang),
                        phrase.as_ref(),
                    )
                };
                let typeable_offers = |lang| {
                    offers(lang)
                        .into_iter()
                        .filter(|word| P::retype_completion(word, lang).is_some())
                        .collect::<Vec<_>>()
                };
                let mut candidates = typeable_offers(lang);
                let prefix = visible.to_lowercase();
                let dict = if lang == Language::English {
                    self.en_dict
                } else {
                    self.he_dict
                };
                let strong = dict.contains(&prefix)
                    || crate::complete::expand(&prefix)
                        .is_some_and(|word| P::retype_completion(&word, lang).is_some())
                    || candidates.iter().any(|word| {
                        crate::dictionary::completion_matches_prefix(&prefix, &word.to_lowercase())
                    });
                let mut candidate_langs = vec![lang; candidates.len()];
                if !correction {
                    let alternate_prefix: String = framed_keys
                        .iter()
                        .copied()
                        .filter_map(|typed| candidate_char(typed, lang.other()))
                        .collect();
                    let alternate_prefix = alternate_prefix.to_lowercase();
                    let alternate: Vec<_> = typeable_offers(lang.other())
                        .into_iter()
                        .filter(|word| {
                            crate::dictionary::completion_matches_prefix(
                                &alternate_prefix,
                                &word.to_lowercase(),
                            )
                        })
                        .collect();
                    if !alternate_prefix.is_empty() && !alternate.is_empty() {
                        let alternate_langs = vec![lang.other(); alternate.len()];
                        if strong {
                            candidates.extend(alternate);
                            candidate_langs.extend(alternate_langs);
                        } else {
                            let mut combined = alternate;
                            combined.extend(candidates);
                            candidates = combined;
                            let mut combined_langs = alternate_langs;
                            combined_langs.extend(candidate_langs);
                            candidate_langs = combined_langs;
                        }
                    }
                }
                st = self.lock();
                if st.generation != generation || st.is_replacing {
                    return;
                }
                if candidates.is_empty() {
                    st.cycle = switched;
                    return;
                }
                // Switching gestures keeps the first typed text as the cycle's undo target.
                let (typed, erase, original_lang, trailing_spaces) =
                    if let Some(previous) = &switched {
                        let tail = visible;
                        let head = current.strip_suffix(&tail).unwrap_or_default();
                        candidates = candidates
                            .into_iter()
                            .map(|word| format!("{head}{word}"))
                            .collect();
                        (
                            previous.typed.clone(),
                            previous.on_screen,
                            previous.original_lang,
                            previous.trailing_spaces.clone(),
                        )
                    } else {
                        (source_keys.clone(), source_keys.len(), lang, separators)
                    };
                (
                    typed,
                    candidates,
                    candidate_langs,
                    0,
                    0,
                    erase,
                    original_lang,
                    trailing_spaces,
                )
            }
        };

        if P::preview_alternatives() && !accept {
            let original = reading::<P>(&typed, original_lang);
            let page = if index >= candidates.len() {
                resume_index
            } else {
                index / crate::complete::MAX_CANDIDATES * crate::complete::MAX_CANDIDATES
            };
            let mut choices: Vec<String> = candidates
                .iter()
                .enumerate()
                .skip(page)
                .take(crate::complete::MAX_CANDIDATES)
                .map(|(i, word)| format!("{}{}", if i == index { "• " } else { "" }, word))
                .collect();
            choices.push(format!(
                "{}{} (original)",
                if index >= candidates.len() {
                    "• "
                } else {
                    ""
                },
                original
            ));
            let generation = st.generation;
            st.cycle = Some(Cycle {
                preview: true,
                preview_ready: false,
                typed,
                trailing_spaces,
                candidates,
                candidate_langs,
                index,
                resume_index,
                on_screen: erase,
                lang: active_lang,
                original_lang,
                correction,
            });
            drop(st);
            P::show_preview(Arc::clone(self), generation, choices);
            return;
        }
        if accept && index >= candidates.len() {
            // Choosing the original is not an edit, undo, exception or learning event.
            if !st.keys.is_empty() {
                st.no_fix = true;
            }
            P::hide_preview();
            return;
        }

        // Past the end of the list is the user's own text, taken from the keys
        // they pressed rather than from a candidate — the odd irreproducible
        // capitalisation (`sHiFtY`) survives that way.
        let back_to_typed = index >= candidates.len();
        let target_lang = if back_to_typed {
            original_lang
        } else {
            candidate_langs[index]
        };
        let retype = if back_to_typed {
            P::retype_original(&typed, original_lang)
        } else {
            // An untypeable candidate is dropped rather than injected in half;
            // the cycle carries on to the next tap.
            match P::retype_completion(&candidates[index], target_lang) {
                Some(r) => r,
                None => {
                    st.cycle = switched;
                    return;
                }
            }
        };

        // The counter tracks words changed, not taps: cycling from one guess to
        // the next is still the one fix, and landing back on what the user
        // typed is none at all.
        let was = reading::<P>(&typed, original_lang);
        let commit = if back_to_typed {
            Some(Commit::Undo {
                suppress: None,
                layout: (active_lang != target_lang).then_some(target_lang),
                rule: None,
                learning_pair: None,
            })
        } else if accept || index == 0 {
            let first_page = candidates
                .iter()
                .take(crate::complete::MAX_CANDIDATES)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            Some(Commit::Fix {
                from: was.clone(),
                to: candidates[index].clone(),
                kind: FixKind::Complete,
                rule: None,
                details: if correction {
                    format!("Requested spelling alternatives. Choices in order: {first_page}. Automatic spelling protections remain unchanged.")
                } else {
                    format!("Requested completion. First page: {first_page}. {} choices remain reachable across pages.\nRanking balances frequency, a bounded letters-saved benefit, context, and prefix-edit penalties. Configured abbreviations take priority.", candidates.len())
                },
                deferred_layout: (active_lang != target_lang).then_some(target_lang),
            })
        } else if active_lang != target_lang {
            Some(Commit::Suggestion {
                layout: target_lang,
            })
        } else {
            None
        };

        // The buffer has to end up holding what is on screen, or the next Space
        // would check a word the user is no longer looking at.
        let visible = if back_to_typed {
            typed.clone()
        } else {
            P::buffer_after(&retype)
        };
        let keep = if trailing_spaces.is_empty() {
            visible.clone()
        } else {
            Vec::new()
        };
        // Cycling only changes the visible suffix; undo has its own original prefix.
        let target = if back_to_typed {
            &was
        } else {
            &candidates[index]
        };
        let prefix = current
            .chars()
            .zip(target.chars())
            .take_while(|(current, target)| current == target)
            .count();
        let undo_prefix = was
            .chars()
            .zip(target.chars())
            .take_while(|(original, target)| original == target)
            .count();
        let Some(mut suffix) = P::retype_completion(
            &target.chars().skip(prefix).collect::<String>(),
            target_lang,
        ) else {
            st.cycle = switched;
            return;
        };
        P::append_retype(
            &mut suffix,
            P::retype_original(&trailing_spaces, target_lang),
        );
        // A completion can be taken back with the undo gesture too — except
        // when it has just handed back the user's own text, which is nothing to
        // undo.
        let undo = (!back_to_typed).then(|| LastFix {
            expected: format!(
                "{}{}",
                target.chars().skip(undo_prefix).collect::<String>(),
                reading::<P>(
                    &trailing_spaces.iter().take(1).copied().collect::<Vec<_>>(),
                    target_lang
                )
            ),
            visible,
            visible_layout: target_lang,
            manual_conversion: false,
            on_screen: P::retype_len(&retype) - undo_prefix
                + usize::from(!trailing_spaces.is_empty()),
            restore: typed[undo_prefix..].to_vec(),
            restore_layout: original_lang,
            trailing_spaces: trailing_spaces.iter().skip(1).copied().collect(),
            terminator: trailing_spaces.first().map(|typed| typed.key),
            layout: (target_lang != original_lang).then_some(original_lang),
            keep: if trailing_spaces.is_empty() {
                typed.clone()
            } else {
                Vec::new()
            },
            learning_pair: if correction && self.en_dict.contains(&was.to_lowercase()) {
                None
            } else {
                completion_correction(&was, target, original_lang, target_lang)
            },
            suppress: undo_word(&was, original_lang),
            rule: None,
            suffix: reading::<P>(
                &trailing_spaces.iter().skip(1).copied().collect::<Vec<_>>(),
                target_lang,
            ),
        });

        if switched.is_some() {
            retry.finish_completion_on_commit = true;
            st.pending_learning = None;
        }
        retry.clear_phrase_on_commit |= !trailing_spaces.is_empty();
        st.cycle = Some(Cycle {
            preview: false,
            preview_ready: false,
            typed,
            trailing_spaces: trailing_spaces.clone(),
            candidates,
            candidate_langs,
            index,
            resume_index,
            on_screen: P::retype_len(&retype),
            lang: active_lang,
            original_lang,
            correction,
        });
        // The gesture types nothing. Replace the word suffix and replay only
        // the separators we already tracked at this cursor.
        self.start_replacement(
            st,
            Plan {
                expected: format!(
                    "{}{}",
                    current.chars().skip(prefix).collect::<String>(),
                    reading::<P>(&trailing_spaces, active_lang)
                ),
                erase: erase - prefix + trailing_spaces.len(),
                retype: suffix,
                terminator: None,
                verify: None,
            },
            keep,
            undo,
            commit,
            Some(retry),
        );
    }

    /// Undo normal corrections; only repeat conversions explicitly requested by the user.
    pub(super) fn double_tap_fixed(
        self: &Arc<Self>,
        st: MutexGuard<'_, AppState<P>>,
        fix: LastFix<P>,
        retry: RetryContext<P>,
    ) {
        if !fix.manual_conversion || !fix.trailing_spaces.is_empty() || !fix.suffix.is_empty() {
            self.undo_fix(st, fix, retry);
        } else {
            self.manual_correct(
                st,
                fix.visible,
                fix.terminator,
                fix.trailing_spaces,
                fix.visible_layout,
                retry,
            );
        }
    }

    /// Explicit intent overrides dictionary, frequency, and saved-word guards.
    pub(super) fn manual_correct(
        self: &Arc<Self>,
        mut st: MutexGuard<'_, AppState<P>>,
        keys: Vec<Typed<P::Key>>,
        terminator: Option<P::Key>,
        trailing_spaces: Vec<Typed<P::Key>>,
        layout: Language,
        mut retry: RetryContext<P>,
    ) {
        retry.layout = Some(layout);
        retry.clear_phrase_on_commit = true;
        retry.finish_completion_on_commit = true;
        st.cycle = None;
        st.pending_learning = None;
        let text = reading::<P>(&keys, layout.other());
        if keys.is_empty() || text.chars().count() != keys.len() {
            return;
        }
        let deferred_layout = Some(layout.other());
        let fix = Some(Fix::Layout {
            start: 0,
            text,
            lang: layout.other(),
        });
        let note = fix.as_ref().map(|fix| note_of::<P>(&keys, fix));
        let source = reading::<P>(&keys, layout);
        let Some(mut rep) = replacement::<P>(&keys, fix) else {
            return;
        };
        let mut undo = undo_of::<P>(&keys, &rep, terminator);
        // Asking for a conversion is not training a word exception.
        undo.suppress = None;
        undo.manual_conversion = true;
        if terminator.is_none() && trailing_spaces.is_empty() {
            undo.keep = keys;
        }
        undo.suffix = reading::<P>(&trailing_spaces, rep.target_layout);
        undo.trailing_spaces = trailing_spaces;
        let mut separators = Vec::with_capacity(undo.trailing_spaces.len() + 1);
        if let Some(key) = terminator {
            separators.push(Typed { key, shift: false });
        }
        separators.extend_from_slice(&undo.trailing_spaces);
        P::append_retype(
            &mut rep.retype,
            P::retype_original(&separators, rep.target_layout),
        );
        st.keys.clear();
        st.no_fix = separators.is_empty();
        self.start_replacement(
            st,
            Plan {
                expected: format!("{}{}", source.chars().skip(source.chars().count() - rep.erase).collect::<String>(), reading::<P>(&separators, layout)),
                erase: rep.erase + separators.len(),
                retype: rep.retype,
                terminator: None,
                verify: None,
            },
            Vec::new(),
            Some(undo),
            note.map(|(from, to, kind)| Commit::Fix {
                from,
                to,
                kind,
                rule: None,
                details: "Manual layout conversion requested by the shortcut. Dictionary and spelling confidence checks do not apply.".into(),
                deferred_layout,
            }),
            Some(retry),
        );
    }

    /// Put back what the user typed before the correction on screen replaced it.
    ///
    /// Everything the user typed after the correction landed is carried along
    /// verbatim: the correction is erased along with that suffix and the
    /// original text, the suffix included, is put back in its place. When
    /// the suffix contains more than spaces, the edit only happens if text before the
    /// caret still matches exactly — see the `verify` contract on
    /// [`super::Plan`].
    pub(super) fn undo_fix(
        self: &Arc<Self>,
        mut st: MutexGuard<'_, AppState<P>>,
        fix: LastFix<P>,
        mut retry: RetryContext<P>,
    ) {
        retry.layout = Some(fix.visible_layout);
        retry.clear_phrase_on_commit = true;
        retry.finish_completion_on_commit = true;
        st.cycle = None;
        let suffix = fix.suffix.clone();
        let suffix_chars = suffix.chars().count();
        // The suffix must be expressed verbatim — never approximated.
        let suffix_tail = if !suffix.is_empty() {
            match P::retype_completion(&suffix, fix.visible_layout) {
                Some(tail) => Some(tail),
                None => {
                    st.last_action = Some(LastAction::Fixed(fix));
                    return;
                }
            }
        } else {
            None
        };
        let mut restore = fix.restore;
        if let Some(key) = fix.terminator {
            restore.push(Typed { key, shift: false });
        }
        let erase = fix.on_screen + suffix_chars;
        let expected = format!("{}{}", fix.expected, suffix);
        let mut retype = P::retype_original(&restore, fix.restore_layout);
        if let Some(tail) = suffix_tail {
            P::append_retype(&mut retype, tail);
        }
        let extended = suffix.chars().any(|ch| ch != ' ');
        let verify = extended.then_some(fix.visible_layout);
        let keep = if extended {
            // The restored word/layout can change the meaning of tracked key positions.
            // A trailing Space already establishes a safe boundary for fresh typing.
            st.keys.clear();
            st.no_fix = !suffix.ends_with(' ');
            Vec::new()
        } else {
            fix.keep
        };
        self.start_replacement(
            st,
            Plan {
                expected,
                erase,
                retype,
                terminator: None,
                verify,
            },
            keep,
            // Undoing an undo would be a redo, which is a different gesture.
            None,
            Some(Commit::Undo {
                suppress: fix.suppress,
                layout: fix.layout,
                rule: fix.rule,
                learning_pair: fix.learning_pair,
            }),
            Some(retry),
        );
    }
}

/// What a [`Fix`] turns into for the injection thread.
pub(super) struct Replacement<P: Platform> {
    pub(super) text: String,
    /// How many of the characters the user typed have to be erased.
    pub(super) erase: usize,
    /// What to put in their place.
    pub(super) retype: P::Retype,
    /// The layout that was live before the fix, when the fix changed it — what
    /// undo has to switch back to.
    pub(super) previous_layout: Option<Language>,
    pub(super) target_layout: Language,
    /// Start of the original word for undo learning, before prefix trimming.
    pub(super) original_start: usize,
}

/// Turn a [`Fix`] into what the injection thread needs.
///
/// Anything before a layout fix's `start` is a previously-typed word that the
/// user concatenated by forgetting a space, and is left intact.
pub(super) fn replacement<P: Platform>(
    keys: &[Typed<P::Key>],
    fix: Option<Fix>,
) -> Option<Replacement<P>> {
    match fix? {
        Fix::Layout { start, text, lang } => Some(Replacement {
            erase: keys.len() - start,
            retype: P::retype_layout(&keys[start..], &text, lang)?,
            text,
            previous_layout: Some(lang.other()),
            target_layout: lang,
            original_start: start,
        }),
        Fix::Suffix {
            text,
            lang,
            previous_layout,
            ..
        } => Some(Replacement {
            erase: keys.len(),
            retype: P::retype_completion(&text, lang)?,
            text,
            previous_layout,
            target_layout: lang,
            original_start: 0,
        }),
        Fix::LayoutSpelling { text, lang } => Some(Replacement {
            erase: keys.len(),
            retype: P::retype_completion(&text, lang)?,
            text,
            previous_layout: Some(lang.other()),
            target_layout: lang,
            original_start: 0,
        }),
        Fix::Spelling { text, lang } => {
            // Keep the identical prefix on screen. This saves paced backspaces
            // and focus queries on macOS, for corrections and their undo alike.
            let original = reading::<P>(keys, lang);
            let prefix = original
                .chars()
                .zip(text.chars())
                .take_while(|(a, b)| a == b)
                .count();
            let suffix: String = text.chars().skip(prefix).collect();
            Some(Replacement {
                erase: keys.len() - prefix,
                retype: P::retype_completion(&suffix, lang)?,
                text: suffix,
                previous_layout: None,
                target_layout: lang,
                original_start: 0,
            })
        }
    }
}

/// Everything needed to take `rep` back again, built from the keys it is about
/// to replace.
pub(super) fn undo_of<P: Platform>(
    keys: &[Typed<P::Key>],
    rep: &Replacement<P>,
    terminator: Option<P::Key>,
) -> LastFix<P> {
    let original = &keys[keys.len() - rep.erase..];
    let was = rep.previous_layout.unwrap_or(rep.target_layout);
    let suffix = P::buffer_after(&rep.retype);
    let mut visible = if suffix.len() == P::retype_len(&rep.retype) {
        keys[..keys.len() - rep.erase].to_vec()
    } else {
        Vec::new()
    };
    visible.extend(suffix);
    LastFix {
        expected: format!(
            "{}{}",
            rep.text,
            reading::<P>(
                &terminator
                    .into_iter()
                    .map(|key| Typed { key, shift: false })
                    .collect::<Vec<_>>(),
                rep.target_layout
            )
        ),
        visible,
        manual_conversion: false,
        visible_layout: rep.target_layout,
        // Everything injected is one character per key, plus the terminator
        // that rides along after it.
        on_screen: P::retype_len(&rep.retype) + usize::from(terminator.is_some()),
        restore: original.to_vec(),
        restore_layout: was,
        trailing_spaces: Vec::new(),
        terminator,
        layout: rep.previous_layout,
        // The terminator has already finished this word, so nothing carries
        // over into the buffer.
        keep: Vec::new(),
        suppress: undo_word(&reading::<P>(&keys[rep.original_start..], was), was),
        rule: None,
        learning_pair: None,
        suffix: String::new(),
    }
}

/// The pair of words the recent-corrections history shows for `fix`, and which
/// pipeline produced it.
///
/// The "before" side is what was on screen, which is not the same reading in
/// each case: layout-based fixes have already switched to `lang`, so what the
/// user was looking at is the *other* layout's reading, while a spelling fix or
/// an expansion stays in the original layout.
pub(super) fn note_of<P: Platform>(keys: &[Typed<P::Key>], fix: &Fix) -> (String, String, FixKind) {
    match fix {
        Fix::Layout { start, text, lang } => (
            reading::<P>(&keys[*start..], lang.other()),
            text.clone(),
            FixKind::Layout,
        ),
        Fix::Suffix {
            text,
            lang,
            previous_layout,
            kind,
        } => (
            reading::<P>(keys, previous_layout.unwrap_or(*lang)),
            text.clone(),
            *kind,
        ),
        Fix::LayoutSpelling { text, lang } => (
            reading::<P>(keys, lang.other()),
            text.clone(),
            FixKind::LayoutSpelling,
        ),
        Fix::Spelling { text, lang } => {
            (reading::<P>(keys, *lang), text.clone(), FixKind::Spelling)
        }
    }
}
