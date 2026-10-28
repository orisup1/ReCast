//! Shared listener state machine, generic over [`Platform`].
//!
//! Native adapters supply capture, focus, and injection through [`Platform`];
//! these modules own the behavior shared by Linux, macOS, and Windows:
//!
//! - [`event_handler`]: physical input, held keys, and focus validation.
//! - [`word_buffer`]: tracked text, undo state, and context invalidation.
//! - [`correction_pipeline`]: automatic correction, suggestions, and planning.
//! - [`gesture_detector`]: modifier taps and shortcut binding changes.
//! - [`injection`]: guarded replacement workers, replay, and retry context.
//! - [`layout_switching`]: layout hotkeys and deferred switch confirmation.
//! - [`platform`]: the native adapter contract and selection data.
//!
//! Public re-exports keep the native adapters independent of this organization.

use crate::dictionary::Dict;
use crate::types::{lock_forgiving, AppControl};
use std::sync::{Arc, Mutex, MutexGuard};

mod correction_pipeline;
mod event_handler;
mod gesture_detector;
mod injection;
mod layout_switching;
mod platform;
mod word_buffer;

pub use gesture_detector::{DOUBLE_TAP_WINDOW, TAP_MAX};
pub use injection::Plan;
pub use platform::{Platform, Selection, MAX_SELECTION_BYTES};
pub use word_buffer::{reading, AppState, Cycle, LastAction, LastFix, LastSkip, Typed};

/// Everything a listener needs, in one handle: the state, the dictionaries, the
/// on/off switch and whatever injection holds.
///
/// Held in an `Arc` and cloned into each injection thread, which is what
/// replaced three different ways of passing the same five things around — a
/// `TapContext` static on macOS, captured clones on Windows, six arguments per
/// function on Linux.
pub struct Engine<P: Platform> {
    pub state: Mutex<AppState<P>>,
    pub control: Arc<AppControl>,
    pub en_dict: Dict,
    pub he_dict: Dict,
    pub injector: P::Injector,
}

impl<P: Platform> Engine<P> {
    pub fn new(
        en_dict: Dict,
        he_dict: Dict,
        control: Arc<AppControl>,
        injector: P::Injector,
    ) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(AppState::new()),
            control,
            en_dict,
            he_dict,
            injector,
        })
    }

    fn lock(&self) -> MutexGuard<'_, AppState<P>> {
        lock_forgiving(&self.state)
    }
}

#[cfg(test)]
mod tests;
