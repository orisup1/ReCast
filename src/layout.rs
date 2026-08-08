use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::types::Language;

// ─────────────────────────────────────────────────────────────────────────────
// Current-layout query (shared) — the signal that lets the dictionary anchor its
// decision on what the user is *actually* typing in, instead of guessing from
// the keystrokes alone.
// ─────────────────────────────────────────────────────────────────────────────

// Brief cache so the per-word lookup doesn't hammer the OS — notably Linux,
// where each query spawns a `hyprctl` subprocess. 300 ms is short enough that a
// manual layout change is picked up almost immediately, long enough to absorb a
// burst of words. Our own `switch_layout_to` updates the cache directly, so
// self-initiated switches are reflected with no staleness.
static LAYOUT_CACHE: Mutex<Option<(Instant, Language)>> = Mutex::new(None);
const LAYOUT_TTL: Duration = Duration::from_millis(300);

/// What happened when a correction asked for a layout.
///
/// This used to be a `bool`, and the bool conflated the two things a caller
/// most needs to tell apart: "already on that layout, nothing to do" and "the
/// switch did not happen". Both were `false`. The undo path
/// (`platform::linux`) reads it as *may I retype now?* and bailed out on
/// `false` — so undoing a correction whose layout happened to already be
/// right did nothing at all, silently, and looked exactly like a missed
/// keystroke.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutSwitch {
    /// Already on the requested layout. Nothing was sent, and nothing needed
    /// to be — this is success.
    AlreadyThere,
    /// Switched, and the OS confirmed the new layout is live.
    Switched,
    /// Refused, or never took effect before the deadline. Retyping now would
    /// spell the word out under the wrong layout.
    Failed,
}

impl LayoutSwitch {
    /// Whether the requested layout is live, so keys may be injected.
    ///
    /// Read only on Linux, and that is not an oversight: `uinput` speaks
    /// keycodes, so what a replayed key *spells* depends on the layout being
    /// live when it lands, and injecting early produces garbage. macOS and
    /// Windows insert the restored word as text, which is layout-independent —
    /// they switch the layout purely so the user's *next* keystroke is in the
    /// right language, and a refusal there costs nothing worth abandoning an
    /// undo over.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn ready(self) -> bool {
        !matches!(self, LayoutSwitch::Failed)
    }

    /// Whether this call is what changed the layout — for the debug log, which
    /// reports what happened rather than what is now true.
    pub fn changed(self) -> bool {
        matches!(self, LayoutSwitch::Switched)
    }
}

/// Best-effort current keyboard layout. `None` when it can't be determined;
/// callers then fall back to a layout-agnostic decision.
pub fn current_layout() -> Option<Language> {
    if let Ok(guard) = LAYOUT_CACHE.lock() {
        if let Some((t, l)) = *guard {
            if t.elapsed() < LAYOUT_TTL {
                return Some(l);
            }
        }
    }
    let fresh = query_layout();
    if let Some(l) = fresh {
        set_layout_cache(l);
    }
    fresh
}

