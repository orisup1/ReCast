use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

use tao::event_loop::EventLoopProxy;
use tao::platform::windows::WindowExtWindows;
use winapi::shared::{minwindef::*, windef::*};
use winapi::um::{commctrl::*, libloaderapi::*, wingdi::*, winuser::*};

use super::{Element, Kind};

pub fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

unsafe fn user32_function(name: &[u8]) -> FARPROC {
    GetProcAddress(
        GetModuleHandleW(wide("user32.dll").as_ptr()),
        name.as_ptr() as _,
    )
}

unsafe fn system_dpi() -> u32 {
    let dc = GetDC(std::ptr::null_mut());
    if dc.is_null() {
        return 96;
    }
    let dpi = GetDeviceCaps(dc, LOGPIXELSY).max(96) as u32;
    ReleaseDC(std::ptr::null_mut(), dc);
    dpi
}

/// Resolve newer DPI APIs dynamically so the executable still starts on early Windows 10.
pub unsafe fn window_dpi(window: HWND) -> u32 {
    type GetDpi = unsafe extern "system" fn(HWND) -> u32;
    static API: OnceLock<Option<GetDpi>> = OnceLock::new();
    let api = API.get_or_init(|| {
        let address = user32_function(b"GetDpiForWindow\0");
        if address.is_null() {
            None
        } else {
            Some(std::mem::transmute::<FARPROC, GetDpi>(address))
        }
    });
    api.map_or_else(|| system_dpi(), |get_dpi| get_dpi(window).max(96))
}

/// Use the system message font at the window's current DPI, including text size preferences.
pub unsafe fn message_font(dpi: u32) -> HFONT {
    type Metrics = unsafe extern "system" fn(UINT, UINT, *mut std::ffi::c_void, UINT, UINT) -> BOOL;
    static API: OnceLock<Option<Metrics>> = OnceLock::new();
    let api = API.get_or_init(|| {
        let address = user32_function(b"SystemParametersInfoForDpi\0");
        if address.is_null() {
            None
        } else {
            Some(std::mem::transmute::<FARPROC, Metrics>(address))
        }
    });
    let mut metrics: NONCLIENTMETRICSW = std::mem::zeroed();
    metrics.cbSize = std::mem::size_of::<NONCLIENTMETRICSW>() as u32;
    if !api.is_some_and(|get_metrics| {
        get_metrics(
            SPI_GETNONCLIENTMETRICS,
            metrics.cbSize,
            &mut metrics as *mut _ as _,
            0,
            dpi,
        ) != 0
    }) {
        SystemParametersInfoW(
            SPI_GETNONCLIENTMETRICS,
            metrics.cbSize,
            &mut metrics as *mut _ as _,
            0,
        );
        let system = system_dpi() as i32;
        metrics.lfMessageFont.lfHeight = metrics.lfMessageFont.lfHeight * dpi as i32 / system;
        metrics.lfMessageFont.lfWidth = metrics.lfMessageFont.lfWidth * dpi as i32 / system;
    }
    CreateFontIndirectW(&metrics.lfMessageFont)
}

unsafe fn heading_font(body: HFONT) -> HFONT {
    let mut font: LOGFONTW = std::mem::zeroed();
    if GetObjectW(
        body as _,
        std::mem::size_of::<LOGFONTW>() as i32,
        &mut font as *mut _ as _,
    ) == 0
    {
        return std::ptr::null_mut();
    }
    font.lfHeight = font.lfHeight * 3 / 2;
    font.lfWeight = FW_SEMIBOLD;
    CreateFontIndirectW(&font)
}

