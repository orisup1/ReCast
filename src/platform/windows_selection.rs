//! Clipboard-free selection rescue for native Edit and RichEdit controls.
use super::engine::{Engine, Platform, Selection, MAX_SELECTION_BYTES};
use super::windows::Windows;
use winapi::um::winuser::*;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation8, IUIAutomation2, IUIAutomationTextPattern, UIA_TextPatternId,
};

struct Apartment;
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() }
    }
}

fn native_edit(focus: usize) -> bool {
    unsafe {
        let mut class = [0u16; 64];
        let count = GetClassNameW(focus as _, class.as_mut_ptr(), class.len() as i32);
        let class = String::from_utf16_lossy(&class[..count.max(0) as usize]);
        matches!(
            class.to_ascii_lowercase().as_str(),
            "edit" | "richedit20w" | "richedit50w"
        )
    }
}

fn writable(focus: usize) -> bool {
    native_edit(focus)
        && unsafe {
            GetWindowLongW(focus as _, GWL_STYLE) as u32 & (ES_PASSWORD | ES_READONLY) == 0
                && IsWindowEnabled(focus as _) != 0
        }
}

/// Native controls expose protection flags without a blocking accessibility query.
pub(super) fn input_allowed(focus: usize) -> bool {
    !native_edit(focus) || writable(focus)
}

/// A failed native match must never fall through to counted Backspaces.
pub(super) fn keyboard_fallback_allowed(focus: usize) -> bool {
    !native_edit(focus)
}

fn message(focus: usize, message: u32, wparam: usize, lparam: isize) -> Option<usize> {
    let mut result = 0;
    (unsafe {
        SendMessageTimeoutW(
            focus as _,
            message,
            wparam,
            lparam,
            SMTO_ABORTIFHUNG | SMTO_BLOCK,
            100,
            &mut result,
        )
    } != 0)
        .then_some(result)
}

fn selection_offsets(focus: usize) -> Option<(u32, u32)> {
    let (mut start, mut end) = (0u32, 0u32);
    message(
        focus,
        EM_GETSEL as u32,
        &mut start as *mut u32 as usize,
        &mut end as *mut u32 as isize,
    )?;
    Some((start, end))
}

fn range(focus: usize) -> Option<(u32, u32)> {
    let (start, end) = selection_offsets(focus)?;
    (end > start && end - start <= MAX_SELECTION_BYTES as u32).then_some((start, end))
}

fn suffix_start(text: &[u16], caret: usize, expected: &[u16]) -> Option<usize> {
    let prefix = text.get(..caret)?;
    // RichEdit may expose different newline widths in WM_GETTEXT and EM_GETSEL.
    if caret != text.len() && text.iter().any(|unit| matches!(unit, 10 | 13)) {
        return None;
    }
    prefix.ends_with(expected).then(|| caret - expected.len())
}

/// Replace the exact `expected` suffix standing immediately before the
/// caret, preserving everything before and after it.
///
/// Alignment is only trusted where `EM_GETSEL` and `WM_GETTEXT` necessarily
/// agree (single-line text or the caret at the end), so anything ambiguous
/// (a selection, multi-line offset drift, unreadable text) fails closed and
/// changes nothing. Edit and RichEdit controls only.
pub(super) fn replace_expected_suffix(
    focus: usize,
    expected: &str,
    replacement: &str,
    valid: impl Fn() -> bool,
    start_edit: impl FnOnce(),
) -> Option<()> {
    let units: Vec<u16> = expected.encode_utf16().collect();
    let replacement_wide: Vec<u16> = replacement.encode_utf16().chain(Some(0)).collect();
    if units.is_empty()
        || replacement.is_empty()
        || replacement.contains('\0')
        || replacement_wide.len() > MAX_SELECTION_BYTES
    {
        return None;
    }
    if !writable(focus) || !valid() {
        return None;
    }
    let (start, end) = selection_offsets(focus)?;
    if start != end {
        // A selection is not ours to collapse.
        return None;
    }
    let caret = start as usize;
    let length = message(focus, WM_GETTEXTLENGTH, 0, 0)?;
    if length == 0 || length > MAX_SELECTION_BYTES {
        return None;
    }
    let mut text = vec![0u16; length + 1];
    let copied = message(focus, WM_GETTEXT, length + 1, text.as_mut_ptr() as isize)?;
    if copied > length || message(focus, WM_GETTEXTLENGTH, 0, 0)? != length {
        return None;
    }
    text.truncate(copied);
    let begin = suffix_start(&text, caret, &units)?;
    if (caret < copied && unsafe { IsWindowUnicode(focus as _) } == 0)
        || selection_offsets(focus)? != (start, end)
        || !valid()
    {
        return None;
    }
    let new_caret = begin + replacement.encode_utf16().count();
    start_edit();
    message(focus, EM_SETSEL as u32, begin, caret as isize)?;
    let (selected_start, selected_end) = selection_offsets(focus)?;
    if !valid()
        || Windows::focus() != Some(focus)
        || selected_start as usize != begin
        || selected_end as usize != caret
    {
        if Windows::focus() == Some(focus)
            && selected_start as usize == begin
            && selected_end as usize == caret
        {
            message(focus, EM_SETSEL as u32, caret, caret as isize)?;
        }
        return None;
    }
    message(
        focus,
        EM_REPLACESEL as u32,
        1,
        replacement_wide.as_ptr() as isize,
    )?;
    (selection_offsets(focus)? == (new_caret as u32, new_caret as u32)).then_some(())
}

