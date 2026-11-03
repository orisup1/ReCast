//! Layout hotkey detection, native modifier snapshots, and switch confirmation.

use super::injection::Commit;
use super::{AppState, Engine, Platform};

impl<P: Platform> AppState<P> {
    /// Whether pressing `key` right now completes something shaped like a
    /// keyboard-layout hotkey.
    ///
    /// Every binding anyone actually uses is one of three shapes: a modifier on
    /// top of another modifier (Alt+Shift, Ctrl+Shift, Shift+Super), a modifier
    /// with Space (Super+Space, the GNOME default), or Caps Lock on its own.
    /// This does not — cannot — know what the session has bound, and does not
    /// need to: a false positive costs one layout query, and a false negative
    /// costs a wrongly-anchored correction.
    ///
    /// `key` is already in `held_keys` by the time this is asked, so the search
    /// for a *second* modifier has to skip it.
    pub(super) fn is_layout_hotkey(&self, key: P::Key) -> bool {
        if key == P::CAPS_LOCK {
            return true;
        }
        if !P::is_modifier(key) && !P::is_terminator(key) {
            return false;
        }
        self.held_keys
            .iter()
            .any(|&held| held != key && P::is_modifier(held))
    }
}

impl<P: Platform> Engine<P> {
    /// Synchronize a native lock-state snapshot, including startup and reconnect.
    pub fn caps_lock_changed(&self, on: bool) {
        let mut st = self.lock();
        if st.caps_lock == on {
            return;
        }
        st.caps_lock = on;
        st.revision = st.revision.wrapping_add(1);
        if st.is_replacing {
            st.invalidate_text();
        }
        crate::layout::invalidate();
    }

    /// Repair modifier transitions missed while native capture was unavailable.
    #[cfg(any(target_os = "macos", target_os = "windows", test))]
    pub fn sync_modifiers(&self, modifiers: impl IntoIterator<Item = (P::Key, bool)>) {
        let mut st = self.lock();
        let mut changed = false;
        for (key, down) in modifiers {
            if !P::is_modifier(key) || key == P::CAPS_LOCK {
                continue;
            }
            changed |= if down {
                st.held_keys.insert(key)
            } else {
                st.held_keys.remove(&key)
            };
        }
        if changed {
            st.invalidate_text();
            st.action_down = None;
            st.completion_down = None;
            st.layout_hotkey = false;
            st.last_key = None;
            drop(st);
            crate::layout::invalidate();
        }
    }

    /// Confirm switches outside the state lock so capture can still cancel them.
    pub(super) fn confirm_replacement_layout(
        &self,
        commit: Option<&Commit>,
        generation: u64,
    ) -> bool {
        // Layout confirmation can block. Keep it off the capture callback and
        // outside the state lock so releases and cancellation remain responsive.
        let switch = match commit {
            Some(Commit::Undo {
                layout: Some(lang), ..
            }) => Some((*lang, P::ABORT_UNDO_IF_LAYOUT_REFUSED)),
            Some(Commit::Fix {
                deferred_layout: Some(lang),
                ..
            }) => Some((*lang, true)),
            Some(Commit::Suggestion { layout }) => Some((*layout, true)),
            _ => None,
        };
        if let Some((lang, required)) = switch {
            let outcome = P::switch_layout_to(lang);
            if outcome.ready() {
                let mut st = self.lock();
                if st.generation == generation {
                    if let Some(cycle) = &mut st.cycle {
                        cycle.lang = lang;
                    }
                }
            }
            if (required && !outcome.ready()) || !self.replacement_valid(generation) {
                return false;
            }
        }
        true
    }
}