/// Resize the existing modal rule editor without relying on fixed dialog units.
pub unsafe fn layout_rules(parent: HWND) {
    if GetDlgItem(parent, 101).is_null() {
        return;
    }
    let dpi = window_dpi(parent);
    let scale = dpi as f64 / 96.0;
    let px = |value: f64| (value * scale).round() as i32;
    let mut rect = std::mem::zeroed();
    GetClientRect(parent, &mut rect);
    let gutter = px(16.0);
    for (id, x, y, width, height) in [
        (100, gutter, gutter, rect.right - 2 * gutter, px(64.0)),
        (
            101,
            gutter,
            px(88.0),
            rect.right - 2 * gutter,
            (rect.bottom - px(152.0)).max(px(60.0)),
        ),
        (
            IDOK,
            rect.right - px(204.0),
            rect.bottom - px(48.0),
            px(88.0),
            px(32.0),
        ),
        (
            IDCANCEL,
            rect.right - px(104.0),
            rect.bottom - px(48.0),
            px(88.0),
            px(32.0),
        ),
    ] {
        let child = GetDlgItem(parent, id);
        if !child.is_null() {
            SetWindowPos(
                child,
                std::ptr::null_mut(),
                x,
                y,
                width,
                height,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }
    let dpi_key = wide("ReCastDialogDpi");
    if GetPropW(parent, dpi_key.as_ptr()) as usize != dpi as usize {
        let font = message_font(dpi);
        if !font.is_null() {
            let font_key = wide("ReCastDialogFont");
            let previous = GetPropW(parent, font_key.as_ptr());
            for id in [100, 101, IDOK, IDCANCEL] {
                SendMessageW(GetDlgItem(parent, id), WM_SETFONT, font as usize, 1);
            }
            SetPropW(parent, font_key.as_ptr(), font as _);
            SetPropW(parent, dpi_key.as_ptr(), dpi as usize as _);
            if !previous.is_null() {
                DeleteObject(previous as _);
            }
        }
    }
}

pub unsafe fn clear_dialog_font(parent: HWND) {
    let font = RemovePropW(parent, wide("ReCastDialogFont").as_ptr());
    RemovePropW(parent, wide("ReCastDialogDpi").as_ptr());
    if !font.is_null() {
        DeleteObject(font as _);
    }
}

struct Callback {
    pending: Mutex<VecDeque<u16>>,
    proxy: EventLoopProxy<()>,
    parent: HWND,
}

unsafe extern "system" fn procedure(
    hwnd: HWND,
    message: UINT,
    w: WPARAM,
    l: LPARAM,
    _: usize,
    data: usize,
) -> LRESULT {
    let callback = &*(data as *const Callback);
    if message == WM_COMMAND && l != 0 {
        let notification = (w >> 16) as u16;
        if notification == BN_CLICKED || notification == CBN_SELCHANGE {
            crate::types::lock_forgiving(&callback.pending).push_back((w & 0xffff) as u16);
            let _ = callback.proxy.send_event(());
        }
    }
    if message == WM_KEYDOWN && w == VK_TAB as usize {
        let next = GetNextDlgTabItem(
            callback.parent,
            GetFocus(),
            if GetKeyState(VK_SHIFT) < 0 { 1 } else { 0 },
        );
        if !next.is_null() {
            SetFocus(next);
        }
        return 0;
    }
    DefSubclassProc(hwnd, message, w, l)
}

pub struct Native {
    parent: HWND,
    callback: Box<Callback>,
    controls: Vec<(Element, HWND)>,
    font: HFONT,
    heading: HFONT,
    dpi: u32,
}

impl Native {
    pub fn new(window: &tao::window::Window, proxy: EventLoopProxy<()>) -> Result<Self, String> {
        unsafe {
            let parent = window.hwnd() as HWND;
            let mut callback = Box::new(Callback {
                pending: Mutex::new(VecDeque::new()),
                proxy,
                parent,
            });
            if SetWindowSubclass(
                parent,
                Some(procedure),
                1,
                &mut *callback as *mut _ as usize,
            ) == 0
            {
                return Err(std::io::Error::last_os_error().to_string());
            }
            Ok(Self {
                parent,
                callback,
                controls: Vec::new(),
                font: std::ptr::null_mut(),
                heading: std::ptr::null_mut(),
                dpi: 0,
            })
        }
    }

    pub fn render(&mut self, elements: &[Element], _: f64, scale: f64) -> Result<(), String> {
        unsafe {
            let dpi = (96.0 * scale).round() as u32;
            let scale_changed = dpi != self.dpi;
            let new_font = if dpi != self.dpi {
                message_font(dpi)
            } else {
                std::ptr::null_mut()
            };
            if !new_font.is_null() {
                let previous = self.font;
                let previous_heading = self.heading;
                self.font = new_font;
                self.heading = heading_font(new_font);
                self.dpi = dpi;
                for (element, control) in &self.controls {
                    let font = if matches!(element.kind, Kind::Heading) && !self.heading.is_null() {
                        self.heading
                    } else {
                        self.font
                    };
                    SendMessageW(*control, WM_SETFONT, font as usize, 1);
                }
                if !previous.is_null() {
                    DeleteObject(previous as _);
                }
                if !previous_heading.is_null() {
                    DeleteObject(previous_heading as _);
                }
            }
            if self.controls.len() != elements.len()
                || self
                    .controls
                    .iter()
                    .zip(elements)
                    .any(|((a, _), b)| a.id != b.id || !a.kind.same_control(&b.kind))
            {
                self.clear();
                for element in elements {
                    let (class, style) = match element.kind {
                        Kind::Button => ("BUTTON", WS_TABSTOP | BS_PUSHBUTTON),
                        Kind::Check(_) => ("BUTTON", WS_TABSTOP | BS_AUTOCHECKBOX),
                        Kind::Choice(..) => {
                            ("COMBOBOX", WS_TABSTOP | WS_VSCROLL | CBS_DROPDOWNLIST)
                        }
                        Kind::Input => ("EDIT", WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL),
                        Kind::Text => (
                            "EDIT",
                            WS_TABSTOP
                                | WS_BORDER
                                | WS_VSCROLL
                                | ES_MULTILINE
                                | ES_AUTOVSCROLL
                                | ES_READONLY,
                        ),
                        _ => ("STATIC", SS_LEFT | SS_NOPREFIX),
                    };
                    let child = CreateWindowExW(
                        0,
                        wide(class).as_ptr(),
                        wide("").as_ptr(),
                        WS_CHILD | WS_VISIBLE | style,
                        0,
                        0,
                        1,
                        1,
                        self.parent,
                        element.id as usize as _,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                    );
                    if child.is_null() {
                        return Err(std::io::Error::last_os_error().to_string());
                    }
                    self.controls.push((
                        Element {
                            text: String::new(),
                            frame: [0.0; 4],
                            ..element.clone()
                        },
                        child,
                    ));
                    let font = if matches!(element.kind, Kind::Heading) && !self.heading.is_null() {
                        self.heading
                    } else {
                        self.font
                    };
                    SendMessageW(child, WM_SETFONT, font as usize, 1);
                    if SetWindowSubclass(
                        child,
                        Some(procedure),
                        1,
                        &mut *self.callback as *mut _ as usize,
                    ) == 0
                    {
                        return Err(std::io::Error::last_os_error().to_string());
                    }
                    match &element.kind {
                        Kind::Choice(options, _) => {
                            for option in options {
                                SendMessageW(
                                    child,
                                    CB_ADDSTRING,
                                    0,
                                    wide(option).as_ptr() as isize,
                                );
                            }
                        }
                        Kind::Input | Kind::Text => {
                            SendMessageW(child, EM_SETLIMITTEXT.into(), 1024 * 1024, 0);
                        }
                        _ => (),
                    }
                }
            }
            for ((previous, control), element) in self.controls.iter_mut().zip(elements) {
                let [x, y, width, height] = element.frame;
                // Combo box height includes its drop-down list on Win32.
                let height = if matches!(element.kind, Kind::Choice(..)) {
                    height + 220.0
                } else {
                    height
                };
                if scale_changed || previous.frame != element.frame {
                    SetWindowPos(
                        *control,
                        std::ptr::null_mut(),
                        (x * scale).round() as i32,
                        (y * scale).round() as i32,
                        (width.max(1.0) * scale).round() as i32,
                        (height.max(1.0) * scale).round() as i32,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
                if !matches!(element.kind, Kind::Input | Kind::Choice(..))
                    && previous.text != element.text
                {
                    let text = match element.kind {
                        Kind::Button | Kind::Check(_) => element.text.replace('&', "&&"),
                        _ => element.text.replace('\n', "\r\n"),
                    };
                    SetWindowTextW(*control, wide(&text).as_ptr());
                }
                match element.kind {
                    Kind::Check(on) => {
                        let state = if on { BST_CHECKED } else { BST_UNCHECKED };
                        if SendMessageW(*control, BM_GETCHECK, 0, 0) != state as isize {
                            SendMessageW(*control, BM_SETCHECK, state, 0);
                        }
                    }
                    Kind::Choice(_, index)
                        if SendMessageW(*control, CB_GETCURSEL, 0, 0) != index as isize =>
                    {
                        SendMessageW(*control, CB_SETCURSEL, index, 0);
                    }
                    _ => (),
                }
                if (IsWindowEnabled(*control) != 0) != element.enabled {
                    EnableWindow(*control, if element.enabled { 1 } else { 0 });
                }
                *previous = element.clone();
            }
        }
        Ok(())
    }

    pub fn actions(&mut self) -> Vec<u16> {
        crate::types::lock_forgiving(&self.callback.pending)
            .drain(..)
            .collect()
    }
    pub fn selection(&self, id: u16) -> usize {
        self.controls
            .iter()
            .find(|(element, _)| element.id == id)
            .map_or(0, |(_, control)| unsafe {
                SendMessageW(*control, CB_GETCURSEL, 0, 0).max(0) as usize
            })
    }
    pub fn checked(&self, id: u16) -> bool {
        self.controls
            .iter()
            .find(|(element, _)| element.id == id)
            .is_some_and(|(_, control)| unsafe {
                SendMessageW(*control, BM_GETCHECK, 0, 0) == BST_CHECKED as isize
            })
    }
    pub fn focus(&self, id: u16) {
        if let Some((_, control)) = self.controls.iter().find(|(element, _)| element.id == id) {
            unsafe {
                SetFocus(*control);
            }
        }
    }
    pub fn clear_input(&self, id: u16) {
        if let Some((_, control)) = self.controls.iter().find(|(element, _)| element.id == id) {
            unsafe {
                SetWindowTextW(*control, wide("").as_ptr());
            }
        }
    }
    pub fn scroll_to_end(&self, id: u16, _: usize) {
        if let Some((_, control)) = self.controls.iter().find(|(element, _)| element.id == id) {
            unsafe {
                SendMessageW(*control, EM_SETSEL.into(), usize::MAX, -1);
                SendMessageW(*control, EM_SCROLLCARET.into(), 0, 0);
            }
        }
    }
    unsafe fn clear(&mut self) {
        for (_, control) in self.controls.drain(..) {
            RemoveWindowSubclass(control, Some(procedure), 1);
            DestroyWindow(control);
        }
    }
}

impl Drop for Native {
    fn drop(&mut self) {
        unsafe {
            RemoveWindowSubclass(self.parent, Some(procedure), 1);
            self.clear();
            if !self.font.is_null() {
                DeleteObject(self.font as _);
            }
            if !self.heading.is_null() {
                DeleteObject(self.heading as _);
            }
        }
    }
}
