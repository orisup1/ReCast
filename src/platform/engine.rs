//! The listener state machine, written once.
//!
//! Capture and injection are genuinely different on the three platforms —
//! `evdev` keycodes replayed through `uinput`, an `rdev` key stream with text
//! inserted by `CGEventKeyboardSetUnicodeString`, the same stream with text
//! inserted by `SendInput`. Everything *between* those two ends is not
//! different, and used to be written out three times anyway: `Typed`,
//! `AppState`, `LastAction`, `LastFix`, `LastSkip`, `Cycle`, `shift_active`,
//! `replacement`, `undo_of`, `reading`, `note_of`, `handle_action_tap`,
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
//! * Platform safety differences, spelled out as associated constants rather
//!   than left implicit.

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

/// Longest a modifier press may last and still count as a *tap* rather than a hold.
/// Ctrl held down is the start of a shortcut; Ctrl let straight back up types
/// nothing and means nothing, which is what makes it usable as a gesture.
///
/// Shared rather than per-platform: this is a user-visible gesture window, and
/// three copies of it meant the gesture could come to feel different depending
/// on the OS for no reason anyone had decided on.
pub const TAP_MAX: Duration = Duration::from_millis(300);

/// Two action modifier taps inside this window are the undo gesture. Wide enough not to
/// demand a drum roll, short enough that two unrelated taps a second apart are
/// not read as one gesture.
pub const DOUBLE_TAP_WINDOW: Duration = Duration::from_millis(500);

/// Keep explicit accessibility edits and their undo payload bounded.
pub const MAX_SELECTION_BYTES: usize = 64 * 1024;

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
    /// Application identity associated with this focus, never the window title.
    fn app_id(focus: &Self::Focus) -> Option<String>;
    fn is_own_focus(_focus: &Self::Focus) -> bool {
        false
    }
    fn current_layout() -> Option<Language> {
        crate::layout::current_layout()
    }
    fn switch_layout_to(lang: Language) -> crate::layout::LayoutSwitch {
        crate::layout::switch_layout_to(lang)
    }
    const REQUIRES_FOCUS: bool = false;
    fn requires_focus() -> bool {
        Self::REQUIRES_FOCUS
    }
    fn input_allowed() -> bool {
        true
    }
    /// Confirm an empty text field; unavailable context must remain suppressed.
    fn input_empty(_: &Self::Injector) -> bool {
        false
    }

    /// Selected text is read only on an explicit gesture, never during typing.
    fn selection(_focus: &Self::Focus) -> Option<Selection> {
        None
    }
    /// Replace only if the same selection still exists. Return its new range.
    fn replace_selection(
        _engine: &Engine<Self>,
        _focus: &Self::Focus,
        _expected: &Selection,
        _text: &str,
        _generation: u64,
    ) -> Option<Selection> {
        None
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

    // ── platform safety differences ────────────────────────────────────────

    /// Only safe when injected events explicitly ignore physical Ctrl flags.
    /// Other platforms still cancel rather than risk injecting Ctrl+Backspace.
    const QUEUE_UNDO_DURING_REPLACEMENT: bool = false;

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

/// Native selection offsets, in UTF-16 units, plus the exact selected text.
#[derive(Clone, Debug, PartialEq)]
pub struct Selection {
    pub start: isize,
    pub length: isize,
    pub text: String,
}

/// What the Ctrl double-tap would do to the word the cursor is sitting on.
///
/// Corrections can be undone; skipped words can be unlisted; unchanged words
/// can be explicitly reconsidered. Cursor movement invalidates all three.
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
        layout: Language,
    },
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
    /// Automatic planner rule, absent for manual conversion and completion.
    rule: Option<&'static str>,
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

fn shortcut_matches<P: Platform>(binding: &str, key: P::Key) -> bool {
    match binding {
        "ctrl" => key == P::CTRL_LEFT || key == P::CTRL_RIGHT,
        "left_ctrl" => key == P::CTRL_LEFT,
        "right_ctrl" => key == P::CTRL_RIGHT,
        "left_shift" => key == P::SHIFT_LEFT,
        "right_shift" => key == P::SHIFT_RIGHT,
        _ => false,
    }
}

/// Listener state, shared by every capture thread of a platform.
pub struct AppState<P: Platform> {
    mode: crate::config::AppMode,
    practice: bool,
    pub keys: WordBuffer<Typed<P::Key>>,
    pub is_replacing: bool,
    selection_replacing: bool,
    pub buffered_keys: WordBuffer<Typed<P::Key>>,
    /// Physical keys currently held down. Tracked from press/release events so
    /// injection can wait for the user to lift the keys it is about to retype —
    /// otherwise the OS sees the synthetic press as a duplicate of the
    /// still-held physical key and drops it.
    pub held_keys: HashSet<P::Key>,
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
    /// [`DOUBLE_TAP_WINDOW`] is the undo gesture.
    pub last_action_tap: Option<Instant>,
    /// An undo gesture completed while the current correction was landing.
    pending_undo: bool,
    shortcut_bindings: Option<(String, String, String)>,
    /// A key combination shaped like a layout-switch hotkey has been pressed and
    /// the modifiers holding it have not all come back up yet. When they do, the
    /// cached layout is dropped — see [`crate::layout::invalidate`].
    pub layout_hotkey: bool,
    /// What the action double-tap would do to the word the cursor is sitting on,
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
    /// Every input event, including releases, invalidates an in-flight query.
    revision: u64,
    focus: Option<P::Focus>,
    no_fix: bool,
}

impl<P: Platform> AppState<P> {
    fn new() -> Self {
        Self {
            mode: crate::config::AppMode::Full,
            practice: false,
            keys: WordBuffer::new(),
            is_replacing: false,
            selection_replacing: false,
            buffered_keys: WordBuffer::new(),
            held_keys: HashSet::new(),
            caps_lock: false,
            completion_down: None,
            action_down: None,
            last_action_tap: None,
            pending_undo: false,
            shortcut_bindings: None,
            layout_hotkey: false,
            last_action: None,
            cycle: None,
            history: History::default(),
            last_key: None,
            last_key_at: Instant::now(),
            generation: 0,
            revision: 0,
            focus: None,
            no_fix: false,
        }
    }

