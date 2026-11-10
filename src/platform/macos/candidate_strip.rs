//! Passive, caret-anchored typing choices. No document text is fetched.
use super::*;
use cocoa::base::{id, nil};
use cocoa::foundation::{NSPoint, NSRect, NSSize, NSString};
use objc::{class, msg_send, sel, sel_impl};
use std::cell::Cell;
use std::sync::atomic::AtomicU64;

static EPOCH: AtomicU64 = AtomicU64::new(0);
static STOPPED: AtomicBool = AtomicBool::new(false);
thread_local! { static PANEL: Cell<id> = const { Cell::new(nil) }; }

pub(super) fn hide() {
    let ticket = EPOCH.fetch_add(1, Ordering::SeqCst) + 1;
    dispatch::Queue::main().exec_async(move || {
        if EPOCH.load(Ordering::SeqCst) != ticket {
            return;
        }
        WATCH.with(|watch| {
            // Hiding also runs during acceptance; only the engine owns its choice.
            watch.borrow_mut().take();
        });
        PANEL.with(|panel| unsafe {
            if panel.get() != nil {
                let _: () = msg_send![panel.get(), orderOut: nil];
            }
        });
    });
}

fn caret() -> Option<NSRect> {
    use core_foundation::{base::TCFType, string::CFString};
    extern "C" {
        fn AXUIElementCopyParameterizedAttributeValue(
            element: *const c_void,
            name: *const c_void,
            parameter: *const c_void,
            result: *mut *const c_void,
        ) -> i32;
    }
    let focus = focused_target()?;
    let caret_bounds = || -> Option<NSRect> {
        let selection = selected_range(&focus)?;
        let insertion = core_foundation_sys::base::CFRange {
            location: selection.location,
            length: 0,
        };
        unsafe {
            let range = AXValueCreate(
                4,
                (&insertion as *const core_foundation_sys::base::CFRange).cast(),
            );
            if range.is_null() {
                return None;
            }
            let name = CFString::new("AXBoundsForRange");
            let mut value = std::ptr::null();
            let result = AXUIElementCopyParameterizedAttributeValue(
                focus.0,
                name.as_concrete_TypeRef().cast(),
                range,
                &mut value,
            );
            CFRelease(range);
            if result != 0 || value.is_null() {
                return None;
            }
            let mut rect = NSRect::new(NSPoint::new(0., 0.), NSSize::new(0., 0.));
            let valid = AXValueGetValue(value, 3, (&mut rect as *mut NSRect).cast()) != 0;
            CFRelease(value);
            (valid
                && rect.origin.x.is_finite()
                && rect.origin.y.is_finite()
                && rect.size.height > 0.
                && rect.size.height < 200.)
                .then_some(rect)
        }
    };
    caret_bounds().or_else(|| {
        // Single-line search controls (including Spotlight) may omit caret bounds.
        // Anchor below their actual field, never below an arbitrary window/document.
        let role = ax_attribute(&focus, "AXRole")?
            .downcast::<CFString>()?
            .to_string();
        if !matches!(
            role.as_str(),
            "AXTextField" | "AXSearchField" | "AXComboBox"
        ) {
            return None;
        }
        let position = ax_attribute(&focus, "AXPosition")?;
        let size = ax_attribute(&focus, "AXSize")?;
        let mut point = NSPoint::new(0., 0.);
        let mut dimensions = NSSize::new(0., 0.);
        unsafe {
            if AXValueGetValue(
                position.as_CFTypeRef(),
                1,
                (&mut point as *mut NSPoint).cast(),
            ) == 0
                || AXValueGetValue(
                    size.as_CFTypeRef(),
                    2,
                    (&mut dimensions as *mut NSSize).cast(),
                ) == 0
            {
                return None;
            }
        }
        (point.x.is_finite()
            && point.y.is_finite()
            && dimensions.width > 0.
            && dimensions.width <= 1600.
            && dimensions.height > 0.
            && dimensions.height <= 120.)
            .then_some(NSRect::new(point, dimensions))
    })
}

