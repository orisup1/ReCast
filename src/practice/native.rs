//! Native edit controls hosted by the existing tray event loop.

use crate::types::AppControl;
use tao::event_loop::EventLoopWindowTarget;

pub struct Window {
    pub window: tao::window::Window,
    #[cfg(target_os = "macos")]
    feedback: cocoa::base::id,
    #[cfg(target_os = "macos")]
    field: cocoa::base::id,
    #[cfg(target_os = "windows")]
    feedback: winapi::shared::windef::HWND,
    #[cfg(target_os = "windows")]
    field: winapi::shared::windef::HWND,
    last_feedback: String,
}

impl Window {
    pub fn new(target: &EventLoopWindowTarget<()>, control: &AppControl) -> Result<Self, String> {
        let window = tao::window::WindowBuilder::new()
            .with_title("Practice ReCast — close to skip")
            .with_inner_size(tao::dpi::LogicalSize::new(680.0, 440.0))
            .with_resizable(false)
            .with_visible(false)
            .build(target)
            .map_err(|e| e.to_string())?;
        #[cfg(target_os = "macos")]
        let (field, feedback) = unsafe { mac_controls(&window) };
        #[cfg(target_os = "windows")]
        let (field, feedback) = unsafe { windows_controls(&window)? };
        super::opened(control);
        window.set_visible(true);
        let practice = Self {
            window,
            feedback,
            field,
            last_feedback: String::new(),
        };
        practice.focus();
        Ok(practice)
    }

    pub fn focus(&self) {
        self.window.set_focus();
        #[cfg(target_os = "macos")]
        unsafe {
            use objc::{msg_send, sel, sel_impl};
            use tao::platform::macos::WindowExtMacOS;
            let window = self.window.ns_window() as cocoa::base::id;
            let _: bool = msg_send![window, makeFirstResponder: self.field];
        }
        #[cfg(target_os = "windows")]
        unsafe {
            winapi::um::winuser::SetFocus(self.field);
        }
    }

    pub fn update(&mut self, control: &AppControl, health: &str) {
        let text = format!("{}\n{health}", super::feedback(control));
        if self.last_feedback == text {
            return;
        }
        #[cfg(target_os = "macos")]
        unsafe {
            mac_text(self.feedback, &text);
        }
        #[cfg(target_os = "windows")]
        unsafe {
            winapi::um::winuser::SetWindowTextW(self.feedback, wide(&text).as_ptr());
        }
        self.last_feedback = text;
    }
}

#[cfg(target_os = "macos")]
unsafe fn mac_text(field: cocoa::base::id, value: &str) {
    use cocoa::base::nil;
    use cocoa::foundation::NSString;
    use objc::{msg_send, sel, sel_impl};
    let text = NSString::alloc(nil).init_str(value);
    let _: () = msg_send![field, setStringValue: text];
    let _: () = msg_send![text, release];
}

#[cfg(target_os = "macos")]
unsafe fn mac_controls(window: &tao::window::Window) -> (cocoa::base::id, cocoa::base::id) {
    use cocoa::base::{id, NO, YES};
    use cocoa::foundation::{NSPoint, NSRect, NSSize};
    use objc::{class, msg_send, sel, sel_impl};
    use tao::platform::macos::WindowExtMacOS;
    let ns_window = window.ns_window() as id;
    let view: id = msg_send![ns_window, contentView];
    let font: id = msg_send![class!(NSFont), systemFontOfSize: 14.0f64];
    let make = |y: f64, height: f64, editable: bool, text: &str| -> id {
        let field: id = msg_send![class!(NSTextField), alloc];
        let field: id = msg_send![field, initWithFrame: NSRect::new(NSPoint::new(20.0, y), NSSize::new(640.0, height))];
        let _: () = msg_send![field, setFont: font];
        let _: () = msg_send![field, setEditable: if editable { YES } else { NO }];
        let _: () = msg_send![field, setSelectable: if editable { YES } else { NO }];
        let _: () = msg_send![field, setBezeled: if editable { YES } else { NO }];
        let _: () = msg_send![field, setDrawsBackground: if editable { YES } else { NO }];
        let cell: id = msg_send![field, cell];
        let _: () = msg_send![cell, setWraps: if editable { NO } else { YES }];
        mac_text(field, text);
        let _: () = msg_send![view, addSubview: field];
        let _: () = msg_send![field, release];
        field
    };
    make(240.0, 180.0, false, super::instructions());
    let field = make(192.0, 32.0, true, "");
    {
        use cocoa::foundation::NSString;
        let label = NSString::alloc(cocoa::base::nil).init_str("Practice typing field");
        let _: () = msg_send![field, setAccessibilityLabel: label];
        let _: () = msg_send![label, release];
    }
    let feedback = make(16.0, 160.0, false, "");
    let _: () = msg_send![ns_window, setInitialFirstResponder: field];
    let _: bool = msg_send![ns_window, makeFirstResponder: field];
    let editor: id = msg_send![ns_window, fieldEditor: YES forObject: field];
    let _: () = msg_send![editor, setAutomaticSpellingCorrectionEnabled: NO];
    let _: () = msg_send![editor, setAutomaticTextReplacementEnabled: NO];
    (field, feedback)
}

#[cfg(target_os = "windows")]
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

#[cfg(target_os = "windows")]
unsafe fn windows_controls(
    window: &tao::window::Window,
) -> Result<(winapi::shared::windef::HWND, winapi::shared::windef::HWND), String> {
    use tao::platform::windows::WindowExtWindows;
    use winapi::um::winuser::*;
    let scale = window.scale_factor();
    let make = |class: &str, text: &str, y: f64, height: f64, style: u32| {
        let child = CreateWindowExW(
            0,
            wide(class).as_ptr(),
            wide(text).as_ptr(),
            WS_CHILD | WS_VISIBLE | style,
            (20.0 * scale) as i32,
            (y * scale) as i32,
            (640.0 * scale) as i32,
            (height * scale) as i32,
            window.hwnd() as _,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        if child.is_null() {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let font = winapi::um::wingdi::GetStockObject(winapi::um::wingdi::DEFAULT_GUI_FONT as i32);
        SendMessageW(child, WM_SETFONT, font as usize, 1);
        Ok(child)
    };
    make("STATIC", super::instructions(), 20.0, 180.0, 0)?;
    make("STATIC", "Practice typing field", 198.0, 18.0, 0)?;
    let field = make(
        "EDIT",
        "",
        216.0,
        34.0,
        WS_BORDER | WS_TABSTOP | ES_AUTOHSCROLL,
    )?;
    SendMessageW(field, EM_SETLIMITTEXT.into(), 128, 0);
    let feedback = make("STATIC", "", 272.0, 160.0, 0)?;
    SetFocus(field);
    Ok((field, feedback))
}
