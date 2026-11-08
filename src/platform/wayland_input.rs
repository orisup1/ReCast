//! Optional compositor input without kernel-buffer pacing or physical key waits.
//!
//! Keymaps use printable physical positions for text and the real Backspace
//! position for deletion. Chromium and compositor bindings may interpret control
//! positions directly even when their XKB symbols have been remapped.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Write;
use std::os::fd::{AsFd, AsRawFd, FromRawFd};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{wl_callback, wl_registry, wl_seat};
use wayland_client::{delegate_noop, Connection, Dispatch, EventQueue, QueueHandle};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};

const PRINTABLE: &[u16] = &[
    2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 30, 31,
    32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 43, 44, 45, 46, 47, 48, 49, 50, 51, 52, 53, 57,
];
const BACKSPACE: u16 = 14;
const KEYS_PER_FLUSH: usize = 16;

struct Batch {
    map: String,
    keys: Vec<u32>,
}

/// Build every map before deletion, excluding physically held text positions.
fn batches(erase: usize, text: &str, held: &HashSet<u16>) -> Option<Vec<Batch>> {
    if erase > 0 && held.contains(&BACKSPACE) {
        return None;
    }
    let available: Vec<_> = PRINTABLE
        .iter()
        .copied()
        .filter(|k| !held.contains(k))
        .collect();
    let mut result = Vec::new();
    let mut remaining = text.chars().peekable();
    let mut deletes = erase;
    while deletes > 0 || remaining.peek().is_some() {
        let mut assigned = HashMap::new();
        let mut keys = vec![u32::from(BACKSPACE); deletes];
        deletes = 0;
        while let Some(&ch) = remaining.peek() {
            let key = if let Some(&key) = assigned.get(&ch) {
                key
            } else if ch == '\n' || ch == '\t' {
                let key = if ch == '\n' { 28 } else { 15 };
                if held.contains(&key) {
                    return None;
                }
                assigned.insert(ch, key);
                key
            } else if !ch.is_control() {
                let used = assigned.keys().filter(|&&c| c != '\n' && c != '\t').count();
                let Some(&key) = available.get(used) else {
                    break;
                };
                assigned.insert(ch, key);
                key
            } else {
                return None;
            };
            keys.push(u32::from(key));
            remaining.next();
        }
        if keys.is_empty() {
            return None;
        }
        let mut definitions = vec![(BACKSPACE, "BackSpace".to_string())];
        definitions.extend(assigned.into_iter().map(|(ch, key)| {
            let symbol = match ch {
                '\n' => "Return".into(),
                '\t' => "Tab".into(),
                _ => format!("U{:04X}", u32::from(ch)),
            };
            (key, symbol)
        }));
        definitions.sort_unstable_by_key(|(key, _)| *key);
        let codes: String = definitions
            .iter()
            .map(|(key, _)| format!("<K{key}>={};", key + 8))
            .collect();
        let symbols: String = definitions
            .iter()
            .map(|(key, symbol)| {
                let groups = (1..=4)
                    .map(|group| format!("type[Group{group}]=\"RECAST_LITERAL\",symbols[Group{group}]=[{symbol}]"))
                    .collect::<Vec<_>>()
                    .join(",");
                format!("key <K{key}> {{{groups}}};")
            })
            .collect();
        // GTK 3 enumerates with a strict upper bound. An unused final code
        // keeps every emitted control/text key discoverable by its bindings.
        // Consume Lock explicitly to prevent implicit Caps Lock conversion,
        // while preserving physical lock indicators on the virtual keyboard.
        // Mirror all four XKB groups: global compositor layout switches must
        // not fail because this keyboard has fewer groups than physical ones.
        let mut map = format!(
            r#"xkb_keymap {{
            xkb_keycodes "recast" {{
                minimum=8; maximum=255; <LAST>=255; <CPLK>=66; <NMLK>=77;
                indicator 1="Caps Lock"; indicator 2="Num Lock";
                {codes}
            }};
            xkb_types "recast" {{
                include "complete"
                type "RECAST_LITERAL" {{
                    modifiers=Lock; map[None]=Level1; map[Lock]=Level1;
                    level_name[Level1]="Any";
                }};
            }};
            xkb_compatibility "recast" {{include "complete"}};
            xkb_symbols "recast" {{
                name[Group1]="recast-injector";
                name[Group2]="recast-injector";
                name[Group3]="recast-injector";
                name[Group4]="recast-injector";
                key <CPLK> {{[Caps_Lock]}}; key <NMLK> {{[Num_Lock]}};
                modifier_map Lock {{<CPLK>}}; modifier_map Mod2 {{<NMLK>}};
                {symbols}
            }};
        }};"#
        );
        map.push('\0');
        result.push(Batch { map, keys });
    }
    Some(result)
}