pub(super) fn show(engine: Arc<Engine<Mac>>, generation: u64, choices: Vec<String>) {
    if STOPPED.load(Ordering::Acquire) {
        return;
    }
    let ticket = EPOCH.fetch_add(1, Ordering::SeqCst) + 1;
    std::thread::spawn(move || {
        if !engine.replacement_valid(generation) {
            return;
        }
        let Some(anchor) = caret() else {
            if EPOCH.load(Ordering::SeqCst) == ticket {
                engine.cancel_preview(generation);
            }
            return;
        };
        let spotlight = focused_target().as_ref().and_then(Mac::app_id).as_deref()
            == Some("com.apple.Spotlight");
        dispatch::Queue::main().exec_async(move || unsafe {
            if STOPPED.load(Ordering::Acquire) || EPOCH.load(Ordering::SeqCst) != ticket || !engine.replacement_valid(generation) { return; }
            if choices.is_empty() { engine.cancel_preview(generation); return; }
            let screens: id = msg_send![class!(NSScreen), screens];
            let count: usize = msg_send![screens, count];
            if count == 0 { engine.cancel_preview(generation); return; }
            let first: id = msg_send![screens, objectAtIndex: 0usize];
            let primary: NSRect = msg_send![first, frame];
            let point = NSPoint::new(anchor.origin.x, primary.size.height - anchor.origin.y - anchor.size.height);
            let mut bounds: Option<NSRect> = None;
            for i in 0..count {
                let screen: id = msg_send![screens, objectAtIndex: i];
                let frame: NSRect = msg_send![screen, frame];
                if point.x >= frame.origin.x && point.x <= frame.origin.x + frame.size.width && point.y >= frame.origin.y && point.y <= frame.origin.y + frame.size.height {
                    bounds = Some(msg_send![screen, visibleFrame]); break;
                }
            }
            let Some(bounds) = bounds else { engine.cancel_preview(generation); return; };
            // Cycling keeps the same focus context; reuse its native observers.
            let watching = WATCH.with(|slot| {
                let mut slot = slot.borrow_mut();
                if slot.as_ref().is_none_or(|watch| watch.generation != generation) {
                    *slot = watch(Arc::clone(&engine), generation);
                }
                slot.is_some()
            });
            if !watching { engine.cancel_preview(generation); return; }
            if !engine.preview_ready(generation) { return; }
            PANEL.with(|slot| {
                let mut panel = slot.get();
                if panel == nil {
                    panel = msg_send![class!(NSPanel), alloc];
                    panel = msg_send![panel, initWithContentRect: NSRect::new(NSPoint::new(0.,0.), NSSize::new(400.,28.)) styleMask: 128u64 backing: 2u64 defer: false];
                    let _: () = msg_send![panel, setReleasedWhenClosed: false];
                    let _: () = msg_send![panel, setHidesOnDeactivate: true];
                    let _: () = msg_send![panel, setIgnoresMouseEvents: true];
                    let _: () = msg_send![panel, setHasShadow: true];
                    let _: () = msg_send![panel, setAnimationBehavior: 2i64];
                    let _: () = msg_send![panel, setBecomesKeyOnlyIfNeeded: true];
                    slot.set(panel);
                }
                // Spotlight floats above ordinary panels. Match a popup menu's
                // level only there, keeping this nonactivating strip visible.
                extern "C" { fn CGWindowLevelForKey(key: u32) -> i32; }
                let level = if spotlight { i64::from(CGWindowLevelForKey(11)) } else { 3 };
                let _: () = msg_send![panel, setLevel: level];
                let font: id = msg_send![class!(NSFont), systemFontOfSize: 13.0f64];
                let mut labels = Vec::with_capacity(choices.len());
                let mut widths = Vec::with_capacity(choices.len());
                for choice in &choices {
                    let selected = choice.starts_with("• ");
                    let choice = choice.strip_prefix("• ").unwrap_or(choice);
                    let string = NSString::alloc(nil).init_str(&format!("\u{2068}{}\u{2069}", choice));
                    let label: id = msg_send![class!(NSTextField), labelWithString: string];
                    let _: () = msg_send![string, release];
                    let _: () = msg_send![label, setFont: font];
                    let color: id = if selected {
                        msg_send![class!(NSColor), selectedMenuItemTextColor]
                    } else {
                        msg_send![class!(NSColor), labelColor]
                    };
                    let _: () = msg_send![label, setTextColor: color];
                    let cell: id = msg_send![label, cell];
                    let _: () = msg_send![cell, setLineBreakMode: 4u64];
                    let size: NSSize = msg_send![label, fittingSize];
                    widths.push((size.width + 24.).clamp(76., 200.));
                    labels.push((label, selected));
                }
                let hint = "Tap again to cycle · Type or Space to accept · Esc to dismiss";
                let hint_string = NSString::alloc(nil).init_str(hint);
                let hint_label: id = msg_send![class!(NSTextField), labelWithString: hint_string];
                let _: () = msg_send![hint_string, release];
                let hint_font: id = msg_send![class!(NSFont), systemFontOfSize: 10.0f64];
                let hint_color: id = msg_send![class!(NSColor), secondaryLabelColor];
                let _: () = msg_send![hint_label, setFont: hint_font];
                let _: () = msg_send![hint_label, setTextColor: hint_color];
                let hint_size: NSSize = msg_send![hint_label, fittingSize];
                let total: f64 = widths.iter().sum();
                let width = (total + 16.).max(hint_size.width + 24.).min(bounds.size.width);
                let height = 58.;
                let x = point.x.clamp(bounds.origin.x, bounds.origin.x + bounds.size.width - width);
                let y = strip_y(point.y, anchor.size.height, height, bounds.origin.y, bounds.size.height);
                let view: id = msg_send![class!(NSView), alloc];
                let view: id = msg_send![view, initWithFrame: NSRect::new(NSPoint::new(0.,0.), NSSize::new(width,height))];
                let scale = ((width - 16.) / total).min(1.);
                let mut left = 8.;
                for ((label, selected), cell_width) in labels.into_iter().zip(widths) {
                    let cell_width = cell_width * scale;
                    if selected {
                        let highlight: id = msg_send![class!(NSView), alloc];
                        let highlight: id = msg_send![highlight, initWithFrame: NSRect::new(NSPoint::new(left,25.), NSSize::new(cell_width - 4.,27.))];
                        let _: () = msg_send![highlight, setWantsLayer: true];
                        let layer: id = msg_send![highlight, layer];
                        let color: id = msg_send![class!(NSColor), selectedContentBackgroundColor];
                        let cg_color: *const c_void = msg_send![color, CGColor];
                        let _: () = msg_send![layer, setBackgroundColor: cg_color];
                        let _: () = msg_send![layer, setCornerRadius: 5.0f64];
                        let _: () = msg_send![view, addSubview: highlight];
                        let _: () = msg_send![highlight, release];
                    }
                    let _: () = msg_send![label, setFrame: NSRect::new(NSPoint::new(left + 8.,29.), NSSize::new((cell_width - 20.).max(0.),18.))];
                    let _: () = msg_send![view, addSubview: label];
                    left += cell_width;
                }
                let _: () = msg_send![hint_label, setFrame: NSRect::new(NSPoint::new(12.,7.), NSSize::new(width - 24.,14.))];
                let _: () = msg_send![view, addSubview: hint_label];
                let _: () = msg_send![panel, setContentView: view];
                let _: () = msg_send![view, release];
                let _: () = msg_send![panel, setFrame: NSRect::new(NSPoint::new(x,y),NSSize::new(width,height)) display: true];
                let _: () = msg_send![panel, orderFrontRegardless];
            });
        });
    });
}

