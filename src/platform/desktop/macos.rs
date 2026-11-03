use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

use cocoa::base::{id, nil, NO, YES};
use cocoa::foundation::{NSPoint, NSRect, NSSize, NSString};
use objc::runtime::{Class, Object, Sel};
use objc::{class, msg_send, sel, sel_impl};
use tao::event_loop::EventLoopProxy;
use tao::platform::macos::WindowExtMacOS;

use super::{Element, Kind};

struct Callback {
    pending: Mutex<VecDeque<u16>>,
    proxy: EventLoopProxy<()>,
}

extern "C" fn activate(this: &Object, _: Sel, sender: id) {
    unsafe {
        let callback = &*(*this.get_ivar::<*mut std::ffi::c_void>("callback") as *mut Callback);
        let tag: isize = msg_send![sender, tag];
        crate::types::lock_forgiving(&callback.pending).push_back(tag as u16);
        let _ = callback.proxy.send_event(());
    }
}

fn action_class() -> &'static Class {
    static CLASS: OnceLock<&'static Class> = OnceLock::new();
    CLASS.get_or_init(|| {
        let mut class = objc::declare::ClassDecl::new("ReCastDesktopActions", class!(NSObject))
            .expect("unique desktop action class");
        class.add_ivar::<*mut std::ffi::c_void>("callback");
        unsafe {
            class.add_method(sel!(activate:), activate as extern "C" fn(&Object, Sel, id));
        }
        class.register()
    })
}

struct Control {
    element: Element,
    view: id,
    value: id,
}

pub struct Native {
    window: id,
    content: id,
    target: id,
    scroll: id,
    callback: Box<Callback>,
    controls: Vec<Control>,
    height: f64,
}

unsafe fn string(value: &str) -> id {
    NSString::alloc(nil).init_str(value)
}

impl Native {
    pub fn new(window: &tao::window::Window, proxy: EventLoopProxy<()>) -> Result<Self, String> {
        unsafe {
            let window = window.ns_window() as id;
            let content: id = msg_send![window, contentView];
            let mut callback = Box::new(Callback {
                pending: Mutex::new(VecDeque::new()),
                proxy,
            });
            let target: id = msg_send![action_class(), new];
            (*target).set_ivar(
                "callback",
                &mut *callback as *mut Callback as *mut std::ffi::c_void,
            );
            Ok(Self {
                window,
                content,
                target,
                scroll: nil,
                callback,
                controls: Vec::new(),
                height: 0.0,
            })
        }
    }