fn set_layout_cache(lang: Language) {
    if let Ok(mut guard) = LAYOUT_CACHE.lock() {
        *guard = Some((Instant::now(), lang));
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn query_layout() -> Option<Language> {
    None
}

// ─────────────────────────────────────────────────────────────────────────────
// Linux: switch layout via hyprctl
// ─────────────────────────────────────────────────────────────────────────────
/// Talk to Hyprland over its control socket instead of spawning `hyprctl`.
///
/// `hyprctl` is a small C++ program whose entire job is to write the command to
/// this socket and print the reply. Shelling out to it cost a fork, an exec, a
/// dynamic link and a wait — measured at **6.3 ms per call** against **0.11 ms**
/// for the socket, a 57× difference — and a correction that changes layout
/// makes two such calls. That was the single largest cost in the retype path,
/// larger than every injection gap in `crate::timing` put together.
///
/// It also removes a dependency on `hyprctl` being installed and on `PATH` for
/// whatever session started the daemon.
#[cfg(target_os = "linux")]
mod hypr {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    use std::path::PathBuf;
    use std::sync::OnceLock;
    use std::time::Duration;

    /// A wedged compositor must not wedge the injection thread with it: this
    /// runs on the thread a correction is waiting on.
    const IO_TIMEOUT: Duration = Duration::from_millis(250);

    fn socket_path() -> Option<&'static PathBuf> {
        static PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
        PATH.get_or_init(|| {
            let sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok()?;
            // Hyprland moved this under XDG_RUNTIME_DIR in 0.40; older builds
            // kept it in /tmp. Both are checked so the daemon works either way.
            let runtime = std::env::var("XDG_RUNTIME_DIR").ok().map(PathBuf::from);
            [
                runtime.map(|r| r.join("hypr").join(&sig).join(".socket.sock")),
                Some(PathBuf::from("/tmp/hypr").join(&sig).join(".socket.sock")),
            ]
            .into_iter()
            .flatten()
            .find(|p| p.exists())
        })
        .as_ref()
    }

    /// Send one command and return the reply, or `None` if this is not a
    /// Hyprland session or the socket would not answer.
    pub fn request(command: &str) -> Option<String> {
        let mut stream = UnixStream::connect(socket_path()?).ok()?;
        let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
        let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
        stream.write_all(command.as_bytes()).ok()?;
        let mut reply = String::new();
        stream.read_to_string(&mut reply).ok()?;
        Some(reply)
    }

    /// Whether this session is one we can drive at all.
    pub fn available() -> bool {
        socket_path().is_some()
    }
}

/// The value of a flat `"key": "value"` string field inside one JSON object.
#[cfg(target_os = "linux")]
fn json_field<'a>(block: &'a str, key: &str) -> Option<&'a str> {
    let at = block.find(&format!("\"{key}\""))?;
    let rest = &block[at + key.len() + 2..];
    let open = rest.find('"')?;
    let rest = &rest[open + 1..];
    let close = rest.find('"')?;
    Some(&rest[..close])
}

/// The language a Hyprland keymap name describes.
#[cfg(target_os = "linux")]
fn language_of_keymap(keymap: &str) -> Option<Language> {
    let keymap = keymap.to_lowercase();
    if keymap.contains("hebrew") || keymap.contains("il") {
        Some(Language::Hebrew)
    } else if keymap.contains("english") || keymap.contains("us") {
        Some(Language::English)
    } else {
        None
    }
}

/// The active layout, read out of a `j/devices` reply.
///
/// Deliberately not "whatever keyboard is flagged `main`". On a session with an
/// input method running, `main` is the *input method's own virtual keyboard* —
/// on the machine this was written on, `hl-virtual-keyboard-fcitx5` — and our
/// own `recast-injector` is in that list too. Reading either means reporting
/// the layout of a device nobody types on, and a wrong reading here is worse
/// than none: `switch_layout_to` skips the switch when it believes the layout
/// is already right, and the correction then types itself out under the layout
/// that was actually live.
///
/// So: our injector never counts, `main` is preferred among the rest, and
/// anything else recognizable is the fallback.
#[cfg(target_os = "linux")]
fn parse_layout(devices_json: &str) -> Option<Language> {
    let keyboards = devices_json.find("\"keyboards\"").map(|at| &devices_json[at..])?;
    let mut fallback = None;
    for block in keyboards.split('{').skip(1) {
        // Stop at the end of the keyboards array; later sections (mice,
        // tablets) have no keymaps but should not be walked regardless.
        if block.starts_with(']') {
            break;
        }
        let name = json_field(block, "name").unwrap_or("");
        if name == "recast-injector" {
            continue;
        }
        let Some(lang) = json_field(block, "active_keymap").and_then(language_of_keymap) else {
            continue;
        };
        let is_main = block.contains("\"main\": true") || block.contains("\"main\":true");
        if is_main {
            return Some(lang);
        }
        fallback.get_or_insert(lang);
    }
    fallback
}

