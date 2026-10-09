//! Native, read-only correction details with an explicit word-rule action.

use crate::types::Correction;

#[cfg(target_os = "macos")]
pub fn show(correction: &Correction) -> Result<bool, String> {
    use cocoa::appkit::NSApp;
    use cocoa::base::{id, nil, NO, YES};
    use cocoa::foundation::{NSPoint, NSRect, NSSize, NSString};
    use objc::{class, msg_send, sel, sel_impl};

    unsafe {
        let _: () = msg_send![NSApp(), activateIgnoringOtherApps: YES];
        let alert: id = msg_send![class!(NSAlert), new];
        let title = NSString::alloc(nil).init_str("Why this correction");
        let _: () = msg_send![alert, setMessageText: title];
        let _: () = msg_send![title, release];
        let ignored = crate::complete::ignored(&correction.from);
        for (index, label) in [
            "Close",
            if ignored {
                "Already ignored"
            } else {
                "Ignore this word"
            },
        ]
        .into_iter()
        .enumerate()
        {
            let label = NSString::alloc(nil).init_str(label);
            let button: id = msg_send![alert, addButtonWithTitle: label];
            let _: () = msg_send![label, release];
            if ignored && index == 1 {
                let _: () = msg_send![button, setEnabled: NO];
            }
        }
        let scroll: id = msg_send![class!(NSScrollView), alloc];
        let scroll: id = msg_send![scroll, initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(600.0, 360.0))];
        let _: () = msg_send![scroll, setHasVerticalScroller: YES];
        let text: id = msg_send![class!(NSTextView), alloc];
        let text: id = msg_send![text, initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(600.0, 360.0))];
        let _: () = msg_send![text, setRichText: NO];
        let _: () = msg_send![text, setEditable: NO];
        let _: () = msg_send![text, setSelectable: YES];
        let _: () = msg_send![text, setVerticallyResizable: YES];
        let container: id = msg_send![text, textContainer];
        let _: () = msg_send![container, setWidthTracksTextView: YES];
        let body = NSString::alloc(nil).init_str(&correction.inspection());
        let _: () = msg_send![text, setString: body];
        let _: () = msg_send![body, release];
        let label = NSString::alloc(nil).init_str("Correction evidence");
        let _: () = msg_send![text, setAccessibilityLabel: label];
        let _: () = msg_send![label, release];
        let _: () = msg_send![scroll, setDocumentView: text];
        let _: () = msg_send![alert, setAccessoryView: scroll];
        let response: isize = msg_send![alert, runModal];
        let _: () = msg_send![text, release];
        let _: () = msg_send![scroll, release];
        let _: () = msg_send![alert, release];
        Ok(response == 1001 && !ignored)
    }
}

#[cfg(target_os = "windows")]
pub fn show(correction: &Correction) -> Result<bool, String> {
    use winapi::shared::{minwindef::*, windef::HWND};
    use winapi::um::winuser::*;

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(Some(0)).collect()
    }

    unsafe extern "system" fn dialog(hwnd: HWND, message: UINT, w: WPARAM, l: LPARAM) -> isize {
        match message {
            WM_INITDIALOG => {
                let correction = &*(l as *const Correction);
                SetWindowTextW(hwnd, wide("Why this correction").as_ptr());
                let mut rect = std::mem::zeroed();
                GetClientRect(hwnd, &mut rect);
                let make = |class: &str, label: &str, x, y, width, height, style, id| {
                    let child = CreateWindowExW(
                        0,
                        wide(class).as_ptr(),
                        wide(label).as_ptr(),
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
                    SendMessageW(
                        child,
                        WM_SETFONT,
                        winapi::um::wingdi::GetStockObject(
                            winapi::um::wingdi::DEFAULT_GUI_FONT as i32,
                        ) as usize,
                        1,
                    );
                    child
                };
                let text = make(
                    "EDIT",
                    &correction.inspection().replace('\n', "\r\n"),
                    12,
                    12,
                    rect.right - 24,
                    rect.bottom - 64,
                    WS_BORDER
                        | WS_TABSTOP
                        | WS_VSCROLL
                        | ES_MULTILINE
                        | ES_AUTOVSCROLL
                        | ES_READONLY,
                    101,
                );
                let ignored = crate::complete::ignored(&correction.from);
                let ignore = make(
                    "BUTTON",
                    if ignored {
                        "Already ignored"
                    } else {
                        "Ignore this word"
                    },
                    12,
                    rect.bottom - 40,
                    140,
                    28,
                    WS_TABSTOP,
                    IDOK,
                );
                EnableWindow(ignore, if ignored { 0 } else { 1 });
                make(
                    "BUTTON",
                    "Close",
                    rect.right - 96,
                    rect.bottom - 40,
                    84,
                    28,
                    WS_TABSTOP | BS_DEFPUSHBUTTON,
                    IDCANCEL,
                );
                SetFocus(text);
                SetForegroundWindow(hwnd);
                0
            }
            WM_COMMAND if (w & 0xffff) as i32 == IDOK => {
                EndDialog(hwnd, 1);
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

    // DWORD-aligned empty dialog template; controls are built during initialization.
    let style = WS_POPUP | WS_CAPTION | WS_SYSMENU | DS_MODALFRAME;
    let words: [u16; 12] = [
        style as u16,
        (style >> 16) as u16,
        0,
        0,
        0,
        0,
        0,
        400,
        280,
        0,
        0,
        0,
    ];
    let template: Vec<u32> = words
        .chunks_exact(2)
        .map(|w| u32::from(w[0]) | (u32::from(w[1]) << 16))
        .collect();
    let response = unsafe {
        DialogBoxIndirectParamW(
            std::ptr::null_mut(),
            template.as_ptr() as _,
            std::ptr::null_mut(),
            Some(dialog),
            correction as *const _ as isize,
        )
    };
    if response == -1 {
        Err(std::io::Error::last_os_error().to_string())
    } else {
        Ok(response == 1)
    }
}