pub(super) fn selection(focus: usize) -> Option<Selection> {
    if Windows::focus() != Some(focus) || !writable(focus) {
        return None;
    }
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok().ok()?;
        let _apartment = Apartment;
        let automation: IUIAutomation2 =
            CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER).ok()?;
        automation.SetConnectionTimeout(100).ok()?;
        automation.SetTransactionTimeout(100).ok()?;
        let element = automation
            .ElementFromHandle(windows::Win32::Foundation::HWND(focus as _))
            .ok()?;
        if element.CurrentIsPassword().ok()?.as_bool() {
            return None;
        }
        let pattern: IUIAutomationTextPattern =
            element.GetCurrentPatternAs(UIA_TextPatternId).ok()?;
        let (start, end) = range(focus)?;
        let selections = pattern.GetSelection().ok()?;
        if selections.Length().ok()? != 1 {
            return None;
        }
        let native_text = selections
            .GetElement(0)
            .ok()?
            .GetText(MAX_SELECTION_BYTES as i32 + 1)
            .ok()?;
        let text = String::try_from(&native_text).ok()?;
        if text.len() > MAX_SELECTION_BYTES
            || text.contains('\0')
            || text.encode_utf16().count() != (end - start) as usize
            || range(focus)? != (start, end)
            || Windows::focus() != Some(focus)
        {
            return None;
        }
        Some(Selection {
            start: start as isize,
            length: (end - start) as isize,
            text,
        })
    }
}

pub(super) fn replace(
    engine: &Engine<Windows>,
    focus: usize,
    expected: &Selection,
    text: &str,
    generation: u64,
) -> Option<Selection> {
    replace_verified(focus, expected, text, || {
        engine.replacement_valid(generation)
    })
}