#[cfg(target_os = "linux")]
fn query_layout() -> Option<Language> {
    let json = match hypr::request("j/devices") {
        Some(reply) => reply,
        // Not a Hyprland session, or the socket refused. Fall back to the
        // subprocess so a setup that only has `hyprctl` still works.
        None => {
            let out = std::process::Command::new("hyprctl")
                .args(["devices", "-j"])
                .output()
                .ok()?;
            String::from_utf8(out.stdout).ok()?
        }
    };
    parse_layout(&json)
}

#[cfg(target_os = "linux")]
pub fn switch_layout_to(lang: Language) -> LayoutSwitch {
    // Already on the requested layout. Nothing to send — and, unlike before,
    // this is reported as the success it is.
    if current_layout() == Some(lang) {
        return LayoutSwitch::AlreadyThere;
    }

    let index = match lang {
        Language::English => "0",
        Language::Hebrew => "1",
    };
    let sent = if hypr::available() {
        // Hyprland answers "ok" and nothing else on success.
        hypr::request(&format!("/switchxkblayout all {index}"))
            .is_some_and(|reply| reply.trim() == "ok")
    } else {
        match std::process::Command::new("hyprctl")
            .args(["switchxkblayout", "all", index])
            .status()
        {
            Ok(status) => status.success(),
            Err(e) => {
                eprintln!("Failed to switch layout using hyprctl: {e}");
                false
            }
        }
    };
    if !sent {
        return LayoutSwitch::Failed;
    }

    // Then wait for it to actually be live. Accepting the command is not the
    // same as having applied it — measured at 0.3–0.9 ms apart on an idle
    // Hyprland — and `uinput` speaks keycodes, so a word injected inside that
    // gap is spelled out under the *old* layout and arrives as garbage. macOS
    // and Windows have polled for this all along; Linux fired and hoped, which
    // is why corrections involving a layout change failed intermittently.
    //
    // Cheap now that a query is a socket round trip rather than a subprocess:
    // the usual case confirms on the first or second poll.
    let gaps = crate::timing::injection();
    let deadline = Instant::now() + gaps.layout_confirm;
    loop {
        if query_layout() == Some(lang) {
            set_layout_cache(lang);
            return LayoutSwitch::Switched;
        }
        if Instant::now() >= deadline {
            return LayoutSwitch::Failed;
        }
        crate::timing::pause(gaps.layout_poll);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Windows: switch layout via HKL activation (LoadKeyboardLayoutW)
// ─────────────────────────────────────────────────────────────────────────────
// The Win32 spellings are kept exactly as the API documents them — `HKL`,
// `HWND`, `DWORD` — because these declarations have to be checked against
// Microsoft's headers by eye, and a renamed `Hkl` makes that harder for the
// one reader who ever needs to.
#[allow(clippy::upper_case_acronyms)]
#[cfg(target_os = "windows")]
fn query_layout() -> Option<Language> {
    use std::ffi::c_void;
    type DWORD = u32;
    type HKL = isize;
    type HWND = *mut c_void;
    extern "system" {
        fn GetForegroundWindow() -> HWND;
        fn GetWindowThreadProcessId(hWnd: HWND, lpdwProcessId: *mut DWORD) -> DWORD;
        fn GetCurrentThreadId() -> DWORD;
        fn GetKeyboardLayout(idThread: DWORD) -> HKL;
    }
    unsafe {
        let hwnd = GetForegroundWindow();
        let tid = if !hwnd.is_null() {
            let mut pid: DWORD = 0;
            GetWindowThreadProcessId(hwnd, &mut pid)
        } else {
            GetCurrentThreadId()
        };
        let langid = (GetKeyboardLayout(tid) as usize & 0xFFFF) as u16;
        // The low 10 bits are the *primary* language; the high 6 are the
        // sublanguage (regional variant). Match on the primary id so every
        // English variant (en-US 0x0409, en-GB 0x0809, …) and every Hebrew
        // variant counts, instead of only the two canonical US/IL layouts.
        match langid & 0x03ff {
            0x0d => Some(Language::Hebrew),
            0x09 => Some(Language::English),
            _ => None,
        }
    }
}

// The Win32 spellings are kept exactly as the API documents them — `HKL`,
// `HWND`, `DWORD` — because these declarations have to be checked against
// Microsoft's headers by eye, and a renamed `Hkl` makes that harder for the
// one reader who ever needs to.
#[allow(clippy::upper_case_acronyms)]
#[cfg(target_os = "windows")]
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
        fn GetForegroundWindow() -> HWND;
        fn GetWindowThreadProcessId(hWnd: HWND, lpdwProcessId: *mut DWORD) -> DWORD;
        fn GetCurrentThreadId() -> DWORD;
        fn GetKeyboardLayout(idThread: DWORD) -> HKL;
        fn PostMessageW(hWnd: HWND, Msg: u32, wParam: WPARAM, lParam: LPARAM) -> BOOL;
        fn GetKeyboardLayoutList(nBuff: i32, lpList: *mut HKL) -> i32;
        fn ActivateKeyboardLayout(hkl: HKL, Flags: u32) -> HKL;
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
        // Determine the active keyboard layout of the foreground window's thread.
        let hwnd = GetForegroundWindow();
        let tid = if !hwnd.is_null() {
            let mut pid: DWORD = 0;
            GetWindowThreadProcessId(hwnd, &mut pid)
        } else {
            GetCurrentThreadId()
        };

        // Pre-switch check: if the foreground window is already on the
        // requested layout, skip the switch entirely. Mirrors the Linux
        // hyprctl-based early-exit so check_and_switch_candidates returns
        // false (no replacement) when typing in the correct layout already.
        // Compare on the *primary* language (low 10 bits) so every regional
        // variant counts — an en-GB user is already "English" and must not be
        // flipped to en-US, and a he-IL variant must satisfy a Hebrew request.
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
                let wide: Vec<u16> =
                    klid.encode_utf16().chain(std::iter::once(0)).collect();
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

        // Prefer notifying the focused window (foreground thread) to switch.
        // This is more reliable than ActivateKeyboardLayout alone.
        let posted_ok = if !hwnd.is_null() {
            PostMessageW(
                hwnd,
                WM_INPUTLANGCHANGEREQUEST,
                INPUTLANGCHANGE_SYSCHARSET,
                target_hkl as LPARAM,
            ) != 0
        } else {
            false
        };

        if !posted_ok {
            // Fallback: activate for current thread (may not affect the
            // foreground app, but keeps behavior best-effort).
            let hkl = ActivateKeyboardLayout(target_hkl, KLF_ACTIVATE);
            if hkl == 0 {
                return LayoutSwitch::Failed;
            }
        }

        // Poll for the input subsystem to apply the change instead of a fixed
        // pessimistic sleep. Returns as soon as the layout flips.
        let gaps = crate::timing::injection();
        let deadline = std::time::Instant::now() + gaps.layout_confirm;
        loop {
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

// ─────────────────────────────────────────────────────────────────────────────
// macOS: switch layout via TIS (Carbon framework)
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(target_os = "macos")]
use core_foundation::base::TCFType;
#[cfg(target_os = "macos")]
use core_foundation::string::CFString;
#[cfg(target_os = "macos")]
use core_foundation_sys::base::CFTypeRef;
#[cfg(target_os = "macos")]
use core_foundation_sys::string::CFStringRef;

#[cfg(target_os = "macos")]
#[repr(C)]
struct __TISInputSource {
    _private: [u8; 0],
}
#[cfg(target_os = "macos")]
type TISInputSourceRef = *mut __TISInputSource;

#[cfg(target_os = "macos")]
#[link(name = "Carbon", kind = "framework")]
extern "C" {
    fn TISCopyInputSourceForLanguage(language: CFStringRef) -> TISInputSourceRef;
    fn TISSelectInputSource(source: TISInputSourceRef) -> i32;
    fn TISCopyCurrentKeyboardInputSource() -> TISInputSourceRef;
    // Read a property of an input source. The returned value follows the Get
    // rule (not owned — must NOT be released).
    fn TISGetInputSourceProperty(
        source: TISInputSourceRef,
        key: CFStringRef,
    ) -> *const std::ffi::c_void;
    // The list of language codes ("en", "he", "iw", …) an input source enters.
    static kTISPropertyInputSourceLanguages: CFStringRef;
    fn CFRelease(cf: CFTypeRef);
}

#[cfg(target_os = "macos")]
pub fn switch_layout_to(lang: Language) -> LayoutSwitch {
    use std::time::{Duration, Instant};

    // Already on the target layout. Uses the language-based detection so any
    // English/Hebrew *variant* counts.
    if current_layout() == Some(lang) {
        return LayoutSwitch::AlreadyThere;
    }

    let code = match lang {
        Language::English => "en",
        Language::Hebrew => "he",
    };
    unsafe {
        let cf_lang = CFString::new(code);
        let src = TISCopyInputSourceForLanguage(cf_lang.as_concrete_TypeRef());
        if src.is_null() {
            eprintln!("No input source found for language code '{}'", code);
            return LayoutSwitch::Failed;
        }

        let status = TISSelectInputSource(src);
        if status != 0 {
            eprintln!(
                "TISSelectInputSource failed for '{}' with status {}",
                code, status
            );
            CFRelease(src as CFTypeRef);
            return LayoutSwitch::Failed;
        }

        // TISSelectInputSource is asynchronous: the focused app does not see
        // the new layout the instant the call returns. If we retype before the
        // switch propagates, the injected keys are interpreted under the OLD
        // layout and the "corrected" word comes out as garbage. Poll the
        // current input source until it actually equals the target (or a
        // deadline elapses), so callers can retype immediately afterwards —
        // parity with the Linux/Windows pollers.
        let deadline = Instant::now() + Duration::from_millis(300);
        let mut landed;
        loop {
            let cur = TISCopyCurrentKeyboardInputSource();
            landed = !cur.is_null()
                && core_foundation_sys::base::CFEqual(src as CFTypeRef, cur as CFTypeRef) != 0;
            if !cur.is_null() {
                CFRelease(cur as CFTypeRef);
            }
            if landed || Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }

        CFRelease(src as CFTypeRef);
        if landed {
            set_layout_cache(lang);
        }
        // Only report success once the switch is confirmed. On timeout this is
        // `Failed`, so the caller skips the retype rather than typing the word
        // out under the old layout (garbage) — parity with the other two.
        if landed {
            LayoutSwitch::Switched
        } else {
            LayoutSwitch::Failed
        }
    }
}

#[cfg(target_os = "macos")]
fn query_layout() -> Option<Language> {
    use core_foundation_sys::array::{CFArrayGetCount, CFArrayGetValueAtIndex, CFArrayRef};

    unsafe {
        let cur = TISCopyCurrentKeyboardInputSource();
        if cur.is_null() {
            return None;
        }
        // Inspect the *current* source's own language list rather than testing
        // it for equality against the default "en"/"he" source. A user on any
        // English variant (ABC, British, Colemak, Dvorak…) has a current source
        // that is not equal to the canonical "en" source, so the old equality
        // test returned None for them and silently disabled layout anchoring.
        // The languages array lists the primary language first.
        let langs =
            TISGetInputSourceProperty(cur, kTISPropertyInputSourceLanguages) as CFArrayRef;
        let mut result = None;
        if !langs.is_null() {
            let count = CFArrayGetCount(langs);
            for i in 0..count {
                let value = CFArrayGetValueAtIndex(langs, i) as CFStringRef;
                if value.is_null() {
                    continue;
                }
                let code = CFString::wrap_under_get_rule(value).to_string();
                // Hebrew is "he" (modern) or "iw" (legacy ISO code).
                if code.starts_with("he") || code.starts_with("iw") {
                    result = Some(Language::Hebrew);
                    break;
                }
                if code.starts_with("en") {
                    result = Some(Language::English);
                    break;
                }
            }
        }
        CFRelease(cur as CFTypeRef);
        result
    }
}

#[cfg(all(test, target_os = "linux"))]
mod linux_tests {
    use super::*;

    /// Trimmed from a real `j/devices` reply on a Hyprland session running
    /// fcitx5, which is where this bug was found: the keyboard flagged `main`
    /// is the input method's virtual one, and ReCast's own injector is in the
    /// list beside it.
    const DEVICES: &str = r#"{"mice": [], "keyboards": [
        {"address": "0x1", "name": "at-translated-set-2-keyboard", "rules": "",
         "model": "", "layout": "us,il", "variant": "", "options": "",
         "active_keymap": "Hebrew", "capsLock": false, "numLock": false, "main": false},
        {"address": "0x2", "name": "recast-injector", "rules": "",
         "model": "", "layout": "us", "variant": "", "options": "",
         "active_keymap": "English (US)", "capsLock": false, "numLock": false, "main": false},
        {"address": "0x3", "name": "hl-virtual-keyboard-fcitx5", "rules": "",
         "model": "", "layout": "us,il", "variant": "", "options": "",
         "active_keymap": "Hebrew", "capsLock": false, "numLock": false, "main": true}
    ], "tablets": []}"#;

    #[test]
    fn the_layout_comes_from_a_keyboard_someone_types_on() {
        assert_eq!(parse_layout(DEVICES), Some(Language::Hebrew));
    }

    #[test]
    fn our_own_injector_never_decides_the_answer() {
        // The injector says English while every real keyboard says Hebrew. If
        // it were allowed to win, `switch_layout_to` would believe a switch to
        // English was unnecessary and the correction would be typed out in
        // Hebrew — the intermittent garbage this was reported as.
        let only_injector_is_main = DEVICES
            .replace(r#""name": "recast-injector"#, r#""name": "recast-injector-x"#)
            .replace(r#""name": "hl-virtual-keyboard-fcitx5", "rules": "",
         "model": "", "layout": "us,il", "variant": "", "options": "",
         "active_keymap": "Hebrew", "capsLock": false, "numLock": false, "main": true"#,
                     r#""name": "recast-injector", "rules": "",
         "model": "", "layout": "us", "variant": "", "options": "",
         "active_keymap": "English (US)", "capsLock": false, "numLock": false, "main": true"#);
        // With our injector as `main` and claiming English, the answer must
        // still come from the physical keyboard.
        assert_eq!(parse_layout(&only_injector_is_main), Some(Language::Hebrew));
    }

    #[test]
    fn a_reply_with_nothing_recognizable_is_not_guessed_at() {
        assert_eq!(parse_layout("{}"), None);
        assert_eq!(parse_layout(r#"{"keyboards": []}"#), None);
    }

    /// Against the compositor that is actually running, when there is one.
    /// Skips elsewhere — CI has no Hyprland — but on a developer's machine it
    /// is the only thing that checks the socket path, the protocol and the
    /// parser against reality rather than against a fixture I wrote.
    #[test]
    fn the_live_session_answers_if_there_is_one() {
        if !hypr::available() {
            eprintln!("no Hyprland socket — skipping the live check");
            return;
        }
        let reply = hypr::request("j/devices").expect("the socket answers");
        assert!(
            reply.contains("\"keyboards\""),
            "unexpected reply shape: {}",
            &reply[..reply.len().min(120)]
        );
        assert!(
            parse_layout(&reply).is_some(),
            "a live session must yield a layout"
        );
        // And the cached front door agrees with the raw read.
        assert_eq!(query_layout(), parse_layout(&reply));
    }

    #[test]
    fn already_being_there_is_success_not_failure() {
        // The distinction the undo path turned on: only `Failed` may stop a
        // retype. This is what a bare bool could not express.
        assert!(LayoutSwitch::AlreadyThere.ready());
        assert!(LayoutSwitch::Switched.ready());
        assert!(!LayoutSwitch::Failed.ready());
        // ...and only an actual switch counts as a change, for the debug log.
        assert!(!LayoutSwitch::AlreadyThere.changed());
        assert!(LayoutSwitch::Switched.changed());
    }
}