struct State;
impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
impl Dispatch<wl_callback::WlCallback, Arc<AtomicBool>> for State {
    fn event(
        _: &mut Self,
        _: &wl_callback::WlCallback,
        _: wl_callback::Event,
        done: &Arc<AtomicBool>,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        done.store(true, Ordering::Release);
    }
}
delegate_noop!(State: ignore wl_seat::WlSeat);
delegate_noop!(State: ignore ZwpVirtualKeyboardManagerV1);
delegate_noop!(State: ignore ZwpVirtualKeyboardV1);

struct Keyboard {
    connection: Connection,
    queue: EventQueue<State>,
    keyboard: ZwpVirtualKeyboardV1,
}
impl Keyboard {
    fn connect() -> Option<Self> {
        let connection = Connection::connect_to_env().ok()?;
        let (globals, queue) = registry_queue_init::<State>(&connection).ok()?;
        // Picking an arbitrary seat in a multi-seat session could target a
        // different user context. Retain conservative replay there.
        if globals
            .contents()
            .with_list(|list| list.iter().filter(|g| g.interface == "wl_seat").count())
            != 1
        {
            return None;
        }
        let qh = queue.handle();
        let seat: wl_seat::WlSeat = globals.bind(&qh, 1..=1, ()).ok()?;
        let manager: ZwpVirtualKeyboardManagerV1 = globals.bind(&qh, 1..=1, ()).ok()?;
        let keyboard = manager.create_virtual_keyboard(&seat, &qh, ());
        connection.roundtrip().ok()?;
        Some(Self {
            connection,
            queue,
            keyboard,
        })
    }

    /// Confirm compositor acceptance before sending keys. A missing reply
    /// must never leave capture gated forever.
    fn sync(&mut self) -> Option<()> {
        let done = Arc::new(AtomicBool::new(false));
        self.connection
            .display()
            .sync(&self.queue.handle(), Arc::clone(&done));
        self.connection.flush().ok()?;
        let deadline = Instant::now() + Duration::from_millis(30);
        loop {
            self.queue.dispatch_pending(&mut State).ok()?;
            if done.load(Ordering::Acquire) {
                return Some(());
            }
            let left = deadline.checked_duration_since(Instant::now())?;
            let Some(read) = self.queue.prepare_read() else {
                continue;
            };
            let mut poll = nix::libc::pollfd {
                fd: self.connection.as_fd().as_raw_fd(),
                events: nix::libc::POLLIN,
                revents: 0,
            };
            let timeout = left.as_millis().clamp(1, 30) as i32;
            if unsafe { nix::libc::poll(&mut poll, 1, timeout) } <= 0 {
                return None;
            }
            read.read().ok()?;
        }
    }

    fn emit(
        &mut self,
        batches: Vec<Batch>,
        locked_mods: u32,
        valid: &mut impl FnMut() -> bool,
        started: &mut impl FnMut(),
    ) -> Option<()> {
        self.queue.dispatch_pending(&mut State).ok()?;
        for batch in batches {
            if !valid() {
                return None;
            }
            let name = b"recast-keymap\0";
            let fd =
                unsafe { nix::libc::memfd_create(name.as_ptr().cast(), nix::libc::MFD_CLOEXEC) };
            if fd < 0 {
                return None;
            }
            let mut file = unsafe { File::from_raw_fd(fd) };
            file.write_all(batch.map.as_bytes()).ok()?;
            self.keyboard
                .keymap(1, file.as_fd(), batch.map.len().try_into().ok()?);
            self.keyboard.modifiers(0, 0, locked_mods & 18, 0);
            self.sync()?;
            for chunk in batch.keys.chunks(KEYS_PER_FLUSH) {
                if !valid() {
                    return None;
                }
                let mut now = std::mem::MaybeUninit::<nix::libc::timespec>::uninit();
                if unsafe { nix::libc::clock_gettime(nix::libc::CLOCK_MONOTONIC, now.as_mut_ptr()) }
                    != 0
                {
                    return None;
                }
                let now = unsafe { now.assume_init() };
                let time = (now.tv_sec as u64 * 1000 + now.tv_nsec as u64 / 1_000_000) as u32;
                started();
                for &key in chunk {
                    self.keyboard.key(time, key, 1);
                    self.keyboard.key(time, key, 0);
                }
                self.connection.flush().ok()?;
            }
        }
        Some(())
    }
}

