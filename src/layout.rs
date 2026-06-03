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
// Windows: switch layout via HKL activation (LoadKeyboardLayoutW)
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(target_os = "windows")]
pub fn switch_layout_to(lang: Language) -> bool {
    use std::ffi::c_void;
    use std::thread;
    use std::time::Duration;

    type DWORD = u32;
    type HKL = isize;
    type HWND = *mut c_void;
    type WPARAM = usize;
    type LPARAM = isize;
    type BOOL = i32;

    const KLF_ACTIVATE: u32 = 0x00000001;
    const WM_INPUTLANGCHANGEREQUEST: u32 = 0x0050;
    const INPUTLANGCHANGE_SYSCHARSET: WPARAM = 0x0001;

    extern "system" {
        fn GetForegroundWindow() -> HWND;
        fn GetWindowThreadProcessId(hWnd: HWND, lpdwProcessId: *mut DWORD) -> DWORD;
        fn GetCurrentThreadId() -> DWORD;
        fn GetKeyboardLayout(idThread: DWORD) -> HKL;
        fn PostMessageW(hWnd: HWND, Msg: u32, wParam: WPARAM, lParam: LPARAM) -> BOOL;
        fn GetKeyboardLayoutList(nBuff: i32, lpList: *mut HKL) -> i32;
        fn ActivateKeyboardLayout(hkl: HKL, Flags: u32) -> HKL;
        fn LoadKeyboardLayoutW(pwszKLID: *const u16, Flags: u32) -> HKL;
    }

    // NOTE: KLID strings vary by Windows / keyboard layout variant.
    // For example, Hebrew Standard is commonly `0002040d` (not `0000040d`).
    let (desired_langid, klids): (u16, &[&str]) = match lang {
        // English (United States)
        Language::English => (0x0409u16, &["00000409"]),
        // Hebrew (Israel)
        Language::Hebrew => (0x040du16, &["0002040d", "0000040d"]),
    };

    unsafe {
        // Determine the active keyboard layout of the foreground window's thread.
        let hwnd = GetForegroundWindow();
        let tid = if !hwnd.is_null() {
            let mut pid: DWORD = 0;
            GetWindowThreadProcessId(hwnd, &mut pid)
        } else {
            GetCurrentThreadId()
        };

        // Pre-switch check: if the foreground window is already on the
        // requested layout, skip the switch entirely. Mirrors the Linux
        // hyprctl-based early-exit so check_and_switch_candidates returns
        // false (no replacement) when typing in the correct layout already.
        let current_hkl = GetKeyboardLayout(tid);
        let current_langid = (current_hkl as usize & 0xFFFF) as u16;
        if current_langid == desired_langid {
            return false;
        }

        // Find an installed keyboard layout whose LANGID matches.
        let mut installed: Vec<HKL> = vec![0 as HKL; 64];
        let count = GetKeyboardLayoutList(installed.len() as i32, installed.as_mut_ptr());
        let installed_hkl = if count > 0 {
            installed[..(count as usize)]
                .iter()
                .copied()
                .find(|h| (*h as usize & 0xFFFF) as u16 == desired_langid)
        } else {
            None
        };

        let target_hkl: HKL = if let Some(hkl) = installed_hkl {
            hkl
        } else {
            // Fallback: try to load/activate known KLIDs.
            let mut loaded_hkl: HKL = 0;
            for klid in klids {
                let wide: Vec<u16> =
                    klid.encode_utf16().chain(std::iter::once(0)).collect();
                let hkl = LoadKeyboardLayoutW(wide.as_ptr(), KLF_ACTIVATE);
                if hkl != 0 {
                    loaded_hkl = hkl;
                    break;
                }
            }
            loaded_hkl
        };

        if target_hkl == 0 {
            return false;
        }

        // Prefer notifying the focused window (foreground thread) to switch.
        // This is more reliable than ActivateKeyboardLayout alone.
        let posted_ok = if !hwnd.is_null() {
            PostMessageW(
                hwnd,
                WM_INPUTLANGCHANGEREQUEST,
                INPUTLANGCHANGE_SYSCHARSET,
                target_hkl as LPARAM,
            ) != 0
        } else {
            false
        };

        if !posted_ok {
            // Fallback: activate for current thread (may not affect the
            // foreground app, but keeps behavior best-effort).
            let hkl = ActivateKeyboardLayout(target_hkl, KLF_ACTIVATE);
            if hkl == 0 {
                return false;
            }
        }

        // Poll for the input subsystem to apply the change instead of a fixed
        // pessimistic sleep. Returns as soon as the layout flips, capped at 180 ms.
        let deadline = std::time::Instant::now() + Duration::from_millis(180);
        loop {
            let updated_hkl = GetKeyboardLayout(tid);
            let updated_langid = (updated_hkl as usize & 0xFFFF) as u16;
            if updated_langid == desired_langid {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(2));
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
    use std::time::{Duration, Instant};

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

        // Already on the target layout — nothing to switch, and (matching the
        // Linux/Windows early-exit) report "no switch performed".
        let current_src = TISCopyCurrentKeyboardInputSource();
        let already_target = !current_src.is_null()
            && core_foundation_sys::base::CFEqual(src as CFTypeRef, current_src as CFTypeRef) != 0;
        if !current_src.is_null() {
            CFRelease(current_src as CFTypeRef);
        }
        if already_target {
            CFRelease(src as CFTypeRef);
            return false;
        }

        let status = TISSelectInputSource(src);
        if status != 0 {
            eprintln!(
                "TISSelectInputSource failed for '{}' with status {}",
                code, status
            );
            CFRelease(src as CFTypeRef);
            return false;
        }

        // TISSelectInputSource is asynchronous: the focused app does not see
        // the new layout the instant the call returns. If we retype before the
        // switch propagates, the injected keys are interpreted under the OLD
        // layout and the "corrected" word comes out as garbage. Poll the
        // current input source until it actually equals the target (or a
        // deadline elapses), so callers can retype immediately afterwards —
        // parity with the Linux/Windows pollers.
        let deadline = Instant::now() + Duration::from_millis(300);
        let mut landed = false;
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
        // Only report success once the switch is confirmed. On timeout, return
        // false so the caller skips the retype rather than typing the word out
        // under the old layout (garbage) — parity with the Windows poller.
        landed
    }
}
