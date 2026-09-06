//! The listener state machine, written once.
//!
//! Capture and injection are genuinely different on the three platforms —
//! `evdev` keycodes replayed through `uinput`, an `rdev` key stream with text
//! inserted by `CGEventKeyboardSetUnicodeString`, the same stream with text
//! inserted by `SendInput`. Everything *between* those two ends is not
//! different, and used to be written out three times anyway: `Typed`,
//! `AppState`, `LastAction`, `LastFix`, `LastSkip`, `Cycle`, `shift_active`,
//! `replacement`, `undo_of`, `reading`, `note_of`, `handle_ctrl_tap`,
//! `undo_fix`, `unlist_and_correct` and the body of `replace_word` all existed
//! per platform, near-identical, with nothing keeping them in step. A fix
//! landed in one and forgotten in the other two was the most likely regression
//! in this codebase.
//!
//! So the state machine lives here, generic over [`Platform`], and each OS
//! module supplies only what is actually its own: how a key is classified, how
//! text is turned into something injectable, and how to inject it. What is left
//! per platform is roughly a quarter of what was there, and every line of it is
//! a line that could not have been shared.
//!
//! # What a platform still decides
//!
//! * Its key type ([`Platform::Key`]) and how the interesting keys are spelled
//!   in it.
//! * What a replacement *is* ([`Platform::Retype`]): key positions to replay on
//!   Linux, because `uinput` has no way to inject text; a finished string on
//!   macOS and Windows, because they do and the result then does not depend on
//!   the layout switch having propagated.
//! * How to put it on screen ([`Platform::inject`]).
//! * Two deliberate divergences, spelled out as associated constants rather
//!   than left implicit: [`Platform::DEDUP_WINDOW`] and
//!   [`Platform::ABORT_UNDO_IF_LAYOUT_REFUSED`].

use std::collections::HashSet;
use std::hash::Hash;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use crate::dictionary::{
    check_and_correct, complete_candidates, declined_by_list, Dict, Fix, History, Outcome, Run,
};
use crate::types::{
    lock_forgiving, AppControl, FixKind, Language, ReplaceGuard, Replaceable, WordBuffer,
};

/// Longest a Ctrl press may last and still count as a *tap* rather than a hold.
/// Ctrl held down is the start of a shortcut; Ctrl let straight back up types
/// nothing and means nothing, which is what makes it usable as a gesture.
///
/// Shared rather than per-platform: this is a user-visible gesture window, and
/// three copies of it meant the gesture could come to feel different depending
/// on the OS for no reason anyone had decided on.
pub const TAP_MAX: Duration = Duration::from_millis(300);

/// Two Ctrl taps inside this window are the undo gesture. Wide enough not to
/// demand a drum roll, short enough that two unrelated taps a second apart are
/// not read as one gesture.
pub const DOUBLE_TAP_WINDOW: Duration = Duration::from_millis(500);

// ─────────────────────────────────────────────────────────────────────────────
// What a platform supplies
// ─────────────────────────────────────────────────────────────────────────────

/// Everything the shared state machine needs an OS to answer.
pub trait Platform: Sized + Send + Sync + 'static {
    /// The key type this platform's capture backend produces —
    /// `evdev::KeyCode` or `rdev::Key`.
    type Key: Copy + Eq + Hash + Send + Sync + std::fmt::Debug + 'static;

    /// What goes on screen in place of what was erased.
    ///
    /// Key positions on Linux (`uinput` speaks keycodes, so a replacement has
    /// to be spelled out as presses) and a finished string on macOS and
    /// Windows. This is the single difference the rest of the module is
    /// generic over.
    type Retype: Send + 'static;

    /// Whatever injection needs a handle to: the `uinput` device on Linux, the
    /// re-entry gate on macOS and Windows.
    type Injector: Send + Sync + 'static;
    type Focus: PartialEq + Send + Sync + 'static;

    /// Stable identity of the focused target, without reading its text.
    /// None where the desktop cannot expose focus; input events still cancel.
    fn focus() -> Option<Self::Focus>;
    fn current_layout() -> Option<Language> {
        crate::layout::current_layout()
    }
    fn switch_layout_to(lang: Language) -> crate::layout::LayoutSwitch {
        crate::layout::switch_layout_to(lang)
    }
    const REQUIRES_FOCUS: bool = false;
    fn input_allowed() -> bool {
        true
    }
    /// Confirm an empty text field; unavailable context must remain suppressed.
    fn input_empty(_: &Self::Injector) -> bool {
        false
    }

    // ── the keys the state machine names ────────────────────────────────────

    const SHIFT_LEFT: Self::Key;
    const SHIFT_RIGHT: Self::Key;
    const CTRL_LEFT: Self::Key;
    const CTRL_RIGHT: Self::Key;
    const CAPS_LOCK: Self::Key;
    const BACKSPACE: Self::Key;

    /// Space or Enter — the keys that finish a word and ask for it to be
    /// checked.
    fn is_terminator(key: Self::Key) -> bool;

    /// Cursor and focus keys, and (on Linux, where they arrive as keys) mouse
    /// buttons. They end the current word without checking it, so a stale
    /// buffer cannot leak into the next one.
    fn is_reset(key: Self::Key) -> bool;

    /// Whether this key is a modifier: shift, control, alt, super, caps lock.
    ///
    /// Used for one thing — spotting the *shape* of a keyboard-layout hotkey, so
    /// the cached layout can be dropped before it goes stale (see
    /// [`crate::layout::invalidate`]). Every combination anyone binds to a
    /// layout switch is either two modifiers or a modifier and space, and
    /// naming the modifiers is the whole of what the engine needs to recognise
    /// that.
    fn is_modifier(key: Self::Key) -> bool;

    // ── characters ──────────────────────────────────────────────────────────

    /// The English character this key types with `shift` in the state it was
    /// typed in. Letters come back **lowercase** whatever the shift — the
    /// dictionaries are lowercase and the capitalization is tracked separately
    /// — but a symbol key gives its shifted form, or `!` reads as `1`.
    fn english_char(key: Self::Key, shift: bool) -> Option<char>;

    /// The English character this key types with no shift at all. Only used to
    /// decide whether a key belongs in the word buffer.
    fn english_char_plain(key: Self::Key) -> Option<char>;

    /// The Hebrew character this key types. Hebrew has no case, and shift there
    /// types punctuation, so there is no shifted variant.
    fn hebrew_char(key: Self::Key) -> Option<char>;

    // ── building a replacement ──────────────────────────────────────────────

    /// Reproduce exactly what the user typed, as something injectable.
    ///
    /// Linux replays their own key presses, which is what makes an
    /// irreproducible capitalisation (`sHiFtY`) survive; `lang` gates the
    /// shifts, because a Hebrew target has no capitals and shift there types
    /// punctuation. macOS and Windows build the string [`reading`] gives.
    fn retype_original(keys: &[Typed<Self::Key>], lang: Language) -> Self::Retype;

    /// A layout fix: the same keys under the other layout. `keys` is the run
    /// being replaced and `text` is what it spells there — Linux uses the
    /// former (the layout has already changed, so replaying them produces that
    /// text) and macOS and Windows the latter.
    fn retype_layout(keys: &[Typed<Self::Key>], text: &str, lang: Language)
        -> Option<Self::Retype>;

    /// Arbitrary text — a spelling correction, an expansion, a completion.
    /// `None` when this platform cannot type some character of it, which is a
    /// reason to drop the fix rather than inject half a word.
    fn retype_text(text: &str) -> Option<Self::Retype>;

    /// How many characters this will put on screen. Every key produces exactly
    /// one character, which is what lets an erase count be a key count.
    fn retype_len(retype: &Self::Retype) -> usize;

    /// The word buffer to leave behind once this is on screen: the last word of
    /// it, since an abbreviation expansion may carry spaces and only the tail
    /// is still in progress.
    fn buffer_after(retype: &Self::Retype) -> Vec<Typed<Self::Key>>;

    // ── injection ───────────────────────────────────────────────────────────

    /// Put the replacement on screen: wait for whatever has to be released,
    /// erase `plan.erase` characters, type the replacement, press the
    /// terminator if there is one, and replay any keys the user got in
    /// meanwhile.
    ///
    /// Returns those replayed keys, which the shared caller needs: keys typed
    /// during a replacement have moved the cursor on, and undo erases backwards
    /// from the cursor.
    fn inject(
        engine: &Engine<Self>,
        plan: Plan<Self>,
        generation: u64,
    ) -> Option<Vec<Typed<Self::Key>>>;

    /// The re-entry gate, when this platform has one. macOS and Windows filter
    /// their own injected events with an atomic flag; Linux filters by device
    /// name instead and has nothing here.
    fn injecting_flag(injector: &Self::Injector) -> Option<&std::sync::atomic::AtomicBool>;

    // ── the two deliberate divergences ──────────────────────────────────────

    /// How long the same key repeating counts as one physical press.
    ///
    /// `Some` on Linux only, where one press arrives on several evdev nodes.
    /// macOS and Windows have a single event stream and need no such guard —
    /// applying one there would swallow genuine auto-repeat.
    const DEDUP_WINDOW: Option<Duration> = None;

    /// Whether a refused layout switch abandons an undo.
    ///
    /// True on Linux: `uinput` speaks keycodes, so what a replayed key spells
    /// depends on the layout being live when it lands, and replaying into the
    /// old layout would just re-enter the correction. False on macOS and
    /// Windows, which put the restored text back as *text* — layout-independent
    /// — and switch only so the user's next keystroke is in the right language.
    const ABORT_UNDO_IF_LAYOUT_REFUSED: bool = false;
}