struct Slot {
    keyboard: Option<Keyboard>,
    starting: bool,
    attempted: Option<Instant>,
}
static SLOT: Mutex<Slot> = Mutex::new(Slot {
    keyboard: None,
    starting: false,
    attempted: None,
});

/// Discovery runs away from capture; unsupported compositors keep uinput.
pub fn available() -> bool {
    if std::env::var("XDG_SESSION_TYPE").as_deref() != Ok("wayland")
        || !crate::config::Config::global().wayland_input_enabled
    {
        return false;
    }
    let Ok(mut slot) = SLOT.try_lock() else {
        return false;
    };
    if slot.keyboard.is_some() {
        return true;
    }
    if !slot.starting
        && slot
            .attempted
            .is_none_or(|when| when.elapsed() >= Duration::from_secs(5))
    {
        slot.starting = true;
        slot.attempted = Some(Instant::now());
        if std::thread::Builder::new()
            .name("recast-wayland".into())
            .spawn(|| {
                let keyboard = Keyboard::connect();
                let mut slot = crate::types::lock_forgiving(&SLOT);
                slot.keyboard = keyboard;
                slot.starting = false;
            })
            .is_err()
        {
            slot.starting = false;
        }
    }
    false
}

pub fn inject(
    erase: usize,
    text: &str,
    held: &HashSet<u16>,
    locked_mods: u32,
    mut valid: impl FnMut() -> bool,
    mut started: impl FnMut(),
) -> Option<()> {
    let prepared = batches(erase, text, held)?;
    let mut slot = SLOT.try_lock().ok()?;
    let result = slot
        .keyboard
        .as_mut()?
        .emit(prepared, locked_mods, &mut valid, &mut started);
    if result.is_none() {
        // Never leave unflushed text queued for a later correction. Disconnect
        // also releases any key whose delivery was interrupted by an I/O error.
        slot.keyboard = None;
        slot.attempted = None;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_positions_exclude_controls_and_physical_holds() {
        let held = HashSet::from([2, 18, 57]);
        let prepared = batches(4, "Hello שלום ", &held).unwrap();
        for batch in prepared {
            assert!(batch.keys[..4].iter().all(|&key| key == 14));
            assert!(batch.keys[4..]
                .iter()
                .all(|key| PRINTABLE.contains(&(*key as u16)) && !held.contains(&(*key as u16))));
            assert!(batch.map.contains("BackSpace"));
            assert!(batch.map.contains("U05E9"));
        }
        assert!(batches(1, "text", &HashSet::from([14])).is_none());
        assert!(batches(1, "\0", &HashSet::new()).is_none());
    }

    #[test]
    fn generated_maps_keep_control_keys_visible_to_gtk_lookup() {
        let Some(xkb) = xkbcommon_dl::xkbcommon_option() else {
            return;
        };
        let prepared = batches(5, "eive ", &HashSet::new()).unwrap();
        for batch in prepared {
            let text = std::ffi::CString::from_vec_with_nul(batch.map.into_bytes()).unwrap();
            unsafe {
                let context =
                    (xkb.xkb_context_new)(xkbcommon_dl::xkb_context_flags::XKB_CONTEXT_NO_FLAGS);
                assert!(!context.is_null());
                let map = (xkb.xkb_keymap_new_from_string)(
                    context,
                    text.as_ptr(),
                    xkbcommon_dl::xkb_keymap_format::XKB_KEYMAP_FORMAT_TEXT_V1,
                    xkbcommon_dl::xkb_keymap_compile_flags::XKB_KEYMAP_COMPILE_NO_FLAGS,
                );
                assert!(!map.is_null());
                let maximum = (xkb.xkb_keymap_max_keycode)(map);
                let mut syms = std::ptr::null();
                let count = (xkb.xkb_keymap_key_get_syms_by_level)(map, 22, 0, 0, &mut syms);
                let backspace = count == 1 && *syms == 0xff08;
                (xkb.xkb_keymap_unref)(map);
                (xkb.xkb_context_unref)(context);
                assert!(backspace);
                assert!(
                    batch.keys.iter().all(|key| key + 8 < maximum),
                    "GTK's key binding enumeration excludes the highest code"
                );
            }
        }
    }

    #[test]
    fn literal_case_and_hebrew_are_independent_of_caps_and_num_lock() {
        let Some(xkb) = xkbcommon_dl::xkbcommon_option() else {
            return;
        };
        type LedQuery =
            unsafe extern "C" fn(*mut xkbcommon_dl::xkb_state, *const std::ffi::c_char) -> i32;
        let library =
            unsafe { nix::libc::dlopen(c"libxkbcommon.so.0".as_ptr(), nix::libc::RTLD_LAZY) };
        assert!(!library.is_null());
        let symbol = unsafe { nix::libc::dlsym(library, c"xkb_state_led_name_is_active".as_ptr()) };
        assert!(!symbol.is_null());
        let led_active = unsafe { std::mem::transmute::<*mut std::ffi::c_void, LedQuery>(symbol) };
        let prepared = batches(0, "aAש", &HashSet::new()).unwrap();
        for batch in prepared {
            let text = std::ffi::CString::from_vec_with_nul(batch.map.into_bytes()).unwrap();
            unsafe {
                let context =
                    (xkb.xkb_context_new)(xkbcommon_dl::xkb_context_flags::XKB_CONTEXT_NO_FLAGS);
                let map = (xkb.xkb_keymap_new_from_string)(
                    context,
                    text.as_ptr(),
                    xkbcommon_dl::xkb_keymap_format::XKB_KEYMAP_FORMAT_TEXT_V1,
                    xkbcommon_dl::xkb_keymap_compile_flags::XKB_KEYMAP_COMPILE_NO_FLAGS,
                );
                assert!(!map.is_null());
                let state = (xkb.xkb_state_new)(map);
                assert_eq!((xkb.xkb_keymap_num_layouts)(map), 4);
                for (group, locks) in
                    (0..4).flat_map(|group| [0, 2, 16, 18].map(|locks| (group, locks)))
                {
                    (xkb.xkb_state_update_mask)(state, 0, 0, locks, 0, 0, group);
                    let visible: String = batch
                        .keys
                        .iter()
                        .map(|key| {
                            let sym = (xkb.xkb_state_key_get_one_sym)(state, key + 8);
                            char::from_u32((xkb.xkb_keysym_to_utf32)(sym)).unwrap()
                        })
                        .collect();
                    assert_eq!(visible, "aAש");
                    let caps = std::ffi::CString::new("Caps Lock").unwrap();
                    let num = std::ffi::CString::new("Num Lock").unwrap();
                    assert_eq!(led_active(state, caps.as_ptr()), i32::from(locks & 2 != 0));
                    assert_eq!(led_active(state, num.as_ptr()), i32::from(locks & 16 != 0));
                }
                (xkb.xkb_state_unref)(state);
                (xkb.xkb_keymap_unref)(map);
                (xkb.xkb_context_unref)(context);
            }
        }
        unsafe {
            nix::libc::dlclose(library);
        }
    }

    #[test]
    fn long_unicode_text_uses_complete_maps_before_any_deletion() {
        let text: String = (0x5d0..0x660).filter_map(char::from_u32).collect();
        let prepared = batches(7, &text, &HashSet::new()).unwrap();
        assert!(prepared.len() > 1);
        assert_eq!(
            prepared.iter().map(|batch| batch.keys.len()).sum::<usize>(),
            7 + text.chars().count()
        );
        assert!(prepared[0].keys[..7].iter().all(|&key| key == 14));
        assert!(prepared
            .iter()
            .skip(1)
            .all(|batch| !batch.keys.contains(&14)));
    }
}