    fn sync_shortcuts(&mut self, config: &crate::config::Config) {
        if !self
            .shortcut_bindings
            .as_ref()
            .is_some_and(|(action, completion, undo)| {
                action == &config.action_shortcut
                    && completion == &config.completion_shortcut
                    && undo == &config.undo_shortcut
            })
        {
            self.action_down = None;
            self.completion_down = None;
            self.last_action_tap = None;
            self.pending_undo = false;
            self.shortcut_bindings = Some((
                config.action_shortcut.clone(),
                config.completion_shortcut.clone(),
                config.undo_shortcut.clone(),
            ));
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
        self.last_action_tap = None;
        self.pending_undo = false;
    }
    fn invalidate_text(&mut self) {
        self.revision = self.revision.wrapping_add(1);
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
        if !replacing {
            self.selection_replacing = false;
        }
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

type FocusSnapshot<'a, P> = (MutexGuard<'a, AppState<P>>, Option<<P as Platform>::Focus>);

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

    fn read_focus(&self) -> (Option<P::Focus>, Option<crate::config::AppMode>) {
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
    fn refresh_focus<'a>(
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
        st.mode = mode;
        st.practice = self
            .control
            .practice_open
            .load(std::sync::atomic::Ordering::Relaxed)
            && focus.as_ref().is_some_and(P::is_own_focus);
        Some((st, focus))
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

    /// Unplugged evdev devices cannot deliver releases. Cancel stale work and
    /// remove their held keys without interpreting a release as an undo tap.
    #[cfg(target_os = "linux")]
    pub fn input_device_removed(&self, held: &HashSet<P::Key>) {
        let mut st = self.lock();
        st.invalidate_text();
        st.held_keys.retain(|key| !held.contains(key));
        st.completion_down = None;
        st.action_down = None;
        st.layout_hotkey = false;
        st.last_key = None;
        crate::layout::invalidate();
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
        if self.lock().generation != generation || !self.control.is_enabled() || !P::input_allowed()
        {
            return false;
        }
        let (focus, mode) = self.read_focus();
        let st = self.lock();
        mode.is_some_and(|mode| mode != crate::config::AppMode::Off && mode == st.mode)
            && st.generation == generation
            && st.focus == focus
            && (!P::requires_focus() || focus.is_some())
    }

    // ── capture ─────────────────────────────────────────────────────────────

    /// A key went down.
    pub fn key_press(self: &Arc<Self>, key: P::Key) {
        let config = crate::config::Config::global();
        let mut st = self.lock();
        st.sync_shortcuts(&config);

        // One physical press arrives on several evdev nodes; the same key again
        // inside the window is that, not a second press.
        if let Some(window) = P::DEDUP_WINDOW {
            if st.last_key == Some(key) && st.last_key_at.elapsed() < window {
                return;
            }
        }
        st.last_key_at = Instant::now();
        st.last_key = Some(key);
        st.revision = st.revision.wrapping_add(1);

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
        if key == P::CAPS_LOCK {
            st.caps_lock = !st.caps_lock;
        }
        let is_action = shortcut_matches::<P>(&config.action_shortcut, key)
            || shortcut_matches::<P>(&config.undo_shortcut, key);
        let is_completion = shortcut_matches::<P>(&config.completion_shortcut, key);
        let bare = fresh_press && st.held_keys.len() == 1;
        st.completion_down = (is_completion && bare).then(Instant::now);
        st.action_down = (is_action && bare).then(Instant::now);
        if !is_action {
            st.last_action_tap = None;
        }
        if !is_action && !is_completion {
            st.forget_gestures();
        }
        let shift = st.shift_active();
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

        // Cancel before any OS query: the worker must see a shortcut/deletion
        // immediately, even while the compositor is slow to answer focus.
        let during_injection = P::injecting_flag(&self.injector)
            .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed));
        if key == P::BACKSPACE && (during_injection || st.is_replacing || chorded_shortcut) {
            let no_fix = st.no_fix;
            st.invalidate_text();
            st.no_fix = no_fix;
            return;
        }
        if during_injection
            || chorded_shortcut
            || P::is_reset(key)
            || (st.is_replacing && self.control.has_app_rules())
        {
            st.invalidate_text();
            return;
        }

        let is_text = P::english_char_plain(key).is_some() || P::hebrew_char(key).is_some();
        let needs_focus = self.control.has_app_rules()
            || self
                .control
                .practice_open
                .load(std::sync::atomic::Ordering::Relaxed)
            || key == P::BACKSPACE
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

        // Record key press for typing pattern analysis (dwell, digraphs).
        // Use Debug representation as a stable-ish key name.
        if !st.practice
            && st.mode == crate::config::AppMode::Full
            && crate::config::Config::global().personal_enabled
        {
            crate::personal::record_key_press(&format!("{key:?}"));
        }

        if key == P::BACKSPACE {
            // Deletion cancels a stale rewrite, but never starts suppression.
            if st.focus != focus {
                let no_fix = st.no_fix;
                st.invalidate_text();
                st.no_fix = no_fix;
            } else {
                st.keys.pop();
            }
        } else if P::is_terminator(key) {
            self.word_finished(st, key, shift, focus);
        } else if is_text {
            if st.keys.is_empty() && !st.is_replacing {
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
        focus: Option<P::Focus>,
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
        if !P::input_allowed() || (P::requires_focus() && st.focus.is_none()) || st.focus != focus {
            st.invalidate_text();
            // This terminator already ended the untrusted word.
            st.no_fix = false;
            return;
        }

        let outcome = self.check(&st.keys, st.history.run(), st.mode);
        // Record what this word turned out to be before anything else happens
        // to it: the next word is decided with this one behind it. A word whose
        // language could not be told is not recorded at all — see
        // `dictionary::observed`.
        if let Some(lang) = outcome.lang {
            st.history.push(lang);
        }
        let result = outcome.fix;
        let rule = outcome.rule;
        // Describe the fix for the history before `replacement` consumes it.
        let note = result.as_ref().map(|fix| note_of::<P>(&st.keys, fix));
        if let Some(rep) = replacement::<P>(&st.keys, result) {
            let mut undo = undo_of::<P>(&st.keys, &rep, Some(key));
            undo.rule = rule;
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
                note.map(|(from, to, kind)| Commit::Fix {
                    from,
                    to,
                    kind,
                    rule,
                    deferred_layout: None,
                }),
            );
            return;
        }

        if !st.practice && st.mode == crate::config::AppMode::Full {
            if let Some(lang) = outcome.lang {
                crate::personal::record_word(&reading::<P>(&st.keys, lang));
            }
        }

        if let Some(word) = (!st.practice)
            .then(|| {
                declined_by_list(
                    &st.keys,
                    |t: Typed<P::Key>| P::english_char(t.key, t.shift),
                    |t: Typed<P::Key>| P::hebrew_char(t.key),
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
                word,
            }));
        }
        if st.last_action.is_none() {
            if let Some(layout) = P::current_layout() {
                st.last_action = Some(LastAction::Unchanged {
                    keys: st.keys.to_vec(),
                    terminator: Some(key),
                    layout,
                });
            }
        }
        st.keys.clear();
    }