// ─────────────────────────────────────────────────────────────────────────────
// Shared state
// ─────────────────────────────────────────────────────────────────────────────

/// One key of the word being typed, with the shift state it was typed under.
/// The buffer holds key *positions*, which carry no case of their own, so the
/// shift has to be recorded here or the capitalization is lost by the time a
/// correction is typed back.
#[derive(Clone, Copy, Debug)]
pub struct Typed<K> {
    pub key: K,
    pub shift: bool,
}

/// What the Ctrl double-tap would do to the word the cursor is sitting on.
///
/// The gesture is a *toggle* over the user's lists, which is why both cases
/// live behind one field: a word has either just been corrected (so the gesture
/// takes the correction back and retires the word) or just been left alone
/// because it is already retired (so the gesture un-retires it and corrects it
/// after all). Never both, and never anything else — a word that is simply
/// spelled right does not arm it.
pub enum LastAction<P: Platform> {
    /// A correction landed and the cursor is still on it.
    Fixed(LastFix<P>),
    /// A word was passed over only because it is on one of the user's lists.
    Skipped(LastSkip<P>),
}

/// A correction that is on screen right now, with the cursor still sitting
/// immediately after it — everything the Ctrl double-tap needs to put back what
/// the user actually typed.
///
/// It is only kept for that moment. Undo erases backwards from the cursor, so
/// once the user types anything else the correction is no longer what sits
/// there and the payload is dropped (see [`Engine::key_press`]).
pub struct LastFix<P: Platform> {
    /// Characters our injection put on screen, terminator included — what has
    /// to come back off.
    on_screen: usize,
    /// What was there before, ready to go back: the user's own keys on Linux,
    /// the text they spelled elsewhere.
    restore: P::Retype,
    /// Terminator to press again afterwards; `None` for a completion, which
    /// interrupted a word rather than finishing one.
    terminator: Option<P::Key>,
    /// Layout to switch back to, when the correction was the one that changed
    /// it. Restoring the letters without restoring the layout would leave the
    /// user typing the wrong language into the word they just rescued.
    layout: Option<Language>,
    /// Word buffer to leave behind: a completion's original prefix, empty for a
    /// word the terminator already finished.
    keep: Vec<Typed<P::Key>>,
    /// The reading to stop correcting for the rest of the session. Undo that
    /// only rewrote the screen would be undone again by the next repetition of
    /// the same word (see `complete::suppress`).
    suppress: Option<String>,
}

/// A word the pipelines passed over because the user had already told us to
/// leave it alone — what the Ctrl double-tap needs to change its mind.
///
/// The keys are kept rather than the decision, because there is no decision
/// yet: the word was never put through the pipelines with the list out of the
/// way. The gesture takes it off the list and runs them then.
pub struct LastSkip<P: Platform> {
    /// The word as typed, to run the pipelines over once it is off the list.
    keys: Vec<Typed<P::Key>>,
    /// The terminator already on screen after it, erased with the word and
    /// pressed again afterwards exactly as on the normal path.
    terminator: Option<P::Key>,
    /// The reading that is on the list.
    word: String,
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
    typed: Vec<Typed<P::Key>>,
    candidates: Vec<String>,
    index: usize,
    /// Characters the current offer put on screen, to erase for the next one.
    on_screen: usize,
}

