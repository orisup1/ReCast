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

/// Carbon input-source calls assert main-queue ownership on current macOS.
/// Workers hold no engine/cache lock while waiting; main-thread callers run inline.
fn on_main<T: Send>(work: impl FnOnce() -> T + Send) -> T {
    use objc::{class, msg_send, sel, sel_impl};
    let main: bool = unsafe { msg_send![class!(NSThread), isMainThread] };
    if main {
        work()
    } else {
        dispatch::Queue::main().exec_sync(work)
    }
}

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
    static kTISPropertyInputSourceIsEnabled: CFStringRef;
    static kTISPropertyUnicodeKeyLayoutData: CFStringRef;
    fn UCKeyTranslate(
        layout: *const std::ffi::c_void,
        key: u16,
        action: u16,
        modifiers: u32,
        keyboard: u32,
        options: u32,
        dead: *mut u32,
        capacity: u32,
        length: *mut u32,
        text: *mut u16,
    ) -> i32;
    fn LMGetKbdType() -> u8;
    fn CFRelease(cf: CFTypeRef);
}

/// Compare the live layout's physical keys with the capture adapter's mapping.
/// Dead keys and multi-character outputs cannot be represented by the word buffer.
pub fn mapping_matches(expected: impl Fn(u16, bool, Language) -> Option<char> + Send) -> bool {
    on_main(move || unsafe {
        use core_foundation_sys::data::{CFDataGetBytePtr, CFDataGetLength, CFDataRef};
        let source = TISCopyCurrentKeyboardInputSource();
        if source.is_null() {
            return false;
        }
        let data = TISGetInputSourceProperty(source, kTISPropertyUnicodeKeyLayoutData) as CFDataRef;
        let language = source_language(source);
        let matches = !data.is_null()
            && CFDataGetLength(data) > 0
            && language.is_some_and(|language| {
                (0..51).all(|key| {
                    [false, true].into_iter().all(|shift| {
                        let Some(character) = expected(key, shift, language) else {
                            return true;
                        };
                        let (mut dead, mut length, mut text) = (0, 0, [0u16; 8]);
                        UCKeyTranslate(
                            CFDataGetBytePtr(data).cast(),
                            key,
                            0,
                            if shift { 2 } else { 0 },
                            LMGetKbdType() as u32,
                            0,
                            &mut dead,
                            8,
                            &mut length,
                            text.as_mut_ptr(),
                        ) == 0
                            && dead == 0
                            && length == 1
                            && u32::from(text[0]) == character as u32
                    })
                })
            });
        CFRelease(source as CFTypeRef);
        matches
    })
}

pub fn enabled_languages() -> (bool, bool) {
    on_main(enabled_languages_on_main)
}

fn enabled_languages_on_main() -> (bool, bool) {
    let enabled = |code| unsafe {
        let language = CFString::new(code);
        let source = TISCopyInputSourceForLanguage(language.as_concrete_TypeRef());
        if source.is_null() {
            return false;
        }
        let value = TISGetInputSourceProperty(source, kTISPropertyInputSourceIsEnabled);
        let result =
            !value.is_null() && core_foundation_sys::number::CFBooleanGetValue(value.cast());
        CFRelease(source as CFTypeRef);
        result
    };
    (enabled("en"), enabled("he"))
}

pub fn switch_layout_to(lang: Language) -> LayoutSwitch {
    use std::time::{Duration, Instant};

    // Already on the target layout. Uses the language-based detection so any
    // English/Hebrew *variant* counts.
    if super::fresh_layout() == Some(lang) {
        return LayoutSwitch::AlreadyThere;
    }

    let code = match lang {
        Language::English => "en",
        Language::Hebrew => "he",
    };
    let selected = on_main(move || unsafe {
        let cf_lang = CFString::new(code);
        let src = TISCopyInputSourceForLanguage(cf_lang.as_concrete_TypeRef());
        if src.is_null() {
            eprintln!("No input source found for language code '{code}'");
            return false;
        }
        let status = TISSelectInputSource(src);
        CFRelease(src as CFTypeRef);
        if status != 0 {
            eprintln!("TISSelectInputSource failed for '{code}' with status {status}");
        }
        status == 0
    });
    if !selected {
        return LayoutSwitch::Failed;
    }

    // Poll outside the dispatched closure so a worker never sleeps on the
    // main queue. Read the actual source, not the cached pre-switch layout.
    let deadline = Instant::now() + Duration::from_millis(300);
    loop {
        if query_layout() == Some(lang) {
            set_layout_cache(lang);
            return LayoutSwitch::Switched;
        }
        if Instant::now() >= deadline {
            return LayoutSwitch::Failed;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

pub fn query_layout() -> Option<Language> {
    on_main(query_layout_on_main)
}

fn query_layout_on_main() -> Option<Language> {
    unsafe {
        let source = TISCopyCurrentKeyboardInputSource();
        if source.is_null() {
            return None;
        }
        let result = source_language(source);
        CFRelease(source as CFTypeRef);
        result
    }
}

unsafe fn source_language(cur: TISInputSourceRef) -> Option<Language> {
    use core_foundation_sys::array::{CFArrayGetCount, CFArrayGetValueAtIndex, CFArrayRef};

    unsafe {
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
        result
    }
}
