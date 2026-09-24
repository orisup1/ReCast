//! Read-only live log window hosted by the tray event loop.

use tao::event_loop::EventLoopWindowTarget;

use crate::types::LiveLog;

pub struct Window {
    pub window: tao::window::Window,
    #[cfg(target_os = "macos")]
    text: cocoa::base::id,
    #[cfg(target_os = "windows")]
    text: winapi::shared::windef::HWND,
    last_text: String,
}

impl Window {
    pub fn new(target: &EventLoopWindowTarget<()>) -> Result<Self, String> {
        let window = tao::window::WindowBuilder::new()
            .with_title("ReCast live log")
            .with_inner_size(tao::dpi::LogicalSize::new(760.0, 460.0))
            .with_resizable(false)
            .with_visible(false)
            .build(target)
            .map_err(|error| error.to_string())?;
        #[cfg(target_os = "macos")]
        let text = unsafe { mac_controls(&window) };
        #[cfg(target_os = "windows")]
        let text = unsafe { windows_controls(&window)? };
        window.set_visible(true);
        window.set_focus();
        Ok(Self {
            window,
            text,
            last_text: String::new(),
        })
    }

    pub fn focus(&self) {
        self.window.set_focus();
    }

    pub fn update(&mut self, log: &LiveLog) {
        self.window.set_title(if log.active() {
            "ReCast live log — recording"
        } else {
            "ReCast live log — stopped"
        });
        let text = log.text();
        if text == self.last_text {
            return;
        }
        #[cfg(target_os = "macos")]
        unsafe {
            use cocoa::base::nil;
            use cocoa::foundation::{NSRange, NSString};
            use objc::{msg_send, sel, sel_impl};
            let value = NSString::alloc(nil).init_str(&text);
            let _: () = msg_send![self.text, setString: value];
            let _: () = msg_send![self.text, scrollRangeToVisible: NSRange::new(text.encode_utf16().count() as _, 0)];
            let _: () = msg_send![value, release];
        }
        #[cfg(target_os = "windows")]
        unsafe {
            use winapi::um::winuser::{SendMessageW, SetWindowTextW, EM_SCROLLCARET, EM_SETSEL};
            SetWindowTextW(self.text, wide(&text.replace('\n', "\r\n")).as_ptr());
            SendMessageW(self.text, EM_SETSEL.into(), usize::MAX, -1);
            SendMessageW(self.text, EM_SCROLLCARET.into(), 0, 0);
        }
        self.last_text = text;
    }
}

#[cfg(target_os = "macos")]
unsafe fn mac_controls(window: &tao::window::Window) -> cocoa::base::id {
    use cocoa::base::{id, nil, NO, YES};
    use cocoa::foundation::{NSPoint, NSRect, NSSize, NSString};
    use objc::{class, msg_send, sel, sel_impl};
    use tao::platform::macos::WindowExtMacOS;

    let native = window.ns_window() as id;
    let content: id = msg_send![native, contentView];
    let label: id = msg_send![class!(NSTextField), alloc];
    let label: id = msg_send![label, initWithFrame: NSRect::new(NSPoint::new(16.0, 424.0), NSSize::new(728.0, 24.0))];
    let hint = NSString::alloc(nil)
        .init_str("Start/stop from ReCast menu. Closing stops logging. Memory only.");
    let _: () = msg_send![label, setStringValue: hint];
    let _: () = msg_send![label, setBezeled: NO];
    let _: () = msg_send![label, setDrawsBackground: NO];
    let _: () = msg_send![label, setEditable: NO];
    let _: () = msg_send![content, addSubview: label];
    let _: () = msg_send![hint, release];
    let _: () = msg_send![label, release];

    let scroll: id = msg_send![class!(NSScrollView), alloc];
    let scroll: id = msg_send![scroll, initWithFrame: NSRect::new(NSPoint::new(16.0, 16.0), NSSize::new(728.0, 396.0))];
    let _: () = msg_send![scroll, setHasVerticalScroller: YES];
    let text: id = msg_send![class!(NSTextView), alloc];
    let text: id = msg_send![text, initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(728.0, 396.0))];
    let font: id = msg_send![class!(NSFont), userFixedPitchFontOfSize: 12.0f64];
    let _: () = msg_send![text, setFont: font];
    let _: () = msg_send![text, setEditable: NO];
    let _: () = msg_send![text, setSelectable: YES];
    let _: () = msg_send![text, setVerticallyResizable: YES];
    let container: id = msg_send![text, textContainer];
    let _: () = msg_send![container, setWidthTracksTextView: YES];
    let _: () = msg_send![scroll, setDocumentView: text];
    let _: () = msg_send![content, addSubview: scroll];
    let _: () = msg_send![text, release];
    let _: () = msg_send![scroll, release];
    text
}

#[cfg(target_os = "windows")]
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

#[cfg(target_os = "windows")]
unsafe fn windows_controls(
    window: &tao::window::Window,
) -> Result<winapi::shared::windef::HWND, String> {
    use tao::platform::windows::WindowExtWindows;
    use winapi::um::winuser::*;

    let scale = window.scale_factor();
    let make = |class: &str, value: &str, y: f64, height: f64, style: u32| {
        let child = CreateWindowExW(
            0,
            wide(class).as_ptr(),
            wide(value).as_ptr(),
            WS_CHILD | WS_VISIBLE | style,
            (16.0 * scale) as i32,
            (y * scale) as i32,
            (728.0 * scale) as i32,
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
    make(
        "STATIC",
        "Start/stop from ReCast tray. Closing stops logging. Memory only.",
        12.0,
        24.0,
        0,
    )?;
    let text = make(
        "EDIT",
        "",
        44.0,
        396.0,
        WS_BORDER | WS_VSCROLL | ES_MULTILINE | ES_AUTOVSCROLL | ES_READONLY,
    )?;
    SendMessageW(text, EM_SETLIMITTEXT.into(), 1024 * 1024, 0);
    Ok(text)
}