/// Listener state, shared by every capture thread of a platform.
pub struct AppState<P: Platform> {
    pub keys: WordBuffer<Typed<P::Key>>,
    pub is_replacing: bool,
    pub buffered_keys: WordBuffer<Typed<P::Key>>,
    /// Physical keys currently held down. Tracked from press/release events so
    /// injection can wait for the user to lift the keys it is about to retype —
    /// otherwise the OS sees the synthetic press as a duplicate of the
    /// still-held physical key and drops it.
    pub held_keys: HashSet<P::Key>,
    /// Caps Lock latch. Together with the held shifts it is what decides
    /// whether a letter came out capitalized.
    pub caps_lock: bool,
    /// Right Shift went down and nothing else has been pressed since — so if it
    /// comes back up untouched, it was a tap, which is the completion request.
    pub right_shift_tap: bool,
    /// When a Ctrl key went down with nothing pressed since. `None` once
    /// another key joins it, because that makes it a shortcut rather than a
    /// tap.
    pub ctrl_down: Option<Instant>,
    /// When the last completed Ctrl tap happened; a second one inside
    /// [`DOUBLE_TAP_WINDOW`] is the undo gesture.
    pub last_ctrl_tap: Option<Instant>,
    /// A key combination shaped like a layout-switch hotkey has been pressed and
    /// the modifiers holding it have not all come back up yet. When they do, the
    /// cached layout is dropped — see [`crate::layout::invalidate`].
    pub layout_hotkey: bool,
    /// What the Ctrl double-tap would do to the word the cursor is sitting on,
    /// if it would do anything.
    pub last_action: Option<LastAction<P>>,
    /// The completion cycle in progress, if the user is tapping through guesses.
    pub cycle: Option<Cycle<P>>,
    /// What language the last few finished words were in — the context every
    /// ambiguous decision is missing when it looks at one word on its own.
    ///
    /// Kept here because this is the only place that knows when a word is
    /// finished, and cleared alongside the gestures whenever the cursor moves
    /// somewhere else: the run is a claim about the text being written *here*.
    pub history: History,
    /// The last key seen and when — the multi-node deduplication guard. Only
    /// read where [`Platform::DEDUP_WINDOW`] is set, which is Linux.
    last_key: Option<P::Key>,
    last_key_at: Instant,
    /// Incremented whenever the cursor or text can no longer be trusted.
    generation: u64,
    focus: Option<P::Focus>,
    no_fix: bool,
}

impl<P: Platform> AppState<P> {
    fn new() -> Self {
        Self {
            keys: WordBuffer::new(),
            is_replacing: false,
            buffered_keys: WordBuffer::new(),
            held_keys: HashSet::new(),
            caps_lock: false,
            right_shift_tap: false,
            ctrl_down: None,
            last_ctrl_tap: None,
            layout_hotkey: false,
            last_action: None,
            cycle: None,
            history: History::default(),
            last_key: None,
            last_key_at: Instant::now(),
            generation: 0,
            focus: None,
            no_fix: false,
        }
    }

    /// Whether a letter pressed right now would come out capitalized.
    fn shift_active(&self) -> bool {
        let held =
            self.held_keys.contains(&P::SHIFT_LEFT) || self.held_keys.contains(&P::SHIFT_RIGHT);
        held != self.caps_lock
    }

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
    fn is_layout_hotkey(&self, key: P::Key) -> bool {
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

    /// Add a key to whichever buffer is live: the word being typed, or the one
    /// holding what the user got in while a correction was landing.
    fn push_key(&mut self, typed: Typed<P::Key>) {
        if self.is_replacing {
            self.buffered_keys.push(typed);
        } else {
            self.keys.push(typed);
        }
    }

    /// Undo and the completion cycle both describe the text sitting at the
    /// cursor right now. Anything that moves the text on makes both claims
    /// about something that is no longer there.
    fn forget_gestures(&mut self) {
        self.last_action = None;
        self.cycle = None;
        self.last_ctrl_tap = None;
    }
    fn invalidate_text(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.keys.clear();
        self.buffered_keys.clear();
        self.forget_gestures();
        self.history.clear();
        self.no_fix = true;
    }
}

impl<P: Platform> Replaceable for AppState<P> {
    fn set_replacing(&mut self, replacing: bool) {
        self.is_replacing = replacing;
    }
    fn clear_buffered(&mut self) {
        self.buffered_keys.clear();
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The engine
// ─────────────────────────────────────────────────────────────────────────────

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

    // ── things the platform's injection asks the engine ─────────────────────

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

    /// Keys the user managed to type while a correction was landing. Read while
    /// holding the lock and returned by value: the injected keystrokes re-enter
    /// the listener, which needs the same lock, so holding it across an
    /// injection is a deadlock.
    pub fn buffered(&self) -> Vec<Typed<P::Key>> {
        self.lock().buffered_keys.to_vec()
    }

    /// Cancel pending work and discard text whose cursor is no longer known.
    pub fn forget_everything(&self) {
        self.lock().invalidate_text();
    }

    #[cfg_attr(target_os = "linux", allow(dead_code))]
    pub fn mouse_click(&self) {
        self.forget_everything();
    }

    #[cfg(target_os = "macos")]
    pub fn caps_lock_changed(&self, on: bool) {
        let mut st = self.lock();
        st.caps_lock = on;
        if st.is_replacing {
            st.invalidate_text();
        }
        crate::layout::invalidate();
    }

    /// Called after waits and immediately before destructive injection.
    pub fn replacement_valid(&self, generation: u64) -> bool {
        if !self.control.is_enabled() || !P::input_allowed() {
            return false;
        }
        let focus = P::focus();
        let st = self.lock();
        st.generation == generation && st.focus == focus && (!P::REQUIRES_FOCUS || focus.is_some())
    }

    // ── capture ─────────────────────────────────────────────────────────────

    /// A key went down.
    pub fn key_press(self: &Arc<Self>, key: P::Key) {
        let mut st = self.lock();

        // One physical press arrives on several evdev nodes; the same key again
        // inside the window is that, not a second press.
        if let Some(window) = P::DEDUP_WINDOW {
            if st.last_key == Some(key) && st.last_key_at.elapsed() < window {
                return;
            }
        }
        st.last_key_at = Instant::now();
        st.last_key = Some(key);

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

        st.held_keys.insert(key);
        // Noted on the way down, acted on when the modifiers come back up: that
        // is when the compositor has had the whole combination and the layout it
        // was asking for is live.
        st.layout_hotkey |= st.is_layout_hotkey(key);
        if key == P::CAPS_LOCK {
            st.caps_lock = !st.caps_lock;
        }
        // Any key other than Right Shift itself means the shift is being *held*
        // for something, not tapped, so it is no longer a completion request.
        st.right_shift_tap = key == P::SHIFT_RIGHT && st.held_keys.len() == 1;
        // Same idea for Ctrl, which is the undo gesture: a Ctrl with another
        // key on top of it is a shortcut, and only a bare press/release pair is
        // a tap.
        let is_ctrl = key == P::CTRL_LEFT || key == P::CTRL_RIGHT;
        st.ctrl_down = (is_ctrl && st.held_keys.len() == 1).then(Instant::now);
        if !is_ctrl && key != P::SHIFT_RIGHT {
            st.forget_gestures();
        }
        let shift = st.shift_active();

        // Record key press for typing pattern analysis (dwell, digraphs).
        // Use Debug representation as a stable-ish key name.
        let key_name = format!("{:?}", key);
        crate::personal::record_key_press(&key_name);

        // On backends that identify their own events, a real key arriving
        // during injection invalidates the snapshot already being erased.
        let during_injection = P::injecting_flag(&self.injector)
            .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed));
        if key == P::BACKSPACE {
            // Deletion cancels a stale rewrite, but never starts suppression.
            if during_injection || st.is_replacing || chorded_shortcut || st.focus != P::focus() {
                let no_fix = st.no_fix;
                st.invalidate_text();
                st.no_fix = no_fix;
            } else {
                st.keys.pop();
            }
        } else if during_injection || chorded_shortcut || P::is_reset(key) {
            st.invalidate_text();
        } else if P::is_terminator(key) {
            self.word_finished(st, key, shift);
        } else if P::english_char_plain(key).is_some() || P::hebrew_char(key).is_some() {
            if st.keys.is_empty() && !st.is_replacing {
                st.focus = P::focus();
                if st.no_fix && P::input_empty(&self.injector) {
                    st.no_fix = false;
                }
            }
            st.push_key(Typed { key, shift });
            if !st.is_replacing && st.keys.is_empty() {
                // The word buffer gave up on an overlong token.
                st.no_fix = true;
            }
        }
    }

