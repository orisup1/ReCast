//! Reading and setting the keyboard layout on Windows.
//!
//! The Win32 spellings are kept exactly as the API documents them — `HKL`,
//! `HWND`, `DWORD` — because these declarations have to be checked against
//! Microsoft's headers by eye, and a renamed `Hkl` makes that harder for the
//! one reader who ever needs to.
#![allow(clippy::upper_case_acronyms)]

use super::{set_layout_cache, LayoutSwitch};
use crate::types::Language;

/// Inspect installed layouts without loading a keyboard or changing the active layout.
pub fn enabled_languages() -> (bool, bool) {
    use winapi::um::winuser::GetKeyboardLayoutList;
    unsafe {
        let count = GetKeyboardLayoutList(0, std::ptr::null_mut());
        if count <= 0 {
            return (false, false);
        }
        let mut layouts = vec![std::ptr::null_mut(); count as usize];
        let count = GetKeyboardLayoutList(count, layouts.as_mut_ptr()).max(0) as usize;
        let primary = |id| {
            layouts[..count.min(layouts.len())]
                .iter()
                .any(|layout| *layout as usize & 0x03ff == id)
        };
        (primary(0x09), primary(0x0d))
    }
}

/// Use the focused control, which can belong to a different thread than its host.
pub fn focused_target() -> Option<usize> {
    use winapi::um::winuser::{GetGUIThreadInfo, GUITHREADINFO};
    unsafe {
        let mut info: GUITHREADINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<GUITHREADINFO>() as u32;
        (GetGUIThreadInfo(0, &mut info) != 0 && !info.hwndFocus.is_null())
            .then_some(info.hwndFocus as usize)
    }
}

pub fn query_layout() -> Option<Language> {
    use winapi::um::winuser::{GetKeyboardLayout, GetWindowThreadProcessId};
    let focus = focused_target()?;
    unsafe {
        let tid = GetWindowThreadProcessId(focus as _, std::ptr::null_mut());
        if tid == 0 {
            return None;
        }
        match GetKeyboardLayout(tid) as usize & 0x03ff {
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
        fn GetWindowThreadProcessId(hWnd: HWND, lpdwProcessId: *mut DWORD) -> DWORD;
        fn GetKeyboardLayout(idThread: DWORD) -> HKL;
        fn PostMessageW(hWnd: HWND, Msg: u32, wParam: WPARAM, lParam: LPARAM) -> BOOL;
        fn GetKeyboardLayoutList(nBuff: i32, lpList: *mut HKL) -> i32;
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
        let Some(focus) = focused_target() else {
            return LayoutSwitch::Failed;
        };
        let hwnd = focus as HWND;
        let tid = GetWindowThreadProcessId(hwnd, std::ptr::null_mut());
        if tid == 0 {
            return LayoutSwitch::Failed;
        }

        // Pre-switch check: if the focused control is already on the
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

        // Windows directs this request to the focused control, not its host.
        if PostMessageW(
            hwnd,
            WM_INPUTLANGCHANGEREQUEST,
            INPUTLANGCHANGE_SYSCHARSET,
            target_hkl as LPARAM,
        ) == 0
        {
            return LayoutSwitch::Failed;
        }

        // Poll for the input subsystem to apply the change instead of a fixed
        // pessimistic sleep. Returns as soon as the layout flips.
        let gaps = crate::timing::injection();
        let deadline = std::time::Instant::now() + gaps.layout_confirm;
        loop {
            if focused_target() != Some(focus) {
                return LayoutSwitch::Failed;
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use winapi::shared::minwindef::{LPARAM, LRESULT, UINT, WPARAM};
    use winapi::shared::windef::HWND;
    use winapi::um::commctrl::{DefSubclassProc, SetWindowSubclass};
    use winapi::um::winuser::*;

    unsafe extern "system" fn host(
        hwnd: HWND,
        message: UINT,
        wparam: WPARAM,
        lparam: LPARAM,
        _: usize,
        _: usize,
    ) -> LRESULT {
        if message == WM_INPUTLANGCHANGEREQUEST {
            return 0; // The host does not handle the embedded control's layout.
        }
        DefSubclassProc(hwnd, message, wparam, lparam)
    }

    #[test]
    #[ignore = "takes desktop focus and requires installed English and Hebrew keyboards"]
    fn layout_switch_reaches_focused_control_instead_of_host() {
        use std::time::{Duration, Instant};
        assert_eq!(enabled_languages(), (true, true));
        let (ready, receive) = std::sync::mpsc::channel();
        let (stop, stopped) = std::sync::mpsc::channel();
        let ui = std::thread::spawn(move || unsafe {
            let wide = |text: &str| text.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
            let parent = CreateWindowExW(
                0,
                wide("STATIC").as_ptr(),
                wide("ReCast layout test").as_ptr(),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                100,
                100,
                600,
                300,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                winapi::um::libloaderapi::GetModuleHandleW(std::ptr::null()),
                std::ptr::null_mut(),
            );
            assert!(!parent.is_null());
            assert_ne!(SetWindowSubclass(parent, Some(host), 1, 0), 0);
            let child = CreateWindowExW(
                0,
                wide("EDIT").as_ptr(),
                wide("").as_ptr(),
                WS_CHILD | WS_VISIBLE | ES_MULTILINE,
                0,
                0,
                500,
                200,
                parent,
                std::ptr::null_mut(),
                winapi::um::libloaderapi::GetModuleHandleW(std::ptr::null()),
                std::ptr::null_mut(),
            );
            assert!(!child.is_null());
            SetForegroundWindow(parent);
            SetFocus(child);
            ready.send(child as usize).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut message = std::mem::zeroed();
            while stopped.try_recv() == Err(std::sync::mpsc::TryRecvError::Empty)
                && Instant::now() < deadline
            {
                while PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            DestroyWindow(parent);
        });
        let focus = receive.recv_timeout(Duration::from_secs(3)).unwrap();
        let result = std::panic::catch_unwind(|| {
            assert_eq!(focused_target(), Some(focus));
            for language in [Language::English, Language::Hebrew, Language::English] {
                assert!(switch_layout_to(language).ready());
                assert_eq!(query_layout(), Some(language));
            }
        });
        let _ = stop.send(());
        ui.join().unwrap();
        result.unwrap();
    }
}
