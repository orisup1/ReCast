//! Which keyboard layout is live, and how to change it.
//!
//! The decision core anchors on the *current* layout — a key sequence that
//! already reads as a real word in the layout the user is typing in is left
//! alone — so this is not a detail of the injection path but an input to every
//! correction. When it answers `None` the pipelines fall back to a
//! layout-agnostic decision, which is strictly worse; when it answers *wrongly*
//! a correction is typed out under the layout that was actually live and
//! arrives as garbage. Both failure modes are in the per-OS modules below.
//!
//! One file per platform for the same reason the capture backends are split:
//! the three have nothing in common but this file's cache and vocabulary.
//! Linux is by far the largest of them, because "the current layout" is a
//! question with five different answers depending on what is running the
//! session.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::types::Language;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "linux")]
pub use linux::switch_layout_to;
#[cfg(target_os = "macos")]
pub use macos::switch_layout_to;
#[cfg(target_os = "windows")]
pub use windows::switch_layout_to;

/// What ReCast can do about the layout on this system, for `--status`.
///
/// Worth reporting because the answer is "nothing" on more setups than one
/// would like, and the symptom of that is not an error but a feature quietly
/// only half working: the speller still fixes typos, and layout mistypes go
/// through untouched.
#[cfg(target_os = "linux")]
pub use linux::describe_backend;

/// Follow the session's layout-change notifications, where it has any.
///
/// Linux only, and only for the backends that can push: it is the one platform
/// where asking can mean a subprocess, and the one where the answer comes from
/// something other than the OS. macOS and Windows both answer from a library
/// call cheap enough that the cache alone is the whole of the optimisation.
#[cfg(target_os = "linux")]
pub use linux::spawn_watcher;

#[cfg(not(target_os = "linux"))]
pub fn spawn_watcher() {}

#[cfg(not(target_os = "linux"))]
pub fn describe_backend() -> String {
    // macOS and Windows each have exactly one way to do this, and it is part of
    // the OS rather than of the session.
    if cfg!(target_os = "macos") {
        "Carbon TISSelectInputSource".to_string()
    } else {
        "Win32 keyboard layout activation".to_string()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Current-layout query (shared) — the signal that lets the dictionary anchor its
// decision on what the user is *actually* typing in, instead of guessing from
// the keystrokes alone.
// ─────────────────────────────────────────────────────────────────────────────

// Brief cache so the per-word lookup doesn't hammer the OS — notably Linux,
// where a query can mean a socket round trip or a subprocess. 300 ms is short
// enough that a manual layout change is picked up almost immediately, long
// enough to absorb a burst of words. Our own `switch_layout_to` updates the
// cache directly, so self-initiated switches are reflected with no staleness.
static LAYOUT_CACHE: Mutex<Option<(Instant, Language)>> = Mutex::new(None);
const LAYOUT_TTL: Duration = Duration::from_millis(300);

/// What happened when a correction asked for a layout.
///
/// This used to be a `bool`, and the bool conflated the two things a caller
/// most needs to tell apart: "already on that layout, nothing to do" and "the
/// switch did not happen". Both were `false`. The undo path
/// (`platform::linux`) reads it as *may I retype now?* and bailed out on
/// `false` — so undoing a correction whose layout happened to already be
/// right did nothing at all, silently, and looked exactly like a missed
/// keystroke.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutSwitch {
    /// Already on the requested layout. Nothing was sent, and nothing needed
    /// to be — this is success.
    AlreadyThere,
    /// Switched, and the OS confirmed the new layout is live.
    Switched,
    /// Refused, or never took effect before the deadline. Retyping now would
    /// spell the word out under the wrong layout.
    Failed,
}

impl LayoutSwitch {
    /// Whether the requested layout is live, so keys may be injected.
    ///
    /// Read only on Linux, and that is not an oversight: `uinput` speaks
    /// keycodes, so what a replayed key *spells* depends on the layout being
    /// live when it lands, and injecting early produces garbage. macOS and
    /// Windows insert the restored word as text, which is layout-independent —
    /// they switch the layout purely so the user's *next* keystroke is in the
    /// right language, and a refusal there costs nothing worth abandoning an
    /// undo over.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn ready(self) -> bool {
        !matches!(self, LayoutSwitch::Failed)
    }

    /// Whether this call is what changed the layout — for the debug log, which
    /// reports what happened rather than what is now true.
    pub fn changed(self) -> bool {
        matches!(self, LayoutSwitch::Switched)
    }
}

/// Best-effort current keyboard layout. `None` when it can't be determined;
/// callers then fall back to a layout-agnostic decision.
pub fn current_layout() -> Option<Language> {
    if let Ok(guard) = LAYOUT_CACHE.lock() {
        if let Some((t, l)) = *guard {
            if t.elapsed() < LAYOUT_TTL {
                return Some(l);
            }
        }
    }
    let fresh = query_layout();
    if let Some(l) = fresh {
        set_layout_cache(l);
    }
    fresh
}

fn set_layout_cache(lang: Language) {
    if let Ok(mut guard) = LAYOUT_CACHE.lock() {
        *guard = Some((Instant::now(), lang));
    }
}

/// Forget the cached layout, so the next question goes to the OS.
///
/// The cache exists because a per-word query can be a socket round trip or a
/// subprocess, and it is right almost all of the time — our own switches update
/// it directly, and nothing else changes the layout except the user. But the
/// user changing it *by hand* is exactly the case the TTL handles badly: for up
/// to [`LAYOUT_TTL`] afterwards the anchor is stale, and a stale anchor does not
/// merely miss a correction. It decides against the wrong layout and then injects
/// under the live one, which is the garbage the whole program exists to prevent.
///
/// The listener sees every keystroke, so it can see the *shape* of a
/// layout-switch hotkey — two modifiers, or a modifier and space — and say so
/// here. It cannot know whether that combination is bound to anything, and it
/// does not need to: the cost of being wrong is one extra query.
pub fn invalidate() {
    if let Ok(mut guard) = LAYOUT_CACHE.lock() {
        *guard = None;
    }
}