    /// A word was ended by Space or Enter: check it, and either correct it or
    /// arm the gesture that would un-list it.
    fn word_finished(
        self: &Arc<Self>,
        mut st: MutexGuard<'_, AppState<P>>,
        key: P::Key,
        shift: bool,
    ) {
        if st.is_replacing {
            st.buffered_keys.push(Typed { key, shift });
            return;
        }
        if std::mem::take(&mut st.no_fix) || !self.control.is_enabled() {
            st.keys.clear();
            return;
        }
        if st.keys.is_empty() {
            return;
        }
        if !P::input_allowed()
            || (P::REQUIRES_FOCUS && st.focus.is_none())
            || st.focus != P::focus()
        {
            st.invalidate_text();
            // This terminator already ended the untrusted word.
            st.no_fix = false;
            return;
        }

        let outcome = self.check(&st.keys, st.history.run());
        // Record what this word turned out to be before anything else happens
        // to it: the next word is decided with this one behind it. A word whose
        // language could not be told is not recorded at all — see
        // `dictionary::observed`.
        if let Some(lang) = outcome.lang {
            st.history.push(lang);
        }
        let result = outcome.fix;
        // Describe the fix for the history before `replacement` consumes it.
        let note = result.as_ref().map(|fix| note_of::<P>(&st.keys, fix));
        if let Some(rep) = replacement::<P>(&st.keys, result) {
            let undo = undo_of::<P>(&st.keys, &rep, Some(key));
            // +1 for the terminator the user physically typed, which is erased
            // along with the word and pressed again afterwards.
            let erase = rep.erase + 1;
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
                note.map(|(from, to, kind)| Commit::Fix { from, to, kind }),
            );
            return;
        }

        if let Some(lang) = outcome.lang {
            crate::personal::record_word(&reading::<P>(&st.keys, lang));
        }

