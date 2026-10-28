//! Bare modifier taps, double taps, and shortcut binding changes.

use super::injection::RetryContext;
use super::{AppState, Engine, LastAction, Platform};
use std::sync::{Arc, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

/// Longest a modifier press may last and still count as a *tap* rather than a hold.
/// Ctrl held down is the start of a shortcut; Ctrl let straight back up types
/// nothing and means nothing, which is what makes it usable as a gesture.
///
/// Shared rather than per-platform: this is a user-visible gesture window, and
/// three copies of it meant the gesture could come to feel different depending
/// on the OS for no reason anyone had decided on.
pub const TAP_MAX: Duration = Duration::from_millis(300);

/// Two action modifier taps inside this window request layout conversion. Wide enough not to
/// demand a drum roll, short enough that two unrelated taps a second apart are
/// not read as one gesture.
pub const DOUBLE_TAP_WINDOW: Duration = Duration::from_millis(500);

pub(super) fn shortcut_matches<P: Platform>(binding: &str, key: P::Key) -> bool {
    match binding {
        "ctrl" => key == P::CTRL_LEFT || key == P::CTRL_RIGHT,
        "left_ctrl" => key == P::CTRL_LEFT,
        "right_ctrl" => key == P::CTRL_RIGHT,
        "left_shift" => key == P::SHIFT_LEFT,
        "right_shift" => key == P::SHIFT_RIGHT,
        _ => false,
    }
}

impl<P: Platform> AppState<P> {
    pub(super) fn sync_shortcuts(&mut self, config: &crate::config::Config) {
        if !self
            .shortcut_bindings
            .as_ref()
            .is_some_and(|(action, completion, undo, correction)| {
                action == &config.action_shortcut
                    && completion == &config.completion_shortcut
                    && undo == &config.undo_shortcut
                    && correction == &config.correction_shortcut
            })
        {
            self.action_down = None;
            self.completion_down = None;
            self.last_action_tap = None;
            self.pending_undo = false;
            self.pending_conversion = false;
            self.pending_suggestion = None;
            self.shortcut_bindings = Some((
                config.action_shortcut.clone(),
                config.completion_shortcut.clone(),
                config.undo_shortcut.clone(),
                config.correction_shortcut.clone(),
            ));
        }
    }

    /// Undo and the completion cycle both describe the text sitting at the
    /// cursor right now. Anything that moves the text on makes both claims
    /// about something that is no longer there.
    pub(super) fn forget_gestures(&mut self) {
        self.last_action = None;
        self.cycle = None;
        self.last_action_tap = None;
        self.pending_undo = false;
        self.pending_conversion = false;
        self.pending_suggestion = None;
    }
}

impl<P: Platform> Engine<P> {
    /// A bare double tap undoes a correction or converts an unchanged word.
    /// The optional single-tap binding undoes corrections or unlists words.
    /// Both gestures require unchanged cursor and focus context.
    pub(super) fn action_tap(self: &Arc<Self>, mut st: MutexGuard<'_, AppState<P>>, key: P::Key) {
        let Some(down) = st.action_down.take() else {
            return;
        };
        // A held modifier is not a gesture.
        if down.elapsed() > TAP_MAX {
            st.last_action_tap = None;
            return;
        }
        let now = Instant::now();
        let config = crate::config::Config::global();
        let shortcut = &config.undo_shortcut;
        let single = ((shortcut == "left_ctrl" && key == P::CTRL_LEFT)
            || (shortcut == "right_ctrl" && key == P::CTRL_RIGHT))
            && ((st.is_replacing && !st.selection_replacing)
                || matches!(
                    st.last_action,
                    Some(LastAction::Fixed(_) | LastAction::Skipped(_))
                ));
        let double = shortcut_matches::<P>(&config.action_shortcut, key);
        if !double {
            st.last_action_tap = None;
            if single {
                self.perform_action(st, true);
            }
            return;
        }
        if st
            .last_action_tap
            .take()
            .is_some_and(|prev| now.duration_since(prev) <= DOUBLE_TAP_WINDOW)
        {
            self.perform_action(st, false);
            return;
        }
        st.last_action_tap = Some(now);
        if single {
            // A shared key must wait long enough to distinguish undo from conversion.
            let engine = Arc::clone(self);
            let bindings = st.shortcut_bindings.clone();
            thread::spawn(move || {
                thread::sleep(super::DOUBLE_TAP_WINDOW);
                let mut st = engine.lock();
                st.sync_shortcuts(&crate::config::Config::global());
                if st.last_action_tap == Some(now)
                    && st.shortcut_bindings == bindings
                    && st.held_keys.is_empty()
                {
                    st.last_action_tap = None;
                    engine.perform_action(st, true);
                }
            });
        }
    }

    pub(super) fn perform_action(
        self: &Arc<Self>,
        mut st: MutexGuard<'_, AppState<P>>,
        single: bool,
    ) {
        if !self.control.is_enabled() {
            return;
        }
        if st.is_replacing {
            st.pending_undo = P::QUEUE_UNDO_DURING_REPLACEMENT;
            st.pending_conversion = !single;
            return;
        }
        let Some((mut st, focus)) = self.refresh_focus(st) else {
            return;
        };
        if !single
            && st.keys.is_empty()
            && matches!(st.last_action, None | Some(LastAction::Selection { .. }))
        {
            if let Some(focus) = focus {
                if matches!(st.last_action, Some(LastAction::Selection { .. }))
                    && st.focus.as_ref() != Some(&focus)
                {
                    st.invalidate_text();
                    return;
                }
                st.focus = Some(focus);
                self.rescue_selection(st);
            }
            return;
        }
        if (P::requires_focus() && st.focus.is_none()) || st.focus != focus || !P::input_allowed() {
            st.invalidate_text();
            return;
        }
        let retry = RetryContext::capture(&st, None);
        match st.last_action.take() {
            Some(LastAction::Fixed(fix)) if !single => self.double_tap_fixed(st, fix, retry),
            Some(LastAction::Fixed(fix)) => self.undo_fix(st, fix, retry),
            Some(LastAction::Skipped(skip)) if !single => {
                if let Some(layout) = skip.layout.or_else(P::current_layout) {
                    self.manual_correct(
                        st,
                        skip.keys,
                        skip.terminator,
                        skip.trailing_spaces,
                        layout,
                        retry,
                    );
                } else {
                    st.last_action = retry.last_action;
                }
            }
            Some(LastAction::Skipped(skip)) => self.unlist_and_correct(st, skip),
            Some(LastAction::Unchanged {
                keys,
                terminator,
                trailing_spaces,
                layout,
            }) if !single => {
                if let Some(layout) = layout.or_else(P::current_layout) {
                    self.manual_correct(st, keys, terminator, trailing_spaces, layout, retry);
                } else {
                    st.last_action = retry.last_action;
                }
            }
            None if !single && !st.keys.is_empty() => {
                if let Some(layout) = P::current_layout() {
                    let keys = st.keys.to_vec();
                    self.manual_correct(st, keys, None, Vec::new(), layout, retry);
                }
            }
            _ => {}
        }
    }
}