fn strip_y(
    caret_bottom: f64,
    caret_height: f64,
    height: f64,
    bottom: f64,
    screen_height: f64,
) -> f64 {
    let below = caret_bottom - height - 4.;
    let preferred = if below >= bottom {
        below
    } else {
        caret_bottom + caret_height + 4.
    };
    preferred.clamp(bottom, (bottom + screen_height - height).max(bottom))
}

#[cfg(test)]
mod tests {
    use super::strip_y;

    #[test]
    fn strip_avoids_caret_at_screen_edges() {
        assert_eq!(strip_y(300., 18., 58., 0., 800.), 238.);
        assert_eq!(strip_y(20., 18., 58., 0., 800.), 42.);
        assert_eq!(strip_y(-780., 18., 58., -800., 800.), -758.);
        assert_eq!(strip_y(20., 18., 58., 0., 50.), 0.);
    }
}

// Native focus notifications cancel without a timer or document polling.
struct Watch {
    generation: u64,
    observer: *const c_void,
    context: *mut Arc<Engine<Mac>>,
    workspace: id,
}
thread_local! { static WATCH: std::cell::RefCell<Option<Watch>> = const { std::cell::RefCell::new(None) }; }
extern "C" {
    fn AXObserverCreate(
        pid: i32,
        callback: unsafe extern "C" fn(*const c_void, *const c_void, *const c_void, *mut c_void),
        observer: *mut *const c_void,
    ) -> i32;
    fn AXObserverAddNotification(
        observer: *const c_void,
        element: *const c_void,
        notification: *const c_void,
        context: *mut c_void,
    ) -> i32;
    fn AXObserverGetRunLoopSource(
        observer: *const c_void,
    ) -> core_foundation_sys::runloop::CFRunLoopSourceRef;
    fn AXUIElementCreateApplication(pid: i32) -> *const c_void;
}
unsafe extern "C" fn changed(
    _: *const c_void,
    _: *const c_void,
    _: *const c_void,
    context: *mut c_void,
) {
    let engine = &*context.cast::<Arc<Engine<Mac>>>();
    engine.forget_everything();
}
impl Drop for Watch {
    fn drop(&mut self) {
        unsafe {
            use core_foundation_sys::runloop::*;
            let workspace: id = msg_send![class!(NSWorkspace), sharedWorkspace];
            let center: id = msg_send![workspace, notificationCenter];
            let _: () = msg_send![center, removeObserver: self.workspace];
            let _: () = msg_send![self.workspace, release];
            if !self.observer.is_null() {
                CFRunLoopRemoveSource(
                    CFRunLoopGetMain(),
                    AXObserverGetRunLoopSource(self.observer),
                    kCFRunLoopCommonModes,
                );
                CFRelease(self.observer);
            }
            drop(Box::from_raw(self.context));
        }
    }
}
extern "C" fn application_changed(this: &objc::runtime::Object, _: objc::runtime::Sel, _: id) {
    unsafe {
        let context = *this.get_ivar::<*mut c_void>("context");
        (&*context.cast::<Arc<Engine<Mac>>>()).forget_everything();
    }
}
fn workspace_observer_class() -> &'static objc::runtime::Class {
    static CLASS: OnceLock<&'static objc::runtime::Class> = OnceLock::new();
    CLASS.get_or_init(|| {
        let mut class =
            objc::declare::ClassDecl::new("ReCastCandidateFocusObserver", class!(NSObject))
                .expect("unique candidate observer class");
        class.add_ivar::<*mut c_void>("context");
        unsafe {
            class.add_method(
                sel!(applicationChanged:),
                application_changed
                    as extern "C" fn(&objc::runtime::Object, objc::runtime::Sel, id),
            );
        }
        class.register()
    })
}
fn watch(engine: Arc<Engine<Mac>>, generation: u64) -> Option<Watch> {
    use core_foundation::{base::TCFType, string::CFString};
    use core_foundation_sys::runloop::*;
    let focus = focused_target()?;
    unsafe {
        let mut pid = 0;
        if AXUIElementGetPid(focus.0, &mut pid) != 0 {
            return None;
        }
        let mut observer = std::ptr::null();
        if AXObserverCreate(pid, changed, &mut observer) != 0 {
            observer = std::ptr::null();
        }
        let context = Box::into_raw(Box::new(engine));
        let target: id = msg_send![workspace_observer_class(), new];
        (*target).set_ivar("context", context.cast::<c_void>());
        let workspace: id = msg_send![class!(NSWorkspace), sharedWorkspace];
        let center: id = msg_send![workspace, notificationCenter];
        let name = NSString::alloc(nil).init_str("NSWorkspaceDidActivateApplicationNotification");
        let _: () = msg_send![center, addObserver: target selector: sel!(applicationChanged:) name: name object: nil];
        let _: () = msg_send![name, release];
        let watch = Watch {
            generation,
            observer,
            context,
            workspace: target,
        };
        let app = AXUIElementCreateApplication(pid);
        if !app.is_null() && !observer.is_null() {
            for name in [
                "AXFocusedUIElementChanged",
                "AXApplicationDeactivated",
                "AXFocusedWindowChanged",
            ] {
                let name = CFString::new(name);
                let _ = AXObserverAddNotification(
                    observer,
                    app,
                    name.as_concrete_TypeRef().cast(),
                    context.cast(),
                ) == 0;
            }
        }
        if !app.is_null() {
            CFRelease(app);
        }
        if !observer.is_null() {
            CFRunLoopAddSource(
                CFRunLoopGetMain(),
                AXObserverGetRunLoopSource(observer),
                kCFRunLoopCommonModes,
            );
        }
        Some(watch)
    }
}

/// Called on the main thread after the event loop exits. Invalidate queued work first.
pub(super) fn shutdown() {
    STOPPED.store(true, Ordering::Release);
    EPOCH.fetch_add(1, Ordering::SeqCst);
    WATCH.with(|watch| {
        watch.borrow_mut().take();
    });
    PANEL.with(|slot| unsafe {
        let panel = slot.replace(nil);
        if panel != nil {
            let _: () = msg_send![panel, close];
            let _: () = msg_send![panel, release];
        }
    });
}