        if let Some(word) = declined_by_list(
            &st.keys,
            |t: Typed<P::Key>| P::english_char(t.key, t.shift),
            |t: Typed<P::Key>| P::hebrew_char(t.key),
            |t: Typed<P::Key>| t.shift,
            P::current_layout(),
        ) {
            // Nothing happened to this word, and the only reason is that the
            // user has it listed. Arm the gesture to change their mind about it.
            st.last_action = Some(LastAction::Skipped(LastSkip {
                keys: st.keys.to_vec(),
                terminator: Some(key),
                word,
            }));
        }
        st.keys.clear();
    }

    /// A key came back up.
    ///
    /// Releases matter for three things: knowing which keys the user is still
    /// holding (so injection can avoid a press the OS would swallow as a
    /// duplicate), spotting the Right Shift *tap* that asks for a completion,
    /// and spotting the Ctrl double-tap that takes a correction back.
    ///
    /// Both gestures are built on modifier taps for the same reason: Ctrl and
    /// Right Shift are the only keys on every keyboard that type nothing and
    /// mean nothing to the focused application on their own, so a tap of either
    /// can't move focus, indent a line or open the editor's own completion
    /// popup the way Tab would — nothing has to be un-done when ReCast
    /// declines. Holding either one (for a capital, for a shortcut) is
    /// unaffected; only a press and release with nothing in between counts.
    pub fn key_release(self: &Arc<Self>, key: P::Key) {
        let mut st = self.lock();

        crate::personal::record_key_release(&format!("{key:?}"));

        st.held_keys.remove(&key);

        // A layout hotkey has been let go of. Drop the cached layout rather than
        // let the next word be anchored on what was true before it — the 300 ms
        // TTL is otherwise long enough to decide a word or two against the wrong
        // layout and inject the result under the right one.
        if st.layout_hotkey && P::is_modifier(key) {
            st.layout_hotkey = false;
            crate::layout::invalidate();
        }

        if key == P::CTRL_LEFT || key == P::CTRL_RIGHT {
            self.ctrl_tap(st);
            return;
        }
        if key != P::SHIFT_RIGHT || !std::mem::take(&mut st.right_shift_tap) {
            return;
        }
        if st.is_replacing || st.no_fix || !self.control.is_enabled() {
            return;
        }
        if (P::REQUIRES_FOCUS && st.focus.is_none())
            || st.focus != P::focus()
            || !P::input_allowed()
        {
            st.invalidate_text();
            return;
        }
        self.completion_tap(st);
    }

    /// The completion key was tapped: either step to the next guess in the
    /// cycle already running, or start one from the word in the buffer.
    fn completion_tap(self: &Arc<Self>, mut st: MutexGuard<'_, AppState<P>>) {
        let (typed, candidates, index, erase) = match st.cycle.take() {
            Some(cycle) => {
                let next = if cycle.index >= cycle.candidates.len() {
                    0
                } else {
                    cycle.index + 1
                };
                (cycle.typed, cycle.candidates, next, cycle.on_screen)
            }
            None => {
                if st.keys.is_empty() {
                    return;
                }
                let candidates = complete_candidates(
                    &st.keys,
                    |t: Typed<P::Key>| P::english_char(t.key, t.shift),
                    |t: Typed<P::Key>| t.shift,
                    self.en_dict,
                    P::current_layout(),
                );
                if candidates.is_empty() {
                    return;
                }
                (st.keys.to_vec(), candidates, 0, st.keys.len())
            }
        };

        // Past the end of the list is the user's own text, taken from the keys
        // they pressed rather than from a candidate — the odd irreproducible
        // capitalisation (`sHiFtY`) survives that way.
        let back_to_typed = index >= candidates.len();
        let retype = if back_to_typed {
            P::retype_original(&typed, Language::English)
        } else {
            // An untypeable candidate is dropped rather than injected in half;
            // the cycle carries on to the next tap.
            match P::retype_text(&candidates[index]) {
                Some(r) => r,
                None => return,
            }
        };

        // The counter tracks words changed, not taps: cycling from one guess to
        // the next is still the one fix, and landing back on what the user
        // typed is none at all.
        let was = reading::<P>(&typed, Language::English);
        let commit = if back_to_typed {
            Some(Commit::Undo { suppress: None })
        } else if index == 0 {
            Some(Commit::Fix {
                from: was.clone(),
                to: candidates[index].clone(),
                kind: FixKind::Complete,
            })
        } else {
            None
        };

        // The buffer has to end up holding what is on screen, or the next Space
        // would check a word the user is no longer looking at.
        let keep = if back_to_typed {
            typed.clone()
        } else {
            P::buffer_after(&retype)
        };
        // A completion can be taken back with the undo gesture too — except
        // when it has just handed back the user's own text, which is nothing to
        // undo.
        let undo = (!back_to_typed).then(|| LastFix {
            on_screen: P::retype_len(&retype),
            restore: P::retype_original(&typed, Language::English),
            terminator: None,
            layout: None,
            keep: typed.clone(),
            suppress: non_empty(was),
        });

        st.cycle = Some(Cycle {
            typed,
            candidates,
            index,
            on_screen: P::retype_len(&retype),
        });
        // The completion key types nothing, so only what is on screen for the
        // partial word is erased and there is no terminator to press again.
        self.start_replacement(
            st,
            Plan {
                erase,
                retype,
                terminator: None,
            },
            keep,
            undo,
            commit,
        );
    }

    /// A Ctrl key came back up. If it was a bare tap and the second one inside
    /// [`DOUBLE_TAP_WINDOW`], act on the word the cursor is sitting on — take
    /// back the correction that landed on it, or take it off the user's list
    /// and correct it after all. Which of the two is decided by what happened
    /// to the word, not by the gesture: see [`LastAction`].
    ///
    /// Either way it erases backwards from the cursor, so it is only ever
    /// offered for a word nothing has been typed over yet
    /// ([`AppState::last_action`], cleared by the next keystroke). That is the
    /// same bargain every in-place autocorrect makes, and it is what keeps a
    /// mistimed double-tap from eating text further back.
    fn ctrl_tap(self: &Arc<Self>, mut st: MutexGuard<'_, AppState<P>>) {
        let Some(down) = st.ctrl_down.take() else {
            return;
        };
        // Held rather than tapped: the user was using Ctrl for what it is for.
        if down.elapsed() > TAP_MAX {
            st.last_ctrl_tap = None;
            return;
        }
        let now = Instant::now();
        match st.last_ctrl_tap.take() {
            Some(prev) if now.duration_since(prev) <= DOUBLE_TAP_WINDOW => {}
            // First tap of a possible pair: remember it and wait for the second.
            _ => {
                st.last_ctrl_tap = Some(now);
                return;
            }
        }

        if st.is_replacing || !self.control.is_enabled() {
            return;
        }
        if (P::REQUIRES_FOCUS && st.focus.is_none())
            || st.focus != P::focus()
            || !P::input_allowed()
        {
            st.invalidate_text();
            return;
        }
        match st.last_action.take() {
            Some(LastAction::Fixed(fix)) => self.undo_fix(st, fix),
            Some(LastAction::Skipped(skip)) => self.unlist_and_correct(st, skip),
            None => {}
        }
    }

    /// Put back what the user typed before the correction on screen replaced it.
    fn undo_fix(self: &Arc<Self>, mut st: MutexGuard<'_, AppState<P>>, fix: LastFix<P>) {
        // Put the layout back before the keys go out. On Linux that is a
        // precondition rather than a courtesy — `uinput` speaks keycodes, so
        // what they spell depends on the layout that is live when they land,
        // and replaying them under the old one would just re-enter the
        // correction. `.ready()`, not a bare bool: "already on that layout" is
        // a reason to carry on, not to abandon the undo.
        if let Some(lang) = fix.layout {
            let outcome = P::switch_layout_to(lang);
            if P::ABORT_UNDO_IF_LAYOUT_REFUSED && !outcome.ready() {
                // Leave the text alone rather than churn it, and leave the word
                // correctable rather than retire it on the strength of an undo
                // that never happened.
                return;
            }
        }
        st.cycle = None;
        self.start_replacement(
            st,
            Plan {
                erase: fix.on_screen,
                retype: fix.restore,
                terminator: fix.terminator,
            },
            fix.keep,
            // Undoing an undo would be a redo, which is a different gesture.
            None,
            Some(Commit::Undo {
                suppress: fix.suppress,
            }),
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
    fn unlist_and_correct(
        self: &Arc<Self>,
        mut st: MutexGuard<'_, AppState<P>>,
        skip: LastSkip<P>,
    ) {
        crate::complete::unlist(&skip.word);
        // The run is read but not added to: this word was already recorded when
        // it was first finished, and the gesture is a second opinion about it
        // rather than a second word.
        let result = self.check(&skip.keys, st.history.run()).fix;
        let note = result.as_ref().map(|fix| note_of::<P>(&skip.keys, fix));
        // Off the list, but the pipelines have nothing to say about it after
        // all — which is a fine outcome, and not one to rewrite the screen over.
        let Some(rep) = replacement::<P>(&skip.keys, result) else {
            return;
        };
        st.cycle = None;
        let erase = rep.erase + usize::from(skip.terminator.is_some());
        self.start_replacement(
            st,
            Plan {
                erase,
                retype: rep.retype,
                terminator: skip.terminator,
            },
            Vec::new(),
            None,
            note.map(|(from, to, kind)| Commit::Fix { from, to, kind }),
        );
    }

    /// Run the pipelines over a finished word. `run` is the language of the
    /// words before it, which the caller reads off [`AppState::history`] while
    /// it still holds the lock.
    fn check(&self, keys: &[Typed<P::Key>], run: Run) -> Outcome {
        check_and_correct(
            keys,
            |t: Typed<P::Key>| P::english_char(t.key, t.shift),
            |t: Typed<P::Key>| P::hebrew_char(t.key),
            |t: Typed<P::Key>| t.shift,
            run,
            self.en_dict,
            self.he_dict,
            P::current_layout(),
            P::switch_layout_to,
        )
    }

    // ── injection ───────────────────────────────────────────────────────────

    /// Gate the listener and hand the replacement to a thread of its own.
    ///
    /// The lock is taken by the caller and dropped here, before the thread
    /// starts: injection re-enters the listener, which needs the same lock.
    fn start_replacement(
        self: &Arc<Self>,
        mut st: MutexGuard<'_, AppState<P>>,
        plan: Plan<P>,
        keep: Vec<Typed<P::Key>>,
        undo: Option<LastFix<P>>,
        commit: Option<Commit>,
    ) {
        let generation = st.generation;
        st.is_replacing = true;
        drop(st);
        let engine = Arc::clone(self);
        thread::spawn(move || engine.replace_word(plan, keep, undo, generation, commit));
    }

    /// Erase what the user typed and put the replacement in its place.
    ///
    /// `keep` is the word buffer to leave behind — what is now on screen for
    /// the word still in progress, so a completion the user keeps typing over
    /// is checked as the word they can see rather than as the tail they added.
    /// `undo` is the payload the Ctrl double-tap would put back, kept only if
    /// the user typed nothing while this was landing.
    fn replace_word(
        &self,
        plan: Plan<P>,
        keep: Vec<Typed<P::Key>>,
        undo: Option<LastFix<P>>,
        generation: u64,
        commit: Option<Commit>,
    ) {
        // Armed for the whole replacement: whatever happens below — including a
        // panic — `is_replacing` and the injecting gate are cleared, rather than
        // leaving the listener shut for the rest of the session.
        let _gate = ReplaceGuard::new(&self.state, P::injecting_flag(&self.injector));

        if !self.replacement_valid(generation) {
            let mut st = self.lock();
            if st.generation == generation {
                st.invalidate_text();
            }
            return;
        }
        let Some(buffered) = P::inject(self, plan, generation) else {
            let mut st = self.lock();
            if st.generation == generation {
                st.invalidate_text();
            }
            return;
        };
        match commit {
            Some(Commit::Fix { from, to, kind }) => {
                self.control.record_fix(&from, &to, kind);
                crate::personal::record_confusion(&from, &to);
                crate::personal::record_word(&to);
            }
            Some(Commit::Undo { suppress }) => {
                if let Some(word) = suppress {
                    crate::complete::suppress(&word);
                    crate::complete::learn(&word);
                }
                self.control.record_undo();
            }
            None => {}
        }

        let mut st = self.lock();
        if st.generation != generation {
            return;
        }
        st.keys.replace_with(keep);
        st.keys.extend(buffered.iter().copied());
        // Undo erases backwards from the cursor, so it is only valid while the
        // cursor is still sitting on what we just injected. Keys the user got
        // in during the replacement were replayed after it and have moved it on.
        st.last_action = if buffered.is_empty() {
            undo.map(LastAction::Fixed)
        } else {
            None
        };
        // Reset the dedup guard so the injected terminator is not silently
        // dropped for sharing a keycode with the physical press that triggered
        // this replacement (both arrive within the window).
        st.last_key = None;
        // `buffered_keys` and `is_replacing` are the guard's, and it clears them
        // after this lock is dropped — on this path and on a panicking one
        // alike.
    }
}

enum Commit {
    Fix {
        from: String,
        to: String,
        kind: FixKind,
    },
    Undo {
        suppress: Option<String>,
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

// ─────────────────────────────────────────────────────────────────────────────
// Turning a Fix into a replacement
// ─────────────────────────────────────────────────────────────────────────────

/// What a [`Fix`] turns into for the injection thread.
pub struct Replacement<P: Platform> {
    /// How many of the characters the user typed have to be erased.
    erase: usize,
    /// What to put in their place.
    retype: P::Retype,
    /// The layout that was live before the fix, when the fix changed it — what
    /// undo has to switch back to.
    previous_layout: Option<Language>,
}

/// Turn a [`Fix`] into what the injection thread needs.
///
/// Anything before a layout fix's `start` is a previously-typed word that the
/// user concatenated by forgetting a space, and is left intact.
fn replacement<P: Platform>(keys: &[Typed<P::Key>], fix: Option<Fix>) -> Option<Replacement<P>> {
    match fix? {
        Fix::Layout { start, text, lang } => Some(Replacement {
            erase: keys.len() - start,
            retype: P::retype_layout(&keys[start..], &text, lang)?,
            previous_layout: Some(lang.other()),
        }),
        Fix::LayoutSpelling { text, lang } => Some(Replacement {
            erase: keys.len(),
            retype: P::retype_text(&text)?,
            previous_layout: Some(lang.other()),
        }),
        // Same layout, different letters: erase the whole word and type the
        // corrected spelling instead.
        Fix::Spelling { text } => Some(Replacement {
            erase: keys.len(),
            retype: P::retype_text(&text)?,
            previous_layout: None,
        }),
    }
}

/// Everything needed to take `rep` back again, built from the keys it is about
/// to replace.
fn undo_of<P: Platform>(
    keys: &[Typed<P::Key>],
    rep: &Replacement<P>,
    terminator: Option<P::Key>,
) -> LastFix<P> {
    let original = &keys[keys.len() - rep.erase..];
    let was = rep.previous_layout.unwrap_or(Language::English);
    LastFix {
        // Everything injected is one character per key, plus the terminator
        // that rides along after it.
        on_screen: P::retype_len(&rep.retype) + usize::from(terminator.is_some()),
        restore: P::retype_original(original, was),
        terminator,
        layout: rep.previous_layout,
        // The terminator has already finished this word, so nothing carries
        // over into the buffer.
        keep: Vec::new(),
        suppress: non_empty(reading::<P>(original, was)),
    }
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
            Language::English => P::english_char(t.key, t.shift).map(|c| {
                if t.shift {
                    c.to_ascii_uppercase()
                } else {
                    c
                }
            }),
            // Hebrew has no case, so the shift the user held says nothing.
            Language::Hebrew => P::hebrew_char(t.key),
        })
        .collect()
}

/// The pair of words the recent-corrections history shows for `fix`, and which
/// pipeline produced it.
///
/// The "before" side is what was on screen, which is not the same reading in
/// each case: layout-based fixes have already switched to `lang`, so what the
/// user was looking at is the *other* layout's reading, while a spelling fix or
/// an expansion never left English.
fn note_of<P: Platform>(keys: &[Typed<P::Key>], fix: &Fix) -> (String, String, FixKind) {
    match fix {
        Fix::Layout { start, text, lang } => (
            reading::<P>(&keys[*start..], lang.other()),
            text.clone(),
            FixKind::Layout,
        ),
        Fix::LayoutSpelling { text, lang } => (
            reading::<P>(keys, lang.other()),
            text.clone(),
            FixKind::LayoutSpelling,
        ),
        Fix::Spelling { text } => (
            reading::<P>(keys, Language::English),
            text.clone(),
            FixKind::Spelling,
        ),
    }
}

fn non_empty(s: String) -> Option<String> {
    (!s.is_empty()).then_some(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc,
    };

    static FOCUS: AtomicUsize = AtomicUsize::new(1);
    struct Simulated;
    struct Screen {
        text: Mutex<String>,
        injecting: std::sync::atomic::AtomicBool,
        ready: mpsc::SyncSender<()>,
        proceed: Mutex<mpsc::Receiver<()>>,
    }
    impl Platform for Simulated {
        type Key = char;
        type Retype = String;
        type Injector = Screen;
        type Focus = usize;
        const SHIFT_LEFT: char = '\x01';
        const SHIFT_RIGHT: char = '\x02';
        const CTRL_LEFT: char = '\x03';
        const CTRL_RIGHT: char = '\x04';
        const CAPS_LOCK: char = '\x05';
        const BACKSPACE: char = '\x08';
        fn is_terminator(key: char) -> bool {
            key == ' ' || key == '\n'
        }
        fn is_reset(key: char) -> bool {
            matches!(key, '\x1b' | '\x10'..='\x14')
        }
        fn is_modifier(key: char) -> bool {
            ('\x01'..='\x05').contains(&key)
        }
        fn english_char(key: char, _: bool) -> Option<char> {
            Self::english_char_plain(key)
        }
        fn english_char_plain(key: char) -> Option<char> {
            (!key.is_control()).then_some(key)
        }
        fn hebrew_char(key: char) -> Option<char> {
            #[cfg(target_os = "linux")]
            return crate::keymap::english_char_to_evkey_shifted(key)
                .and_then(|(k, _)| crate::keymap::evkey_to_hebrew_char(k));
            #[cfg(not(target_os = "linux"))]
            return crate::keymap::english_char_to_key(key)
                .and_then(|(k, _)| crate::keymap::key_to_hebrew_char(k));
        }
        fn retype_original(keys: &[Typed<char>], lang: Language) -> String {
            reading::<Self>(keys, lang)
        }
        fn retype_layout(_: &[Typed<char>], text: &str, _: Language) -> Option<String> {
            Some(text.to_string())
        }
        fn retype_text(text: &str) -> Option<String> {
            Some(text.to_string())
        }
        fn retype_len(text: &String) -> usize {
            text.chars().count()
        }
        fn buffer_after(text: &String) -> Vec<Typed<char>> {
            text.chars()
                .map(|key| Typed { key, shift: false })
                .collect()
        }
        fn injecting_flag(screen: &Screen) -> Option<&std::sync::atomic::AtomicBool> {
            Some(&screen.injecting)
        }
        fn focus() -> Option<usize> {
            Some(FOCUS.load(Ordering::SeqCst))
        }
        fn input_empty(screen: &Screen) -> bool {
            screen.text.lock().unwrap().is_empty()
        }
        fn current_layout() -> Option<Language> {
            Some(Language::English)
        }
        fn switch_layout_to(_: Language) -> crate::layout::LayoutSwitch {
            panic!("English spelling and undo must not switch layouts");
        }
        fn inject(
            engine: &Engine<Self>,
            plan: Plan<Self>,
            generation: u64,
        ) -> Option<Vec<Typed<char>>> {
            engine.injector.ready.send(()).unwrap();
            engine
                .injector
                .proceed
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(3))
                .unwrap();
            if !engine.replacement_valid(generation) {
                return None;
            }
            let buffered = engine.buffered();
            let mut screen = engine.injector.text.lock().unwrap();
            for _ in 0..plan.erase + buffered.len() {
                assert!(
                    screen.pop().is_some(),
                    "replacement erased beyond the typed text"
                );
            }
            screen.push_str(&plan.retype);
            if let Some(key) = plan.terminator {
                screen.push(key);
            }
            screen.extend(buffered.iter().map(|t| t.key));
            Some(buffered)
        }
    }

    struct Session {
        engine: Arc<Engine<Simulated>>,
        ready: mpsc::Receiver<()>,
        proceed: mpsc::SyncSender<()>,
    }
    impl Session {
        fn new() -> Self {
            let (ready_tx, ready) = mpsc::sync_channel(1);
            let (proceed, proceed_rx) = mpsc::sync_channel(1);
            let engine = Engine::new(
                crate::dictionary::en_dict(),
                crate::dictionary::he_dict(),
                Arc::new(AppControl::new_for_test()),
                Screen {
                    text: Mutex::new(String::new()),
                    injecting: std::sync::atomic::AtomicBool::new(false),
                    ready: ready_tx,
                    proceed: Mutex::new(proceed_rx),
                },
            );
            Self {
                engine,
                ready,
                proceed,
            }
        }
        fn type_text(&self, text: &str) {
            for key in text.chars() {
                // Capture runs before the application receives the character.
                self.engine.key_press(key);
                self.engine.injector.text.lock().unwrap().push(key);
                self.engine.key_release(key);
            }
        }
        fn tap(&self, key: char) {
            self.engine.key_press(key);
            self.engine.key_release(key);
        }
        fn backspace(&self) {
            self.engine.injector.text.lock().unwrap().pop();
            self.tap(Simulated::BACKSPACE);
        }
        fn pending(&self) {
            self.ready
                .recv_timeout(Duration::from_secs(3))
                .expect("no correction scheduled");
        }
        fn finish(&self) {
            self.proceed.send(()).unwrap();
            let deadline = Instant::now() + Duration::from_secs(3);
            while self.engine.lock().is_replacing {
                assert!(Instant::now() < deadline, "replacement did not finish");
                thread::sleep(Duration::from_millis(1));
            }
        }
        fn text(&self) -> String {
            self.engine.injector.text.lock().unwrap().clone()
        }
    }

    #[test]
    fn typing_correction_undo_and_interrupted_replacements() {
        let s = Session::new();
        s.type_text("keyboad ");
        s.pending();
        s.finish();
        assert_eq!(s.text(), "keyboard ");
        assert_eq!(s.engine.control.fixed_count(), 1);
        s.tap(Simulated::CTRL_LEFT);
        s.tap(Simulated::CTRL_LEFT);
        s.pending();
        s.finish();
        assert_eq!(s.text(), "keyboad ");
        assert_eq!(s.engine.control.undo_count(), 1);
        assert!(crate::complete::suppressed("keyboad"));
        assert!(s.engine.lock().last_action.is_none());

        // Pause the worker at injection, then move the cursor, paste, edit, or
        // change focus. None may erase, learn, count a fix, or re-arm undo.
        for interrupt in 0..6 {
            let s = Session::new();
            s.type_text("recieve ");
            s.pending();
            match interrupt {
                0 => s.engine.mouse_click(),
                1 => s.tap('\x1b'),
                2 => {
                    s.engine.key_press(Simulated::CTRL_LEFT);
                    s.tap('v');
                    s.engine.key_release(Simulated::CTRL_LEFT);
                }
                3 => s.tap(Simulated::BACKSPACE),
                4 => {
                    FOCUS.store(2, Ordering::SeqCst);
                }
                _ => {
                    s.engine.injector.injecting.store(true, Ordering::SeqCst);
                    s.type_text("x");
                }
            }
            s.finish();
            assert_eq!(
                s.text(),
                if interrupt == 5 {
                    "recieve x"
                } else {
                    "recieve "
                },
                "interruption {interrupt}"
            );
            assert_eq!(s.engine.control.fixed_count(), 0);
            assert!(s.engine.lock().last_action.is_none());
            if interrupt == 3 {
                assert!(!s.engine.lock().no_fix);
                s.type_text("recieve ");
                s.pending();
                s.finish();
                assert_eq!(s.text(), "recieve receive ");
            }
            FOCUS.store(1, Ordering::SeqCst);
        }

        // Shortcut letters and Ctrl+Space are not text or word terminators.
        let s = Session::new();
        s.type_text("recie");
        s.engine.key_press(Simulated::CTRL_LEFT);
        s.tap('v');
        s.tap(' ');
        s.engine.key_release(Simulated::CTRL_LEFT);
        s.type_text("ve ");
        assert!(!s.engine.lock().is_replacing);
        assert!(s.engine.lock().keys.is_empty());
        // An empty terminator must clear suppression for the following word.
        s.type_text("recieve ");
        s.pending();
        s.finish();
        assert_eq!(s.text(), "recieve receive ");

        // Fast typing is replayed, and undo cannot eat those later keys.
        let s = Session::new();
        s.type_text("recieve ");
        s.pending();
        s.type_text("next");
        s.finish();
        assert_eq!(s.text(), "receive next");
        assert!(s.engine.lock().last_action.is_none());

        // A programmatic focus change between typing and the terminator is
        // caught even without a mouse or navigation-key event.
        let s = Session::new();
        s.type_text("recieve");
        FOCUS.store(2, Ordering::SeqCst);
        s.type_text(" ");
        assert!(!s.engine.lock().is_replacing);
        assert_eq!(s.text(), "recieve ");
        FOCUS.store(1, Ordering::SeqCst);

        // Backspacing within a word must not suspend its correction.
        let s = Session::new();
        s.type_text("recievex");
        s.backspace();
        s.type_text(" ");
        s.pending();
        s.finish();
        assert_eq!(s.text(), "receive ");
        s.type_text("recieve ");
        s.pending();
        s.finish();
        assert_eq!(s.text(), "receive receive ");

        // Erasing a fully tracked word restores correction immediately.
        let s = Session::new();
        s.type_text("bad");
        for _ in 0..3 {
            s.backspace();
        }
        s.type_text("recieve ");
        s.pending();
        s.finish();
        assert_eq!(s.text(), "receive ");

        // A finished word remains known through its separator and correction.
        for original in ["old ", "recieve ", "old  ", "old\n"] {
            let s = Session::new();
            s.type_text(original);
            if original == "recieve " {
                s.pending();
                s.finish();
                assert_eq!(s.text(), "receive ");
            }
            let count = s.text().chars().count();
            for _ in 0..count {
                s.backspace();
            }
            s.type_text("recieve ");
            s.pending();
            s.finish();
            assert_eq!(s.text(), "receive ", "after erasing {original:?}");
        }

        // Real deletion often includes repeats after the field is already empty.
        // A click or a delete shortcut also loses context; an empty field is safe.
        let s = Session::new();
        s.engine.mouse_click();
        for _ in 0..3 {
            s.type_text("recieve ");
            s.pending();
            s.finish();
            assert_eq!(s.text(), "receive ");
            for _ in 0..12 {
                s.backspace();
            }
        }
        s.type_text("old");
        s.engine.key_press(Simulated::CTRL_LEFT);
        s.tap(Simulated::BACKSPACE);
        s.engine.injector.text.lock().unwrap().clear();
        s.engine.key_release(Simulated::CTRL_LEFT);
        s.type_text("recieve ");
        s.pending();
        s.finish();
        assert_eq!(s.text(), "receive ");

        // Arrows and forward delete lose the cursor context. Deleting the
        // tracked suffix must not make an unknown word safe to rewrite.
        for reset in '\x10'..='\x14' {
            let s = Session::new();
            s.type_text("old ");
            s.tap(reset);
            s.type_text("x");
            s.backspace();
            s.type_text("recieve");
            s.tap(Simulated::SHIFT_RIGHT);
            assert!(!s.engine.lock().is_replacing);
            s.type_text(" ");
            assert!(!s.engine.lock().is_replacing);
            s.type_text("recieve ");
            s.pending();
            s.finish();
            assert_eq!(s.text(), "old recieve receive ");
        }

        // Backspacing into a finished word does not suspend subsequent typing.
        let s = Session::new();
        s.type_text("old ");
        s.backspace();
        s.backspace();
        s.type_text("recieve ");
        s.pending();
        s.finish();
        s.type_text("recieve ");
        s.pending();
        s.finish();
        assert_eq!(s.text(), "olreceive receive ");

        // Explicit completion still works on a fully tracked, edited prefix.
        let s = Session::new();
        s.type_text("helx");
        s.backspace();
        s.tap(Simulated::SHIFT_RIGHT);
        s.pending();
        s.finish();
        assert!(s.text().starts_with("hel") && s.text().len() > 3);
        let count = s.engine.lock().cycle.as_ref().unwrap().candidates.len();
        for _ in 0..count {
            s.tap(Simulated::SHIFT_RIGHT);
            s.pending();
            s.finish();
        }
        assert_eq!(
            s.text(),
            "hel",
            "completion cycle restores the exact prefix"
        );
    }
}
