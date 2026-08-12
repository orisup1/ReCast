//! The half of [`Platform`](super::engine::Platform) that macOS and Windows
//! share.
//!
//! Both capture `rdev::Key` and both insert corrections as *text* — macOS
//! through `CGEventKeyboardSetUnicodeString`, Windows through one `SendInput`
//! batch of `KEYEVENTF_UNICODE` code units — so every question the engine asks
//! that is not "how do I put this on screen" has the same answer on the two of
//! them. Only the injection and the startup path genuinely differ, and those
//! stay in `macos.rs` and `windows.rs`.
//!
//! Kept as free functions rather than a blanket impl: the two platforms are
//! still separate `Platform` types with their own `inject`, and one-line
//! delegations from each of them are greppable in a way a macro-generated impl
//! is not.

use rdev::Key;

use super::engine::Typed;
use crate::keymap::{
    english_char_to_key, key_to_english_char, key_to_english_char_shifted, key_to_hebrew_char,
};

/// Space or Enter — the keys that finish a word.
pub fn is_terminator(key: Key) -> bool {
    matches!(key, Key::Space | Key::Return)
}

/// Cursor and focus keys. They end the current word without checking it, so a
/// stale buffer doesn't leak into the next word.
///
/// Mouse buttons are not here: macOS and Windows both deliver those as their
/// own event kind, and their listeners call
/// [`Engine::mouse_click`](super::engine::Engine::mouse_click) instead. Linux
/// sees them as ordinary keys on the same evdev stream and lists them in its
/// own `is_reset`.
pub fn is_reset(key: Key) -> bool {
    matches!(
        key,
        Key::Tab
            | Key::Escape
            | Key::LeftArrow
            | Key::RightArrow
            | Key::UpArrow
            | Key::DownArrow
            | Key::Home
            | Key::End
            | Key::PageUp
            | Key::PageDown
            | Key::Insert
            | Key::Delete
    )
}

pub fn english_char(key: Key, shift: bool) -> Option<char> {
    key_to_english_char_shifted(key, shift)
}

pub fn english_char_plain(key: Key) -> Option<char> {
    key_to_english_char(key)
}

pub fn hebrew_char(key: Key) -> Option<char> {
    key_to_hebrew_char(key)
}

/// Any text at all — a layout fix's finished word, a spelling correction, a
/// completion. Nothing here can fail: unlike Linux, which has to find a key for
/// every character, these platforms type the characters themselves.
///
/// A layout fix's `text` already carries the capitalization the user typed, so
/// inserting characters rather than key positions means there is no shift to
/// replay.
pub fn retype_text(text: &str) -> Option<String> {
    Some(text.to_string())
}

/// Characters, not bytes — the corrected word may be Hebrew, and the count is
/// what the erase is computed from.
pub fn retype_len(text: &str) -> usize {
    text.chars().count()
}

/// The word buffer that matches `text` now being on screen. Only the last word
/// of it is still in progress — an abbreviation expansion may carry spaces —
/// and anything the English layout can't type is dropped rather than guessed at.
pub fn buffer_after(text: &str) -> Vec<Typed<Key>> {
    text.rsplit(' ')
        .next()
        .unwrap_or_default()
        .chars()
        .filter_map(|c| english_char_to_key(c).map(|(key, shift)| Typed { key, shift }))
        .collect()
}

/// The keys the state machine names, in rdev's spelling.
pub const SHIFT_LEFT: Key = Key::ShiftLeft;
pub const SHIFT_RIGHT: Key = Key::ShiftRight;
pub const CTRL_LEFT: Key = Key::ControlLeft;
pub const CTRL_RIGHT: Key = Key::ControlRight;
pub const CAPS_LOCK: Key = Key::CapsLock;
pub const BACKSPACE: Key = Key::Backspace;
