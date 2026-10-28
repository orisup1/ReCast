//! Physical input capture, held-key tracking, and focus validation.

use super::gesture_detector::shortcut_matches;
use super::word_buffer::MAX_UNDO_SPACES;
use super::{AppState, Engine, LastAction, Platform, Typed, TAP_MAX};
#[cfg(target_os = "linux")]
use std::collections::HashSet;
use std::sync::{Arc, MutexGuard};
use std::time::{Instant, SystemTime};

type FocusSnapshot<'a, P> = (MutexGuard<'a, AppState<P>>, Option<<P as Platform>::Focus>);

impl<P: Platform> Engine<P> {
    pub(super) fn read_focus(&self) -> (Option<P::Focus>, Option<crate::config::AppMode>) {
        let focus = P::focus();
        let app = if !self.control.has_app_rules() {
            None
        } else {
            focus.as_ref().and_then(P::app_id)
        };
        let mode = P::input_allowed()
            .then(|| {
                self.control
                    .effective_app_mode(app.as_deref(), focus.as_ref().is_some_and(P::is_own_focus))
            })
            .flatten();
        (focus, mode)
    }

    /// OS queries must not prevent releases or mouse clicks from canceling work.
    /// If another event arrives meanwhile, abandon the stale event's text state.
    pub(super) fn refresh_focus<'a>(
        &'a self,
        st: MutexGuard<'a, AppState<P>>,
    ) -> Option<FocusSnapshot<'a, P>> {
        let revision = st.revision;
        drop(st);
        let (focus, mode) = self.read_focus();
        let mut st = self.lock();
        if st.revision != revision || matches!(mode, None | Some(crate::config::AppMode::Off)) {
            st.invalidate_text();
            return None;
        }
        let mode = mode.unwrap();
        if st.mode != mode {
            st.invalidate_text();
        }
        if st.focus != focus {
            // Foreground targets can have different layouts without a hotkey.
            crate::layout::invalidate();
            st.phrase.clear();
        }
        st.mode = mode;
        st.practice = self
            .control
            .practice_open
            .load(std::sync::atomic::Ordering::Relaxed)
            && focus.as_ref().is_some_and(P::is_own_focus);
        if st.practice || st.mode != crate::config::AppMode::Full {
            st.phrase.clear();
        }
        Some((st, focus))
    }

    /// Cancel pending work and discard text whose cursor is no longer known.
    pub fn forget_everything(&self) {
        let mut st = self.lock();
        st.invalidate_text();
        st.action_down = None;
        st.completion_down = None;
    }

    /// Keys already held when a device opens will not arrive as fresh presses.
    /// Seed physical state without inventing modifier taps or changing Caps Lock.
    #[cfg(target_os = "linux")]
    pub fn input_device_added(&self, device: usize, held: &HashSet<P::Key>) {
        let mut st = self.lock();
        st.invalidate_text();
        st.device_keys.insert(device, held.clone());
        st.held_keys.extend(held.iter().copied());
        st.action_down = None;
        st.completion_down = None;
        st.layout_hotkey = false;
        st.last_key = None;
        crate::layout::invalidate();
    }

    /// Unplugged evdev devices cannot deliver releases. Cancel stale work and
    /// remove their held keys without interpreting a release as an undo tap.
    #[cfg(target_os = "linux")]
    pub fn input_device_removed(&self, device: usize, held: &HashSet<P::Key>) {
        let mut st = self.lock();
        st.invalidate_text();
        let held = st
            .device_keys
            .remove(&device)
            .unwrap_or_else(|| held.clone());
        for key in held {
            if !st.device_keys.values().any(|keys| keys.contains(&key)) {
                st.held_keys.remove(&key);
            }
        }
        st.completion_down = None;
        st.action_down = None;
        st.layout_hotkey = false;
        st.last_key = None;
        crate::layout::invalidate();
    }

    #[cfg_attr(target_os = "linux", allow(dead_code))]
    pub fn mouse_click(&self) {
        self.lock().start_new_context();
    }

    /// A key went down.
    #[cfg(any(not(target_os = "linux"), test))]
    pub fn key_press(self: &Arc<Self>, key: P::Key) {
        self.press(key, false, SystemTime::now(), |_| {});
    }

    /// Preserve the kernel timestamp when queued evdev events are delivered.
    #[cfg(target_os = "linux")]
    pub fn key_press_at(self: &Arc<Self>, device: usize, key: P::Key, timestamp: SystemTime) {
        self.press(key, false, timestamp, |st| {
            st.device_keys.entry(device).or_default().insert(key);
        });
    }

    /// Kernel repeats can be delivered together after a slow desktop reply.
    /// Their delivery spacing must not be mistaken for duplicate device reports.
    #[cfg(target_os = "linux")]
    pub fn key_repeat_at(self: &Arc<Self>, device: usize, key: P::Key, timestamp: SystemTime) {
        self.press(key, true, timestamp, |st| {
            st.device_keys.entry(device).or_default().insert(key);
        });
    }

    fn press(
        self: &Arc<Self>,
        key: P::Key,
        repeat: bool,
        timestamp: SystemTime,
        track: impl FnOnce(&mut AppState<P>),
    ) {
        let config = crate::config::Config::global();
        let mut st = self.lock();
        track(&mut st);
        st.sync_shortcuts(&config);
        if !config.personal_enabled {
            st.phrase.forget_learning();
        }

        if st.duplicate_press(key, repeat, timestamp, P::DEDUP_WINDOW) {
            return;
        }
        st.revision = st.revision.wrapping_add(1);
        if self
            .control
            .word_rules_open
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            st.invalidate_text();
            return;
        }
        if P::is_mouse_button(key) {
            st.start_new_context();
            return;
        }

        // Check for chorded shortcut BEFORE inserting the key into held_keys:
        // if a non-modifier key is pressed while a modifier (Ctrl/Alt/Super,
        // not Shift) is already held, the user is invoking a shortcut that may
        // change the text (paste, select-all, undo, etc.). The buffer no longer
        // matches what's on screen, so suppress the next correction.
        let is_modifier_key = P::is_modifier(key);
        let is_shift = key == P::SHIFT_LEFT || key == P::SHIFT_RIGHT;
        let other_modifier_held = st.held_keys.iter().any(|&k| {
            P::is_modifier(k) && k != P::SHIFT_LEFT && k != P::SHIFT_RIGHT && k != P::CAPS_LOCK
        });
        let chorded_shortcut = !is_modifier_key && !is_shift && other_modifier_held;

        let fresh_press = st.held_keys.insert(key);
        // Noted on the way down, acted on when the modifiers come back up: that
        // is when the compositor has had the whole combination and the layout it
        // was asking for is live.
        st.layout_hotkey |= st.is_layout_hotkey(key);
        if key == P::CAPS_LOCK && fresh_press {
            st.caps_lock = !st.caps_lock;
            if st.is_replacing {
                st.invalidate_text();
                return;
            }
        }
        let is_action = shortcut_matches::<P>(&config.action_shortcut, key)
            || shortcut_matches::<P>(&config.undo_shortcut, key);
        let is_completion = shortcut_matches::<P>(&config.completion_shortcut, key);
        let is_correction = shortcut_matches::<P>(&config.correction_shortcut, key);
        let bare = fresh_press && st.held_keys.len() == 1;
        // A Shift tap may overlap the release of the prefix's final letter.
        // A new key pressed after Shift still cancels the gesture below.
        let suggestion_bare = bare
            || (fresh_press
                && is_shift
                && st.held_keys.iter().all(|&held| {
                    held == key || (!P::is_modifier(held) && P::english_char_plain(held).is_some())
                }));
        st.completion_down =
            ((is_completion || is_correction) && suggestion_bare).then(Instant::now);
        st.action_down = (is_action && bare).then(Instant::now);
        if !is_action {
            st.last_action_tap = None;
        }
        let preserve_spaces = P::is_terminator(key)
            && !P::is_line_break(key)
            && !chorded_shortcut
            && (st
                .last_action
                .as_mut()
                .and_then(LastAction::trailing_spaces_mut)
                .is_some_and(|spaces| spaces.len() < MAX_UNDO_SPACES)
                || (st.is_replacing
                    && st.buffered_keys.len() < MAX_UNDO_SPACES
                    && st
                        .buffered_keys
                        .iter()
                        .all(|typed| P::is_terminator(typed.key) && !P::is_line_break(typed.key))));
        if !is_action && !is_completion && !is_correction && !preserve_spaces {
            st.forget_gestures();
        } else if preserve_spaces {
            st.cycle = None;
        }
        let shift = if P::english_char_plain(key).is_some_and(|c| c.is_ascii_alphabetic()) {
            st.shift_active()
        } else {
            st.held_keys.contains(&P::SHIFT_LEFT) || st.held_keys.contains(&P::SHIFT_RIGHT)
        };
        if st.selection_replacing && !is_action {
            st.invalidate_text();
            return;
        }

        // A bare action modifier changes no text. Keep its tap while injecting so an
        // eager undo does not cancel a half-written correction. Chords still
        // cancel below, and any later text clears the queued undo.
        if P::QUEUE_UNDO_DURING_REPLACEMENT && st.is_replacing && is_action && bare {
            return;
        }
        if st.is_replacing
            && (is_completion || is_correction)
            && suggestion_bare
            && (is_shift || P::QUEUE_UNDO_DURING_REPLACEMENT)
        {
            return;
        }

        // Command modifiers can turn injected Backspace into a destructive
        // shortcut before another physical key arrives. Preserve the safe
        // queued gestures above, but cancel all other command-modifier presses.
        if st.is_replacing && is_modifier_key && !is_shift {
            st.invalidate_text();
            return;
        }

        // Enter may submit or focus another field before a worker can edit it.
        if P::is_line_break(key) && st.is_replacing {
            st.invalidate_text();
            st.no_fix = false;
            return;
        }

        // Cancel before any OS query: the worker must see a shortcut/deletion
        // immediately, even while the compositor is slow to answer focus.
        let during_injection = P::injecting_flag(&self.injector)
            .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed));
        if key == P::BACKSPACE && (during_injection || st.is_replacing || chorded_shortcut) {
            st.invalidate_text();
            return;
        }
        if during_injection
            || chorded_shortcut
            || P::is_reset(key)
            || (st.is_replacing && self.control.has_app_rules())
        {
            // A modifier+Space layout shortcut at an already known boundary
            // changes no captured word. Cancel old actions without suppressing
            // the first word typed under the newly selected layout.
            if chorded_shortcut
                && P::is_terminator(key)
                && !P::is_line_break(key)
                && st.keys.is_empty()
                && !st.no_fix
                && !st.is_replacing
                && !during_injection
            {
                st.start_new_context();
            } else {
                st.invalidate_text();
            }
            return;
        }

        let is_text = P::english_char_plain(key).is_some() || P::hebrew_char(key).is_some();
        // Unmapped keys can insert text (for example, keypad digits) or invoke
        // application commands. Their effects cannot be counted or replayed,
        // so cancel pending edits before querying focus and await a boundary.
        if !is_text && !is_modifier_key && key != P::BACKSPACE && !P::is_terminator(key) {
            st.invalidate_text();
            return;
        }
        let needs_focus = self.control.has_app_rules()
            || self
                .control
                .practice_open
                .load(std::sync::atomic::Ordering::Relaxed)
            || key == P::BACKSPACE
            || preserve_spaces
            || (st.pending_learning.is_some() && !is_modifier_key)
            || (P::is_terminator(key) && !st.keys.is_empty() && !st.is_replacing)
            || (is_text && st.keys.is_empty() && !st.is_replacing);
        let focus = if needs_focus {
            let Some((next, focus)) = self.refresh_focus(st) else {
                return;
            };
            st = next;
            focus
        } else {
            None
        };

        if key == P::BACKSPACE {
            st.finish_completion_use(false);
            st.phrase.clear();
            st.pending_learning = None;
        } else if !st.is_replacing && (is_text || P::is_terminator(key)) {
            if let Some(pending) = st.pending_learning.take() {
                if let Some(from) = &pending.from {
                    if !st.practice
                        && st.mode == crate::config::AppMode::Full
                        && self.control.is_enabled()
                        && P::input_allowed()
                        && st.focus == focus
                    {
                        crate::personal::record_confusion(from, &pending.to);
                        crate::personal::record_word(&pending.to);
                        if pending.phrase_boundary {
                            st.phrase.clear();
                        } else {
                            st.retain_phrase_word(&pending.to);
                        }
                    }
                } else if st.keys.is_empty() {
                    // A suggestion made after Space was already a finished word.
                    // Advancing to the next word accepts it without another Space.
                    if !st.practice
                        && st.mode == crate::config::AppMode::Full
                        && self.control.is_enabled()
                        && P::input_allowed()
                        && st.focus == focus
                    {
                        crate::personal::record_word(&pending.to);
                        if let Some((from, to)) = &pending.manual_pair {
                            crate::personal::record_explicit_confusion(from, to);
                        }
                        st.finish_completion_use(true);
                        st.retain_phrase_word(&pending.to);
                    }
                } else if P::is_terminator(key) {
                    st.pending_learning = Some(pending);
                } else {
                    st.finish_completion_use(false);
                }
                // Continuing a completion is not acceptance of the offered word.
            }
        }

        // Record key press for typing pattern analysis (dwell, digraphs).
        // Use Debug representation as a stable-ish key name.
        if !st.practice && st.mode == crate::config::AppMode::Full && config.personal_enabled {
            crate::personal::record_key_press(&format!("{key:?}"));
        }

        if key == P::BACKSPACE {
            if st.focus != focus {
                st.invalidate_text();
            } else if !st.keys.is_empty() {
                st.keys.pop();
                if st.keys.is_empty() && !st.no_fix {
                    st.deletion_tail = Some(Vec::new());
                }
            } else if let Some(mut tail) = st.deletion_tail.take() {
                tail.pop();
                // Re-enter the known word only after its separators are gone.
                // Until then the current word is still empty.
                if tail
                    .last()
                    .is_some_and(|typed| !P::is_terminator(typed.key))
                {
                    let start = tail
                        .iter()
                        .rposition(|typed| P::is_terminator(typed.key))
                        .map_or(0, |index| index + 1);
                    st.keys.replace_with(tail.drain(start..));
                    st.deletion_tail = None;
                } else {
                    st.deletion_tail = Some(tail);
                }
                st.no_fix = false;
            } else {
                // A cursor move, shortcut, or unknown prefix still requires a boundary.
                st.invalidate_text();
            }
        } else if P::is_terminator(key) {
            self.word_finished(st, key, shift, focus);
        } else if is_text {
            st.deletion_tail = None;
            if st.keys.is_empty() && !st.is_replacing {
                // A new target starts a new capture. Keep this separate from
                // action focus checks: undo must still reject the old target.
                if st.focus.is_some() && focus.is_some() && st.focus != focus {
                    st.start_new_context();
                }
                st.focus = focus;
                let revision = st.revision;
                let suppressed = st.no_fix;
                let practice = st.practice;
                drop(st);
                // With exclusions, resume at a word boundary without asking
                // a newly focused (possibly excluded) field for its value.
                let empty = suppressed
                    && (!self.control.has_app_rules() || practice)
                    && P::input_empty(&self.injector);
                st = self.lock();
                if st.revision != revision {
                    st.invalidate_text();
                    return;
                }
                if empty {
                    st.no_fix = false;
                }
            }
            st.push_key(Typed { key, shift });
            if st.is_replacing && st.buffered_keys.is_empty() {
                // An overflowing replay buffer cannot describe the visible suffix.
                st.invalidate_text();
                return;
            }
            if !st.is_replacing && st.keys.is_empty() {
                // The word buffer gave up on an overlong token.
                st.no_fix = true;
            }
        }
    }

    /// A key came back up.
    ///
    /// Track held keys and configured modifier taps. Only a short, bare
    /// press/release pair counts; chords and holds retain their normal meaning.
    #[cfg(any(not(target_os = "linux"), test))]
    pub fn key_release(self: &Arc<Self>, key: P::Key) {
        self.release(key, |_| true);
    }

    /// A physical key stays down until every device holding it releases it.
    #[cfg(target_os = "linux")]
    pub fn key_release_from(self: &Arc<Self>, device: usize, key: P::Key) {
        self.release(key, |st| {
            if let Some(keys) = st.device_keys.get_mut(&device) {
                keys.remove(&key);
            }
            !st.device_keys.values().any(|keys| keys.contains(&key))
        });
    }

    fn release(self: &Arc<Self>, key: P::Key, track: impl FnOnce(&mut AppState<P>) -> bool) {
        let config = crate::config::Config::global();
        let mut st = self.lock();
        if !track(&mut st) {
            return;
        }
        st.sync_shortcuts(&config);
        st.revision = st.revision.wrapping_add(1);
        st.held_keys.remove(&key);
        if self
            .control
            .word_rules_open
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            st.invalidate_text();
            return;
        }

        // A layout hotkey has been let go of. Drop the cached layout rather than
        // let the next word be anchored on what was true before it — the 300 ms
        // TTL is otherwise long enough to decide a word or two against the wrong
        // layout and inject the result under the right one.
        if st.layout_hotkey && P::is_modifier(key) {
            st.layout_hotkey = false;
            crate::layout::invalidate();
        }

        // Ordinary releases have no text action. Tracking the held key above
        // is enough unless personalization needs its timing; an accessibility
        // round trip here only delays the next key and any queued undo tap.
        let is_action = shortcut_matches::<P>(&config.action_shortcut, key)
            || shortcut_matches::<P>(&config.undo_shortcut, key);
        let is_completion = shortcut_matches::<P>(&config.completion_shortcut, key);
        let is_correction = shortcut_matches::<P>(&config.correction_shortcut, key);
        if !is_action && !is_completion && !is_correction && !config.personal_enabled {
            return;
        }

        if self.control.has_app_rules()
            || self
                .control
                .practice_open
                .load(std::sync::atomic::Ordering::Relaxed)
        {
            let Some((next, _)) = self.refresh_focus(st) else {
                return;
            };
            st = next;
        }
        if !st.practice && st.mode == crate::config::AppMode::Full && config.personal_enabled {
            crate::personal::record_key_release(&format!("{key:?}"));
        }

        if is_action {
            self.action_tap(st, key);
            return;
        }
        if !(is_completion || is_correction)
            || st
                .completion_down
                .take()
                .is_none_or(|down| down.elapsed() > TAP_MAX)
        {
            return;
        }
        if !self.control.is_enabled() {
            return;
        }
        if st.is_replacing {
            if !st.selection_replacing
                && st.cycle.is_some()
                && (key == P::SHIFT_LEFT
                    || key == P::SHIFT_RIGHT
                    || P::QUEUE_UNDO_DURING_REPLACEMENT)
            {
                st.pending_suggestion = Some(is_correction);
            }
            return;
        }
        self.request_suggestion(st, is_correction);
    }
}
