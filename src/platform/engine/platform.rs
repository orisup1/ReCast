//! Capture, focus, and injection contract implemented by each operating system.

use super::{Engine, Plan, Typed};
use crate::types::Language;
use std::hash::Hash;
use std::time::Duration;

/// Keep explicit accessibility edits and their undo payload bounded.
pub const MAX_SELECTION_BYTES: usize = 64 * 1024;

/// Native selection offsets (UTF-16 on macOS/Windows, Unicode scalars on AT-SPI), plus exact text.
#[derive(Clone, Debug, PartialEq)]
pub struct Selection {
    pub start: isize,
    pub length: isize,
    pub text: String,
}

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
    /// Fresh native validation, called without the engine lock before editing.
    fn replacement_allowed() -> bool {
        Self::input_allowed()
    }
    fn input_block_reason() -> &'static str {
        "Secure Input — correction resumes when secure entry ends."
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

    /// Space or Enter — the keys that finish a word. Enter is never replayed.
    fn is_terminator(key: Self::Key) -> bool;

    /// Enter ends phrase context even when a correction is still pending.
    fn is_line_break(key: Self::Key) -> bool;

    /// Cursor and focus keys, and (on Linux, where they arrive as keys) mouse
    /// buttons. They end the current word without checking it, so a stale
    /// buffer cannot leak into the next one.
    fn is_reset(key: Self::Key) -> bool;

    /// Mouse buttons may arrive on a keyboard event stream, as on Linux.
    /// A click starts fresh capture instead of marking the next word incomplete.
    fn is_mouse_button(_key: Self::Key) -> bool {
        false
    }

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
    /// punctuation. macOS and Windows build the string [`super::reading`] gives.
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

    /// Completion and in-language spelling stay in the supplied layout,
    /// including Hebrew on keycode injectors.
    fn retype_completion(text: &str, _lang: Language) -> Option<Self::Retype> {
        Self::retype_text(text)
    }

    /// How many characters this will put on screen. Every key produces exactly
    /// one character, which is what lets an erase count be a key count.
    fn retype_len(retype: &Self::Retype) -> usize;

    /// Join injectable payloads without losing the original separator keys.
    fn append_retype(retype: &mut Self::Retype, tail: Self::Retype);

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

    /// The injection lifecycle flag, when this platform has one. Native capture
    /// backends distinguish our injected events from physical input themselves;
    /// Linux filters by device name and has no flag here.
    fn injecting_flag(injector: &Self::Injector) -> Option<&std::sync::atomic::AtomicBool>;

    /// Release deferred native input. True means later input made text context stale.
    fn injection_finished() -> bool {
        false
    }

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
