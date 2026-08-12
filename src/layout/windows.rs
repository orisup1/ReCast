//! Reading and setting the keyboard layout on Windows.
//!
//! The Win32 spellings are kept exactly as the API documents them — `HKL`,
//! `HWND`, `DWORD` — because these declarations have to be checked against
//! Microsoft's headers by eye, and a renamed `Hkl` makes that harder for the
//! one reader who ever needs to.
#![allow(clippy::upper_case_acronyms)]

use super::{set_layout_cache, LayoutSwitch};
use crate::types::Language;

pub fn query_layout() -> Option<Language> {
    use std::ffi::c_void;
    type DWORD = u32;
    type HKL = isize;
    type HWND = *mut c_void;
    extern "system" {
        fn GetForegroundWindow() -> HWND;
        fn GetWindowThreadProcessId(hWnd: HWND, lpdwProcessId: *mut DWORD) -> DWORD;
        fn GetCurrentThreadId() -> DWORD;
        fn GetKeyboardLayout(idThread: DWORD) -> HKL;
    }
    unsafe {
        let hwnd = GetForegroundWindow();
        let tid = if !hwnd.is_null() {
            let mut pid: DWORD = 0;
            GetWindowThreadProcessId(hwnd, &mut pid)
        } else {
            GetCurrentThreadId()
        };
        let langid = (GetKeyboardLayout(tid) as usize & 0xFFFF) as u16;
        // The low 10 bits are the *primary* language; the high 6 are the
        // sublanguage (regional variant). Match on the primary id so every
        // English variant (en-US 0x0409, en-GB 0x0809, …) and every Hebrew
        // variant counts, instead of only the two canonical US/IL layouts.
        match langid & 0x03ff {
            0x0d => Some(Language::Hebrew),
            0x09 => Some(Language::English),
            _ => None,
        }
    }
}

pub fn switch_layout_to(lang: Language) -> LayoutSwitch {
    use std::ffi::c_void;

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
        // early-exit so no replacement happens when typing in the correct
        // layout already. Compare on the *primary* language (low 10 bits) so
        // every regional variant counts — an en-GB user is already "English"
        // and must not be flipped to en-US, and a he-IL variant must satisfy a
        // Hebrew request.
        let desired_primary = desired_langid & 0x03ff;
        let current_hkl = GetKeyboardLayout(tid);
        let current_langid = (current_hkl as usize & 0xFFFF) as u16;
        if current_langid & 0x03ff == desired_primary {
            return LayoutSwitch::AlreadyThere;
        }

        // Find an installed keyboard layout whose primary language matches —
        // prefer the user's own installed variant over loading a new one.
        let mut installed: Vec<HKL> = vec![0 as HKL; 64];
        let count = GetKeyboardLayoutList(installed.len() as i32, installed.as_mut_ptr());
        let installed_hkl = if count > 0 {
            installed[..(count as usize)]
                .iter()
                .copied()
                .find(|h| ((*h as usize & 0xFFFF) as u16) & 0x03ff == desired_primary)
        } else {
            None
        };

        let target_hkl: HKL = if let Some(hkl) = installed_hkl {
            hkl
        } else {
            // Fallback: try to load/activate known KLIDs.
            let mut loaded_hkl: HKL = 0;
            for klid in klids {
                let wide: Vec<u16> = klid.encode_utf16().chain(std::iter::once(0)).collect();
                let hkl = LoadKeyboardLayoutW(wide.as_ptr(), KLF_ACTIVATE);
                if hkl != 0 {
                    loaded_hkl = hkl;
                    break;
                }
            }
            loaded_hkl
        };

        if target_hkl == 0 {
            return LayoutSwitch::Failed;
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
                return LayoutSwitch::Failed;
            }
        }

        // Poll for the input subsystem to apply the change instead of a fixed
        // pessimistic sleep. Returns as soon as the layout flips.
        let gaps = crate::timing::injection();
        let deadline = std::time::Instant::now() + gaps.layout_confirm;
        loop {
            let updated_hkl = GetKeyboardLayout(tid);
            let updated_langid = (updated_hkl as usize & 0xFFFF) as u16;
            if updated_langid & 0x03ff == desired_primary {
                set_layout_cache(lang);
                return LayoutSwitch::Switched;
            }
            if std::time::Instant::now() >= deadline {
                return LayoutSwitch::Failed;
            }
            crate::timing::pause(gaps.layout_poll);
        }
    }
}