    /// A key came back up.
    ///
    /// Track held keys and configured modifier taps. Only a short, bare
    /// press/release pair counts; chords and holds retain their normal meaning.
    pub fn key_release(self: &Arc<Self>, key: P::Key) {
        let config = crate::config::Config::global();
        let mut st = self.lock();
        st.sync_shortcuts(&config);
        st.revision = st.revision.wrapping_add(1);
        st.held_keys.remove(&key);

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
        if !is_action && !is_completion && !config.personal_enabled {
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
        if !st.practice
            && st.mode == crate::config::AppMode::Full
            && crate::config::Config::global().personal_enabled
        {
            crate::personal::record_key_release(&format!("{key:?}"));
        }

        if is_action {
            self.action_tap(st, key);
            return;
        }
        if !is_completion
            || st
                .completion_down
                .take()
                .is_none_or(|down| down.elapsed() > TAP_MAX)
        {
            return;
        }
        if st.is_replacing || st.no_fix || !self.control.is_enabled() {
            return;
        }
        let Some((mut st, focus)) = self.refresh_focus(st) else {
            return;
        };
        if (P::requires_focus() && st.focus.is_none()) || st.focus != focus || !P::input_allowed() {
            st.invalidate_text();
            return;
        }
        self.completion_tap(st);
    }

    /// The completion key was tapped: either step to the next guess in the
    /// cycle already running, or start one from the word in the buffer.
    fn completion_tap(self: &Arc<Self>, mut st: MutexGuard<'_, AppState<P>>) {
        if st.mode != crate::config::AppMode::Full
            || !crate::config::Config::global().complete_enabled
        {
            st.cycle = None;
            return;
        }
        let current = match &st.cycle {
            Some(cycle) => cycle
                .candidates
                .get(cycle.index)
                .cloned()
                .unwrap_or_else(|| reading::<P>(&cycle.typed, Language::English)),
            None => reading::<P>(&st.keys, Language::English),
        };
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
            Some(Commit::Undo {
                suppress: None,
                layout: None,
                rule: None,
            })
        } else if index == 0 {
            Some(Commit::Fix {
                from: was.clone(),
                to: candidates[index].clone(),
                kind: FixKind::Complete,
                rule: None,
                deferred_layout: None,
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
        let Some(suffix) = P::retype_text(&target.chars().skip(prefix).collect::<String>()) else {
            return;
        };
        // A completion can be taken back with the undo gesture too — except
        // when it has just handed back the user's own text, which is nothing to
        // undo.
        let undo = (!back_to_typed).then(|| LastFix {
            on_screen: P::retype_len(&retype) - prefix,
            restore: P::retype_original(&typed[prefix..], Language::English),
            terminator: None,
            layout: None,
            keep: typed.clone(),
            suppress: non_empty(was),
            rule: None,
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
                erase: erase - prefix,
                retype: suffix,
                terminator: None,
            },
            keep,
            undo,
            commit,
        );
    }

    /// An action modifier came back up. If it was a bare tap and the second one inside
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
    fn action_tap(self: &Arc<Self>, mut st: MutexGuard<'_, AppState<P>>, key: P::Key) {
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
        if !single && !shortcut_matches::<P>(&config.action_shortcut, key) {
            st.last_action_tap = None;
            return;
        }
        match st.last_action_tap.take() {
            _ if single => {}
            Some(prev) if now.duration_since(prev) <= DOUBLE_TAP_WINDOW => {}
            // First tap of a possible pair: remember it and wait for the second.
            _ => {
                st.last_action_tap = Some(now);
                return;
            }
        }

        if !self.control.is_enabled() {
            return;
        }
        if st.is_replacing {
            st.pending_undo = P::QUEUE_UNDO_DURING_REPLACEMENT;
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
        match st.last_action.take() {
            Some(LastAction::Fixed(fix)) => self.undo_fix(st, fix),
            Some(LastAction::Skipped(skip)) => self.unlist_and_correct(st, skip),
            Some(LastAction::Unchanged {
                keys,
                terminator,
                layout,
            }) if !single => {
                if P::current_layout() == Some(layout) {
                    self.manual_correct(st, keys, terminator, layout);
                }
            }
            None if !single && !st.no_fix && !st.keys.is_empty() => {
                if let Some(layout) = P::current_layout() {
                    let keys = st.keys.to_vec();
                    self.manual_correct(st, keys, None, layout);
                }
            }
            _ => {}
        }
    }

    fn rescue_selection(self: &Arc<Self>, mut st: MutexGuard<'_, AppState<P>>) {
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

    /// Explicit intent overrides automatic ambiguity guards, never dictionary validity.
    fn manual_correct(
        self: &Arc<Self>,
        mut st: MutexGuard<'_, AppState<P>>,
        keys: Vec<Typed<P::Key>>,
        terminator: Option<P::Key>,
        layout: Language,
    ) {
        let text = reading::<P>(&keys, layout.other());
        let manual =
            crate::dictionary::manual_layout(&text, layout.other(), self.en_dict, self.he_dict);
        let deferred_layout = manual.as_ref().map(|_| layout.other());
        let fix = manual.or_else(|| self.check(&keys, st.history.run(), st.mode).fix);
        let note = fix.as_ref().map(|fix| note_of::<P>(&keys, fix));
        let Some(rep) = replacement::<P>(&keys, fix) else {
            return;
        };
        let mut undo = undo_of::<P>(&keys, &rep, terminator);
        // Asking for a conversion is not training a word exception.
        undo.suppress = None;
        if terminator.is_none() {
            undo.keep = keys;
        }
        st.keys.clear();
        st.no_fix = terminator.is_none();
        self.start_replacement(
            st,
            Plan {
                erase: rep.erase + usize::from(terminator.is_some()),
                retype: rep.retype,
                terminator,
            },
            Vec::new(),
            Some(undo),
            note.map(|(from, to, kind)| Commit::Fix {
                from,
                to,
                kind,
                rule: None,
                deferred_layout,
            }),
        );
    }

    /// Put back what the user typed before the correction on screen replaced it.
    fn undo_fix(self: &Arc<Self>, mut st: MutexGuard<'_, AppState<P>>, fix: LastFix<P>) {
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
                layout: fix.layout,
                rule: fix.rule,
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
        let result = self.check(&skip.keys, st.history.run(), st.mode).fix;
        let note = result.as_ref().map(|fix| note_of::<P>(&skip.keys, fix));
        // An explicit request can also resolve ambiguity after unlisting.
        let Some(rep) = replacement::<P>(&skip.keys, result) else {
            if let Some(layout) = P::current_layout() {
                self.manual_correct(st, skip.keys, skip.terminator, layout);
            }
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
            note.map(|(from, to, kind)| Commit::Fix {
                from,
                to,
                kind,
                rule: None,
                deferred_layout: None,
            }),
        );
    }

    /// Run the pipelines over a finished word. `run` is the language of the
    /// words before it, which the caller reads off [`AppState::history`] while
    /// it still holds the lock.
    fn check(&self, keys: &[Typed<P::Key>], run: Run, mode: crate::config::AppMode) -> Outcome {
        check_and_correct(
            keys,
            |t: Typed<P::Key>| P::english_char(t.key, t.shift),
            |t: Typed<P::Key>| P::hebrew_char(t.key),
            |t: Typed<P::Key>| t.shift,
            run,
            self.en_dict,
            self.he_dict,
            P::current_layout(),
            mode == crate::config::AppMode::LayoutOnly,
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
        self: &Arc<Self>,
        plan: Plan<P>,
        keep: Vec<Typed<P::Key>>,
        undo: Option<LastFix<P>>,
        generation: u64,
        commit: Option<Commit>,
    ) {
        // Armed for the whole replacement: whatever happens below — including a
        // panic — `is_replacing` and the injecting gate are cleared, rather than
        // leaving the listener shut for the rest of the session.
        let gate = ReplaceGuard::new(&self.state, P::injecting_flag(&self.injector));

        if !self.replacement_valid(generation) {
            let mut st = self.lock();
            if st.generation == generation {
                st.invalidate_text();
            }
            return;
        }
        // Layout confirmation can block. Keep it off the capture callback and
        // outside the state lock so releases and cancellation remain responsive.
        let switch = match &commit {
            Some(Commit::Undo {
                layout: Some(lang), ..
            }) => Some((*lang, P::ABORT_UNDO_IF_LAYOUT_REFUSED)),
            Some(Commit::Fix {
                deferred_layout: Some(lang),
                ..
            }) => Some((*lang, true)),
            _ => None,
        };
        if let Some((lang, required)) = switch {
            let outcome = P::switch_layout_to(lang);
            if (required && !outcome.ready()) || !self.replacement_valid(generation) {
                let mut st = self.lock();
                if st.generation == generation {
                    st.invalidate_text();
                }
                return;
            }
        }
        let Some(buffered) = P::inject(self, plan, generation) else {
            let mut st = self.lock();
            if st.generation == generation {
                st.invalidate_text();
            }
            return;
        };
        let (practice, mode) = {
            let st = self.lock();
            (st.practice, st.mode)
        };
        let mut learn = None;
        match commit {
            Some(Commit::Fix {
                from,
                to,
                kind,
                rule,
                ..
            }) => {
                if practice {
                    crate::practice::fixed(&self.control, &from, &to, kind);
                }
                if !practice {
                    self.control.record_fix(&from, &to, kind);
                    if let Some(rule) = rule {
                        crate::personal::record_rule(rule, false);
                    }
                    if mode == crate::config::AppMode::Full {
                        crate::personal::record_confusion(&from, &to);
                        crate::personal::record_word(&to);
                    }
                }
            }
            Some(Commit::Undo { suppress, rule, .. }) => {
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
            None => {}
        }

        let mut st = self.lock();
        if st.generation == generation {
            st.keys.replace_with(keep);
            st.keys.extend(buffered.iter().copied());
            // Undo is only safe while no later text follows the replacement.
            st.last_action = if buffered.is_empty() {
                undo.map(LastAction::Fixed)
            } else {
                None
            };
            st.last_key = None;
        }
        drop(st);
        drop(gate);

        let mut st = self.lock();
        let pending = (!st.is_replacing && std::mem::take(&mut st.pending_undo))
            .then(|| st.last_action.take())
            .flatten();
        if let Some(LastAction::Fixed(fix)) = pending {
            self.undo_fix(st, fix);
        } else {
            drop(st);
        }
        // Persistence must not extend the injection window: real typing during
        // a slow disk write used to be treated as an interrupted replacement.
        if let Some(word) = learn {
            crate::complete::learn(&word);
        }
    }
}

enum Commit {
    Fix {
        from: String,
        to: String,
        kind: FixKind,
        rule: Option<&'static str>,
        deferred_layout: Option<Language>,
    },
    Undo {
        suppress: Option<String>,
        layout: Option<Language>,
        rule: Option<&'static str>,
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
    /// Start of the original word for undo learning, before prefix trimming.
    original_start: usize,
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
            original_start: start,
        }),
        Fix::LayoutSpelling { text, lang } => Some(Replacement {
            erase: keys.len(),
            retype: P::retype_text(&text)?,
            previous_layout: Some(lang.other()),
            original_start: 0,
        }),
        Fix::Spelling { text } => {
            // Keep the identical prefix on screen. This saves paced backspaces
            // and focus queries on macOS, for corrections and their undo alike.
            let original = reading::<P>(keys, Language::English);
            let prefix = original
                .chars()
                .zip(text.chars())
                .take_while(|(a, b)| a == b)
                .count();
            let suffix: String = text.chars().skip(prefix).collect();
            Some(Replacement {
                erase: keys.len() - prefix,
                retype: P::retype_text(&suffix)?,
                previous_layout: None,
                original_start: 0,
            })
        }
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
        suppress: non_empty(reading::<P>(&keys[rep.original_start..], was)),
        rule: None,
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

    static SELECTION: Mutex<Option<Selection>> = Mutex::new(None);
    static FOCUS: AtomicUsize = AtomicUsize::new(1);
    static SIMULATED_LAYOUT: AtomicUsize = AtomicUsize::new(0);
    static INPUT_ALLOWED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
    static ALLOW_LAYOUT_SWITCH: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);
    struct FocusGate {
        ready: mpsc::SyncSender<()>,
        proceed: mpsc::Receiver<()>,
    }
    static FOCUS_GATE: Mutex<Option<FocusGate>> = Mutex::new(None);
    static LAYOUT_GATE: Mutex<Option<FocusGate>> = Mutex::new(None);
    struct Simulated;
    struct Screen {
        text: Mutex<String>,
        erasures: Mutex<Vec<usize>>,
        injecting: std::sync::atomic::AtomicBool,
        ready: mpsc::SyncSender<()>,
        proceed: Mutex<mpsc::Receiver<()>>,
    }
    impl Platform for Simulated {
        type Key = char;
        type Retype = String;
        type Injector = Screen;
        type Focus = usize;
        const REQUIRES_FOCUS: bool = true;
        const QUEUE_UNDO_DURING_REPLACEMENT: bool = true;
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
            let gate = FOCUS_GATE.lock().unwrap().take();
            if let Some(gate) = gate {
                gate.ready.send(()).unwrap();
                gate.proceed.recv_timeout(Duration::from_secs(3)).unwrap();
            }
            let focus = FOCUS.load(Ordering::SeqCst);
            (focus != 0).then_some(focus)
        }
        fn app_id(focus: &usize) -> Option<String> {
            Some(if *focus == 2 { "secret.exe" } else { "Editor" }.to_string())
        }
        fn is_own_focus(focus: &usize) -> bool {
            *focus == 3
        }
        fn input_allowed() -> bool {
            INPUT_ALLOWED.load(Ordering::SeqCst)
        }
        fn selection(_: &usize) -> Option<Selection> {
            SELECTION.lock().unwrap().clone()
        }
        fn replace_selection(
            engine: &Engine<Self>,
            _: &usize,
            expected: &Selection,
            text: &str,
            generation: u64,
        ) -> Option<Selection> {
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
            let mut selection = SELECTION.lock().unwrap();
            if selection.as_ref() != Some(expected) {
                return None;
            }
            let mut screen = engine.injector.text.lock().unwrap();
            let mut utf16: Vec<_> = screen.encode_utf16().collect();
            utf16.splice(
                expected.start as usize..(expected.start + expected.length) as usize,
                text.encode_utf16(),
            );
            *screen = String::from_utf16(&utf16).unwrap();
            let after = Selection {
                start: expected.start,
                length: text.encode_utf16().count() as isize,
                text: text.to_owned(),
            };
            *selection = Some(after.clone());
            Some(after)
        }
        fn input_empty(screen: &Screen) -> bool {
            screen.text.lock().unwrap().is_empty()
        }
        fn current_layout() -> Option<Language> {
            match SIMULATED_LAYOUT.load(Ordering::SeqCst) {
                0 => Some(Language::English),
                1 => Some(Language::Hebrew),
                _ => None,
            }
        }
        fn switch_layout_to(lang: Language) -> crate::layout::LayoutSwitch {
            assert!(
                ALLOW_LAYOUT_SWITCH.load(Ordering::SeqCst),
                "English spelling and undo must not switch layouts"
            );
            if let Some(gate) = LAYOUT_GATE.lock().unwrap().take() {
                gate.ready.send(()).unwrap();
                gate.proceed.recv_timeout(Duration::from_secs(3)).unwrap();
            }
            SIMULATED_LAYOUT.store(usize::from(lang == Language::Hebrew), Ordering::SeqCst);
            crate::layout::LayoutSwitch::Switched
        }
        fn inject(
            engine: &Engine<Self>,
            plan: Plan<Self>,
            generation: u64,
        ) -> Option<Vec<Typed<char>>> {
            engine.injector.erasures.lock().unwrap().push(plan.erase);
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
                    erasures: Mutex::new(Vec::new()),
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

    #[cfg(target_os = "linux")]
    #[test]
    fn unplugging_cancels_pending_work_and_releases_device_keys() {
        let s = Session::new();
        let generation = {
            let mut st = s.engine.lock();
            st.held_keys.extend([Simulated::CTRL_LEFT, 'x']);
            st.keys.push(Typed {
                key: 'a',
                shift: false,
            });
            st.action_down = Some(Instant::now());
            st.completion_down = Some(Instant::now());
            st.generation
        };
        s.engine
            .input_device_removed(&HashSet::from([Simulated::CTRL_LEFT]));
        let st = s.engine.lock();
        assert_ne!(st.generation, generation);
        assert!(st.keys.is_empty() && st.no_fix);
        assert_eq!(st.held_keys, HashSet::from(['x']));
        assert!(st.action_down.is_none() && st.completion_down.is_none());
    }

    #[test]
    fn spelling_rewrites_only_the_changed_suffix_and_undo_learns_the_whole_word() {
        for (before, after, erase) in [
            ("keyboad", "keyboard", 1),
            ("recieve", "receive", 4),
            ("Recieve,", "Receive,", 5),
            ("abc", "abcdef", 0),
            ("abc", "ab", 1),
            ("ab", "世界", 2),
            ("hélo", "héllo", 1),
        ] {
            let keys: Vec<_> = before
                .chars()
                .map(|key| Typed { key, shift: false })
                .collect();
            let rep = replacement::<Simulated>(&keys, Some(Fix::Spelling { text: after.into() }))
                .unwrap();
            assert_eq!(rep.erase, erase, "{before}");
            let undo = undo_of::<Simulated>(&keys, &rep, Some(' '));
            let prefix: String = before.chars().take(keys.len() - rep.erase).collect();
            assert_eq!(format!("{prefix}{}", rep.retype), after);
            assert_eq!(format!("{prefix}{}", undo.restore), before);
            assert_eq!(undo.suppress.as_deref(), Some(before));
            assert_eq!(undo.on_screen, rep.retype.chars().count() + 1);
        }
    }

    #[test]
    fn typing_correction_undo_and_interrupted_replacements() {
        // Undo changes process-wide suppression and learning state. Run this
        // scenario alone so it cannot change the parallel corpus test's results.
        const CHILD: &str = "RECAST_ENGINE_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "platform::engine::tests::typing_correction_undo_and_interrupted_replacements",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
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

        // An eager undo during injection waits for the correction to finish;
        // neither Ctrl tap may invalidate a partly written word.
        for shortcut in ["none", "right_ctrl"] {
            crate::config::Config::update_live(|cfg| cfg.undo_shortcut = shortcut.into());
            let s = Session::new();
            s.type_text("acheive ");
            s.pending();
            s.engine.injector.injecting.store(true, Ordering::SeqCst);
            if shortcut == "right_ctrl" {
                s.tap(Simulated::CTRL_RIGHT);
            } else {
                s.tap(Simulated::CTRL_LEFT);
                s.tap(Simulated::CTRL_LEFT);
            }
            assert!(s.engine.lock().pending_undo);
            s.proceed.send(()).unwrap();
            s.pending();
            assert_eq!(s.text(), "achieve ");
            s.finish();
            assert_eq!(s.text(), "acheive ");
            assert_eq!(s.engine.control.undo_count(), 1);
            crate::complete::unlist("acheive");
            // A delayed learning write must not resurrect a newer unlist.
            crate::complete::learn("acheive");
            crate::complete::learn("acheive");
            assert!(!crate::complete::learned("acheive"));
        }
        crate::config::Config::update_live(|cfg| cfg.undo_shortcut = "none".into());

        // Cursor changes and later typing still cancel an eager undo.
        for click in [false, true] {
            let s = Session::new();
            s.type_text("recieve ");
            s.pending();
            s.tap(Simulated::CTRL_LEFT);
            s.tap(Simulated::CTRL_LEFT);
            assert!(s.engine.lock().pending_undo);
            if click {
                s.engine.mouse_click();
            } else {
                s.type_text("next");
            }
            s.finish();
            assert_eq!(s.text(), if click { "recieve " } else { "receive next" });
            assert_eq!(s.engine.control.undo_count(), 0);
            assert!(!s.engine.lock().pending_undo);
        }

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

        // A complete second word arriving during injection must remain intact;
        // its terminator must not let undo erase either word afterward.
        let s = Session::new();
        s.type_text("recieve ");
        s.pending();
        s.type_text("next ");
        s.finish();
        s.tap(Simulated::CTRL_LEFT);
        s.tap(Simulated::CTRL_LEFT);
        assert_eq!(s.text(), "receive next ");
        assert!(!s.engine.lock().is_replacing);
        assert_eq!(s.engine.control.undo_count(), 0);

        // Pausing or disabling while a worker is waiting cancels that rewrite.
        for pause in [true, false] {
            let s = Session::new();
            s.type_text("recieve ");
            s.pending();
            if pause {
                s.engine.control.pause_for(Duration::from_secs(60));
            } else {
                s.engine.control.set_enabled(false);
            }
            s.finish();
            assert_eq!(s.text(), "recieve ");
            assert_eq!(s.engine.control.fixed_count(), 0);
            assert!(s.engine.lock().last_action.is_none());
        }

        // Focus can move after a successful correction, before an undo gesture.
        let s = Session::new();
        s.type_text("recieve ");
        s.pending();
        s.finish();
        FOCUS.store(2, Ordering::SeqCst);
        s.tap(Simulated::CTRL_LEFT);
        s.tap(Simulated::CTRL_LEFT);
        assert_eq!(s.text(), "receive ");
        assert!(!s.engine.lock().is_replacing);
        assert_eq!(s.engine.control.undo_count(), 0);
        FOCUS.store(1, Ordering::SeqCst);

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
        assert_eq!(
            *s.engine.injector.erasures.lock().unwrap(),
            [0],
            "first completion must append without deleting the prefix"
        );
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

        // Live settings must also stop an already cached completion cycle.
        s.tap(Simulated::SHIFT_RIGHT);
        s.pending();
        s.finish();
        let completed = s.text();
        crate::config::Config::update_live(|cfg| cfg.complete_enabled = false);
        s.tap(Simulated::SHIFT_RIGHT);
        assert!(!s.engine.lock().is_replacing);
        assert_eq!(s.text(), completed);
        assert!(s.engine.lock().cycle.is_none());
        // Turning completion off must still let the user undo its last offer.
        s.tap(Simulated::CTRL_LEFT);
        s.tap(Simulated::CTRL_LEFT);
        s.pending();
        s.finish();
        assert_eq!(s.text(), "hel");
        crate::config::Config::update_live(|cfg| cfg.complete_enabled = true);

        // Expansions keep their full text across cycles even though the live
        // word buffer may contain only the last word. Undo restores the prefix.
        let s = Session::new();
        s.type_text("btw");
        {
            let mut st = s.engine.lock();
            st.cycle = Some(Cycle {
                typed: st.keys.to_vec(),
                candidates: vec!["by the way".into(), "between".into()],
                index: 2,
                on_screen: 3,
            });
        }
        for expected in ["by the way", "between", "btw", "by the way"] {
            s.tap(Simulated::SHIFT_RIGHT);
            s.pending();
            s.finish();
            assert_eq!(s.text(), expected);
        }
        s.tap(Simulated::CTRL_LEFT);
        s.tap(Simulated::CTRL_LEFT);
        s.pending();
        s.finish();
        assert_eq!(s.text(), "btw");

        // Exclusions stop capture before any planner, debug log or learning call.
        // A new exclusion also cancels a correction planned before the UI change.
        let s = Session::new();
        s.type_text("recieve ");
        s.pending();
        *lock_forgiving(&s.engine.control.excluded_apps) = vec!["editor".into()];
        s.finish();
        assert_eq!(s.text(), "recieve ");
        lock_forgiving(&s.engine.control.excluded_apps).clear();
        s.type_text(" recieve ");
        s.pending();
        s.finish();
        assert_eq!(s.text(), "recieve  receive ");

        for focus in [0, 2] {
            let s = Session::new();
            *lock_forgiving(&s.engine.control.excluded_apps) = vec!["secret.exe".into()];
            FOCUS.store(focus, Ordering::SeqCst);
            s.type_text("recieve hello akuo ");
            s.type_text("hel");
            s.tap(Simulated::SHIFT_RIGHT);
            s.tap(Simulated::CTRL_LEFT);
            s.tap(Simulated::CTRL_LEFT);
            let st = s.engine.lock();
            assert!(st.keys.is_empty() && st.buffered_keys.is_empty());
            assert!(!st.is_replacing && st.last_action.is_none());
            assert!(st.held_keys.is_empty());
            assert_eq!(s.engine.control.fixed_count(), 0);
        }
        FOCUS.store(1, Ordering::SeqCst);
        let s = Session::new();
        *lock_forgiving(&s.engine.control.excluded_apps) = vec!["secret.exe".into()];
        s.type_text("recieve ");
        s.pending();
        // A focus change while the injection worker waits still cancels it.
        FOCUS.store(2, Ordering::SeqCst);
        s.finish();
        assert_eq!(s.text(), "recieve ");
        FOCUS.store(1, Ordering::SeqCst);
        s.type_text(" ");
        s.type_text("recieve ");
        s.pending();
        s.finish();
        assert_eq!(s.text(), "recieve  receive ");

        let s = Session::new();
        *lock_forgiving(&s.engine.control.excluded_apps) = vec!["secret.exe".into()];
        s.type_text("recieve ");
        s.pending();
        // With exclusions active, new typing cancels pending work immediately
        // instead of delaying buffered-key accounting behind an app query.
        s.type_text("x");
        s.finish();
        assert_eq!(s.text(), "recieve x");
        assert_eq!(s.engine.control.fixed_count(), 0);

        // Temporary app pause cancels pending injection and blocks all capture.
        let s = Session::new();
        s.type_text("recieve ");
        s.pending();
        s.engine.control.pause_in_app("EDITOR");
        s.finish();
        assert_eq!(s.text(), "recieve ");
        for focus in [1, 0, 3, 1] {
            FOCUS.store(focus, Ordering::SeqCst);
            s.type_text("recieve hel");
            s.tap(Simulated::SHIFT_RIGHT);
            s.tap(Simulated::CTRL_LEFT);
            s.tap(Simulated::CTRL_LEFT);
            let st = s.engine.lock();
            assert!(st.keys.is_empty() && st.buffered_keys.is_empty());
            assert!(!st.is_replacing && st.last_action.is_none());
            assert_eq!(s.engine.control.paused_app().as_deref(), Some("editor"));
        }
        assert_eq!(s.engine.control.fixed_count(), 0);
        s.engine
            .control
            .listener_ready
            .store(true, Ordering::Relaxed);
        assert!(
            crate::platform::status_for::<Simulated>(&s.engine.control, None)
                .starts_with("Paused in editor")
        );
        // The existing status poll notices leaving even without a keystroke.
        FOCUS.store(2, Ordering::SeqCst);
        assert!(
            crate::platform::status_for::<Simulated>(&s.engine.control, None).starts_with("Active")
        );
        assert!(s.engine.control.paused_app().is_none());
        FOCUS.store(1, Ordering::SeqCst);
        s.type_text(" recieve ");
        s.pending();
        s.finish();
        assert!(s.text().ends_with(" receive "));
        // Expiration and manual resume never remove saved restrictions.
        *lock_forgiving(&s.engine.control.excluded_apps) = vec!["editor".into()];
        s.engine.control.pause_in_app("editor");
        FOCUS.store(2, Ordering::SeqCst);
        s.type_text(" ");
        assert!(s.engine.control.paused_app().is_none());
        FOCUS.store(1, Ordering::SeqCst);
        s.engine.control.pause_in_app("editor");
        s.engine.control.resume_app();
        s.type_text("recieve ");
        assert!(!s.engine.lock().is_replacing);
        assert!(s.text().ends_with("recieve "));
        assert_eq!(
            s.engine.control.app_mode(Some("editor")),
            Some(crate::config::AppMode::Off)
        );

        // Switching to layout-only cancels an already-planned spelling fix.
        let s = Session::new();
        s.type_text("recieve ");
        s.pending();
        *lock_forgiving(&s.engine.control.layout_only_apps) = vec!["editor".into()];
        s.finish();
        assert_eq!(s.text(), "recieve ");
        s.type_text(" recieve keyb");
        s.tap(Simulated::SHIFT_RIGHT);
        assert!(!s.engine.lock().is_replacing);
        assert_eq!(s.text(), "recieve  recieve keyb");
        s.engine
            .control
            .listener_ready
            .store(true, Ordering::Relaxed);
        let application = ("Text Editor".into(), "editor".into());
        assert!(
            crate::platform::status_for::<Simulated>(&s.engine.control, Some(&application))
                .starts_with("Active in Text Editor · Layout only —")
        );
        let stale_application = ("Other App".into(), "other".into());
        assert!(crate::platform::status_for::<Simulated>(
            &s.engine.control,
            Some(&stale_application)
        )
        .starts_with("Active in Editor · Layout only —"));
        SIMULATED_LAYOUT.store(2, Ordering::SeqCst);
        assert!(
            crate::platform::status_for::<Simulated>(&s.engine.control, None)
                .starts_with("Keyboard layout unavailable")
        );
        SIMULATED_LAYOUT.store(0, Ordering::SeqCst);
        FOCUS.store(0, Ordering::SeqCst);
        assert!(
            crate::platform::status_for::<Simulated>(&s.engine.control, None)
                .starts_with("Text focus unavailable —")
        );
        assert!(
            crate::platform::status_for::<Simulated>(&s.engine.control, Some(&application))
                .starts_with("Enabled in Text Editor · Layout only —")
        );
        lock_forgiving(&s.engine.control.layout_only_apps).clear();
        assert!(
            crate::platform::status_for::<Simulated>(&s.engine.control, Some(&application))
                .starts_with("Enabled in Text Editor · Full correction —")
        );
        s.type_text(" recieve ");
        assert!(
            !s.engine.lock().is_replacing,
            "status must not bypass missing focus"
        );
        *lock_forgiving(&s.engine.control.excluded_apps) = vec!["editor".into()];
        assert!(
            crate::platform::status_for::<Simulated>(&s.engine.control, Some(&application))
                .starts_with("Excluded application —")
        );
        lock_forgiving(&s.engine.control.excluded_apps).clear();
        // Knowing the frontmost app is not permission to bypass missing focus
        // or end a pause whose actual focused application is still unknown.
        s.engine.control.pause_in_app("secret.exe");
        assert!(
            crate::platform::status_for::<Simulated>(&s.engine.control, Some(&application))
                .starts_with("Paused in secret.exe")
        );
        assert_eq!(s.engine.control.paused_app().as_deref(), Some("secret.exe"));
        s.engine.control.resume_app();
        FOCUS.store(1, Ordering::SeqCst);

        // Single-tap undo uses the same focus and modifier-chord guards.
        INPUT_ALLOWED.store(false, Ordering::SeqCst);
        assert!(
            crate::platform::status_for::<Simulated>(&s.engine.control, None)
                .contains("Secure Input")
        );
        INPUT_ALLOWED.store(true, Ordering::SeqCst);
        s.engine.control.pause_for(Duration::from_secs(60));
        assert!(
            crate::platform::status_for::<Simulated>(&s.engine.control, None)
                .starts_with("Paused · 1 min remaining —")
        );
        let disabled =
            AppControl::new_with_config_and_state(crate::config::Config::global(), false);
        disabled.pause_for(Duration::from_secs(60));
        assert!(crate::platform::status_for::<Simulated>(&disabled, None)
            .starts_with("Disabled until you enable it —"));
        s.engine.control.resume();
        *lock_forgiving(&s.engine.control.excluded_apps) = vec!["editor".into()];
        assert!(
            crate::platform::status_for::<Simulated>(&s.engine.control, None)
                .starts_with("Excluded")
        );

        // Layout-only still fixes the layout, but never expands abbreviations.
        ALLOW_LAYOUT_SWITCH.store(true, Ordering::SeqCst);
        let s = Session::new();
        *lock_forgiving(&s.engine.control.layout_only_apps) = vec!["editor".into()];
        s.type_text(" akuo ");
        s.pending();
        s.finish();
        assert_eq!(s.text(), " שלום ");
        SIMULATED_LAYOUT.store(0, Ordering::SeqCst);
        ALLOW_LAYOUT_SWITCH.store(false, Ordering::SeqCst);
        let abbrev = crate::complete::user_path("abbrev.txt").unwrap();
        std::fs::create_dir_all(abbrev.parent().unwrap()).unwrap();
        std::fs::write(&abbrev, "zzpractice = should not expand\n").unwrap();
        crate::complete::reload_user_files();
        s.type_text("zzpractice teh ");
        assert!(!s.engine.lock().is_replacing);
        assert!(s.text().ends_with("zzpractice teh "));
        std::fs::remove_file(abbrev).unwrap();
        crate::complete::reload_user_files();

        crate::config::Config::update_live(|cfg| cfg.undo_shortcut = "right_ctrl".into());
        let s = Session::new();
        s.type_text("acheive ");
        s.pending();
        s.finish();
        s.tap(Simulated::CTRL_RIGHT);
        s.pending();
        s.finish();
        assert_eq!(s.text(), "acheive ");
        assert_eq!(s.engine.control.undo_count(), 1);
        let s = Session::new();
        s.type_text("recieve ");
        s.pending();
        s.finish();
        s.engine.key_press(Simulated::CTRL_RIGHT);
        s.tap('v');
        s.engine.key_release(Simulated::CTRL_RIGHT);
        assert!(!s.engine.lock().is_replacing);
        assert_eq!(s.engine.control.undo_count(), 0);
        let s = Session::new();
        s.type_text("recieve ");
        s.pending();
        s.finish();
        FOCUS.store(2, Ordering::SeqCst);
        s.tap(Simulated::CTRL_RIGHT);
        assert!(!s.engine.lock().is_replacing);
        assert_eq!(s.engine.control.undo_count(), 0);
        crate::config::Config::update_live(|cfg| cfg.undo_shortcut = "none".into());

        // Practice runs actual layout correction, undo, and completion without learning.
        FOCUS.store(3, Ordering::SeqCst);
        ALLOW_LAYOUT_SWITCH.store(true, Ordering::SeqCst);
        let s = Session::new();
        s.engine
            .control
            .practice_open
            .store(true, Ordering::Relaxed);
        s.type_text("akuo ");
        s.pending();
        s.finish();
        assert_eq!(s.text(), "שלום ");
        assert_eq!(s.engine.control.practice_stage.load(Ordering::Relaxed), 1);
        s.tap(Simulated::CTRL_LEFT);
        s.tap(Simulated::CTRL_LEFT);
        s.pending();
        s.finish();
        assert_eq!(s.text(), "akuo ");
        assert_eq!(s.engine.control.practice_stage.load(Ordering::Relaxed), 2);
        assert!(!crate::complete::suppressed("akuo"));
        assert!(!crate::complete::learned("akuo"));
        s.engine.mouse_click();
        s.engine.injector.text.lock().unwrap().clear();
        s.type_text(" keyb");
        s.tap(Simulated::SHIFT_RIGHT);
        s.pending();
        s.finish();
        assert_eq!(s.text(), " keyboard");
        assert_eq!(s.engine.control.practice_stage.load(Ordering::Relaxed), 3);
        assert_eq!(s.engine.control.fixed_count(), 0);
        assert_eq!(s.engine.control.undo_count(), 0);
        ALLOW_LAYOUT_SWITCH.store(false, Ordering::SeqCst);
        FOCUS.store(1, Ordering::SeqCst);

        // Undo layout confirmation runs on the worker, without the typing
        // lock. A click while it stalls must prevent destructive injection.
        ALLOW_LAYOUT_SWITCH.store(true, Ordering::SeqCst);
        SIMULATED_LAYOUT.store(0, Ordering::SeqCst);
        let s = Session::new();
        s.type_text("akuo ");
        s.pending();
        s.finish();
        let (ready, waiting) = mpsc::sync_channel(1);
        let (proceed, reply) = mpsc::sync_channel(1);
        *LAYOUT_GATE.lock().unwrap() = Some(FocusGate {
            ready,
            proceed: reply,
        });
        s.tap(Simulated::CTRL_LEFT);
        s.tap(Simulated::CTRL_LEFT);
        waiting.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(
            s.engine.state.try_lock().is_ok(),
            "layout wait holds the typing lock"
        );
        s.engine.mouse_click();
        proceed.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while s.engine.lock().is_replacing {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(s.text(), "שלום ");
        assert_eq!(s.engine.control.undo_count(), 0);
        assert!(s.ready.try_recv().is_err());
        ALLOW_LAYOUT_SWITCH.store(false, Ordering::SeqCst);
        SIMULATED_LAYOUT.store(0, Ordering::SeqCst);

        // Hold the OS reply indefinitely: releases and clicks still run, and
        // the late reply must not resurrect the canceled word. This used to
        // hold state.lock() for the entire focus query (up to 250 ms on Linux).
        let s = Session::new();
        *lock_forgiving(&s.engine.control.excluded_apps) = vec!["secret.exe".into()];
        let (ready, waiting) = mpsc::sync_channel(1);
        let (proceed, reply) = mpsc::sync_channel(1);
        *FOCUS_GATE.lock().unwrap() = Some(FocusGate {
            ready,
            proceed: reply,
        });
        let engine = Arc::clone(&s.engine);
        let worker = thread::spawn(move || engine.key_press('r'));
        waiting.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(
            s.engine.state.try_lock().is_ok(),
            "OS query holds the typing lock"
        );
        let started = Instant::now();
        s.engine.key_release('r');
        s.engine.mouse_click();
        eprintln!(
            "release + cancellation during stalled focus query: {:?}",
            started.elapsed()
        );
        proceed.send(()).unwrap();
        worker.join().unwrap();
        assert!(s.engine.lock().keys.is_empty());
        assert!(s.engine.lock().held_keys.is_empty());

        // All ambiguous words use the same explicit conversion rule, before
        // or after a terminator, and in either direction.
        ALLOW_LAYOUT_SWITCH.store(true, Ordering::SeqCst);
        for (english, hebrew) in [("do", "גם"), ("go", "עם"), ("to", "אם")] {
            for reverse in [false, true] {
                for suffix in ["", " "] {
                    SIMULATED_LAYOUT.store(usize::from(reverse), Ordering::SeqCst);
                    let s = Session::new();
                    s.type_text(&format!("{english}{suffix}"));
                    let (before, after) = if reverse {
                        (hebrew, english)
                    } else {
                        (english, hebrew)
                    };
                    *s.engine.injector.text.lock().unwrap() = format!("{before}{suffix}");
                    assert!(!s.engine.lock().is_replacing);
                    s.tap(Simulated::CTRL_LEFT);
                    s.tap(Simulated::CTRL_LEFT);
                    s.pending();
                    s.finish();
                    assert_eq!(s.text(), format!("{after}{suffix}"));
                    s.tap(Simulated::CTRL_LEFT);
                    s.tap(Simulated::CTRL_LEFT);
                    s.pending();
                    s.finish();
                    assert_eq!(s.text(), format!("{before}{suffix}"));
                    assert!(!crate::complete::suppressed(before));
                }
            }
        }
        SIMULATED_LAYOUT.store(0, Ordering::SeqCst);
        crate::config::Config::update_live(|cfg| cfg.undo_shortcut = "left_ctrl".into());
        let s = Session::new();
        s.type_text("do ");
        s.tap(Simulated::CTRL_LEFT);
        assert!(!s.engine.lock().is_replacing);
        s.tap(Simulated::CTRL_LEFT);
        s.pending();
        s.finish();
        assert_eq!(s.text(), "גם ");
        s.tap(Simulated::CTRL_LEFT);
        s.pending();
        s.finish();
        assert_eq!(s.text(), "do ");
        crate::config::Config::update_live(|cfg| cfg.undo_shortcut = "none".into());
        for interruption in ["none", "click", "focus", "disabled", "secure"] {
            let s = Session::new();
            s.type_text(if interruption == "none" {
                "zzzzqqqq "
            } else {
                "do "
            });
            let original = s.text();
            match interruption {
                "click" => s.engine.mouse_click(),
                "focus" => {
                    FOCUS.store(2, Ordering::SeqCst);
                }
                "disabled" => s.engine.control.set_enabled(false),
                "secure" => INPUT_ALLOWED.store(false, Ordering::SeqCst),
                _ => {}
            }
            s.tap(Simulated::CTRL_LEFT);
            s.tap(Simulated::CTRL_LEFT);
            // No native selection exists, so a selection query is harmless.
            let deadline = Instant::now() + Duration::from_secs(3);
            while s.engine.lock().is_replacing {
                assert!(Instant::now() < deadline);
                thread::sleep(Duration::from_millis(1));
            }
            assert_eq!(s.text(), original);
            assert!(s.ready.try_recv().is_err());
            FOCUS.store(1, Ordering::SeqCst);
            INPUT_ALLOWED.store(true, Ordering::SeqCst);
        }
        for interruption in ["none", "click", "typing", "focus", "selection"] {
            let s = Session::new();
            let original = "prefix AKUO GUKO 🙂 suffix";
            *s.engine.injector.text.lock().unwrap() = original.into();
            *SELECTION.lock().unwrap() = Some(Selection {
                start: 7,
                length: 9,
                text: "AKUO GUKO".into(),
            });
            s.engine.mouse_click();
            s.tap(Simulated::CTRL_LEFT);
            s.tap(Simulated::CTRL_LEFT);
            s.pending();
            match interruption {
                "click" => s.engine.mouse_click(),
                "typing" => s.engine.key_press('x'),
                "focus" => {
                    FOCUS.store(2, Ordering::SeqCst);
                }
                "selection" => {
                    *SELECTION.lock().unwrap() = None;
                }
                _ => {}
            }
            s.finish();
            if interruption == "none" {
                assert_eq!(s.text(), "prefix שלום עולם 🙂 suffix");
                s.tap(Simulated::CTRL_LEFT);
                s.tap(Simulated::CTRL_LEFT);
                s.pending();
                s.finish();
                assert_eq!(s.text(), original);
            } else {
                assert_eq!(s.text(), original);
            }
            FOCUS.store(1, Ordering::SeqCst);
            *SELECTION.lock().unwrap() = None;
        }
        // Rebinding exercises the same screen/injection path as the defaults.
        for (binding, action_key, completion_binding, completion_key) in [
            (
                "left_shift",
                Simulated::SHIFT_LEFT,
                "right_ctrl",
                Simulated::CTRL_RIGHT,
            ),
            (
                "right_shift",
                Simulated::SHIFT_RIGHT,
                "left_ctrl",
                Simulated::CTRL_LEFT,
            ),
            (
                "left_ctrl",
                Simulated::CTRL_LEFT,
                "left_shift",
                Simulated::SHIFT_LEFT,
            ),
            (
                "right_ctrl",
                Simulated::CTRL_RIGHT,
                "right_shift",
                Simulated::SHIFT_RIGHT,
            ),
        ] {
            crate::config::Config::update_live(|cfg| {
                cfg.action_shortcut = binding.into();
                cfg.completion_shortcut = completion_binding.into();
                cfg.undo_shortcut = "none".into();
            });
            SIMULATED_LAYOUT.store(0, Ordering::SeqCst);
            let s = Session::new();
            s.type_text("keyb");
            // A held key, a repeated press, or a chord must not complete.
            s.engine.key_press(completion_key);
            s.engine.lock().completion_down =
                Some(Instant::now() - TAP_MAX - Duration::from_millis(1));
            s.engine.key_release(completion_key);
            assert!(!s.engine.lock().is_replacing);
            s.engine.key_press(completion_key);
            s.engine.key_press(completion_key);
            s.engine.key_release(completion_key);
            assert!(!s.engine.lock().is_replacing);
            s.tap(completion_key);
            s.pending();
            s.finish();
            assert_eq!(s.text(), "keyboard");
            s.tap(action_key);
            assert!(!s.engine.lock().is_replacing);
            s.tap(action_key);
            s.pending();
            s.finish();
            assert_eq!(s.text(), "keyb");

            let s = Session::new();
            s.type_text("do ");
            s.engine.key_press(action_key);
            s.engine.lock().action_down = Some(Instant::now() - TAP_MAX - Duration::from_millis(1));
            s.engine.key_release(action_key);
            s.tap(action_key);
            assert!(!s.engine.lock().is_replacing);
            s.tap(action_key);
            s.pending();
            s.finish();
            assert_eq!(s.text(), "גם ");
            s.tap(action_key);
            s.tap(action_key);
            s.pending();
            s.finish();
            assert_eq!(s.text(), "do ");

            let s = Session::new();
            s.type_text("keyb");
            s.engine.key_press(completion_key);
            s.engine.key_press(action_key);
            s.engine.key_release(action_key);
            s.engine.key_release(completion_key);
            assert!(!s.engine.lock().is_replacing);
            assert!(s.ready.try_recv().is_err());
        }
        // Disable both shortcuts, then ensure changing a binding mid-tap
        // cannot reinterpret an old key-down or the first half of a pair.
        crate::config::Config::update_live(|cfg| {
            cfg.action_shortcut = "none".into();
            cfg.completion_shortcut = "none".into();
        });
        let s = Session::new();
        s.type_text("keyb");
        for key in [
            Simulated::CTRL_LEFT,
            Simulated::CTRL_RIGHT,
            Simulated::SHIFT_LEFT,
            Simulated::SHIFT_RIGHT,
        ] {
            s.tap(key);
            s.tap(key);
        }
        assert!(!s.engine.lock().is_replacing);
        s.engine.key_press(Simulated::SHIFT_LEFT);
        crate::config::Config::update_live(|cfg| cfg.completion_shortcut = "left_shift".into());
        s.engine.key_release(Simulated::SHIFT_LEFT);
        assert!(!s.engine.lock().is_replacing);
        crate::config::Config::update_live(|cfg| cfg.action_shortcut = "ctrl".into());
        let s = Session::new();
        s.type_text("do ");
        s.tap(Simulated::CTRL_LEFT);
        crate::config::Config::update_live(|cfg| cfg.action_shortcut = "left_ctrl".into());
        s.tap(Simulated::CTRL_LEFT);
        assert!(!s.engine.lock().is_replacing);
        s.tap(Simulated::CTRL_LEFT);
        s.pending();
        s.finish();
        assert_eq!(s.text(), "גם ");
        crate::config::Config::update_live(|cfg| {
            cfg.action_shortcut = "ctrl".into();
            cfg.completion_shortcut = "right_shift".into();
        });
        ALLOW_LAYOUT_SWITCH.store(false, Ordering::SeqCst);
    }
}
