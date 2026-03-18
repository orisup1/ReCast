use crate::types::Language;

// ─────────────────────────────────────────────────────────────────────────────
// Linux: switch layout via hyprctl
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(target_os = "linux")]
pub fn switch_layout_to(lang: Language) -> bool {
    use std::process::Command;

    // First check what layout we are currently on to avoid infinite loops and
    // unnecessary delays.
    if let Ok(output) = Command::new("hyprctl").args(&["devices", "-j"]).output() {
        if let Ok(stdout) = String::from_utf8(output.stdout) {
            let mut is_currently_hebrew = false;
            let mut is_currently_english = false;

            for block in stdout.split('{') {
                if block.contains("\"main\": true") || block.contains("\"main\":true") {
                    if let Some(idx) = block.find("\"active_keymap\":") {
                        let remainder = &block[idx + 16..];
                        if let Some(start) = remainder.find('"') {
                            let val_remainder = &remainder[start + 1..];
                            if let Some(end) = val_remainder.find('"') {
                                let keymap = val_remainder[..end].to_lowercase();
                                if keymap.contains("hebrew") || keymap.contains("il") {
                                    is_currently_hebrew = true;
                                } else if keymap.contains("english") || keymap.contains("us") {
                                    is_currently_english = true;
                                }
                            }
                        }
                    }
                }
            }

            if lang == Language::English && is_currently_english {
                return false; // Already in English
            }
            if lang == Language::Hebrew && is_currently_hebrew {
                return false; // Already in Hebrew
            }
        }
    }

    let index = match lang {
        Language::English => "0",
        Language::Hebrew => "1",
    };
    match Command::new("hyprctl")
        .args(&["switchxkblayout", "all", index])
        .status()
    {
        Ok(status) => status.success(),
        Err(e) => {
            eprintln!("Failed to switch layout using hyprctl: {}", e);
            false
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// macOS: switch layout via TIS (Carbon framework)
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(target_os = "macos")]
use core_foundation::base::TCFType;
#[cfg(target_os = "macos")]
use core_foundation::string::CFString;
#[cfg(target_os = "macos")]
use core_foundation_sys::base::CFTypeRef;
#[cfg(target_os = "macos")]
use core_foundation_sys::string::CFStringRef;

#[cfg(target_os = "macos")]
#[repr(C)]
struct __TISInputSource;
#[cfg(target_os = "macos")]
type TISInputSourceRef = *mut __TISInputSource;

#[cfg(target_os = "macos")]
#[link(name = "Carbon", kind = "framework")]
extern "C" {
    fn TISCopyInputSourceForLanguage(language: CFStringRef) -> TISInputSourceRef;
    fn TISSelectInputSource(source: TISInputSourceRef) -> i32;
    fn TISCopyCurrentKeyboardInputSource() -> TISInputSourceRef;
    fn CFRelease(cf: CFTypeRef);
}

#[cfg(target_os = "macos")]
pub fn switch_layout_to(lang: Language) -> bool {
    let code = match lang {
        Language::English => "en",
        Language::Hebrew => "he",
    };
    unsafe {
        let cf_lang = CFString::new(code);
        let src = TISCopyInputSourceForLanguage(cf_lang.as_concrete_TypeRef());
        if src.is_null() {
            eprintln!("No input source found for language code '{}'", code);
            return false;
        }
        let current_src = TISCopyCurrentKeyboardInputSource();
        let mut switched = false;
        if current_src.is_null()
            || core_foundation_sys::base::CFEqual(
                src as CFTypeRef,
                current_src as CFTypeRef,
            ) == 0
        {
            let status = TISSelectInputSource(src);
            if status != 0 {
                eprintln!(
                    "TISSelectInputSource failed for '{}' with status {}",
                    code, status
                );
            } else {
                switched = true;
            }
        }
        if !current_src.is_null() {
            CFRelease(current_src as CFTypeRef);
        }
        CFRelease(src as CFTypeRef);
        switched
    }
}