    pub fn render(
        &mut self,
        elements: &[Element],
        height: f64,
        _: f64,
        scrolling: bool,
    ) -> Result<(), String> {
        unsafe {
            if scrolling && self.scroll == nil {
                let scroll: id = msg_send![class!(NSScrollView), new];
                let document: id = msg_send![class!(NSView), new];
                let _: () = msg_send![scroll, setHasVerticalScroller: YES];
                let _: () = msg_send![scroll, setAutohidesScrollers: YES];
                let _: () = msg_send![scroll, setDocumentView: document];
                let _: () = msg_send![self.content, addSubview: scroll];
                self.content = document;
                self.scroll = scroll;
                let _: () = msg_send![document, release];
            }
            let height = if scrolling {
                let root: id = msg_send![self.window, contentView];
                let bounds: NSRect = msg_send![root, bounds];
                let _: () = msg_send![self.scroll, setFrame: bounds];
                let size: NSSize = msg_send![self.scroll, contentSize];
                let document_height = elements
                    .iter()
                    .map(|e| e.frame[1] + e.frame[3] + 16.0)
                    .fold(height, f64::max);
                let previous: NSRect = msg_send![self.content, frame];
                let clip: id = msg_send![self.scroll, contentView];
                let visible: NSRect = msg_send![clip, bounds];
                let top = (previous.size.height - visible.origin.y - visible.size.height).max(0.0);
                let _: () =
                    msg_send![self.content, setFrameSize: NSSize::new(size.width, document_height)];
                let _: () = msg_send![clip, scrollToPoint: NSPoint::new(0.0, (document_height - size.height - top).max(0.0))];
                let _: () = msg_send![self.scroll, reflectScrolledClipView: clip];
                document_height
            } else {
                height
            };
            if self.controls.len() != elements.len()
                || self
                    .controls
                    .iter()
                    .zip(elements)
                    .any(|(a, b)| a.element.id != b.id || !a.element.kind.same_control(&b.kind))
            {
                self.clear();
                for element in elements {
                    let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1.0, 1.0));
                    let (view, value) = match &element.kind {
                        Kind::Button | Kind::Check(_) => {
                            let button: id = msg_send![class!(NSButton), alloc];
                            let button: id = msg_send![button, initWithFrame: frame];
                            let _: () = msg_send![button, setButtonType: if matches!(element.kind, Kind::Check(_)) { 3isize } else { 0isize }];
                            if matches!(element.kind, Kind::Button) {
                                let _: () = msg_send![button, setBezelStyle: 1isize];
                            }
                            (button, button)
                        }
                        Kind::Choice(options, _) => {
                            let menu: id = msg_send![class!(NSPopUpButton), alloc];
                            let menu: id = msg_send![menu, initWithFrame: frame pullsDown: NO];
                            for option in options {
                                let text = string(option);
                                let _: () = msg_send![menu, addItemWithTitle: text];
                                let _: () = msg_send![text, release];
                            }
                            (menu, menu)
                        }
                        Kind::Text => {
                            let scroll: id = msg_send![class!(NSScrollView), alloc];
                            let scroll: id = msg_send![scroll, initWithFrame: frame];
                            let _: () = msg_send![scroll, setHasVerticalScroller: YES];
                            let text: id = msg_send![class!(NSTextView), alloc];
                            let text: id = msg_send![text, initWithFrame: frame];
                            let _: () = msg_send![text, setEditable: NO];
                            let _: () = msg_send![text, setRichText: NO];
                            let _: () = msg_send![text, setSelectable: YES];
                            let _: () = msg_send![text, setVerticallyResizable: YES];
                            let _: () = msg_send![text, setHorizontallyResizable: NO];
                            let _: () =
                                msg_send![text, setMaxSize: NSSize::new(f64::MAX, f64::MAX)];
                            let container: id = msg_send![text, textContainer];
                            let _: () = msg_send![container, setWidthTracksTextView: YES];
                            let _: () = msg_send![scroll, setDocumentView: text];
                            let _: () = msg_send![text, release];
                            (scroll, text)
                        }
                        _ => {
                            let field: id = msg_send![class!(NSTextField), alloc];
                            let field: id = msg_send![field, initWithFrame: frame];
                            let input = matches!(element.kind, Kind::Input);
                            let _: () = msg_send![field, setEditable: if input { YES } else { NO }];
                            let _: () =
                                msg_send![field, setSelectable: if input { YES } else { NO }];
                            let _: () = msg_send![field, setBezeled: if input { YES } else { NO }];
                            let _: () =
                                msg_send![field, setDrawsBackground: if input { YES } else { NO }];
                            let cell: id = msg_send![field, cell];
                            let _: () = msg_send![cell, setWraps: if input { NO } else { YES }];
                            (field, field)
                        }
                    };
                    let font: id = msg_send![class!(NSFont), systemFontOfSize: if matches!(element.kind, Kind::Heading) { 19.0f64 } else { 13.0f64 }];
                    let _: () = msg_send![value, setFont: font];
                    if matches!(element.kind, Kind::Input | Kind::Choice(..) | Kind::Text) {
                        let label = string(&element.text);
                        let _: () = msg_send![value, setAccessibilityLabel: label];
                        let _: () = msg_send![label, release];
                    }
                    if matches!(
                        element.kind,
                        Kind::Button | Kind::Check(_) | Kind::Choice(..)
                    ) {
                        let _: () = msg_send![value, setTag: element.id as isize];
                        let _: () = msg_send![value, setTarget: self.target];
                        let _: () = msg_send![value, setAction: sel!(activate:)];
                    }
                    let _: () = msg_send![self.content, addSubview: view];
                    self.controls.push(Control {
                        element: Element {
                            text: String::new(),
                            frame: [0.0; 4],
                            ..element.clone()
                        },
                        view,
                        value,
                    });
                }
                // Explicit key-view order also works when full keyboard access is enabled.
                let focusable: Vec<_> = self
                    .controls
                    .iter()
                    .filter(|c| {
                        matches!(
                            c.element.kind,
                            Kind::Button
                                | Kind::Check(_)
                                | Kind::Choice(..)
                                | Kind::Input
                                | Kind::Text
                        )
                    })
                    .map(|c| c.value)
                    .collect();
                for index in 0..focusable.len() {
                    let _: () = msg_send![focusable[index], setNextKeyView: focusable[(index + 1) % focusable.len()]];
                }
            }
            for (control, element) in self.controls.iter_mut().zip(elements) {
                let [x, y, w, h] = element.frame;
                let frame = NSRect::new(
                    NSPoint::new(x, height - y - h),
                    NSSize::new(w.max(1.0), h.max(1.0)),
                );
                let moved = control.element.frame != element.frame || self.height != height;
                if moved {
                    let _: () = msg_send![control.view, setFrame: frame];
                }
                if moved && matches!(element.kind, Kind::Text) {
                    let content: NSSize = msg_send![control.view, contentSize];
                    let previous: NSRect = msg_send![control.value, frame];
                    let _: () = msg_send![control.value, setFrameSize: NSSize::new(content.width, previous.size.height.max(content.height))];
                }
                if !matches!(element.kind, Kind::Input) && control.element.text != element.text {
                    let text = string(&element.text);
                    match element.kind {
                        Kind::Button | Kind::Check(_) => {
                            let _: () = msg_send![control.value, setTitle: text];
                        }
                        Kind::Choice(..) => (),
                        Kind::Text => {
                            let _: () = msg_send![control.value, setString: text];
                        }
                        _ => {
                            let _: () = msg_send![control.value, setStringValue: text];
                        }
                    }
                    let _: () = msg_send![text, release];
                }
                if matches!(element.kind, Kind::Label | Kind::Heading) {
                    let _: () = msg_send![control.value, setAlignment: if element.rtl { 1isize } else { 0isize }];
                    let _: () = msg_send![control.value, setBaseWritingDirection: if element.rtl { 1isize } else { 0isize }];
                }
                match &element.kind {
                    Kind::Check(on) => {
                        let current: isize = msg_send![control.value, state];
                        let state = if *on { 1isize } else { 0isize };
                        if current != state {
                            let _: () = msg_send![control.value, setState: state];
                        }
                    }
                    Kind::Choice(_, index) => {
                        let current: isize = msg_send![control.value, indexOfSelectedItem];
                        if current != *index as isize {
                            let _: () =
                                msg_send![control.value, selectItemAtIndex: *index as isize];
                        }
                    }
                    _ => (),
                }
                if matches!(
                    element.kind,
                    Kind::Button | Kind::Check(_) | Kind::Choice(..) | Kind::Input
                ) {
                    let enabled: cocoa::base::BOOL = msg_send![control.value, isEnabled];
                    if (enabled != NO) != element.enabled {
                        let _: () = msg_send![control.value, setEnabled: if element.enabled { YES } else { NO }];
                    }
                }
                control.element = element.clone();
            }
            self.height = height;
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
            .find(|c| c.element.id == id)
            .map_or(0, |control| unsafe {
                let index: isize = msg_send![control.value, indexOfSelectedItem];
                index.max(0) as usize
            })
    }
    pub fn checked(&self, id: u16) -> bool {
        self.controls
            .iter()
            .find(|c| c.element.id == id)
            .is_some_and(|control| unsafe {
                let state: isize = msg_send![control.value, state];
                state == 1
            })
    }
    pub fn focus(&self, id: u16) {
        if let Some(control) = self.controls.iter().find(|c| c.element.id == id) {
            unsafe {
                let _: bool = msg_send![self.window, makeFirstResponder: control.value];
                if matches!(control.element.kind, Kind::Input) {
                    let editor: id =
                        msg_send![self.window, fieldEditor: YES forObject: control.value];
                    let _: () = msg_send![editor, setAutomaticSpellingCorrectionEnabled: NO];
                    let _: () = msg_send![editor, setAutomaticTextReplacementEnabled: NO];
                }
            }
        }
    }
    pub fn clear_input(&self, id: u16) {
        if let Some(control) = self.controls.iter().find(|c| c.element.id == id) {
            unsafe {
                let text = string("");
                let _: () = msg_send![control.value, setStringValue: text];
                let _: () = msg_send![text, release];
            }
        }
    }
    pub fn scroll_to_end(&self, id: u16, utf16_length: usize) {
        if let Some(control) = self.controls.iter().find(|c| c.element.id == id) {
            unsafe {
                let _: () = msg_send![control.value, scrollRangeToVisible: cocoa::foundation::NSRange::new(utf16_length as _, 0)];
            }
        }
    }
    unsafe fn clear(&mut self) {
        // A page can be replaced while one of its views is the first responder.
        let _: bool = msg_send![self.window, makeFirstResponder: nil];
        for control in self.controls.drain(..) {
            let _: () = msg_send![control.view, removeFromSuperview];
            let _: () = msg_send![control.view, release];
        }
    }
}

impl Drop for Native {
    fn drop(&mut self) {
        unsafe {
            self.clear();
            if self.scroll != nil {
                let _: () = msg_send![self.scroll, removeFromSuperview];
                let _: () = msg_send![self.scroll, release];
            }
            let _: () = msg_send![self.target, release];
        }
    }
}