fn replace_verified(
    focus: usize,
    expected: &Selection,
    text: &str,
    valid: impl Fn() -> bool,
) -> Option<Selection> {
    if text.is_empty()
        || text.len() > MAX_SELECTION_BYTES
        || text.contains('\0')
        || selection(focus).as_ref() != Some(expected)
        || !valid()
    {
        return None;
    }
    let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    let end = expected.start.checked_add(wide.len() as isize - 1)?;
    // These system messages are marshalled across processes; custom WM_USER messages are not.
    message(focus, EM_REPLACESEL as u32, 1, wide.as_ptr() as isize)?;
    if Windows::focus() != Some(focus) || !valid() {
        return None;
    }
    message(focus, EM_SETSEL as u32, expected.start as usize, end)?;
    let after = selection(focus)?;
    (after.start == expected.start && after.text == text).then_some(after)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn exact_suffix_handles_utf16_carets_and_rejects_multiline_offset_drift() {
        let wide = |s: &str| s.encode_utf16().collect::<Vec<_>>();
        let text = wide("🙂 שלום right");
        let caret = wide("🙂 שלום").len();
        assert_eq!(suffix_start(&text, caret, &wide("שלום")), Some(3));
        assert_eq!(suffix_start(&text, caret - 1, &wide("שלום")), None);
        assert_eq!(suffix_start(&text, text.len() + 1, &wide("right")), None);
        let lines = wide("left\r\nשלום right");
        assert_eq!(suffix_start(&lines, 10, &wide("שלום")), None);
        assert_eq!(suffix_start(&lines, lines.len(), &wide("right")), Some(11));
    }

    #[test]
    fn native_fields_block_password_capture_and_failed_match_fallback() {
        unsafe {
            let class: Vec<_> = "EDIT\0".encode_utf16().collect();
            for (style, enabled, allowed) in [
                (0, true, true),
                (ES_PASSWORD, true, false),
                (ES_READONLY, true, false),
                (0, false, false),
            ] {
                let hwnd = CreateWindowExW(
                    0,
                    class.as_ptr(),
                    std::ptr::null(),
                    WS_POPUP | style,
                    0,
                    0,
                    100,
                    100,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    winapi::um::libloaderapi::GetModuleHandleW(std::ptr::null()),
                    std::ptr::null_mut(),
                );
                assert!(!hwnd.is_null());
                EnableWindow(hwnd, i32::from(enabled));
                let focus = hwnd as usize;
                let capture = input_allowed(focus);
                let fallback = keyboard_fallback_allowed(focus);
                let mut began = false;
                assert!(replace_expected_suffix(
                    focus,
                    "stale ",
                    "replacement ",
                    || true,
                    || began = true
                )
                .is_none());
                DestroyWindow(hwnd);
                assert_eq!(capture, allowed, "style {style}, enabled {enabled}");
                assert!(
                    !fallback,
                    "rejected native matches must not emit Backspaces"
                );
                assert!(!began, "a pre-edit refusal must permit layout restoration");
            }
        }
    }

    #[test]
    #[ignore = "opens a disposable native Windows Edit control and takes focus"]
    fn native_selection_round_trip() {
        let (send, receive) = std::sync::mpsc::channel();
        let ui = std::thread::spawn(move || unsafe {
            let class: Vec<u16> = "EDIT\0".encode_utf16().collect();
            let initial: Vec<u16> = "left akuo right\0".encode_utf16().collect();
            let hwnd = CreateWindowExW(
                0,
                class.as_ptr(),
                initial.as_ptr(),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE | ES_MULTILINE,
                100,
                100,
                600,
                300,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                winapi::um::libloaderapi::GetModuleHandleW(std::ptr::null()),
                std::ptr::null_mut(),
            );
            assert!(!hwnd.is_null());
            SetForegroundWindow(hwnd);
            SetFocus(hwnd);
            SendMessageW(hwnd, EM_SETSEL as u32, 5, 9);
            send.send(hwnd as usize).unwrap();
            let deadline = Instant::now() + Duration::from_secs(15);
            let mut event = std::mem::zeroed();
            while IsWindow(hwnd) != 0 && Instant::now() < deadline {
                while PeekMessageW(&mut event, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                    TranslateMessage(&event);
                    DispatchMessageW(&event);
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            if IsWindow(hwnd) != 0 {
                DestroyWindow(hwnd);
            }
        });
        let focus = receive.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(
            !keyboard_fallback_allowed(focus),
            "a native selection must not be erased by fallback"
        );
        message(focus, EM_SETSEL as u32, 9, 9).unwrap();
        for (expected, replacement) in [("akuo", "שלום🙂"), ("שלום🙂", "akuo")] {
            replace_expected_suffix(focus, expected, replacement, || true, || {}).unwrap();
        }
        message(focus, EM_SETSEL as u32, 5, 9).unwrap();
        let before = selection(focus)
            .expect("native Edit must expose its selected text; keep this test in foreground");
        assert_eq!(before.text, "akuo");
        assert!(replace_verified(focus, &before, "שלום", || false).is_none());
        assert_eq!(selection(focus).as_ref(), Some(&before));
        let after = replace_verified(focus, &before, "שלום", || true).expect("native replacement");
        assert!(replace_verified(focus, &before, "stale", || true).is_none());
        assert_eq!(
            replace_verified(focus, &after, &before.text, || true).as_ref(),
            Some(&before)
        );
        unsafe {
            let mut text = [0u16; 64];
            let count = GetWindowTextW(focus as _, text.as_mut_ptr(), text.len() as i32);
            assert_eq!(
                String::from_utf16(&text[..count as usize]).unwrap(),
                "left akuo right"
            );
            SendMessageW(focus as _, EM_SETREADONLY as u32, 1, 0);
        }
        assert!(selection(focus).is_none());
        assert!(!keyboard_fallback_allowed(focus));
        unsafe {
            PostMessageW(focus as _, WM_CLOSE, 0, 0);
        }
        ui.join().unwrap();
    }
}
