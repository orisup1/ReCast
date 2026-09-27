//! Native word-rule editors; persistence and validation live in `complete`.

use crate::complete::{RuleEditor, RuleKind};

pub fn edit(kind: RuleKind, control: &crate::types::AppControl) {
    let editor = match RuleEditor::open(kind) {
        Ok(editor) => editor,
        Err(error) => {
            crate::notify::notify("Could not open word rules", &error);
            return;
        }
    };
    control
        .word_rules_open
        .store(true, std::sync::atomic::Ordering::Relaxed);
    if let Err(error) = show(editor) {
        crate::notify::notify("Could not open word rules", &error);
    }
    control
        .word_rules_open
        .store(false, std::sync::atomic::Ordering::Relaxed);
}

#[cfg(target_os = "macos")]
fn show(mut editor: RuleEditor) -> Result<(), String> {
    use cocoa::appkit::NSApp;
    use cocoa::base::{id, nil, NO, YES};
    use cocoa::foundation::{NSPoint, NSRect, NSSize, NSString};
    use objc::{class, msg_send, sel, sel_impl};
    unsafe {
        let _: () = msg_send![NSApp(), activateIgnoringOtherApps: YES];
        let alert: id = msg_send![class!(NSAlert), new];
        let string = |s: &str| NSString::alloc(nil).init_str(s);
        let title = string(editor.kind.title());
        let _: () = msg_send![alert, setMessageText: title];
        let _: () = msg_send![title, release];
        for label in ["Save", "Cancel"] {
            let label = string(label);
            let _: id = msg_send![alert, addButtonWithTitle: label];
            let _: () = msg_send![label, release];
        }
        let scroll: id = msg_send![class!(NSScrollView), alloc];
        let scroll: id = msg_send![scroll, initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(580.0, 300.0))];
        let _: () = msg_send![scroll, setHasVerticalScroller: YES];
        let text: id = msg_send![class!(NSTextView), alloc];
        let text: id = msg_send![text, initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(580.0, 300.0))];
        let _: () = msg_send![text, setRichText: NO];
        let _: () = msg_send![text, setAutomaticSpellingCorrectionEnabled: NO];
        let _: () = msg_send![text, setAutomaticTextReplacementEnabled: NO];
        let _: () = msg_send![text, setAutomaticQuoteSubstitutionEnabled: NO];
        let _: () = msg_send![text, setAutomaticDashSubstitutionEnabled: NO];
        let _: () = msg_send![text, setVerticallyResizable: YES];
        let label = string(editor.kind.title());
        let _: () = msg_send![text, setAccessibilityLabel: label];
        let _: () = msg_send![label, release];
        let initial = string(&editor.text);
        let _: () = msg_send![text, setString: initial];
        let _: () = msg_send![initial, release];
        let container: id = msg_send![text, textContainer];
        let _: () = msg_send![container, setWidthTracksTextView: YES];
        let _: () = msg_send![scroll, setDocumentView: text];
        let _: () = msg_send![alert, setAccessoryView: scroll];
        let window: id = msg_send![alert, window];
        let _: () = msg_send![window, setInitialFirstResponder: text];
        let mut error = String::new();
        loop {
            let hint = string(&format!(
                "{}\nCorrection pauses while this editor is open.{}",
                editor.kind.hint(),
                error
            ));
            let _: () = msg_send![alert, setInformativeText: hint];
            let _: () = msg_send![hint, release];
            let response: isize = msg_send![alert, runModal];
            if response != 1000 {
                break;
            }
            let value: id = msg_send![text, string];
            let bytes: *const std::ffi::c_char = msg_send![value, UTF8String];
            editor.text = std::ffi::CStr::from_ptr(bytes)
                .to_string_lossy()
                .into_owned();
            match editor.save() {
                Ok(()) => break,
                Err(message) => error = format!("\n\n{message}"),
            }
        }
        let _: () = msg_send![text, release];
        let _: () = msg_send![scroll, release];
        let _: () = msg_send![alert, release];
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn show(mut editor: RuleEditor) -> Result<(), String> {
    use winapi::shared::{minwindef::*, windef::HWND};
    use winapi::um::winuser::*;
    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(Some(0)).collect()
    }
    unsafe extern "system" fn dialog(hwnd: HWND, message: UINT, w: WPARAM, l: LPARAM) -> isize {
        match message {
            WM_INITDIALOG => {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, l);
                let editor = &*(l as *const RuleEditor);
                SetWindowTextW(hwnd, wide(editor.kind.title()).as_ptr());
                let mut rect = std::mem::zeroed();
                GetClientRect(hwnd, &mut rect);
                let width = rect.right;
                let height = rect.bottom;
                let make = |class: &str, text: &str, x, y, width, height, style, id| {
                    let child = CreateWindowExW(
                        0,
                        wide(class).as_ptr(),
                        wide(text).as_ptr(),
                        WS_CHILD | WS_VISIBLE | style,
                        x,
                        y,
                        width,
                        height,
                        hwnd,
                        id as _,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                    );
                    let font = winapi::um::wingdi::GetStockObject(
                        winapi::um::wingdi::DEFAULT_GUI_FONT as i32,
                    );
                    SendMessageW(child, WM_SETFONT, font as usize, 1);
                    child
                };
                let hint = format!(
                    "{}\r\nCorrection pauses while this editor is open.",
                    editor.kind.hint()
                );
                make("STATIC", &hint, 12, 12, width - 24, 64, 0, 100);
                let text = make(
                    "EDIT",
                    &editor.text.replace('\n', "\r\n"),
                    12,
                    80,
                    width - 24,
                    height - 132,
                    WS_BORDER
                        | WS_TABSTOP
                        | WS_VSCROLL
                        | ES_MULTILINE
                        | ES_AUTOVSCROLL
                        | ES_WANTRETURN,
                    101,
                );
                SendMessageW(text, EM_SETLIMITTEXT.into(), 1024 * 1024, 0);
                make(
                    "BUTTON",
                    "Save",
                    width - 188,
                    height - 40,
                    80,
                    28,
                    WS_TABSTOP | BS_DEFPUSHBUTTON,
                    IDOK,
                );
                make(
                    "BUTTON",
                    "Cancel",
                    width - 96,
                    height - 40,
                    84,
                    28,
                    WS_TABSTOP,
                    IDCANCEL,
                );
                SetFocus(text);
                0
            }
            WM_COMMAND if (w & 0xffff) as i32 == IDOK => {
                let editor = &mut *(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut RuleEditor);
                let field = GetDlgItem(hwnd, 101);
                let len = GetWindowTextLengthW(field) as usize;
                let mut buffer = vec![0u16; len + 1];
                let read = GetWindowTextW(field, buffer.as_mut_ptr(), buffer.len() as i32);
                editor.text =
                    String::from_utf16_lossy(&buffer[..read as usize]).replace("\r\n", "\n");
                match editor.save() {
                    Ok(()) => {
                        EndDialog(hwnd, IDOK as isize);
                    }
                    Err(error) => {
                        MessageBoxW(
                            hwnd,
                            wide(&error).as_ptr(),
                            wide("Rules not saved").as_ptr(),
                            MB_OK | MB_ICONERROR,
                        );
                    }
                }
                1
            }
            WM_COMMAND if (w & 0xffff) as i32 == IDCANCEL => {
                EndDialog(hwnd, 0);
                1
            }
            WM_CLOSE => {
                EndDialog(hwnd, 0);
                1
            }
            _ => 0,
        }
    }
    // Empty native dialog template. Controls are created in WM_INITDIALOG;
    // u32 storage provides the DWORD alignment required by Win32.
    let style = WS_POPUP | WS_CAPTION | WS_SYSMENU | DS_MODALFRAME;
    let words: [u16; 12] = [
        style as u16,
        (style >> 16) as u16,
        0,
        0,
        0,
        0,
        0,
        380,
        260,
        0,
        0,
        0,
    ];
    let template: Vec<u32> = words
        .chunks_exact(2)
        .map(|w| u32::from(w[0]) | (u32::from(w[1]) << 16))
        .collect();
    let result = unsafe {
        DialogBoxIndirectParamW(
            std::ptr::null_mut(),
            template.as_ptr() as _,
            std::ptr::null_mut(),
            Some(dialog),
            &mut editor as *mut _ as isize,
        )
    };
    if result == -1 {
        Err(std::io::Error::last_os_error().to_string())
    } else {
        Ok(())
    }
}