#[cfg(target_os = "linux")]
use linux::query_layout;
#[cfg(target_os = "macos")]
use macos::query_layout;
#[cfg(target_os = "windows")]
use windows::query_layout;

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn query_layout() -> Option<Language> {
    None
}

/// The language an xkb layout code or a human-readable keymap name describes.
///
/// Both spellings turn up depending on who is being asked: Hyprland and X11
/// report `us,il`, sway and KDE report `English (US)` / `Hebrew`, GNOME reports
/// `('xkb', 'il')`. Matching on either is what lets one function serve all of
/// them.
///
/// `None` for anything else, which is the honest answer and not a fallback: a
/// third layout in the user's list is neither English nor Hebrew, and guessing
/// would put a correction into a language ReCast has no dictionary for.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn language_of_keymap(keymap: &str) -> Option<Language> {
    let keymap = keymap.trim().to_lowercase();
    // Word-ish matching for the two-letter codes: a bare `il` or a variant like
    // `il(biblical)` is Hebrew, but `mil` and `brasil` are nobody's layout code
    // and `us` must not match `belarus`.
    let code = keymap
        .split(|c: char| !c.is_ascii_alphanumeric())
        .next()
        .unwrap_or("");
    if keymap.contains("hebrew") || code == "il" || code == "he" || code == "iw" {
        Some(Language::Hebrew)
    } else if keymap.contains("english") || code == "us" || code == "en" || code == "gb" {
        Some(Language::English)
    } else {
        None
    }
}

/// Track consecutive layout query failures. When the layout backend cannot
/// determine the current layout (returns None), we increment a counter. After
/// a threshold, we warn the user once that they may need to set
/// RECAST_LAYOUT_BACKEND.
const LAYOUT_FAILURE_THRESHOLD: u32 = 50;

static LAYOUT_FAILURES: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static LAYOUT_WARNING_EMITTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Record a layout query failure. If failures exceed the threshold and no
/// warning has been emitted yet, print a hint to stderr.
pub fn record_layout_failure() {
    let failures = LAYOUT_FAILURES.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    if failures >= LAYOUT_FAILURE_THRESHOLD
        && !LAYOUT_WARNING_EMITTED.load(std::sync::atomic::Ordering::Relaxed)
    {
        LAYOUT_WARNING_EMITTED.store(true, std::sync::atomic::Ordering::Relaxed);
        eprintln!(
            "recast: layout query failed {} times — cannot determine current keyboard layout.\n\
             If you are on Linux, try setting RECAST_LAYOUT_BACKEND=hyprland|sway|kde|gnome|x11\n\
             (run `recast --status` to see what backend was detected).",
            failures
        );
    }
}

/// Reset the layout failure counter on a successful query.
pub fn reset_layout_failures() {
    LAYOUT_FAILURES.store(0, std::sync::atomic::Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn already_being_there_is_success_not_failure() {
        // The distinction the undo path turned on: only `Failed` may stop a
        // retype. This is what a bare bool could not express.
        assert!(LayoutSwitch::AlreadyThere.ready());
        assert!(LayoutSwitch::Switched.ready());
        assert!(!LayoutSwitch::Failed.ready());
        // ...and only an actual switch counts as a change, for the debug log.
        assert!(!LayoutSwitch::AlreadyThere.changed());
        assert!(LayoutSwitch::Switched.changed());
    }

    #[test]
    fn invalidating_sends_the_next_question_back_to_the_os() {
        set_layout_cache(Language::Hebrew);
        assert_eq!(
            LAYOUT_CACHE.lock().unwrap().map(|(_, lang)| lang),
            Some(Language::Hebrew),
            "the cache holds what it was told"
        );
        invalidate();
        assert!(
            LAYOUT_CACHE.lock().unwrap().is_none(),
            "a hand-switched layout must not be answered from the old cache"
        );
    }

    #[test]
    fn both_spellings_of_a_layout_name_are_understood() {
        // xkb codes, as Hyprland and X11 report them.
        assert_eq!(language_of_keymap("us"), Some(Language::English));
        assert_eq!(language_of_keymap("il"), Some(Language::Hebrew));
        // Human names, as sway and KDE report them.
        assert_eq!(language_of_keymap("English (US)"), Some(Language::English));
        assert_eq!(language_of_keymap("Hebrew"), Some(Language::Hebrew));
        // Variants keep their base language.
        assert_eq!(language_of_keymap("il(biblical)"), Some(Language::Hebrew));
        assert_eq!(language_of_keymap("us(dvorak)"), Some(Language::English));
    }

    #[test]
    fn a_third_layout_is_not_guessed_at() {
        // A user with us,il,ru gets three entries and only two of them are
        // ours. Answering "English" for the Russian one would have ReCast
        // switch to a layout it cannot spell in.
        assert_eq!(language_of_keymap("ru"), None);
        assert_eq!(language_of_keymap("Greek"), None);
        assert_eq!(language_of_keymap(""), None);
        // Substrings that used to match, back when this was `contains("us")`:
        // `belarus` would have read as English and `mil` as Hebrew.
        assert_eq!(language_of_keymap("by"), None);
        assert_eq!(language_of_keymap("Belarusian"), None);
    }
}
