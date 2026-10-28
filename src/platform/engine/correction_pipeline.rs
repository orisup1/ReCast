//! Automatic correction, explicit suggestions, and replacement planning.

use super::injection::{Commit, RetryContext};
use super::word_buffer::{hebrew_char, non_empty, MAX_UNDO_SPACES};
use super::{
    reading, AppState, Cycle, Engine, LastAction, LastFix, LastSkip, Plan, Platform, Typed,
};
use crate::dictionary::{
    all_completion_candidates, check_and_correct_in_context, correction_candidates,
    declined_by_list, Fix, Outcome, Run,
};
use crate::types::{FixKind, Language};
use std::sync::{Arc, MutexGuard};

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
        if st.keys.is_empty() {
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
        let outcome = self.check(&st.keys, st.history.run(), st.mode, &st.phrase);
        // Record what this word turned out to be before anything else happens
        // to it: the next word is decided with this one behind it. A word whose
        // language could not be told is not recorded at all — see
        // `dictionary::observed`.
        if let Some(lang) = outcome.lang {
            st.history.push(lang);
        }
        let details = format!("Reason: {}\n{}", outcome.reason, outcome.details);
        let result = outcome.fix;
        let rule = outcome.rule;
        // Describe the fix for the history before `replacement` consumes it.
        let note = result.as_ref().map(|fix| note_of::<P>(&st.keys, fix));
        if let Some(rep) = replacement::<P>(&st.keys, result) {
            let mut undo = undo_of::<P>(&st.keys, &rep, Some(key));
            undo.rule = rule;
            undo.learning_pair = note
                .as_ref()
                .map(|(from, to, _)| (from.clone(), to.clone()));
            // +1 for the terminator the user physically typed, which is erased
            // along with the word and pressed again afterwards.
            let erase = rep.erase + 1;
            let deferred_layout = rep.previous_layout.map(|_| rep.target_layout);
            let mut retry =
                RetryContext::capture(&st, Some(rep.previous_layout.unwrap_or(rep.target_layout)));
            retry.keys.clear();
            retry.last_action = Some(LastAction::Unchanged {
                keys: st.keys.to_vec(),
                terminator: Some(key),
                trailing_spaces: Vec::new(),
                layout: Some(rep.previous_layout.unwrap_or(rep.target_layout)),
            });
            st.keys.clear();
            self.start_replacement(
                st,
                Plan {
                    erase,
                    retype: rep.retype,
                    terminator: Some(key),
                },
                Vec::new(),
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
            if let Some(lang) = outcome.lang {
                crate::personal::record_word(&reading::<P>(&st.keys, lang));
            }
            // Phrase context describes the visible text, even for words whose
            // keystrokes happen to be valid in both languages (for example "my").
            if let Some(lang) = P::current_layout() {
                let word = reading::<P>(&st.keys, lang);
                st.retain_phrase_word(&word);
            } else {
                st.phrase.clear();
            }
        }
        if P::is_line_break(key) {
            st.phrase.clear();
        }

        if let Some(word) = (!st.practice)
            .then(|| {
                declined_by_list(
                    &st.keys,
                    |t: Typed<P::Key>| P::english_char(t.key, t.shift),
                    hebrew_char::<P>,
                    |t: Typed<P::Key>| t.shift,
                    P::current_layout(),
                )
            })
            .flatten()
        {
            // Nothing happened to this word, and the only reason is that the
            // user has it listed. Arm the gesture to change their mind about it.
            st.last_action = Some(LastAction::Skipped(LastSkip {
                keys: st.keys.to_vec(),
                terminator: Some(key),
                trailing_spaces: Vec::new(),
                layout: P::current_layout(),
                word,
            }));
        }
        if st.last_action.is_none() {
            st.last_action = Some(LastAction::Unchanged {
                keys: st.keys.to_vec(),
                terminator: Some(key),
                trailing_spaces: Vec::new(),
                layout: P::current_layout(),
            });
        }
        st.keys.clear();
    }

    pub(super) fn request_suggestion(
        self: &Arc<Self>,
        st: MutexGuard<'_, AppState<P>>,
        correction: bool,
    ) {
        if !self.control.is_enabled() {
            return;
        }
        let Some((mut st, focus)) = self.refresh_focus(st) else {
            return;
        };
        if (P::requires_focus() && st.focus.is_none()) || st.focus != focus || !P::input_allowed() {
            st.invalidate_text();
            return;
        }
        self.suggestion_tap(st, correction);
    }

    /// Completion and explicit correction share cycling and undo, but search
    /// separately so an exact completion cannot hide spelling alternatives.
    fn suggestion_tap(self: &Arc<Self>, mut st: MutexGuard<'_, AppState<P>>, correction: bool) {
        if st.mode != crate::config::AppMode::Full {
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
            Some(cycle) => cycle
                .candidates
                .get(cycle.index)
                .cloned()
                .unwrap_or_else(|| reading::<P>(&cycle.typed, cycle.original_lang)),
            None => reading::<P>(&source_keys, lang),
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
                let (next, resume) = if cycle.index >= cycle.candidates.len() {
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
                        (!st.practice).then_some(&st.phrase),
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
        } else if index == 0 {
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
        // Keep the prefix shared by the original, current offer, and next offer.
        // First completion usually appends only; cycling and undo touch suffixes.
        let target = if back_to_typed {
            &was
        } else {
            &candidates[index]
        };
        let prefix = was
            .chars()
            .zip(current.chars())
            .zip(target.chars())
            .take_while(|((original, current), target)| original == current && current == target)
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
            visible,
            visible_layout: target_lang,
            manual_conversion: false,
            on_screen: P::retype_len(&retype) - prefix + usize::from(!trailing_spaces.is_empty()),
            restore: typed[prefix..].to_vec(),
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
            suppress: non_empty(was.clone()),
            rule: None,
        });

        if switched.is_some() {
            retry.finish_completion_on_commit = true;
            st.pending_learning = None;
        }
        retry.clear_phrase_on_commit |= !trailing_spaces.is_empty();
        st.cycle = Some(Cycle {
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
                erase: erase - prefix + trailing_spaces.len(),
                retype: suffix,
                terminator: None,
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
        if !fix.manual_conversion || !fix.trailing_spaces.is_empty() {
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
                erase: rep.erase + separators.len(),
                retype: rep.retype,
                terminator: None,
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
        let mut restore = fix.restore;
        if let Some(key) = fix.terminator {
            restore.push(Typed { key, shift: false });
        }
        restore.extend(fix.trailing_spaces.iter().copied());
        self.start_replacement(
            st,
            Plan {
                erase: fix.on_screen + fix.trailing_spaces.len(),
                retype: P::retype_original(&restore, fix.restore_layout),
                terminator: None,
            },
            fix.keep,
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

    /// Take the word off the user's lists and run the pipelines over it again —
    /// the other half of the toggle, for a word that was passed over *because*
    /// it was listed.
    ///
    /// The correction is applied exactly as it would have been a moment ago: the
    /// terminator on screen is erased with the word and pressed again after it.
    /// No new undo is armed, because taking this one back would put the word
    /// straight onto the list the gesture just took it off.
    pub(super) fn unlist_and_correct(
        self: &Arc<Self>,
        mut st: MutexGuard<'_, AppState<P>>,
        skip: LastSkip<P>,
    ) {
        st.phrase.clear();
        crate::complete::unlist(&skip.word);
        // The run is read but not added to: this word was already recorded when
        // it was first finished, and the gesture is a second opinion about it
        // rather than a second word.
        let outcome = self.check(&skip.keys, st.history.run(), st.mode, &st.phrase);
        let details = format!("Reason: {}\n{}", outcome.reason, outcome.details);
        let result = outcome.fix;
        let note = result.as_ref().map(|fix| note_of::<P>(&skip.keys, fix));
        let Some(mut rep) = replacement::<P>(&skip.keys, result) else {
            return;
        };
        st.cycle = None;
        let mut separators = Vec::with_capacity(skip.trailing_spaces.len() + 1);
        if let Some(key) = skip.terminator {
            separators.push(Typed { key, shift: false });
        }
        separators.extend_from_slice(&skip.trailing_spaces);
        let erase = rep.erase + separators.len();
        P::append_retype(
            &mut rep.retype,
            P::retype_original(&separators, rep.target_layout),
        );
        let deferred_layout = rep.previous_layout.map(|_| rep.target_layout);
        self.start_replacement(
            st,
            Plan {
                erase,
                retype: rep.retype,
                terminator: None,
            },
            Vec::new(),
            None,
            note.map(|(from, to, kind)| Commit::Fix {
                from,
                to,
                kind,
                rule: None,
                details,
                deferred_layout,
            }),
            None,
        );
    }

    /// Run the pipelines over a finished word. `run` is the language of the
    /// words before it, which the caller reads off [`AppState::history`] while
    /// it still holds the lock.
    fn check(
        &self,
        keys: &[Typed<P::Key>],
        run: Run,
        mode: crate::config::AppMode,
        context: &crate::complete::PhraseContext,
    ) -> Outcome {
        check_and_correct_in_context(
            keys,
            |t: Typed<P::Key>| P::english_char(t.key, t.shift),
            hebrew_char::<P>,
            |t: Typed<P::Key>| t.shift,
            run,
            self.en_dict,
            self.he_dict,
            P::current_layout(),
            mode == crate::config::AppMode::LayoutOnly,
            Some(context),
            // Planning never waits for the compositor while holding typing state.
            // The replacement worker performs and confirms the requested switch.
            |_| crate::layout::LayoutSwitch::Switched,
        )
    }
}

/// What a [`Fix`] turns into for the injection thread.
pub(super) struct Replacement<P: Platform> {
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
            previous_layout,
            target_layout: lang,
            original_start: 0,
        }),
        Fix::LayoutSpelling { text, lang } => Some(Replacement {
            erase: keys.len(),
            retype: P::retype_completion(&text, lang)?,
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
        suppress: non_empty(reading::<P>(&keys[rep.original_start..], was)),
        rule: None,
        learning_pair: None,
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
