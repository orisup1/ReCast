//! Reading and setting the keyboard layout on macOS, through Carbon's Text
//! Input Sources API.
//!
//! One way to do it and it is part of the OS, so unlike Linux there is nothing
//! to probe for: `TISCopyInputSourceForLanguage` finds a source that enters a
//! given language and `TISSelectInputSource` makes it current.

use core_foundation::base::TCFType;
use core_foundation::string::CFString;
use core_foundation_sys::base::CFTypeRef;
use core_foundation_sys::string::CFStringRef;

use super::{set_layout_cache, LayoutSwitch};
use crate::types::Language;

#[repr(C)]
struct __TISInputSource {
    _private: [u8; 0],
}
type TISInputSourceRef = *mut __TISInputSource;

#[link(name = "Carbon", kind = "framework")]
extern "C" {
    fn TISCopyInputSourceForLanguage(language: CFStringRef) -> TISInputSourceRef;
    fn TISSelectInputSource(source: TISInputSourceRef) -> i32;
    fn TISCopyCurrentKeyboardInputSource() -> TISInputSourceRef;
    // Read a property of an input source. The returned value follows the Get
    // rule (not owned — must NOT be released).
    fn TISGetInputSourceProperty(
        source: TISInputSourceRef,
        key: CFStringRef,
    ) -> *const std::ffi::c_void;
    // The list of language codes ("en", "he", "iw", …) an input source enters.
    static kTISPropertyInputSourceLanguages: CFStringRef;
    fn CFRelease(cf: CFTypeRef);
}

pub fn switch_layout_to(lang: Language) -> LayoutSwitch {
    use std::time::{Duration, Instant};

    // Already on the target layout. Uses the language-based detection so any
    // English/Hebrew *variant* counts.
    if super::current_layout() == Some(lang) {
        return LayoutSwitch::AlreadyThere;
    }

    let code = match lang {
        Language::English => "en",
        Language::Hebrew => "he",
    };
    unsafe {
        let cf_lang = CFString::new(code);
        let src = TISCopyInputSourceForLanguage(cf_lang.as_concrete_TypeRef());
        if src.is_null() {
            eprintln!("No input source found for language code '{code}'");
            return LayoutSwitch::Failed;
        }

        let status = TISSelectInputSource(src);
        if status != 0 {
            eprintln!("TISSelectInputSource failed for '{code}' with status {status}");
            CFRelease(src as CFTypeRef);
            return LayoutSwitch::Failed;
        }

        // TISSelectInputSource is asynchronous: the focused app does not see
        // the new layout the instant the call returns. If we retype before the
        // switch propagates, the injected keys are interpreted under the OLD
        // layout and the "corrected" word comes out as garbage. Poll the
        // current input source until it actually equals the target (or a
        // deadline elapses), so callers can retype immediately afterwards —
        // parity with the Linux/Windows pollers.
        let deadline = Instant::now() + Duration::from_millis(300);
        let mut landed;
        loop {
            let cur = TISCopyCurrentKeyboardInputSource();
            landed = !cur.is_null()
                && core_foundation_sys::base::CFEqual(src as CFTypeRef, cur as CFTypeRef) != 0;
            if !cur.is_null() {
                CFRelease(cur as CFTypeRef);
            }
            if landed || Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }

        CFRelease(src as CFTypeRef);
        if landed {
            set_layout_cache(lang);
        }
        // Only report success once the switch is confirmed. On timeout this is
        // `Failed`, so the caller skips the retype rather than typing the word
        // out under the old layout (garbage) — parity with the other two.
        if landed {
            LayoutSwitch::Switched
        } else {
            LayoutSwitch::Failed
        }
    }
}

pub fn query_layout() -> Option<Language> {
    use core_foundation_sys::array::{CFArrayGetCount, CFArrayGetValueAtIndex, CFArrayRef};

    unsafe {
        let cur = TISCopyCurrentKeyboardInputSource();
        if cur.is_null() {
            return None;
        }
        // Inspect the *current* source's own language list rather than testing
        // it for equality against the default "en"/"he" source. A user on any
        // English variant (ABC, British, Colemak, Dvorak…) has a current source
        // that is not equal to the canonical "en" source, so the old equality
        // test returned None for them and silently disabled layout anchoring.
        // The languages array lists the primary language first.
        let langs = TISGetInputSourceProperty(cur, kTISPropertyInputSourceLanguages) as CFArrayRef;
        let mut result = None;
        if !langs.is_null() {
            let count = CFArrayGetCount(langs);
            for i in 0..count {
                let value = CFArrayGetValueAtIndex(langs, i) as CFStringRef;
                if value.is_null() {
                    continue;
                }
                let code = CFString::wrap_under_get_rule(value).to_string();
                // Hebrew is "he" (modern) or "iw" (legacy ISO code).
                if code.starts_with("he") || code.starts_with("iw") {
                    result = Some(Language::Hebrew);
                    break;
                }
                if code.starts_with("en") {
                    result = Some(Language::English);
                    break;
                }
            }
        }
        CFRelease(cur as CFTypeRef);
        result
    }
}
