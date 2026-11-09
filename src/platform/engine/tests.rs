use super::correction_pipeline::{note_of, replacement, undo_of};
use super::word_buffer::MAX_UNDO_SPACES;
use super::*;
use crate::dictionary::{Fix, Run};
use crate::types::Language;
use std::collections::HashSet;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    mpsc,
};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

static SELECTION: Mutex<Option<Selection>> = Mutex::new(None);
static FOCUS: AtomicUsize = AtomicUsize::new(1);
static SIMULATED_LAYOUT: AtomicUsize = AtomicUsize::new(0);
static INPUT_ALLOWED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
static ALLOW_LAYOUT_SWITCH: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
static REFUSE_LAYOUT_SWITCH: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
static CAPTURE_PENDING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static CAPTURE_OBSERVED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static PREPARE_ALLOWED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
static EDIT_STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
static REJECT_UNICODE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static EMPTY_INPUT_SUPPORTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(true);
struct FocusGate {
    ready: mpsc::SyncSender<()>,
    proceed: mpsc::Receiver<()>,
}
static FOCUS_GATE: Mutex<Option<FocusGate>> = Mutex::new(None);
static EMPTY_INPUT_GATE: Mutex<Option<FocusGate>> = Mutex::new(None);
static LAYOUT_GATE: Mutex<Option<FocusGate>> = Mutex::new(None);
static INJECT_RETURN_GATE: Mutex<Option<FocusGate>> = Mutex::new(None);
static CHECK_GATE: Mutex<Option<FocusGate>> = Mutex::new(None);
pub(super) fn pause_word_check() {
    let gate = CHECK_GATE.lock().unwrap().take();
    if let Some(gate) = gate {
        gate.ready.send(()).unwrap();
        // A concurrent manual search has a ten-second debug deadline under CPU contention.
        let seconds = if cfg!(debug_assertions) { 15 } else { 3 };
        gate.proceed
            .recv_timeout(Duration::from_secs(seconds))
            .unwrap();
    }
}
struct SimulatedPlatform<const QUEUE_UNDO: bool, const VERIFY: bool = true>;
type Simulated = SimulatedPlatform<true>;
struct Screen {
    text: Mutex<String>,
    erasures: Mutex<Vec<usize>>,
    injecting: std::sync::atomic::AtomicBool,
    ready: mpsc::SyncSender<()>,
    proceed: Mutex<mpsc::Receiver<()>>,
}
impl<const QUEUE_UNDO: bool, const VERIFY: bool> Platform
    for SimulatedPlatform<QUEUE_UNDO, VERIFY>
{
    const VERIFIED_REPLACEMENT: bool = VERIFY;
    fn preview_alternatives() -> bool {
        std::env::var_os("RECAST_PREVIEW_TEST").is_some()
    }
    fn show_preview(engine: Arc<Engine<Self>>, generation: u64, _: Vec<String>) {
        engine.preview_ready(generation);
    }
    type Key = char;
    type Retype = String;
    type Injector = Screen;
    type Focus = usize;
    const REQUIRES_FOCUS: bool = true;
    const QUEUE_UNDO_DURING_REPLACEMENT: bool = QUEUE_UNDO;
    // Exercise the stricter keycode-injection refusal policy as well.
    const ABORT_UNDO_IF_LAYOUT_REFUSED: bool = true;
    const SHIFT_LEFT: char = '\x01';
    const SHIFT_RIGHT: char = '\x02';
    const CTRL_LEFT: char = '\x03';
    const CTRL_RIGHT: char = '\x04';
    const CAPS_LOCK: char = '\x05';
    const BACKSPACE: char = '\x08';
    fn is_terminator(key: char) -> bool {
        key == ' ' || key == '\n'
    }
    fn is_line_break(key: char) -> bool {
        key == '\n'
    }
    fn is_reset(key: char) -> bool {
        matches!(key, '\x1b' | '\x10'..='\x15')
    }
    fn is_mouse_button(key: char) -> bool {
        key == '\x15'
    }
    fn is_modifier(key: char) -> bool {
        ('\x01'..='\x05').contains(&key)
    }
    fn english_char(key: char, _: bool) -> Option<char> {
        Self::english_char_plain(key)
    }
    fn english_char_plain(key: char) -> Option<char> {
        (!key.is_control() && !Self::is_terminator(key)).then_some(key)
    }
    fn hebrew_char(key: char) -> Option<char> {
        #[cfg(target_os = "linux")]
        return crate::keymap::english_char_to_evkey_shifted(key)
            .and_then(|(k, _)| crate::keymap::evkey_to_hebrew_char(k));
        #[cfg(not(target_os = "linux"))]
        return crate::keymap::english_char_to_key(key)
            .and_then(|(k, _)| crate::keymap::key_to_hebrew_char(k));
    }
    fn retype_original(keys: &[Typed<char>], lang: Language) -> String {
        reading::<Self>(keys, lang)
    }
    fn retype_layout(_: &[Typed<char>], text: &str, _: Language) -> Option<String> {
        Some(text.to_string())
    }
    fn retype_text(text: &str) -> Option<String> {
        Some(text.to_string())
    }
    fn retype_completion(text: &str, _: Language) -> Option<String> {
        (!REJECT_UNICODE.load(Ordering::SeqCst) || text.is_ascii()).then(|| text.to_owned())
    }
    fn retype_len(text: &String) -> usize {
        text.chars().count()
    }
    fn append_retype(text: &mut String, tail: String) {
        text.push_str(&tail);
    }
    fn buffer_after(text: &String) -> Vec<Typed<char>> {
        let text = text.rsplit(char::is_whitespace).next().unwrap_or_default();
        let physical = if text.chars().any(|c| ('א'..='ת').contains(&c)) {
            crate::keymap::convert_selection(text)
        } else {
            text.to_owned()
        };
        physical
            .chars()
            .map(|key| Typed { key, shift: false })
            .collect()
    }
    fn injecting_flag(screen: &Screen) -> Option<&std::sync::atomic::AtomicBool> {
        Some(&screen.injecting)
    }
    fn focus() -> Option<usize> {
        let gate = FOCUS_GATE.lock().unwrap().take();
        if let Some(gate) = gate {
            gate.ready.send(()).unwrap();
            gate.proceed.recv_timeout(Duration::from_secs(3)).unwrap();
        }
        let focus = FOCUS.load(Ordering::SeqCst);
        (focus != 0).then_some(focus)
    }
    fn app_id(focus: &usize) -> Option<String> {
        Some(if *focus == 2 { "secret.exe" } else { "Editor" }.to_string())
    }
    fn is_own_focus(focus: &usize) -> bool {
        *focus == 3
    }
    fn input_allowed() -> bool {
        INPUT_ALLOWED.load(Ordering::SeqCst)
    }
    fn selection(_: &usize) -> Option<Selection> {
        SELECTION.lock().unwrap().clone()
    }
    fn replace_selection(
        engine: &Engine<Self>,
        _: &usize,
        expected: &Selection,
        text: &str,
        generation: u64,
    ) -> Option<Selection> {
        engine.injector.ready.send(()).unwrap();
        engine
            .injector
            .proceed
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(3))
            .unwrap();
        if !engine.replacement_valid(generation) {
            return None;
        }
        let mut selection = SELECTION.lock().unwrap();
        if selection.as_ref() != Some(expected) {
            return None;
        }
        let mut screen = engine.injector.text.lock().unwrap();
        let mut utf16: Vec<_> = screen.encode_utf16().collect();
        utf16.splice(
            expected.start as usize..(expected.start + expected.length) as usize,
            text.encode_utf16(),
        );
        *screen = String::from_utf16(&utf16).unwrap();
        let after = Selection {
            start: expected.start,
            length: text.encode_utf16().count() as isize,
            text: text.to_owned(),
        };
        *selection = Some(after.clone());
        Some(after)
    }
    fn input_empty(screen: &Screen) -> bool {
        let gate = EMPTY_INPUT_GATE.lock().unwrap().take();
        if let Some(gate) = gate {
            gate.ready.send(()).unwrap();
            gate.proceed.recv_timeout(Duration::from_secs(3)).unwrap();
        }
        EMPTY_INPUT_SUPPORTED.load(Ordering::SeqCst) && screen.text.lock().unwrap().is_empty()
    }
    fn current_layout() -> Option<Language> {
        match SIMULATED_LAYOUT.load(Ordering::SeqCst) {
            0 => Some(Language::English),
            1 => Some(Language::Hebrew),
            _ => None,
        }
    }
    fn switch_layout_to(lang: Language) -> crate::layout::LayoutSwitch {
        assert!(
            ALLOW_LAYOUT_SWITCH.load(Ordering::SeqCst),
            "English spelling and undo must not switch layouts"
        );
        if REFUSE_LAYOUT_SWITCH.load(Ordering::SeqCst) {
            return crate::layout::LayoutSwitch::Failed;
        }
        if let Some(gate) = LAYOUT_GATE.lock().unwrap().take() {
            gate.ready.send(()).unwrap();
            gate.proceed.recv_timeout(Duration::from_secs(3)).unwrap();
        }
        SIMULATED_LAYOUT.store(usize::from(lang == Language::Hebrew), Ordering::SeqCst);
        crate::layout::LayoutSwitch::Switched
    }
    fn prepare_injection(_: &Engine<Self>, _: &Plan<Self>, _: u64, _: Option<Language>) -> bool {
        PREPARE_ALLOWED.load(Ordering::SeqCst)
    }
    fn capture_pending(_: &Screen) -> bool {
        let pending = CAPTURE_PENDING.load(Ordering::SeqCst);
        if pending {
            CAPTURE_OBSERVED.store(true, Ordering::SeqCst);
        }
        pending
    }
    fn injection_started(_: &Screen) -> bool {
        EDIT_STARTED.load(Ordering::SeqCst)
    }
    fn inject(
        engine: &Engine<Self>,
        plan: Plan<Self>,
        generation: u64,
    ) -> Option<Vec<Typed<char>>> {
        if !EDIT_STARTED.load(Ordering::SeqCst) {
            return None;
        }
        engine.injector.erasures.lock().unwrap().push(plan.erase);
        engine.injector.ready.send(()).unwrap();
        engine
            .injector
            .proceed
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(3))
            .unwrap();
        if !engine.replacement_valid(generation) {
            return None;
        }
        let buffered = engine.buffered();
        let suffix = if buffered.is_empty() {
            String::new()
        } else {
            reading::<Self>(&buffered, plan.verify.or_else(Self::current_layout)?)
        };
        let mut screen = engine.injector.text.lock().unwrap();
        if plan.verify.is_some()
            && (!VERIFY || !screen.ends_with(&format!("{}{suffix}", plan.expected)))
        {
            return None;
        }
        assert_eq!(
            plan.expected.chars().count(),
            plan.erase,
            "tracked native replacement suffix"
        );
        if buffered.is_empty() {
            assert!(
                screen.ends_with(&plan.expected),
                "screen {screen:?} does not end with tracked suffix {:?}",
                plan.expected
            );
        }
        for _ in 0..plan.erase + buffered.len() {
            assert!(
                screen.pop().is_some(),
                "replacement erased beyond the typed text"
            );
        }
        screen.push_str(&plan.retype);
        if let Some(key) = plan.terminator {
            screen.push(key);
        }
        screen.push_str(&suffix);
        drop(screen);
        let gate = INJECT_RETURN_GATE.lock().unwrap().take();
        if let Some(gate) = gate {
            gate.ready.send(()).unwrap();
            gate.proceed.recv_timeout(Duration::from_secs(3)).unwrap();
        }
        Some(buffered)
    }
}

struct SessionWithPolicy<const QUEUE_UNDO: bool, const VERIFY: bool = true> {
    engine: Arc<Engine<SimulatedPlatform<QUEUE_UNDO, VERIFY>>>,
    ready: mpsc::Receiver<()>,
    proceed: mpsc::SyncSender<()>,
}

#[test]
fn queued_native_letters_and_spaces_use_source_timestamps_for_deduplication() {
    let mut state = AppState::<Simulated>::new();
    let window = Some(Duration::from_millis(5));
    let start = SystemTime::UNIX_EPOCH + Duration::from_secs(1);
    let mut captured = String::new();
    // Dispatch immediately, as when a delayed reader drains one batch.
    // The physical presses were spaced apart by an ordinary typing interval.
    for (index, key) in "address   ".chars().enumerate() {
        let timestamp = start + Duration::from_millis(index as u64 * 20);
        if !state.duplicate_press(key, false, timestamp, window) {
            captured.push(key);
        }
    }
    assert_eq!(captured, "address   ");
}

#[test]
fn mirrored_native_presses_deduplicate_across_releases_and_reader_order() {
    let mut state = AppState::<Simulated>::new();
    let window = Some(Duration::from_millis(5));
    let timestamp = SystemTime::UNIX_EPOCH + Duration::from_secs(1);
    assert!(!state.duplicate_press('s', false, timestamp, window));
    state.held_keys.insert('s');
    state.held_keys.remove(&'s');
    // Another device reader may deliver its duplicate after the release,
    // with an identical, slightly later, or slightly earlier timestamp.
    for mirrored in [
        timestamp,
        timestamp + Duration::from_millis(1),
        timestamp - Duration::from_millis(1),
    ] {
        assert!(state.duplicate_press('s', false, mirrored, window));
        assert!(
            state.held_keys.contains(&'s'),
            "a delayed press still owns its physical hold"
        );
        state.held_keys.remove(&'s');
    }
    let next_press = timestamp + Duration::from_millis(5);
    assert!(!state.duplicate_press('s', false, next_press, window));
}

#[test]
fn queued_native_auto_repeat_bypasses_deduplication() {
    let mut state = AppState::<Simulated>::new();
    let window = Some(Duration::from_millis(5));
    let timestamp = SystemTime::UNIX_EPOCH + Duration::from_secs(1);
    assert!(!state.duplicate_press('s', false, timestamp, window));
    for _ in 0..3 {
        assert!(!state.duplicate_press('s', true, timestamp, window));
    }
    let next_press = timestamp + Duration::from_millis(20);
    assert!(!state.duplicate_press('s', false, next_press, window));
    // Single-stream platforms do not apply a deduplication window.
    assert!(!state.duplicate_press('s', false, timestamp, None));
}

#[cfg(target_os = "linux")]
#[test]
fn overlapping_devices_keep_physical_keys_held_until_the_last_release() {
    let timestamp = SystemTime::UNIX_EPOCH + Duration::from_secs(1);
    for key in [Simulated::SHIFT_LEFT, Simulated::CTRL_LEFT, 'x', ' '] {
        let s = Session::new();
        s.engine.input_device_added(1, &HashSet::new());
        s.engine.input_device_added(2, &HashSet::new());
        s.engine.key_press_at(1, key, timestamp);
        s.engine
            .key_press_at(2, key, timestamp + Duration::from_millis(10));
        s.engine.key_release_from(1, key);
        assert!(s.engine.lock().held_keys.contains(&key));
        let generation = s.engine.lock().generation;
        assert!(s
            .engine
            .replay_snapshot(&[key], generation, Duration::ZERO)
            .is_none());
        assert!(s.engine.lock().last_action_tap.is_none());
        s.engine.key_release_from(2, key);
        assert!(!s.engine.lock().held_keys.contains(&key));
    }
    // Disconnecting one keyboard cannot release a key still held on another.
    let s = Session::new();
    s.engine
        .input_device_added(1, &HashSet::from([Simulated::SHIFT_LEFT]));
    s.engine
        .input_device_added(2, &HashSet::from([Simulated::SHIFT_LEFT, 'x']));
    s.engine
        .input_device_removed(1, &HashSet::from([Simulated::SHIFT_LEFT]));
    assert!(s.engine.lock().held_keys.contains(&Simulated::SHIFT_LEFT));
    assert!(s.engine.lock().held_keys.contains(&'x'));
    s.engine.key_press_at(2, 'a', timestamp);
    assert_eq!(
        reading::<Simulated>(&s.engine.lock().keys, Language::English),
        "A"
    );
    s.engine.key_release_from(2, 'a');
    s.engine
        .input_device_removed(2, &HashSet::from([Simulated::SHIFT_LEFT, 'x']));
    assert!(s.engine.lock().held_keys.is_empty());
}

#[cfg(target_os = "linux")]
#[test]
fn reader_unwinding_releases_its_ownership_before_reconnect() {
    let s = Session::new();
    s.engine
        .input_device_added(1, &HashSet::from([Simulated::SHIFT_LEFT]));
    s.engine.input_device_added(2, &HashSet::from(['y']));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = crate::platform::linux::InputDeviceGuard::new(&s.engine, 1);
        s.engine.key_press_at(1, 'x', SystemTime::now());
        panic!("simulate reader failure");
    }));
    assert!(result.is_err());
    let st = s.engine.lock();
    assert_eq!(st.held_keys, HashSet::from(['y']));
    assert!(!st.device_keys.contains_key(&1));
    assert!(st.device_keys.contains_key(&2));
}

#[cfg(target_os = "linux")]
#[test]
fn device_attach_seeds_held_keys_without_typing_or_modifier_taps() {
    let s = Session::new();
    let held = HashSet::from([
        Simulated::SHIFT_LEFT,
        Simulated::CTRL_LEFT,
        Simulated::CAPS_LOCK,
        'y',
    ]);
    {
        let mut st = s.engine.lock();
        st.held_keys.insert('x');
        st.action_down = Some(Instant::now());
        st.completion_down = Some(Instant::now());
    }
    s.engine.input_device_added(1, &held);
    {
        let st = s.engine.lock();
        assert!(held.iter().all(|key| st.held_keys.contains(key)));
        assert!(
            st.held_keys.contains(&'x'),
            "other device holds must survive"
        );
        assert!(st.action_down.is_none());
        assert!(st.completion_down.is_none());
        assert!(st.keys.is_empty());
        assert!(!st.caps_lock);
    }
    // An already-held Ctrl still makes the next letter a shortcut.
    s.engine.key_press('v');
    assert!(s.engine.lock().keys.is_empty());
    s.engine.key_release('v');
    s.engine.key_release(Simulated::CTRL_LEFT);
    assert!(s.engine.lock().last_action_tap.is_none());
    // An already-held Shift still capitalizes captured letters.
    s.engine.key_press('a');
    assert_eq!(
        reading::<Simulated>(&s.engine.lock().keys, Language::English),
        "A"
    );
    s.engine.key_release('a');
    s.engine.input_device_removed(1, &held);
    let st = s.engine.lock();
    assert!(held.iter().all(|key| !st.held_keys.contains(key)));
    assert!(st.held_keys.contains(&'x'));
}

#[test]
fn changing_focus_invalidates_the_previous_targets_layout_cache() {
    const CHILD: &str = "RECAST_FOCUS_LAYOUT_CACHE_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "platform::engine::tests::changing_focus_invalidates_the_previous_targets_layout_cache",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    use crate::layout::swap_cached_layout_for_test as swap_cache;

    let s = Session::new();
    s.engine.lock().focus = Some(1);
    FOCUS.store(1, Ordering::SeqCst);
    swap_cache(Some(Language::English));
    let (st, focus) = s.engine.refresh_focus(s.engine.lock()).unwrap();
    assert_eq!(focus, Some(1));
    drop(st);
    assert_eq!(swap_cache(None), Some(Language::English));

    // A different foreground thread can have a different keyboard layout,
    // even without a keyboard layout hotkey or an expired cache TTL.
    for target in [2, 0] {
        s.engine.lock().focus = Some(1);
        FOCUS.store(target, Ordering::SeqCst);
        swap_cache(Some(Language::English));
        let (st, focus) = s.engine.refresh_focus(s.engine.lock()).unwrap();
        assert_eq!(focus, (target != 0).then_some(target));
        drop(st);
        assert_eq!(swap_cache(None), None);
    }
}

#[test]
fn mouse_chords_do_not_count_as_modifier_taps() {
    const CHILD: &str = "RECAST_MOUSE_MODIFIER_TAPS_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "platform::engine::tests::mouse_chords_do_not_count_as_modifier_taps",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    crate::config::Config::update_live(|cfg| {
        cfg.action_shortcut = "left_ctrl".into();
        cfg.undo_shortcut = "none".into();
        cfg.completion_shortcut = "right_shift".into();
    });
    for native_click in [false, true] {
        for key in [Simulated::CTRL_LEFT, Simulated::SHIFT_RIGHT] {
            let s = Session::new();
            s.engine.key_press(key);
            if native_click {
                s.engine.mouse_click();
            } else {
                s.tap('\x15'); // Mouse buttons on an evdev key stream.
            }
            assert!(s.engine.lock().held_keys.contains(&key));
            assert!(s.engine.lock().action_down.is_none());
            assert!(s.engine.lock().completion_down.is_none());
            s.engine.key_release(key);
            assert!(s.engine.lock().last_action_tap.is_none());
            assert!(!s.engine.lock().is_replacing);
            // A later genuine tap must remain the first tap of a pair.
            s.tap(Simulated::CTRL_LEFT);
            assert!(s.engine.lock().last_action_tap.is_some());
            assert!(!s.engine.lock().is_replacing);
        }
    }
}

#[test]
fn native_capture_loss_cancels_unfinished_modifier_taps() {
    const CHILD: &str = "RECAST_CAPTURE_MODIFIER_TAPS_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "platform::engine::tests::native_capture_loss_cancels_unfinished_modifier_taps",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    crate::config::Config::update_live(|cfg| {
        cfg.action_shortcut = "left_ctrl".into();
        cfg.undo_shortcut = "none".into();
        cfg.completion_shortcut = "right_shift".into();
    });
    for key in [Simulated::CTRL_LEFT, Simulated::SHIFT_RIGHT] {
        let s = Session::new();
        s.engine.key_press(key);
        {
            let st = s.engine.lock();
            assert!(st.action_down.is_some() || st.completion_down.is_some());
        }
        // Secure Input, tap loss, and ambiguous synthetic flags all discard context.
        s.engine.forget_everything();
        {
            let st = s.engine.lock();
            assert!(st.held_keys.contains(&key));
            assert!(st.action_down.is_none());
            assert!(st.completion_down.is_none());
        }
        s.engine.key_release(key);
        assert!(s.engine.lock().last_action_tap.is_none());
        assert!(!s.engine.lock().is_replacing);
        if key == Simulated::CTRL_LEFT {
            s.tap(key);
            assert!(s.engine.lock().last_action_tap.is_some());
            assert!(!s.engine.lock().is_replacing);
        }
    }
}

#[test]
fn native_modifier_snapshots_recover_missed_transitions_without_rearming_taps() {
    let snapshot = |shift, ctrl| {
        [
            (Simulated::SHIFT_LEFT, shift),
            (Simulated::SHIFT_RIGHT, false),
            (Simulated::CTRL_LEFT, ctrl),
            (Simulated::CTRL_RIGHT, false),
            // Caps Lock is a separate latch; ordinary held keys are not modifiers.
            (Simulated::CAPS_LOCK, false),
            ('x', false),
        ]
    };
    for missed_release in [
        Simulated::CTRL_LEFT,
        Simulated::CTRL_RIGHT,
        Simulated::SHIFT_LEFT,
        Simulated::SHIFT_RIGHT,
    ] {
        let s = Session::new();
        let generation = {
            let mut st = s.engine.lock();
            st.held_keys
                .extend([missed_release, 'x', Simulated::CAPS_LOCK]);
            st.caps_lock = true;
            st.keys.push(Typed {
                key: 'r',
                shift: false,
            });
            st.action_down = Some(Instant::now());
            st.completion_down = Some(Instant::now());
            st.last_action_tap = Some(Instant::now());
            st.layout_hotkey = true;
            st.generation
        };
        s.engine.forget_everything();
        assert!(s.engine.lock().held_keys.contains(&missed_release));
        s.engine.sync_modifiers(snapshot(false, false));
        let st = s.engine.lock();
        assert!(!st.held_keys.contains(&missed_release));
        assert!(st.held_keys.contains(&'x'));
        assert!(st.held_keys.contains(&Simulated::CAPS_LOCK));
        assert!(st.caps_lock);
        assert!(st.keys.is_empty());
        assert!(st.no_fix);
        assert!(st.generation > generation + 1);
        assert!(st.action_down.is_none());
        assert!(st.completion_down.is_none());
        assert!(st.last_action_tap.is_none());
        assert!(!st.layout_hotkey);
    }

    // Capture may resume while a modifier is already held. Do not invent a tap.
    let s = Session::new();
    s.engine.sync_modifiers(snapshot(true, true));
    {
        let st = s.engine.lock();
        assert!(st.held_keys.contains(&Simulated::SHIFT_LEFT));
        assert!(st.held_keys.contains(&Simulated::CTRL_LEFT));
        assert!(st.action_down.is_none());
        assert!(st.completion_down.is_none());
    }

    // A matching snapshot must preserve captured text and genuine gesture timing.
    let (generation, revision, action_down, completion_down) = {
        let mut st = s.engine.lock();
        st.keys.push(Typed {
            key: 'r',
            shift: true,
        });
        st.no_fix = false;
        st.action_down = Some(Instant::now());
        st.completion_down = Some(Instant::now());
        (
            st.generation,
            st.revision,
            st.action_down,
            st.completion_down,
        )
    };
    s.engine.sync_modifiers(snapshot(true, true));
    let st = s.engine.lock();
    assert_eq!(st.generation, generation);
    assert_eq!(st.revision, revision);
    assert_eq!(st.keys.len(), 1);
    assert!(!st.no_fix);
    assert_eq!(st.action_down, action_down);
    assert_eq!(st.completion_down, completion_down);
}

#[cfg(target_os = "linux")]
#[test]
fn linux_replay_rejects_every_physical_modifier_even_outside_its_key_list() {
    use crate::platform::linux::Linux;
    use evdev::KeyCode;
    let mut state = AppState::<Linux>::new();
    for modifier in [
        KeyCode::KEY_LEFTSHIFT,
        KeyCode::KEY_RIGHTSHIFT,
        KeyCode::KEY_LEFTCTRL,
        KeyCode::KEY_RIGHTCTRL,
        KeyCode::KEY_LEFTALT,
        KeyCode::KEY_RIGHTALT,
        KeyCode::KEY_LEFTMETA,
        KeyCode::KEY_RIGHTMETA,
    ] {
        state.held_keys.insert(modifier);
        assert!(state.replay_keys_held(&[KeyCode::KEY_E, KeyCode::KEY_SPACE]));
        state.held_keys.remove(&modifier);
    }
    // Caps Lock is a latch whose state is handled by the key encoder.
    state.held_keys.insert(KeyCode::KEY_CAPSLOCK);
    assert!(!state.replay_keys_held(&[KeyCode::KEY_E, KeyCode::KEY_SPACE]));
}

#[cfg(target_os = "linux")]
#[test]
fn replay_snapshot_rejects_held_suffixes_modifiers_and_cancelled_work() {
    let session = Session::new();
    let engine = &session.engine;
    let replay = ['e', ' ', Simulated::SHIFT_LEFT, Simulated::SHIFT_RIGHT];
    let generation = {
        let mut st = engine.lock();
        st.is_replacing = true;
        st.buffered_keys.push(Typed {
            key: 'x',
            shift: false,
        });
        st.held_keys.insert('x');
        st.generation
    };
    // A held suffix key is absent from the corrected word, but still has
    // to be released before the injector can reproduce it.
    assert!(engine
        .replay_snapshot(&replay, generation, Duration::ZERO)
        .is_none());
    engine.lock().held_keys.remove(&'x');
    for key in replay {
        engine.lock().held_keys.insert(key);
        assert!(engine
            .replay_snapshot(&replay, generation, Duration::ZERO)
            .is_none());
        engine.lock().held_keys.remove(&key);
    }
    for modifier in [Simulated::CTRL_LEFT, Simulated::CTRL_RIGHT] {
        engine.lock().held_keys.insert(modifier);
        assert!(engine
            .replay_snapshot(&replay, generation, Duration::ZERO)
            .is_none());
        engine.lock().held_keys.remove(&modifier);
    }
    let buffered = engine
        .replay_snapshot(&replay, generation, Duration::ZERO)
        .unwrap();
    assert_eq!(
        buffered.iter().map(|typed| typed.key).collect::<String>(),
        "x"
    );
    engine.lock().focus = Some(1);
    assert!(engine.replay_valid(&replay, generation, buffered.len()));
    engine.lock().held_keys.insert(Simulated::CTRL_LEFT);
    assert!(!engine.replay_valid(&replay, generation, buffered.len()));
    engine.lock().held_keys.clear();
    engine.lock().buffered_keys.push(Typed {
        key: 'y',
        shift: false,
    });
    assert!(!engine.replay_valid(&replay, generation, buffered.len()));
    engine.forget_everything();
    assert!(!engine.replay_valid(&replay, generation, buffered.len()));
    assert!(engine
        .replay_snapshot(&replay, generation, Duration::ZERO)
        .is_none());
}

#[cfg(target_os = "linux")]
#[test]
fn replay_snapshot_waits_for_keys_added_during_release() {
    let session = Session::new();
    let engine = Arc::clone(&session.engine);
    let generation = {
        let mut st = engine.lock();
        st.is_replacing = true;
        st.buffered_keys.push(Typed {
            key: 'x',
            shift: false,
        });
        st.held_keys.insert('x');
        st.generation
    };
    let (done, snapshot) = mpsc::sync_channel(1);
    let worker = thread::spawn(move || {
        done.send(engine.replay_snapshot(&['e', ' '], generation, Duration::from_secs(2)))
            .unwrap();
    });
    assert!(matches!(
        snapshot.recv_timeout(Duration::from_millis(20)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    {
        let mut st = session.engine.lock();
        st.buffered_keys.push(Typed {
            key: 'y',
            shift: false,
        });
        st.held_keys.insert('y');
        st.held_keys.remove(&'x');
    }
    assert!(matches!(
        snapshot.recv_timeout(Duration::from_millis(20)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    session.engine.lock().held_keys.remove(&'y');
    let buffered = snapshot
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    assert_eq!(
        buffered.iter().map(|typed| typed.key).collect::<String>(),
        "xy"
    );
    worker.join().unwrap();
}

type Session = SessionWithPolicy<true>;

#[test]
#[ignore = "microbenchmark; run alone with `make bench`"]
fn benchmark_engine_typing() {
    use std::hint::black_box;

    let saved = crate::config::Config::global();
    crate::config::Config::update_live(|config| config.personal_enabled = false);
    let session = Session::new();
    let engine = &session.engine;
    let mut letters = Vec::with_capacity(20_000);
    let mut boundaries = Vec::with_capacity(20_000);

    // Warm lazy lookup tables before measuring steady-state capture and planning.
    for sample in 0..20_100 {
        let started = Instant::now();
        for key in "keyboard".chars() {
            engine.key_press(black_box(key));
            engine.key_release(key);
        }
        let letter_time = started.elapsed();
        let started = Instant::now();
        engine.key_press(black_box(' '));
        engine.key_release(' ');
        let boundary_time = started.elapsed();
        session.checks_ready();
        if sample >= 100 {
            letters.push(letter_time / 8);
            boundaries.push(boundary_time);
        }
    }

    assert!(!engine.lock().is_replacing);
    assert!(engine.lock().keys.is_empty());
    assert_eq!(engine.control.fixed_count(), 0);
    assert!(session.ready.try_recv().is_err());
    for (name, mut samples) in [
        ("letter press/release", letters),
        ("Space press/release", boundaries),
    ] {
        samples.sort_unstable();
        eprintln!(
            "engine {name}: p50 {:?}, p95 {:?}, max {:?} ({} samples)",
            samples[9_999],
            samples[18_999],
            samples[19_999],
            samples.len(),
        );
    }
    crate::config::Config::update_live(|config| *config = saved);
}

impl<const QUEUE_UNDO: bool, const VERIFY: bool> SessionWithPolicy<QUEUE_UNDO, VERIFY> {
    fn new() -> Self {
        let (ready_tx, ready) = mpsc::sync_channel(1);
        let (proceed, proceed_rx) = mpsc::sync_channel(1);
        let engine = Engine::new(
            crate::dictionary::en_dict(),
            crate::dictionary::he_dict(),
            Arc::new(AppControl::new_for_test()),
            Screen {
                text: Mutex::new(String::new()),
                erasures: Mutex::new(Vec::new()),
                injecting: std::sync::atomic::AtomicBool::new(false),
                ready: ready_tx,
                proceed: Mutex::new(proceed_rx),
            },
        );
        Self {
            engine,
            ready,
            proceed,
        }
    }
    fn type_text(&self, text: &str) {
        for key in text.chars() {
            self.type_while_checking(&key.to_string());
            self.checks_ready();
        }
    }
    fn type_while_checking(&self, text: &str) {
        for key in text.chars() {
            // Capture runs before the application receives the character.
            self.engine.key_press(key);
            let visible = if Simulated::current_layout() == Some(Language::Hebrew) {
                Simulated::hebrew_char(key).unwrap_or(key)
            } else if self.engine.lock().shift_active() {
                key.to_ascii_uppercase()
            } else {
                key
            };
            self.engine.injector.text.lock().unwrap().push(visible);
            self.engine.key_release(key);
        }
    }
    fn checks_ready(&self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.engine.lock().word_check_worker {
            assert!(Instant::now() < deadline, "automatic search did not finish");
            thread::sleep(Duration::from_millis(1));
        }
    }
    fn tap(&self, key: char) {
        self.engine.key_press(key);
        self.engine.key_release(key);
        self.suggestions_ready();
    }
    fn suggestions_ready(&self) {
        // Debug corpus tests contend for CPU; release searches retain the tight deadline.
        let seconds = if cfg!(debug_assertions) { 10 } else { 3 };
        let deadline = Instant::now() + Duration::from_secs(seconds);
        while self.engine.lock().suggestion_worker {
            assert!(
                Instant::now() < deadline,
                "suggestion search did not finish"
            );
            thread::sleep(Duration::from_millis(1));
        }
    }
    fn undo(&self) {
        let config = crate::config::Config::global();
        let action = config.action_shortcut.clone();
        let undo = config.undo_shortcut.clone();
        crate::config::Config::update_live(|cfg| {
            cfg.action_shortcut = "left_ctrl".into();
            cfg.undo_shortcut = "right_ctrl".into();
        });
        self.tap(Simulated::CTRL_RIGHT);
        crate::config::Config::update_live(|cfg| {
            cfg.action_shortcut = action;
            cfg.undo_shortcut = undo;
        });
    }
    fn backspace(&self) {
        self.engine.injector.text.lock().unwrap().pop();
        self.tap(Simulated::BACKSPACE);
    }
    fn pending(&self) {
        self.ready
            .recv_timeout(Duration::from_secs(3))
            .expect("no correction scheduled");
    }
    fn finish(&self) {
        self.proceed.send(()).unwrap();
        self.idle();
    }
    fn idle(&self) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while self.engine.lock().is_replacing {
            assert!(Instant::now() < deadline, "replacement did not finish");
            thread::sleep(Duration::from_millis(1));
        }
    }
    fn text(&self) -> String {
        self.engine.injector.text.lock().unwrap().clone()
    }
}

#[test]
fn command_modifiers_cancel_pending_keycode_replacement() {
    const CHILD: &str = "RECAST_COMMAND_MODIFIER_TEST";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "platform::engine::tests::command_modifiers_cancel_pending_keycode_replacement",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    crate::config::Config::update_live(|cfg| {
        cfg.action_shortcut = "left_ctrl".into();
        cfg.undo_shortcut = "right_ctrl".into();
    });
    for modifier in [Simulated::CTRL_LEFT, Simulated::CTRL_RIGHT] {
        let s = SessionWithPolicy::<false>::new();
        assert!(!s.engine.control.has_app_rules());
        s.type_text("recieve");
        s.engine.key_press(' ');
        s.engine.injector.text.lock().unwrap().push(' ');
        s.pending();
        let generation = s.engine.lock().generation;
        s.engine.key_press(modifier);
        assert_ne!(s.engine.lock().generation, generation);
        assert!(!s.engine.replacement_valid(generation));
        s.engine.key_release(' ');
        s.engine.key_release(modifier);
        s.finish();
        assert_eq!(s.text(), "recieve ");
        assert_eq!(s.engine.control.fixed_count(), 0);
        assert!(s.engine.lock().last_action.is_none());
    }

    // Shift completion taps keep their existing queue, and the platform
    // that injects text without physical Ctrl flags still queues undo taps.
    fn gesture_survives<const QUEUE_UNDO: bool>(modifier: char) {
        let s = SessionWithPolicy::<QUEUE_UNDO>::new();
        s.type_text("recieve ");
        s.pending();
        let generation = s.engine.lock().generation;
        s.tap(modifier);
        assert_eq!(s.engine.lock().generation, generation);
        s.finish();
        assert_eq!(s.text(), "receive ");
    }
    gesture_survives::<false>(Simulated::SHIFT_RIGHT);
    gesture_survives::<true>(Simulated::CTRL_LEFT);

    #[cfg(target_os = "linux")]
    {
        let s = SessionWithPolicy::<false>::new();
        s.type_text("recieve ");
        s.pending();
        let generation = s.engine.lock().generation;
        let replay = ['r', ' '];
        assert!(s.engine.replay_valid(&replay, generation, 0));
        FOCUS.store(2, Ordering::SeqCst);
        assert!(!s.engine.replay_valid(&replay, generation, 0));
        FOCUS.store(1, Ordering::SeqCst);
        assert!(s.engine.replay_valid(&replay, generation, 0));
        s.type_text("x");
        assert!(!s.engine.replay_valid(&replay, generation, 0));
        s.finish();
        assert_eq!(s.text(), "receive x");
    }
}

#[test]
fn input_during_commit_bookkeeping_invalidates_snapshot() {
    // Engine simulations share configuration and focus with other tests.
    const CHILD: &str = "RECAST_COMMIT_INPUT_TEST";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "platform::engine::tests::input_during_commit_bookkeeping_invalidates_snapshot",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    let s = Session::new();
    s.type_text("recieve ");
    s.pending();
    let history = s.engine.control.lock_history_for_test();
    s.proceed.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while s.engine.control.fixed_count() == 0 {
        assert!(
            Instant::now() < deadline,
            "correction did not reach bookkeeping"
        );
        thread::yield_now();
    }
    // The worker accepted its replay snapshot before blocking on history.
    s.type_text("x");
    drop(history);
    s.idle();
    assert_eq!(s.text(), "receive x");
    assert!(s.engine.lock().no_fix);
    assert!(s.engine.lock().last_action.is_none());
    assert_eq!(s.engine.control.fixed_count(), 1);
    s.undo();
    assert!(s.ready.try_recv().is_err());
    assert_eq!(s.text(), "receive x");

    s.type_text(" recieve ");
    s.pending();
    s.finish();
    assert_eq!(s.text(), "receive x receive ");
}

#[test]
fn buffered_terminator_overflow_cancels_pending_edits() {
    // Engine simulations share configuration and focus with other tests.
    const CHILD: &str = "RECAST_BUFFERED_TERMINATOR_OVERFLOW_TEST";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "platform::engine::tests::buffered_terminator_overflow_cancels_pending_edits",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    for separator in [" ", "\n"] {
        let s = Session::new();
        let long = "x".repeat(crate::types::MAX_WORD_KEYS + 1);
        s.type_text(&long);
        assert!(s.engine.lock().keys.is_empty());
        s.type_text(separator);
        s.type_text("recieve ");
        s.pending();
        s.finish();
        assert_eq!(s.text(), format!("{long}{separator}receive "));
    }

    let s = Session::new();
    let spaces = " ".repeat(crate::types::MAX_WORD_KEYS + 1);
    s.type_text("recieve ");
    s.pending();
    s.type_text(&spaces);
    s.finish();
    assert_eq!(s.text(), format!("recieve {spaces}"));
    assert!(s.engine.lock().last_action.is_none());
    assert!(s.engine.lock().keys.is_empty());
    assert_eq!(s.engine.control.fixed_count(), 0);

    // The next boundary recovers ordinary correction after cancellation.
    s.type_text(" ");
    s.type_text("recieve ");
    s.pending();
    s.finish();
    assert_eq!(s.text(), format!("recieve {spaces} receive "));
}

#[test]
fn unmapped_keys_cancel_capture_and_pending_edits_until_a_boundary() {
    const CHILD: &str = "RECAST_UNMAPPED_KEYS_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "platform::engine::tests::unmapped_keys_cancel_capture_and_pending_edits_until_a_boundary", "--nocapture"])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    EMPTY_INPUT_SUPPORTED.store(false, Ordering::SeqCst);
    let type_unmapped = |session: &Session, visible: char| {
        // This simulated key is neither mapped text nor a reset/modifier.
        // The application still receives its printable keypad character.
        session.engine.key_press('\x16');
        session.engine.injector.text.lock().unwrap().push(visible);
        session.engine.key_release('\x16');
    };
    for (before, after, visible) in [
        ("recieve", "", '1'),
        ("recie", "ve", '1'),
        ("", "recieve", '+'),
    ] {
        let s = Session::new();
        s.type_text(before);
        type_unmapped(&s, visible);
        s.type_text(&format!("{after} "));
        let untouched = format!("{before}{visible}{after} ");
        assert_eq!(s.text(), untouched);
        assert!(!s.engine.lock().is_replacing);
        assert_eq!(s.engine.control.fixed_count(), 0);
        assert!(s.ready.try_recv().is_err());
        s.type_text("recieve ");
        s.pending();
        s.finish();
        assert_eq!(s.text(), format!("{untouched}receive "));
    }

    let s = Session::new();
    s.type_text("recieve ");
    s.pending();
    let generation = s.engine.lock().generation;
    type_unmapped(&s, '1');
    assert_ne!(s.engine.lock().generation, generation);
    s.finish();
    assert_eq!(s.text(), "recieve 1");
    assert_eq!(s.engine.control.fixed_count(), 0);
    assert!(s.engine.control.history().is_empty());
    assert!(s.engine.lock().last_action.is_none());
    s.type_text(" recieve ");
    s.pending();
    s.finish();
    assert_eq!(s.text(), "recieve 1 receive ");

    // An unclassified function/media key also invalidates a captured token,
    // even when the application happens to leave its visible text intact.
    let s = Session::new();
    s.type_text("recieve");
    s.tap('\x17');
    s.type_text(" ");
    assert_eq!(s.text(), "recieve ");
    assert!(!s.engine.lock().is_replacing);

    // Known modifiers and Backspace retain their existing capture paths.
    crate::config::Config::update_live(|config| {
        config.action_shortcut = "none".into();
        config.undo_shortcut = "none".into();
        config.completion_shortcut = "none".into();
        config.correction_shortcut = "none".into();
    });
    for modifier in [
        Simulated::SHIFT_LEFT,
        Simulated::SHIFT_RIGHT,
        Simulated::CTRL_LEFT,
        Simulated::CTRL_RIGHT,
        Simulated::CAPS_LOCK,
    ] {
        let s = Session::new();
        s.type_text("recie");
        s.tap(modifier);
        assert_eq!(s.engine.lock().keys.len(), 5);
        assert!(!s.engine.lock().no_fix);
    }
    let s = Session::new();
    s.type_text("recieve");
    s.backspace();
    s.type_text("e ");
    s.pending();
    s.finish();
    assert_eq!(s.text(), "receive ");
}

#[test]
fn first_word_after_click_or_new_focus_corrects_without_a_warmup_word() {
    const CHILD: &str = "RECAST_FIRST_WORD_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "platform::engine::tests::first_word_after_click_or_new_focus_corrects_without_a_warmup_word", "--nocapture"])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    // Linux and Windows cannot confirm that the input field is empty.
    // Reproduce a missed first word even when that probe is unavailable.
    EMPTY_INPUT_SUPPORTED.store(false, Ordering::SeqCst);
    ALLOW_LAYOUT_SWITCH.store(true, Ordering::SeqCst);
    for context in ["click", "button", "focus", "layout shortcut"] {
        for previous in ["", "existing ", "prefix"] {
            for (layout, typed, expected) in [
                (0, "recieve", "receive"),
                (0, "akuo", "שלום"),
                (1, "hello", "hello"),
                (1, "neks,,", "מקלדת"),
            ] {
                SIMULATED_LAYOUT.store(0, Ordering::SeqCst);
                FOCUS.store(1, Ordering::SeqCst);
                let s = Session::new();
                s.type_text("old ");
                *s.engine.injector.text.lock().unwrap() = previous.into();
                match context {
                    "click" => s.engine.mouse_click(),
                    "button" => s.tap('\x15'),
                    "focus" => {
                        s.tap('\x1b');
                        FOCUS.store(2, Ordering::SeqCst);
                    }
                    _ => {
                        s.engine.key_press(Simulated::CTRL_LEFT);
                        s.tap(' ');
                        s.engine.key_release(Simulated::CTRL_LEFT);
                    }
                }
                SIMULATED_LAYOUT.store(layout, Ordering::SeqCst);
                s.type_text(&format!("{typed} "));
                s.pending();
                s.finish();
                assert_eq!(
                    s.text(),
                    format!("{previous}{expected} "),
                    "{context}: {typed}"
                );

                // The identical word must not depend on having been typed once.
                SIMULATED_LAYOUT.store(layout, Ordering::SeqCst);
                s.type_text(&format!("{typed} "));
                s.pending();
                s.finish();
                assert_eq!(s.text(), format!("{previous}{expected} {expected} "));
                s.undo();
                s.pending();
                s.finish();
                let original = if layout == 0 {
                    typed.into()
                } else {
                    crate::keymap::convert_selection(typed)
                };
                assert_eq!(s.text(), format!("{previous}{expected} {original} "));
                crate::complete::unlist(&original);
            }
        }
    }
    // Clicking still cancels an older pending edit. It must not poison
    // the first word captured after that canceled worker has stopped.
    SIMULATED_LAYOUT.store(0, Ordering::SeqCst);
    FOCUS.store(1, Ordering::SeqCst);
    let s = Session::new();
    s.type_text("recieve ");
    s.pending();
    s.engine.mouse_click();
    s.finish();
    assert_eq!(s.text(), "recieve ");
    assert_eq!(s.engine.control.fixed_count(), 0);
    s.type_text("recieve ");
    s.pending();
    s.finish();
    assert_eq!(s.text(), "recieve receive ");
}

#[test]
fn explicit_requests_and_continued_typing_work_across_native_boundaries() {
    const CHILD: &str = "RECAST_BOUNDARY_COVERAGE_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "platform::engine::tests::explicit_requests_and_continued_typing_work_across_native_boundaries", "--nocapture"])
            .env(CHILD, "1").output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    EMPTY_INPUT_SUPPORTED.store(false, Ordering::SeqCst);
    for (prefix, word) in [
        ("sha2", "sha256sum"),
        ("my_", "my_tool"),
        ("node.", "node.js"),
    ] {
        crate::complete::set_word_ignored(word, true).unwrap();
        let s = Session::new();
        s.type_text(prefix);
        s.tap(Simulated::SHIFT_RIGHT);
        s.pending();
        s.finish();
        assert_eq!(s.text(), word);
        s.undo();
        s.pending();
        s.finish();
        assert_eq!(s.text(), prefix);
        crate::complete::set_word_ignored(word, false).unwrap();
    }
    for previous in ["", "existing"] {
        for (prefix, key) in [
            ("keyb", Simulated::SHIFT_RIGHT),
            ("keyb,", Simulated::SHIFT_RIGHT),
            ("(keyb.)", Simulated::SHIFT_RIGHT),
            ("hte", Simulated::SHIFT_LEFT),
        ] {
            let s = Session::new();
            s.engine.injector.text.lock().unwrap().push_str(previous);
            s.engine.mouse_click();
            s.type_text(prefix);
            s.tap(key);
            s.pending();
            s.finish();
            assert!(s.text().starts_with(previous));
            assert_ne!(s.text(), format!("{previous}{prefix}"));
            s.type_text(" ");
            s.undo();
            s.pending();
            s.finish();
            assert_eq!(s.text(), format!("{previous}{prefix} "));
            crate::complete::unlist(prefix);
        }
    }
    // Keyboard navigation still protects an incomplete editing fragment.
    let s = Session::new();
    s.tap('\x10');
    s.type_text("recieve ");
    assert!(!s.engine.lock().is_replacing);
    s.type_text("recieve ");
    s.pending();
    s.finish();
    assert_eq!(s.text(), "recieve receive ");

    for buffered in ["next ", "one two ", "next par"] {
        let s = Session::new();
        s.type_text("recieve ");
        s.pending();
        s.type_text(buffered);
        s.finish();
        let expected = buffered.rsplit(' ').next().unwrap();
        assert_eq!(
            reading::<Simulated>(&s.engine.lock().keys, Language::English),
            expected
        );
        if expected.is_empty() {
            s.type_text("recieve ");
            s.pending();
            s.finish();
            assert_eq!(s.text(), format!("receive {buffered}receive "));
        } else {
            s.tap(Simulated::SHIFT_RIGHT);
            s.pending();
            s.finish();
            assert!(s.text().starts_with("receive next "));
        }
    }
    // Native writes can finish while new physical input is already arriving.
    // That late input must cancel stale undo, not erase a subsequent word.
    let s = Session::new();
    let (ready, waiting) = mpsc::sync_channel(1);
    let (proceed, reply) = mpsc::sync_channel(1);
    *INJECT_RETURN_GATE.lock().unwrap() = Some(FocusGate {
        ready,
        proceed: reply,
    });
    s.type_text("recieve ");
    s.pending();
    s.proceed.send(()).unwrap();
    waiting.recv_timeout(Duration::from_secs(3)).unwrap();
    s.type_text("next ");
    proceed.send(()).unwrap();
    s.idle();
    assert_eq!(s.text(), "receive next ");
    assert!(s.engine.lock().last_action.is_none());
    s.undo();
    assert!(s.ready.try_recv().is_err());
    assert_eq!(s.text(), "receive next ");
    s.type_text(" keyb");
    s.tap(Simulated::SHIFT_RIGHT);
    s.pending();
    s.finish();
    assert_eq!(s.text(), "receive next  keyboard");

    // Caps Lock affects letter case, never unshifted punctuation or digits.
    let s = Session::new();
    s.tap(Simulated::CAPS_LOCK);
    s.type_text("1[");
    assert!(s.engine.lock().keys.iter().all(|typed| !typed.shift));

    ALLOW_LAYOUT_SWITCH.store(true, Ordering::SeqCst);
    for source in [0, 1] {
        for spaces in ["", " ", "  "] {
            SIMULATED_LAYOUT.store(source, Ordering::SeqCst);
            let s = Session::new();
            s.engine.injector.text.lock().unwrap().push_str("older ");
            s.engine.mouse_click();
            s.type_text("akuo");
            let original = s.text();
            for _ in 0..2 {
                s.tap(Simulated::CTRL_LEFT);
            }
            s.pending();
            s.finish();
            assert_ne!(s.text(), original);
            s.type_text(spaces);
            for _ in 0..2 {
                s.tap(Simulated::CTRL_LEFT);
            }
            s.pending();
            s.finish();
            assert_eq!(s.text(), format!("{original}{spaces}"));
            assert_eq!(SIMULATED_LAYOUT.load(Ordering::SeqCst), source);
        }
    }
    for source in [0, 1] {
        SIMULATED_LAYOUT.store(source, Ordering::SeqCst);
        let s = Session::new();
        // Navigation leaves an untracked prefix, so only explicit
        // conversion may act on this first captured word.
        s.tap('\x10');
        s.type_text("akuo ");
        let original = s.text();
        assert!(!s.engine.lock().is_replacing);
        assert!(matches!(
            s.engine.lock().last_action,
            Some(LastAction::Unchanged { .. })
        ));
        for _ in 0..2 {
            s.tap(Simulated::CTRL_LEFT);
        }
        s.pending();
        s.finish();
        assert_ne!(s.text(), original);
        for _ in 0..2 {
            s.tap(Simulated::CTRL_LEFT);
        }
        s.pending();
        s.finish();
        assert_eq!(s.text(), original);
    }
    for unknown_at_boundary in [false, true] {
        SIMULATED_LAYOUT.store(if unknown_at_boundary { 2 } else { 0 }, Ordering::SeqCst);
        let s = Session::new();
        s.engine.mouse_click();
        s.type_text("qzxx ");
        // A recovered backend or an external switch must not consume the
        // stored word before the explicit conversion can retry it.
        SIMULATED_LAYOUT.store(usize::from(!unknown_at_boundary), Ordering::SeqCst);
        for _ in 0..2 {
            s.tap(Simulated::CTRL_LEFT);
        }
        s.pending();
        s.finish();
        assert_eq!(s.text(), "/זסס ");
        for _ in 0..2 {
            s.tap(Simulated::CTRL_LEFT);
        }
        s.pending();
        s.finish();
        assert_eq!(s.text(), "qzxx ");
    }
    // Real prefixes that are dictionary words in the wrong layout still
    // expose the intended language in the full explicit suggestion pool.
    for (target, freq, dict, other) in [
        (
            Language::English,
            crate::dictionary::en_freq(),
            crate::dictionary::en_dict(),
            crate::dictionary::he_dict(),
        ),
        (
            Language::Hebrew,
            crate::dictionary::he_freq(),
            crate::dictionary::he_dict(),
            crate::dictionary::en_dict(),
        ),
    ] {
        let mut cases = Vec::new();
        let mut prefixes = HashSet::new();
        freq.for_each_with_prefix("", |word, rank| {
            if cases.len() >= 5
                || rank > 30_000
                || !dict.contains(word)
                || !word.chars().all(|c| {
                    if target == Language::English {
                        c.is_ascii_lowercase()
                    } else {
                        ('א'..='ת').contains(&c)
                    }
                })
            {
                return;
            }
            let len = word.chars().count();
            if len < 4 {
                return;
            }
            let prefix: String = word.chars().take((len - 1).min(6)).collect();
            let alternate = crate::keymap::convert_selection(&prefix);
            if other.contains(&alternate) && prefixes.insert(prefix.clone()) {
                cases.push((
                    word.to_owned(),
                    if target == Language::English {
                        prefix
                    } else {
                        alternate
                    },
                ));
            }
        });
        assert_eq!(cases.len(), 5);
        for (word, physical) in cases {
            SIMULATED_LAYOUT.store(
                usize::from(target.other() == Language::Hebrew),
                Ordering::SeqCst,
            );
            let s = Session::new();
            s.type_text(&physical);
            s.tap(Simulated::SHIFT_RIGHT);
            s.pending();
            s.finish();
            let st = s.engine.lock();
            let cycle = st.cycle.as_ref().unwrap();
            assert!(
                cycle
                    .candidates
                    .iter()
                    .zip(&cycle.candidate_langs)
                    .any(|(offer, lang)| offer == &word && *lang == target),
                "lost alternate {word}: {:?}",
                cycle.candidates
            );
        }
    }
    SIMULATED_LAYOUT.store(0, Ordering::SeqCst);
    let s = Session::new();
    s.type_text("abc");
    let candidates: Vec<String> = [
        "first",
        "second",
        "third",
        "fourth",
        "שלום",
        "מחשב",
        "חלון",
        "בדיקה",
        "fifth",
        "sixth",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let langs: Vec<_> = candidates
        .iter()
        .map(|word| {
            if word.is_ascii() {
                Language::English
            } else {
                Language::Hebrew
            }
        })
        .collect();
    {
        let mut st = s.engine.lock();
        st.cycle = Some(Cycle {
            preview: false,
            preview_ready: false,
            typed: st.keys.to_vec(),
            trailing_spaces: Vec::new(),
            candidates: candidates.clone(),
            candidate_langs: langs.clone(),
            index: candidates.len(),
            resume_index: 0,
            on_screen: 3,
            lang: Language::English,
            original_lang: Language::English,
            correction: false,
        });
    }
    for (index, expected) in candidates.iter().enumerate() {
        s.tap(Simulated::SHIFT_RIGHT);
        s.pending();
        s.finish();
        assert_eq!(s.text(), *expected);
        assert_eq!(Simulated::current_layout(), Some(langs[index]));
        if (index + 1) % crate::complete::MAX_CANDIDATES == 0 || index + 1 == candidates.len() {
            s.tap(Simulated::SHIFT_RIGHT);
            s.pending();
            s.finish();
            assert_eq!(s.text(), "abc");
            assert_eq!(Simulated::current_layout(), Some(Language::English));
        }
    }
    s.tap(Simulated::SHIFT_RIGHT);
    s.pending();
    s.finish();
    assert_eq!(s.text(), candidates[0]);

    let s = Session::new();
    let (ready, waiting) = mpsc::sync_channel(1);
    let (proceed, reply) = mpsc::sync_channel(1);
    *LAYOUT_GATE.lock().unwrap() = Some(FocusGate {
        ready,
        proceed: reply,
    });
    s.type_text("akuo ");
    waiting.recv_timeout(Duration::from_secs(3)).unwrap();
    assert!(
        s.engine.state.try_lock().is_ok(),
        "automatic layout switch holds typing state"
    );
    s.engine.mouse_click();
    proceed.send(()).unwrap();
    s.idle();
    assert_eq!(s.text(), "akuo ");
    assert!(s.ready.try_recv().is_err());
}

#[test]
fn finished_words_remain_available_to_explicit_suggestions() {
    const CHILD: &str = "RECAST_FINISHED_SUGGESTIONS_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "platform::engine::tests::finished_words_remain_available_to_explicit_suggestions",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    ALLOW_LAYOUT_SWITCH.store(true, Ordering::SeqCst);
    crate::config::Config::update_live(|config| {
        config.spell_enabled = false;
        config.rule_stats_enabled = true;
    });
    for (lang, physical, gesture) in [
        (Language::English, "keyb", Simulated::SHIFT_RIGHT),
        (Language::English, "[keyb]", Simulated::SHIFT_RIGHT),
        (Language::English, "hte", Simulated::SHIFT_LEFT),
        (Language::Hebrew, "njac", Simulated::SHIFT_RIGHT),
    ] {
        for spaces in [1, 2, MAX_UNDO_SPACES + 1] {
            SIMULATED_LAYOUT.store(usize::from(lang == Language::Hebrew), Ordering::SeqCst);
            let s = Session::new();
            let tail = " ".repeat(spaces);
            s.type_text(&format!("{physical}{tail}"));
            let original = s.text();
            assert!(s.engine.lock().keys.is_empty());
            s.tap(gesture);
            s.pending();
            s.finish();
            let choices = s.engine.lock().cycle.as_ref().unwrap().candidates.clone();
            assert!(!choices.is_empty());
            assert_eq!(s.text(), format!("{}{tail}", choices[0]));
            assert!(s.engine.lock().keys.is_empty());
            for choice in choices.iter().take(crate::complete::MAX_CANDIDATES).skip(1) {
                s.tap(gesture);
                s.pending();
                s.finish();
                assert_eq!(s.text(), format!("{choice}{tail}"));
                assert!(s.engine.lock().keys.is_empty());
            }
            s.tap(gesture);
            s.pending();
            s.finish();
            assert_eq!(s.text(), original);
            assert!(s.engine.lock().keys.is_empty());
            s.tap(gesture);
            s.pending();
            s.finish();
            s.undo();
            s.pending();
            s.finish();
            assert_eq!(s.text(), original);
            assert!(s.engine.lock().keys.is_empty());
        }
    }
    SIMULATED_LAYOUT.store(0, Ordering::SeqCst);
    let s = Session::new();
    s.type_text("keyb   ");
    SIMULATED_LAYOUT.store(2, Ordering::SeqCst);
    for _ in 0..3 {
        s.tap(Simulated::SHIFT_RIGHT);
    }
    assert_eq!(s.text(), "keyb   ");
    // An external layout change does not reinterpret the finished word.
    SIMULATED_LAYOUT.store(1, Ordering::SeqCst);
    REFUSE_LAYOUT_SWITCH.store(true, Ordering::SeqCst);
    s.tap(Simulated::SHIFT_RIGHT);
    s.idle();
    assert_eq!(s.text(), "keyb   ");
    assert!(s.ready.try_recv().is_err());
    REFUSE_LAYOUT_SWITCH.store(false, Ordering::SeqCst);
    s.tap(Simulated::SHIFT_RIGHT);
    s.pending();
    s.finish();
    assert_eq!(s.text(), "keyboard   ");
    assert_eq!(Simulated::current_layout(), Some(Language::English));
    s.type_text("next");
    assert_eq!(s.text(), "keyboard   next");
    assert_eq!(
        reading::<Simulated>(&s.engine.lock().keys, Language::English),
        "next"
    );
    assert!(s.engine.lock().completion_use.is_none());
    assert!(matches!(
        s.engine.lock().last_action,
        Some(LastAction::Fixed(_))
    ));

    // A rapid second request also works with no in-progress word buffer.
    let s = Session::new();
    s.type_text("keyb  ");
    s.tap(Simulated::SHIFT_RIGHT);
    s.pending();
    let second = s.engine.lock().cycle.as_ref().unwrap().candidates[1].clone();
    s.tap(Simulated::SHIFT_RIGHT);
    s.proceed.send(()).unwrap();
    s.pending();
    s.finish();
    assert_eq!(s.text(), format!("{second}  "));
    s.undo();
    s.pending();
    s.finish();
    assert_eq!(s.text(), "keyb  ");

    // A retained undo must not make suggestions target the previous word.
    crate::config::Config::update_live(|config| config.spell_enabled = true);
    let s = Session::new();
    s.type_text("recieve ");
    s.pending();
    s.finish();
    crate::config::Config::update_live(|config| config.spell_enabled = false);
    s.type_text("keybo  ");
    s.tap(Simulated::SHIFT_RIGHT);
    s.pending();
    s.finish();
    assert_eq!(s.text(), "receive keyboard  ");
    s.undo();
    s.pending();
    s.finish();
    assert_eq!(s.text(), "receive keybo  ");

    // Deleting an extra space must not leave it in the next edit's erase count.
    let s = Session::new();
    s.type_text("keyb ");
    s.tap(Simulated::SHIFT_RIGHT);
    s.pending();
    s.finish();
    s.type_text("  ");
    s.backspace();
    {
        let st = s.engine.lock();
        let Some(LastAction::Fixed(fix)) = &st.last_action else {
            panic!("correction should remain undoable");
        };
        assert_eq!(fix.trailing_spaces.len(), 1);
        assert_eq!(fix.suffix, " ");
    }
    s.undo();
    s.pending();
    s.finish();
    assert_eq!(s.text(), "keyb  ");

    // Once a correction was accepted, a fresh request starts from that
    // visible word rather than resurrecting its original misspelling.
    crate::config::Config::update_live(|config| config.spell_enabled = true);
    let s = Session::new();
    s.type_text("recieve ");
    s.pending();
    s.finish();
    assert_eq!(s.text(), "receive ");
    s.tap(Simulated::SHIFT_RIGHT);
    s.pending();
    s.finish();
    assert_eq!(
        reading::<Simulated>(
            &s.engine.lock().cycle.as_ref().unwrap().typed,
            Language::English
        ),
        "receive"
    );
    s.undo();
    s.pending();
    s.finish();
    assert_eq!(s.text(), "receive ");
    crate::config::Config::update_live(|config| config.spell_enabled = false);

    crate::complete::suppress("keyb");
    let s = Session::new();
    s.type_text("keyb  ");
    assert!(matches!(
        s.engine.lock().last_action,
        Some(LastAction::Skipped(_))
    ));
    s.tap(Simulated::SHIFT_RIGHT);
    s.pending();
    s.finish();
    assert_eq!(s.text(), "keyboard  ");
    s.undo();
    s.pending();
    s.finish();
    assert_eq!(s.text(), "keyb  ");

    let s = Session::new();
    s.type_text("keyb ");
    s.tap(Simulated::SHIFT_RIGHT);
    s.pending();
    s.type_text("next");
    s.finish();
    assert_eq!(s.text(), "keyboard next");
    assert_eq!(
        reading::<Simulated>(&s.engine.lock().keys, Language::English),
        "next"
    );
    assert!(matches!(
        s.engine.lock().last_action,
        Some(LastAction::Fixed(_))
    ));

    let s = Session::new();
    s.type_text("keyb  ");
    s.engine.mouse_click();
    s.tap(Simulated::SHIFT_RIGHT);
    assert!(s.ready.try_recv().is_err());
    assert_eq!(s.text(), "keyb  ");

    let s = Session::new();
    s.type_text(&format!("keyb{}", " ".repeat(MAX_UNDO_SPACES + 2)));
    s.tap(Simulated::SHIFT_RIGHT);
    assert!(s.ready.try_recv().is_err());

    let s = Session::new();
    s.type_text("hello   ");
    for _ in 0..2 {
        s.tap(Simulated::CTRL_LEFT);
    }
    s.pending();
    s.finish();
    assert_eq!(s.text(), "יקךךם   ");
    s.undo();
    s.pending();
    s.finish();
    assert_eq!(s.text(), "hello   ");

    SIMULATED_LAYOUT.store(0, Ordering::SeqCst);
    let s = Session::new();
    s.engine.caps_lock_changed(true);
    s.type_text("keyb");
    assert_eq!(s.text(), "KEYB");
    s.tap(Simulated::SHIFT_RIGHT);
    s.pending();
    // An identical native snapshot must not cancel a pending edit.
    s.engine.caps_lock_changed(true);
    s.finish();
    assert_eq!(s.text(), "KEYBOARD");
    s.undo();
    s.pending();
    s.finish();
    assert_eq!(s.text(), "KEYB");
    let s = Session::new();
    s.engine.caps_lock_changed(true);
    s.type_text("keyb");
    s.tap(Simulated::SHIFT_RIGHT);
    s.pending();
    // A changed latch during injection invalidates the original replay plan.
    s.engine.caps_lock_changed(false);
    s.finish();
    assert_eq!(s.text(), "KEYB");
    assert!(!s.engine.lock().caps_lock);
}

#[test]
fn stale_context_replies_preserve_newer_typing_and_correction() {
    const CHILD: &str = "RECAST_STALE_CONTEXT_TEST";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "platform::engine::tests::stale_context_replies_preserve_newer_typing_and_correction",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    for query in ["focus", "empty"] {
        for newer in ["word", "replacement", "suppressed", "overlap"] {
            let s = Session::new();
            if query == "empty" {
                s.engine.forget_everything();
            }
            let (ready, reached) = mpsc::sync_channel(0);
            let (proceed, resume) = mpsc::channel();
            let gate = if query == "focus" {
                &FOCUS_GATE
            } else {
                &EMPTY_INPUT_GATE
            };
            *gate.lock().unwrap() = Some(FocusGate {
                ready,
                proceed: resume,
            });
            let engine = Arc::clone(&s.engine);
            let worker = thread::spawn(move || engine.key_press('x'));
            reached.recv_timeout(Duration::from_secs(3)).unwrap();
            s.engine.injector.text.lock().unwrap().push('x');
            s.engine.key_release('x');
            // A click starts a fresh field while the old target is still replying.
            if newer != "overlap" {
                s.engine.mouse_click();
                s.engine.injector.text.lock().unwrap().clear();
            }
            if newer == "suppressed" {
                s.engine.injector.text.lock().unwrap().push_str("prefix");
                s.tap('\x10');
            }
            s.type_text("recieve");
            if newer == "replacement" {
                s.type_text(" ");
                s.pending();
            }
            let (generation, revision) = {
                let st = s.engine.lock();
                (st.generation, st.revision)
            };
            proceed.send(()).unwrap();
            worker.join().unwrap();
            if newer == "overlap" {
                // Without a new context, the delayed first key leaves an unknown prefix.
                assert!(s.engine.lock().no_fix);
                assert!(s.engine.lock().keys.is_empty());
                s.type_text(" ");
                assert!(s.ready.try_recv().is_err());
                assert_eq!(s.text(), "xrecieve ");
                continue;
            }
            {
                let st = s.engine.lock();
                assert_eq!(st.generation, generation, "{query}: {newer}");
                assert_eq!(st.revision, revision, "{query}: {newer}");
                assert_eq!(st.no_fix, newer == "suppressed");
                if newer != "replacement" {
                    assert_eq!(reading::<Simulated>(&st.keys, Language::English), "recieve");
                }
            }
            if newer != "replacement" {
                s.type_text(" ");
                if newer == "suppressed" {
                    assert!(s.ready.try_recv().is_err());
                    assert_eq!(s.text(), "prefixrecieve ");
                    continue;
                }
                s.pending();
            }
            s.finish();
            assert_eq!(s.text(), "receive ");
            assert_eq!(s.engine.control.fixed_count(), 1);
        }
    }
}

#[test]
fn ordinary_release_during_focus_check_keeps_correction() {
    const CHILD: &str = "RECAST_FOCUS_RELEASE_TEST";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "platform::engine::tests::ordinary_release_during_focus_check_keeps_correction",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    for event in ["release", "click", "typing", "modifier"] {
        let s = Session::new();
        s.type_text("recieve");
        let (ready, reached) = mpsc::sync_channel(0);
        let (proceed, resume) = mpsc::channel();
        *FOCUS_GATE.lock().unwrap() = Some(FocusGate {
            ready,
            proceed: resume,
        });
        let engine = Arc::clone(&s.engine);
        let worker = thread::spawn(move || engine.key_press(' '));
        reached.recv_timeout(Duration::from_secs(3)).unwrap();
        s.engine.injector.text.lock().unwrap().push(' ');
        s.engine.key_release(' ');
        match event {
            "click" => s.engine.mouse_click(),
            "typing" => s.type_while_checking("x"),
            "modifier" => s.tap(Simulated::CTRL_LEFT),
            _ => (),
        }
        proceed.send(()).unwrap();
        worker.join().unwrap();
        s.checks_ready();
        if event == "release" {
            s.pending();
            s.finish();
            assert_eq!(s.text(), "receive ");
        } else {
            assert!(s.ready.try_recv().is_err());
            assert!(s.text().starts_with("recieve "));
        }
    }
}

#[test]
fn action_taps_allow_typing_rollover_but_not_chords() {
    const CHILD: &str = "RECAST_ACTION_ROLLOVER_TEST";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "platform::engine::tests::action_taps_allow_typing_rollover_but_not_chords",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    crate::config::Config::update_live(|config| {
        config.action_shortcut = "left_ctrl".into();
        config.undo_shortcut = "right_ctrl".into();
    });
    ALLOW_LAYOUT_SWITCH.store(true, Ordering::SeqCst);
    for tail in ['x', ' '] {
        SIMULATED_LAYOUT.store(0, Ordering::SeqCst);
        let s = Session::new();
        s.type_text("qzx");
        s.engine.key_press(tail);
        s.engine.injector.text.lock().unwrap().push(tail);
        s.engine.key_press(Simulated::CTRL_LEFT);
        s.engine.key_release(tail);
        s.engine.key_release(Simulated::CTRL_LEFT);
        s.tap(Simulated::CTRL_LEFT);
        s.pending();
        s.finish();
        assert_ne!(s.text(), format!("qzx{tail}"));
        assert_eq!(s.text().ends_with(' '), tail == ' ');
    }
    // The same rollover must not turn a real Ctrl chord into an action.
    SIMULATED_LAYOUT.store(0, Ordering::SeqCst);
    let s = Session::new();
    s.type_text("qzx");
    s.engine.key_press('x');
    s.engine.key_press(Simulated::CTRL_LEFT);
    s.engine.key_release('x');
    s.engine.key_press('a');
    s.engine.key_release('a');
    s.engine.key_release(Simulated::CTRL_LEFT);
    s.tap(Simulated::CTRL_LEFT);
    assert!(!s.engine.lock().is_replacing);
    assert_eq!(s.text(), "qzx");
}

#[test]
fn suggestion_taps_allow_typing_rollover_but_not_chords() {
    const CHILD: &str = "RECAST_SUGGESTION_ROLLOVER_TEST";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "platform::engine::tests::suggestion_taps_allow_typing_rollover_but_not_chords",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    #[cfg(target_os = "linux")]
    let native_space = crate::keymap::evkey_to_english_char(evdev::KeyCode::KEY_SPACE);
    #[cfg(not(target_os = "linux"))]
    let native_space = crate::keymap::key_to_english_char(rdev::Key::Space);
    assert_eq!(Simulated::english_char_plain(' '), native_space);
    assert!(native_space.is_none());

    for (binding, key) in [
        ("left_shift", Simulated::SHIFT_LEFT),
        ("right_shift", Simulated::SHIFT_RIGHT),
        ("left_ctrl", Simulated::CTRL_LEFT),
        ("right_ctrl", Simulated::CTRL_RIGHT),
    ] {
        for correction in [false, true] {
            crate::config::Config::update_live(|config| {
                config.action_shortcut = "none".into();
                config.undo_shortcut = "none".into();
                config.completion_shortcut = if correction { "none" } else { binding }.into();
                config.correction_shortcut = if correction { binding } else { "none" }.into();
                config.spell_enabled = false;
            });
            for space in [true, false] {
                for chord in [false, true] {
                    let s = Session::new();
                    let (word, wanted) = if correction {
                        ("hte", "the")
                    } else {
                        ("keyb", "keyboard")
                    };
                    let (prefix, tail) = if space {
                        (word, ' ')
                    } else {
                        (&word[..word.len() - 1], word.chars().last().unwrap())
                    };
                    s.type_text(prefix);
                    s.engine.key_press(tail);
                    s.engine.injector.text.lock().unwrap().push(tail);
                    s.engine.key_press(key);
                    s.engine.key_release(tail);
                    if chord {
                        s.engine.key_press('x');
                        s.engine.key_release('x');
                    }
                    s.engine.key_release(key);
                    s.suggestions_ready();
                    if chord {
                        assert!(s.ready.try_recv().is_err(), "{binding}: {space}");
                        assert!(!s.engine.lock().is_replacing);
                        continue;
                    }
                    s.pending();
                    s.finish();
                    let separator = if space { " " } else { "" };
                    assert_eq!(s.text(), format!("{wanted}{separator}"), "{binding}");
                    s.undo();
                    s.pending();
                    s.finish();
                    assert_eq!(s.text(), format!("{word}{separator}"), "{binding}");
                }
            }
        }
    }
}

#[test]
fn undoing_conversion_after_space_keeps_the_next_word_separate() {
    const CHILD: &str = "RECAST_CONVERSION_BOUNDARY_TEST";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "platform::engine::tests::undoing_conversion_after_space_keeps_the_next_word_separate",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    ALLOW_LAYOUT_SWITCH.store(true, Ordering::SeqCst);
    crate::config::Config::update_live(|config| config.action_shortcut = "left_ctrl".into());
    for finished_before_conversion in [false, true] {
        for count in [1, 3] {
            SIMULATED_LAYOUT.store(0, Ordering::SeqCst);
            let s = Session::new();
            let spaces = " ".repeat(count);
            s.type_text("hello");
            if finished_before_conversion {
                s.type_text(&spaces);
            }
            s.tap(Simulated::CTRL_LEFT);
            s.tap(Simulated::CTRL_LEFT);
            s.pending();
            s.finish();
            if !finished_before_conversion {
                s.type_text(&spaces);
            }
            assert_eq!(s.text(), format!("יקךךם{spaces}"));
            s.undo();
            s.pending();
            s.finish();
            assert_eq!(s.text(), format!("hello{spaces}"));
            assert!(
                s.engine.lock().keys.is_empty(),
                "restored word is already finished"
            );
            s.type_text("recieve ");
            s.pending();
            s.finish();
            assert_eq!(s.text(), format!("hello{spaces}receive "));
            assert!(!crate::complete::suppressed("hello"));
        }
    }
}

#[test]
fn suggestions_and_conversion_recover_without_retyping() {
    const CHILD: &str = "RECAST_RETRY_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "platform::engine::tests::suggestions_and_conversion_recover_without_retyping",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    crate::config::Config::update_live(|config| config.rule_stats_enabled = true);
    for (text, key) in [
        ("keyb", Simulated::SHIFT_RIGHT),
        ("hte", Simulated::SHIFT_LEFT),
    ] {
        let s = Session::new();
        s.type_text(text);
        SIMULATED_LAYOUT.store(2, Ordering::SeqCst);
        for _ in 0..3 {
            s.tap(key);
        }
        assert_eq!(s.text(), text);
        assert_eq!(s.engine.lock().keys.len(), text.len());
        assert!(!s.engine.lock().no_fix);
        SIMULATED_LAYOUT.store(0, Ordering::SeqCst);
        s.tap(key);
        s.pending();
        s.finish();
        assert_ne!(s.text(), text);
    }

    // A release overlapping a Shift tap is ordinary typing rollover.
    for key in [Simulated::SHIFT_RIGHT, Simulated::SHIFT_LEFT] {
        let s = Session::new();
        let (prefix, tail) = if key == Simulated::SHIFT_RIGHT {
            ("key", 'b')
        } else {
            ("ht", 'e')
        };
        s.type_text(prefix);
        s.engine.key_press(tail);
        s.engine.injector.text.lock().unwrap().push(tail);
        s.engine.key_press(key);
        s.engine.key_release(tail);
        s.engine.key_release(key);
        s.pending();
        s.finish();
        assert_ne!(s.text(), format!("{prefix}{tail}"));

        // A letter pressed after Shift is still a chord, never a request.
        let s = Session::new();
        s.type_text(prefix);
        s.engine.key_press(key);
        s.engine.key_press(tail);
        s.engine.key_release(tail);
        s.engine.key_release(key);
        assert!(!s.engine.lock().is_replacing);
    }

    // An unsupported abbreviation must not starve usable dictionary offers.
    let path = crate::complete::user_path("abbrev.txt").unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "keyb = 🙂\n").unwrap();
    crate::complete::reload_user_files();
    REJECT_UNICODE.store(true, Ordering::SeqCst);
    let s = Session::new();
    s.type_text("keyb");
    s.tap(Simulated::SHIFT_RIGHT);
    s.pending();
    s.finish();
    assert_eq!(s.text(), "keyboard");
    assert!(s
        .engine
        .lock()
        .cycle
        .as_ref()
        .unwrap()
        .candidates
        .iter()
        .all(|word| word.is_ascii()));
    REJECT_UNICODE.store(false, Ordering::SeqCst);
    std::fs::write(&path, "").unwrap();
    crate::complete::reload_user_files();

    // Rapid taps coalesce to one queued step instead of disappearing.
    let reference = Session::new();
    reference.type_text("keyb");
    for _ in 0..2 {
        reference.tap(Simulated::SHIFT_RIGHT);
        reference.pending();
        reference.finish();
    }
    let s = Session::new();
    s.type_text("keyb");
    s.tap(Simulated::SHIFT_RIGHT);
    s.pending();
    for _ in 0..3 {
        s.tap(Simulated::SHIFT_RIGHT);
    }
    s.proceed.send(()).unwrap();
    s.pending();
    s.finish();
    assert_eq!(s.text(), reference.text());
    assert!(s.engine.lock().pending_suggestion.is_none());
    for cancel in ["typing", "click", "focus", "overflow"] {
        let s = Session::new();
        s.type_text("keyb");
        s.tap(Simulated::SHIFT_RIGHT);
        s.pending();
        s.tap(Simulated::SHIFT_RIGHT);
        match cancel {
            "typing" => s.type_text("x"),
            "click" => s.engine.mouse_click(),
            "focus" => FOCUS.store(2, Ordering::SeqCst),
            _ => s.type_text(&"x".repeat(crate::types::MAX_WORD_KEYS + 1)),
        }
        s.finish();
        assert!(s.ready.try_recv().is_err(), "queued tap survived {cancel}");
        assert!(s.engine.lock().pending_suggestion.is_none());
        FOCUS.store(1, Ordering::SeqCst);
    }

    ALLOW_LAYOUT_SWITCH.store(true, Ordering::SeqCst);
    SIMULATED_LAYOUT.store(1, Ordering::SeqCst);
    let s = Session::new();
    s.type_text("compu");
    let original = s.text();
    REFUSE_LAYOUT_SWITCH.store(true, Ordering::SeqCst);
    s.tap(Simulated::SHIFT_RIGHT);
    s.idle();
    assert_eq!(s.text(), original);
    assert!(s.ready.try_recv().is_err());
    assert!(!s.engine.lock().no_fix);
    assert!(s.engine.lock().cycle.is_none());
    REFUSE_LAYOUT_SWITCH.store(false, Ordering::SeqCst);
    s.tap(Simulated::SHIFT_RIGHT);
    s.pending();
    s.finish();
    assert!(s.text().starts_with("compu"));
    assert_eq!(SIMULATED_LAYOUT.load(Ordering::SeqCst), 0);

    // A failed switch back must retain the displayed offer and cycle index.
    let count = s
        .engine
        .lock()
        .cycle
        .as_ref()
        .unwrap()
        .candidates
        .len()
        .min(crate::complete::MAX_CANDIDATES);
    for _ in 1..count {
        s.tap(Simulated::SHIFT_RIGHT);
        s.pending();
        s.finish();
    }
    let offered = s.text();
    REFUSE_LAYOUT_SWITCH.store(true, Ordering::SeqCst);
    s.tap(Simulated::SHIFT_RIGHT);
    s.idle();
    assert_eq!(s.text(), offered);
    assert_eq!(s.engine.lock().cycle.as_ref().unwrap().index, count - 1);
    REFUSE_LAYOUT_SWITCH.store(false, Ordering::SeqCst);
    s.tap(Simulated::SHIFT_RIGHT);
    s.pending();
    s.finish();
    assert_eq!(s.text(), original);
    assert_eq!(SIMULATED_LAYOUT.load(Ordering::SeqCst), 1);

    // A refused undo leaves the offered word available for acceptance,
    // without recording abandonment or discarding the preceding phrase.
    SIMULATED_LAYOUT.store(0, Ordering::SeqCst);
    let s = Session::new();
    s.type_text("good ");
    SIMULATED_LAYOUT.store(1, Ordering::SeqCst);
    s.type_text("compu");
    s.tap(Simulated::SHIFT_RIGHT);
    s.pending();
    s.finish();
    let offered = s.text();
    let counts = crate::personal::usefulness_counts();
    REFUSE_LAYOUT_SWITCH.store(true, Ordering::SeqCst);
    s.undo();
    s.idle();
    assert_eq!(s.text(), offered);
    assert_eq!(crate::personal::usefulness_counts(), counts);
    assert_eq!(s.engine.lock().phrase.previous_word(), Some("good"));
    REFUSE_LAYOUT_SWITCH.store(false, Ordering::SeqCst);
    s.type_text(" ");
    assert_eq!(crate::personal::usefulness_counts()[2], counts[2] + 1);

    for source in [0, 1] {
        for separator in ["", " "] {
            SIMULATED_LAYOUT.store(source, Ordering::SeqCst);
            let s = Session::new();
            s.type_text(&format!("do{separator}"));
            let original = s.text();
            REFUSE_LAYOUT_SWITCH.store(true, Ordering::SeqCst);
            for _ in 0..2 {
                s.tap(Simulated::CTRL_LEFT);
            }
            s.idle();
            assert_eq!(s.text(), original);
            assert!(s.ready.try_recv().is_err());
            REFUSE_LAYOUT_SWITCH.store(false, Ordering::SeqCst);
            for _ in 0..2 {
                s.tap(Simulated::CTRL_LEFT);
            }
            s.pending();
            s.finish();
            let expected = if source == 0 { "גם" } else { "do" };
            assert_eq!(s.text(), format!("{expected}{separator}"));
        }
    }
    SIMULATED_LAYOUT.store(0, Ordering::SeqCst);
    crate::complete::suppress("qzxx");
    let s = Session::new();
    s.type_text("qzxx ");
    SIMULATED_LAYOUT.store(2, Ordering::SeqCst);
    REFUSE_LAYOUT_SWITCH.store(true, Ordering::SeqCst);
    for _ in 0..2 {
        s.tap(Simulated::CTRL_LEFT);
    }
    s.idle();
    assert!(matches!(
        s.engine.lock().last_action,
        Some(LastAction::Skipped(_))
    ));
    SIMULATED_LAYOUT.store(0, Ordering::SeqCst);
    REFUSE_LAYOUT_SWITCH.store(false, Ordering::SeqCst);
    for _ in 0..2 {
        s.tap(Simulated::CTRL_LEFT);
    }
    s.pending();
    s.finish();
    assert_eq!(s.text(), "/זסס ");
    SIMULATED_LAYOUT.store(0, Ordering::SeqCst);
    ALLOW_LAYOUT_SWITCH.store(false, Ordering::SeqCst);
}

#[cfg(target_os = "linux")]
#[test]
fn unplugging_cancels_pending_work_and_releases_device_keys() {
    let s = Session::new();
    let generation = {
        let mut st = s.engine.lock();
        st.held_keys.extend([Simulated::CTRL_LEFT, 'x']);
        st.keys.push(Typed {
            key: 'a',
            shift: false,
        });
        st.action_down = Some(Instant::now());
        st.completion_down = Some(Instant::now());
        st.generation
    };
    s.engine
        .input_device_removed(1, &HashSet::from([Simulated::CTRL_LEFT]));
    let st = s.engine.lock();
    assert_ne!(st.generation, generation);
    assert!(st.keys.is_empty() && st.no_fix);
    assert_eq!(st.held_keys, HashSet::from(['x']));
    assert!(st.action_down.is_none() && st.completion_down.is_none());
}

#[test]
fn backspacing_next_word_recovers_the_previous_tracked_word() {
    const CHILD: &str = "RECAST_BACKTRACK_WORD_TEST";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "platform::engine::tests::backspacing_next_word_recovers_the_previous_tracked_word",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    EMPTY_INPUT_SUPPORTED.store(false, Ordering::SeqCst);
    for corrected in [false, true] {
        for spaces in [1, 3] {
            let s = Session::new();
            s.type_text(if corrected { "recieve " } else { "receive " });
            if corrected {
                s.pending();
                s.finish();
            }
            s.type_text(&" ".repeat(spaces - 1));
            s.type_text("next");
            for _ in 0.."next".len() + spaces {
                s.backspace();
            }
            assert_eq!(s.text(), "receive");
            assert_eq!(
                reading::<Simulated>(&s.engine.lock().keys, Language::English),
                "receive"
            );
            // Edit inside that recovered word, then let ordinary Space correct it.
            for _ in 0.."eive".len() {
                s.backspace();
            }
            s.type_text("ieve ");
            s.pending();
            s.finish();
            assert_eq!(s.text(), "receive ");
        }
    }
    // Lost cursor context and overlong tokens must not resurrect an older word.
    for lost in ["navigation", "focus", "overflow"] {
        let s = Session::new();
        s.type_text("receive ");
        if lost == "navigation" {
            s.tap('\x10');
            s.backspace();
            s.type_text("recieve ");
            assert!(!s.engine.lock().is_replacing);
        } else if lost == "focus" {
            FOCUS.store(0, Ordering::SeqCst);
            s.type_text("next");
            assert!(s.engine.lock().deletion_tail.is_none());
            FOCUS.store(1, Ordering::SeqCst);
            s.backspace();
            assert!(s.engine.lock().no_fix);
        } else {
            s.type_text(&"x".repeat(crate::types::MAX_WORD_KEYS + 1));
            assert!(s.engine.lock().deletion_tail.is_none());
            s.type_text(" ");
            s.backspace();
            assert!(s.engine.lock().no_fix);
            assert!(s.engine.lock().keys.is_empty());
        }
    }
}

#[test]
fn delete_and_retype_corrects_without_an_empty_field_probe() {
    const CHILD: &str = "RECAST_DELETE_RETYPE_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "platform::engine::tests::delete_and_retype_corrects_without_an_empty_field_probe",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    EMPTY_INPUT_SUPPORTED.store(false, Ordering::SeqCst);
    ALLOW_LAYOUT_SWITCH.store(true, Ordering::SeqCst);
    let mut cycles = 0;
    for (source, typed, corrected) in [
        (0, "recieve", "receive"),
        (0, "adress", "address"),
        (0, "hellorecieve", "receive"),
        (0, "akuo", "שלום"),
        (1, "hello", "hello"),
        (1, "recieve", "receive"),
    ] {
        for spaces in [1, 3] {
            for extra_backspaces in [0, 4] {
                let s = Session::new();
                for round in 0..10 {
                    SIMULATED_LAYOUT.store(source, Ordering::SeqCst);
                    s.type_text(&format!("{typed} "));
                    assert!(s.engine.lock().is_replacing,
                        "{typed}, round {round}, spaces {spaces}, extra Backspaces {extra_backspaces}: first Space must correct; text {:?}", s.text());
                    s.pending();
                    s.type_text(&" ".repeat(spaces - 1));
                    s.finish();
                    assert_eq!(s.text(), format!("{corrected}{}", " ".repeat(spaces)));
                    for _ in 0..corrected.chars().count() + spaces + extra_backspaces {
                        s.backspace();
                    }
                    assert_eq!(s.text(), "");
                    assert!(
                        !s.engine.lock().no_fix,
                        "deleting known text must preserve a fresh capture"
                    );
                    cycles += 1;
                }
            }
        }
    }
    println!("Completed {cycles} correct/Space/delete/retype cycles without an empty-field probe.");
    // Accepting an explicit completion also leaves a known finished word.
    SIMULATED_LAYOUT.store(0, Ordering::SeqCst);
    let s = Session::new();
    s.type_text("keyb");
    s.tap(Simulated::SHIFT_RIGHT);
    s.pending();
    s.finish();
    let completed = s.text();
    assert!(completed.starts_with("keyb") && completed.len() > 4);
    s.type_text(" ");
    for _ in 0..completed.chars().count() + 1 {
        s.backspace();
    }
    s.type_text("recieve ");
    s.pending();
    s.finish();
    assert_eq!(s.text(), "receive ");

    // Navigation still invalidates the known tail; it cannot justify editing
    // a new suffix attached to text that was never captured.
    let s = Session::new();
    SIMULATED_LAYOUT.store(0, Ordering::SeqCst);
    s.type_text("recieve ");
    s.pending();
    s.finish();
    s.tap('\x10');
    s.backspace();
    s.type_text("recieve ");
    assert!(!s.engine.lock().is_replacing);
}

#[test]
fn merged_tokens_reinsert_only_the_final_word_and_undo_restores_both() {
    const CHILD: &str = "RECAST_MERGED_TOKEN_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "platform::engine::tests::merged_tokens_reinsert_only_the_final_word_and_undo_restores_both", "--nocapture"])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    ALLOW_LAYOUT_SWITCH.store(true, Ordering::SeqCst);
    for (layout, physical, expected) in [
        (0, "hellorecieve", "receive "),
        (0, "helloakuo,", "שלום, "),
        (1, "akuohello", "hello "),
        (1, "akuorecieve", "receive "),
        (1, "akuoneks,,", "מקלדת "),
    ] {
        SIMULATED_LAYOUT.store(layout, Ordering::SeqCst);
        let s = Session::new();
        s.type_text(physical);
        let original = s.text();
        s.type_text(" ");
        s.pending();
        s.finish();
        assert_eq!(s.text(), expected, "{physical}");
        assert_eq!(
            s.engine.injector.erasures.lock().unwrap()[0],
            physical.len() + 1,
            "replacement must erase the entire merged token: {physical}"
        );
        s.undo();
        s.pending();
        s.finish();
        assert_eq!(s.text(), format!("{original} "), "undo: {physical}");
        assert_eq!(SIMULATED_LAYOUT.load(Ordering::SeqCst), layout);
        let original_word = original
            .trim_end_matches(|c: char| !c.is_alphanumeric() && !crate::spell::hebrew::is_mark(c));
        assert!(
            crate::complete::suppressed(original_word),
            "undo should protect the entire original word: {original_word:?}"
        );
        s.type_text(&format!("{physical} "));
        assert_eq!(s.text(), format!("{original} {original} "));
        assert!(!s.engine.lock().is_replacing);
        crate::complete::unlist(original_word);
    }
    // A layout switch refused by the OS must not erase either word.
    SIMULATED_LAYOUT.store(0, Ordering::SeqCst);
    REFUSE_LAYOUT_SWITCH.store(true, Ordering::SeqCst);
    let s = Session::new();
    s.type_text("helloakuo ");
    assert_eq!(s.text(), "helloakuo ");
    assert!(s.ready.try_recv().is_err());
}

#[test]
fn spelling_rewrites_only_the_changed_suffix_and_undo_learns_the_whole_word() {
    for (before, after, erase) in [
        ("keyboad", "keyboard", 1),
        ("recieve", "receive", 4),
        ("Recieve,", "Receive,", 5),
        ("abc", "abcdef", 0),
        ("abc", "ab", 1),
        ("ab", "世界", 2),
        ("hélo", "héllo", 1),
    ] {
        let keys: Vec<_> = before
            .chars()
            .map(|key| Typed { key, shift: false })
            .collect();
        let rep = replacement::<Simulated>(
            &keys,
            Some(Fix::Spelling {
                text: after.into(),
                lang: Language::English,
            }),
        )
        .unwrap();
        assert_eq!(rep.erase, erase, "{before}");
        let undo = undo_of::<Simulated>(&keys, &rep, Some(' '));
        let prefix: String = before.chars().take(keys.len() - rep.erase).collect();
        assert_eq!(format!("{prefix}{}", rep.retype), after);
        assert_eq!(
            format!(
                "{prefix}{}",
                Simulated::retype_original(&undo.restore, undo.restore_layout)
            ),
            before
        );
        assert_eq!(undo.suppress.as_deref(), Some(before.trim_end_matches(',')));
        assert_eq!(undo.on_screen, rep.retype.chars().count() + 1);
    }
}

#[test]
fn hebrew_spelling_preserves_prefix_language_and_undo() {
    let before = "שלומ";
    let keys: Vec<_> = crate::keymap::convert_selection(before)
        .chars()
        .map(|key| Typed { key, shift: false })
        .collect();
    let fix = Fix::Spelling {
        text: "שלום".into(),
        lang: Language::Hebrew,
    };
    assert_eq!(note_of::<Simulated>(&keys, &fix).0, before);
    let rep = replacement::<Simulated>(&keys, Some(fix)).unwrap();
    assert_eq!(rep.erase, 1);
    assert_eq!(rep.retype, "ם");
    let undo = undo_of::<Simulated>(&keys, &rep, Some(' '));
    assert_eq!(
        Simulated::retype_original(&undo.restore, undo.restore_layout),
        "מ"
    );
    assert_eq!(undo.on_screen, 2);
    assert_eq!(undo.visible_layout, Language::Hebrew);
    assert_eq!(undo.layout, None);
    assert_eq!(undo.suppress.as_deref(), Some(before));
}

#[test]
fn combined_hebrew_repair_and_undo_use_target_language() {
    let keys: Vec<_> = "akuf."
        .chars()
        .map(|key| Typed { key, shift: false })
        .collect();
    let fix = Fix::LayoutSpelling {
        text: "שלום.".into(),
        lang: Language::Hebrew,
    };
    let note = note_of::<Simulated>(&keys, &fix);
    assert_eq!((note.0.as_str(), note.1.as_str()), ("akuf.", "שלום."));
    let rep = replacement::<Simulated>(&keys, Some(fix)).unwrap();
    assert_eq!(rep.erase, 5);
    assert_eq!(rep.retype, "שלום.");
    let undo = undo_of::<Simulated>(&keys, &rep, Some(' '));
    assert_eq!(
        Simulated::retype_original(&undo.restore, undo.restore_layout),
        "akuf."
    );
    assert_eq!(undo.layout, Some(Language::English));
    assert_eq!(undo.visible_layout, Language::Hebrew);
    assert_eq!(undo.on_screen, 6);
    #[cfg(target_os = "linux")]
    {
        use crate::platform::linux::Linux;
        let keys: Vec<_> = "akuf"
            .chars()
            .map(|c| Typed {
                key: crate::keymap::english_char_to_evkey_shifted(c).unwrap().0,
                shift: false,
            })
            .collect();
        let rep = replacement::<Linux>(
            &keys,
            Some(Fix::LayoutSpelling {
                text: "שלום".into(),
                lang: Language::Hebrew,
            }),
        )
        .unwrap();
        let visible: String = rep
            .retype
            .iter()
            .filter_map(|(key, _)| crate::keymap::evkey_to_hebrew_char(*key))
            .collect();
        assert_eq!(visible, "שלום");
    }
}

#[test]
fn retained_bilingual_repairs_keep_their_layout_and_undo() {
    const CHILD: &str = "RECAST_BILINGUAL_REPAIR_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "platform::engine::tests::retained_bilingual_repairs_keep_their_layout_and_undo",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    crate::config::Config::update_live(|cfg| cfg.personal_enabled = true);
    ALLOW_LAYOUT_SWITCH.store(true, Ordering::SeqCst);
    for _ in 0..3 {
        SIMULATED_LAYOUT.store(0, Ordering::SeqCst);
        let s = Session::new();
        s.type_text("akuo ");
        s.pending();
        s.finish();
        assert_eq!(s.text(), "שלום ");
        assert_eq!(Simulated::current_layout(), Some(Language::Hebrew));
        s.type_text("x");
    }
    assert_eq!(
        crate::personal::personal_correction("akuo").as_deref(),
        Some("שלום")
    );

    // Repeated Hebrew spelling corrections stay in Hebrew, including the
    // learned proposal. Undo restores the entire original word and language.
    ALLOW_LAYOUT_SWITCH.store(false, Ordering::SeqCst);
    for iteration in 0..3 {
        let s = Session::new();
        s.type_text(&format!("{} ", crate::keymap::convert_selection("תודדה")));
        s.pending();
        s.finish();
        assert_eq!(s.text(), "תודה ");
        assert_eq!(Simulated::current_layout(), Some(Language::Hebrew));
        if iteration < 2 {
            s.type_text("x");
        } else {
            assert!(s.engine.control.history()[0]
                .details
                .contains("repeatedly retained"));
            s.undo();
            s.pending();
            s.finish();
            assert_eq!(s.text(), "תודדה ");
            assert_eq!(Simulated::current_layout(), Some(Language::Hebrew));
            assert_eq!(crate::personal::personal_correction("תודדה"), None);
        }
    }

    // Combined Hebrew repairs train one pair and restore the original layout on undo.
    ALLOW_LAYOUT_SWITCH.store(true, Ordering::SeqCst);
    for iteration in 0..3 {
        SIMULATED_LAYOUT.store(0, Ordering::SeqCst);
        let s = Session::new();
        s.type_text("thbyrbhy ");
        s.pending();
        s.finish();
        assert_eq!(s.text(), "אינטרנט ");
        assert_eq!(Simulated::current_layout(), Some(Language::Hebrew));
        if iteration < 2 {
            s.type_text("x");
        } else {
            assert!(s.engine.control.history()[0]
                .details
                .contains("takes precedence over contextual intent"));
            assert_eq!(
                crate::personal::personal_correction("thbyrbhy").as_deref(),
                Some("אינטרנט")
            );
            s.undo();
            s.pending();
            s.finish();
            assert_eq!(s.text(), "thbyrbhy ");
            assert_eq!(Simulated::current_layout(), Some(Language::English));
            assert_eq!(crate::personal::personal_correction("thbyrbhy"), None);
        }
    }
}

#[test]
fn bilingual_offers_context_and_feedback() {
    const CHILD: &str = "RECAST_BILINGUAL_OFFERS_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "platform::engine::tests::bilingual_offers_context_and_feedback",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    crate::config::Config::update_live(|cfg| {
        cfg.rule_stats_enabled = true;
        cfg.personal_enabled = false;
    });
    let before = crate::personal::usefulness_counts();
    let s = Session::new();
    s.type_text("keyb");
    s.tap(Simulated::SHIFT_RIGHT);
    s.pending();
    s.finish();
    s.type_text(" ");
    let counts = crate::personal::usefulness_counts();
    assert_eq!(counts[0] - before[0], 1);
    assert_eq!(counts[2] - before[2], 1);
    assert_eq!(counts[3] - before[3], 1);
    let s = Session::new();
    s.type_text("keyb");
    s.tap(Simulated::SHIFT_RIGHT);
    s.pending();
    s.finish();
    s.tap(Simulated::SHIFT_RIGHT);
    s.pending();
    s.finish();
    s.type_text(" ");
    let counts = crate::personal::usefulness_counts();
    assert_eq!(counts[0] - before[0], 2);
    assert_eq!(counts[1] - before[1], 1);
    assert_eq!(counts[2] - before[2], 2);
    assert_eq!(counts[3] - before[3], 1);
    let s = Session::new();
    s.type_text("keyb");
    s.tap(Simulated::SHIFT_RIGHT);
    s.pending();
    s.finish();
    s.engine.mouse_click();
    s.engine.mouse_click();
    assert_eq!(crate::personal::usefulness_counts()[4] - before[4], 1);

    // Public phrase priors work with personal learning disabled.
    let s = Session::new();
    s.type_text("good ");
    assert_eq!(s.engine.lock().phrase.boost("morning"), 4.0);
    s.engine.mouse_click();
    assert_eq!(s.engine.lock().phrase.boost("morning"), 1.0);

    // Automatic phrase disambiguation uses the previous visible word, even
    // with personalization off. The embedded dictionary leaves baye tied.
    let s = Session::new();
    s.type_text("data baye ");
    s.pending();
    s.finish();
    assert_eq!(s.text(), "data base ");
    let recent = s.engine.control.history();
    assert!(recent[0].details.contains("Phrase context reconsidered"));
    assert!(recent[0].details.contains("Previous word: \"data\""));
    for reset in ["enter", "click", "focus"] {
        let s = Session::new();
        s.type_text("data ");
        match reset {
            "enter" => s.type_text("\n"),
            "click" => s.engine.mouse_click(),
            _ => FOCUS.store(2, Ordering::SeqCst),
        }
        s.type_text("baye ");
        assert!(s.text().ends_with("baye "), "{reset}: {}", s.text());
        assert!(!s.engine.lock().is_replacing);
        FOCUS.store(1, Ordering::SeqCst);
    }
    let s = Session::new();
    s.type_text("data baye ");
    s.pending();
    s.finish();
    s.undo();
    s.pending();
    s.finish();
    assert_eq!(s.text(), "data baye ");
    assert_eq!(s.engine.lock().phrase.boost("base"), 1.0);

    // Spelling offers are explicit, cycleable, and retained on a terminator.
    let s = Session::new();
    s.type_text("recieve");
    assert_eq!(s.text(), "recieve");
    s.tap(Simulated::SHIFT_RIGHT);
    s.pending();
    s.finish();
    assert_eq!(s.text(), "receive");
    s.type_text(" ");
    assert_eq!(s.text(), "receive ");
    assert!(!s.engine.lock().is_replacing);

    ALLOW_LAYOUT_SWITCH.store(true, Ordering::SeqCst);
    for (source, typed, expected_prefix) in [(1, "compu", "compu"), (0, "njac", "מחשב")] {
        SIMULATED_LAYOUT.store(source, Ordering::SeqCst);
        let s = Session::new();
        s.type_text(typed);
        let original = s.text();
        s.tap(Simulated::SHIFT_RIGHT);
        s.pending();
        s.finish();
        assert!(s.text().starts_with(expected_prefix), "{}", s.text());
        assert_eq!(SIMULATED_LAYOUT.load(Ordering::SeqCst), 1 - source);
        let count = s
            .engine
            .lock()
            .cycle
            .as_ref()
            .unwrap()
            .candidates
            .len()
            .min(crate::complete::MAX_CANDIDATES);
        for _ in 0..count {
            s.tap(Simulated::SHIFT_RIGHT);
            s.pending();
            s.finish();
        }
        assert_eq!(s.text(), original);
        assert_eq!(SIMULATED_LAYOUT.load(Ordering::SeqCst), source);
        s.tap(Simulated::SHIFT_RIGHT);
        s.pending();
        s.finish();
        s.undo();
        s.pending();
        s.finish();
        assert_eq!(s.text(), original);
        assert_eq!(SIMULATED_LAYOUT.load(Ordering::SeqCst), source);
    }
    SIMULATED_LAYOUT.store(1, Ordering::SeqCst);
    REFUSE_LAYOUT_SWITCH.store(true, Ordering::SeqCst);
    let s = Session::new();
    s.type_text("compu");
    let original = s.text();
    s.tap(Simulated::SHIFT_RIGHT);
    let deadline = Instant::now() + Duration::from_secs(3);
    while s.engine.lock().is_replacing {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(s.text(), original);
    assert!(s.ready.try_recv().is_err());
    REFUSE_LAYOUT_SWITCH.store(false, Ordering::SeqCst);

    // Focus changes while waiting for the OS must cancel text replacement.
    let s = Session::new();
    s.type_text("compu");
    let original = s.text();
    let (ready, waiting) = mpsc::sync_channel(1);
    let (proceed, reply) = mpsc::sync_channel(1);
    *LAYOUT_GATE.lock().unwrap() = Some(FocusGate {
        ready,
        proceed: reply,
    });
    s.tap(Simulated::SHIFT_RIGHT);
    waiting.recv_timeout(Duration::from_secs(3)).unwrap();
    s.engine.mouse_click();
    proceed.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while s.engine.lock().is_replacing {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(s.text(), original);
    assert!(s.ready.try_recv().is_err());
    SIMULATED_LAYOUT.store(0, Ordering::SeqCst);
    ALLOW_LAYOUT_SWITCH.store(false, Ordering::SeqCst);

    // Repeated retained slips train a bounded class; rejection withdraws evidence.
    crate::config::Config::update_live(|cfg| cfg.personal_enabled = true);
    let before = crate::personal::typo_counts()[0];
    crate::personal::record_confusion("watre", "water");
    assert_eq!(crate::personal::typo_counts()[0], before);
    crate::personal::record_confusion("watre", "water");
    assert_eq!(crate::personal::typo_counts()[0], before + 2);
    crate::personal::reject_confusion("watre", "water");
    assert_eq!(crate::personal::typo_counts()[0], before);
    let english_counts = crate::personal::typo_counts();
    let hebrew_counts = crate::personal::hebrew_typo_counts();
    let dict = crate::dictionary::Dict::of(&["מחשב"]);
    let freq = crate::dictionary::Freq::of(&[("מחשב", 100)]);
    let before_score = crate::spell::hebrew::assess("מחבש", dict, freq, None)
        .best
        .unwrap()
        .score;
    crate::personal::record_confusion("מחבש", "מחשב");
    assert_eq!(crate::personal::hebrew_typo_counts(), hebrew_counts);
    crate::personal::record_confusion("מחבש", "מחשב");
    assert_eq!(
        crate::personal::hebrew_typo_counts()[0],
        hebrew_counts[0] + 2
    );
    assert_eq!(
        crate::personal::typo_counts(),
        english_counts,
        "Hebrew votes must not alter the English channel"
    );
    let trained = crate::spell::hebrew::assess("מחבש", dict, freq, None);
    assert!((before_score - trained.best.unwrap().score - 4.0).abs() < 0.001);
    assert!(trained.details.contains("learned edit discount 4.00"));
    crate::personal::reject_confusion("מחבש", "מחשב");
    assert_eq!(crate::personal::hebrew_typo_counts(), hebrew_counts);
    assert!(
        (crate::spell::hebrew::assess("מחבש", dict, freq, None)
            .best
            .unwrap()
            .score
            - before_score)
            .abs()
            < 0.001
    );
    for _ in 0..2 {
        crate::personal::record_confusion("recieve", "receive");
    }
    let s = Session::new();
    s.type_text("recieve ");
    s.pending();
    s.finish();
    s.undo();
    s.pending();
    s.finish();
    assert_eq!(s.text(), "recieve ");
    assert_eq!(crate::personal::personal_correction("recieve"), None);
    let counts = crate::personal::usefulness_counts();
    assert!(counts[5] > 0 && counts[6] > 0);

    crate::config::Config::update_live(|cfg| {
        cfg.personal_enabled = false;
        cfg.rule_stats_enabled = false;
    });
    assert_eq!(crate::personal::typo_counts(), [0; 3]);
    assert_eq!(crate::personal::hebrew_typo_counts(), [0; 3]);
    let s = Session::new();
    s.type_text("keyb");
    s.tap(Simulated::SHIFT_RIGHT);
    s.pending();
    s.finish();
    s.type_text(" ");
    assert_eq!(crate::personal::usefulness_counts(), counts);
}

#[test]
fn explicit_feedback_requires_acceptance_and_trusted_focus() {
    const CHILD: &str = "RECAST_EXPLICIT_FEEDBACK_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "platform::engine::tests::explicit_feedback_requires_acceptance_and_trusted_focus",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    crate::config::Config::update_live(|config| {
        config.personal_enabled = true;
        config.spell_enabled = false;
        config.complete_enabled = false;
        config.correction_shortcut = "left_shift".into();
    });

    fn offer(original: &str, wanted: &str) -> Session {
        let session = Session::new();
        session.type_text(original);
        assert!(!session.engine.lock().is_replacing);
        session.tap(Simulated::SHIFT_LEFT);
        session.pending();
        let index = session
            .engine
            .lock()
            .cycle
            .as_ref()
            .unwrap()
            .candidates
            .iter()
            .position(|word| word == wanted)
            .expect("explicit correction missing");
        session.finish();
        for _ in 0..index {
            session.tap(Simulated::SHIFT_LEFT);
            session.pending();
            session.finish();
        }
        assert_eq!(session.text().trim_end(), wanted);
        session
    }

    // Neither a successful offer nor later typing in another field accepts it.
    for (original, wanted) in [("hte", "the"), ("watre ", "water")] {
        let session = offer(original, wanted);
        assert_eq!(crate::personal::personal_correction(original.trim()), None);
        assert_eq!(crate::personal::personal_count(wanted), None);
        FOCUS.store(2, Ordering::SeqCst);
        session.type_text("x");
        assert_eq!(crate::personal::personal_correction(original.trim()), None);
        assert_eq!(crate::personal::personal_count(wanted), None);
        assert!(session.engine.lock().pending_learning.is_none());
        FOCUS.store(1, Ordering::SeqCst);
    }

    let session = offer("hte", "the");
    session.undo();
    session.pending();
    session.finish();
    assert_eq!(crate::personal::personal_correction("hte"), None);
    assert_eq!(crate::personal::personal_count("the"), None);
    crate::complete::unlist("hte");

    // One explicit acceptance trains a pair; word counts still count occurrences.
    let session = offer("hte", "the");
    session.type_text(" ");
    assert_eq!(
        crate::personal::personal_correction("hte").as_deref(),
        Some("the")
    );
    assert_eq!(crate::personal::personal_count("the"), Some(1));
    session.undo();
    session.pending();
    session.finish();
    assert_eq!(crate::personal::personal_correction("hte"), None);
    assert_eq!(crate::personal::personal_count("the"), Some(1));

    // A choice made after Space is accepted by advancing to the next word.
    let session = offer("watre ", "water");
    session.type_text("x");
    assert_eq!(
        crate::personal::personal_correction("watre").as_deref(),
        Some("water")
    );
    assert_eq!(crate::personal::personal_count("water"), Some(1));
}

#[test]
fn completions_and_learning_require_retained_choices() {
    const CHILD: &str = "RECAST_COMPLETION_LEARNING_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "platform::engine::tests::completions_and_learning_require_retained_choices",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    crate::config::Config::update_live(|cfg| cfg.personal_enabled = true);
    let s = Session::new();
    s.type_text("recieve ");
    s.pending();
    s.finish();
    assert_eq!(crate::personal::personal_count("receive"), None);
    s.undo();
    s.pending();
    s.finish();
    assert_eq!(s.text(), "recieve ");
    assert_eq!(crate::personal::personal_count("receive"), None);
    assert_eq!(crate::personal::personal_correction("recieve"), None);
    assert!(s.engine.lock().pending_learning.is_none());
    crate::complete::unlist("recieve");

    for count in 1..=2 {
        let s = Session::new();
        s.type_text("recieve ");
        s.pending();
        s.finish();
        assert_eq!(
            crate::personal::personal_count("receive"),
            (count > 1).then_some(count - 1)
        );
        s.type_text("x");
        assert_eq!(crate::personal::personal_count("receive"), Some(count));
    }
    assert_eq!(
        crate::personal::personal_correction("recieve").as_deref(),
        Some("receive")
    );
    crate::complete::set_word_ignored("recieve", true).unwrap();
    let s = Session::new();
    s.type_text("recieve ");
    assert_eq!(s.text(), "recieve ");
    assert!(!s.engine.lock().is_replacing);
    crate::complete::unlist("recieve");
    let s = Session::new();
    s.type_text("recieve ");
    s.pending();
    s.finish();
    FOCUS.store(2, Ordering::SeqCst);
    s.type_text("x");
    assert_eq!(crate::personal::personal_count("receive"), Some(2));
    FOCUS.store(1, Ordering::SeqCst);

    let s = Session::new();
    s.type_text("keyb");
    s.tap(Simulated::SHIFT_RIGHT);
    s.pending();
    s.finish();
    let first = s.text();
    assert_eq!(crate::personal::personal_count(&first), None);
    let count = s
        .engine
        .lock()
        .cycle
        .as_ref()
        .unwrap()
        .candidates
        .len()
        .min(crate::complete::MAX_CANDIDATES);
    for _ in 0..count {
        s.tap(Simulated::SHIFT_RIGHT);
        s.pending();
        s.finish();
    }
    assert_eq!(s.text(), "keyb");
    assert_eq!(crate::personal::personal_count(&first), None);
    assert!(s.engine.lock().pending_learning.is_none());
    s.tap(Simulated::SHIFT_RIGHT);
    s.pending();
    s.finish();
    s.type_text(" ");
    assert_eq!(
        crate::personal::personal_count(s.text().trim_end()),
        Some(1)
    );
    assert_eq!(crate::personal::personal_correction("keyb"), None);
    assert!(!s.engine.lock().is_replacing);

    // Explicit vocabulary is available without a dictionary or frequency entry.
    crate::complete::set_word_ignored("qzcustomname", true).unwrap();
    for count in 1..=2 {
        let s = Session::new();
        s.type_text("qzc");
        s.tap(Simulated::SHIFT_RIGHT);
        s.pending();
        s.finish();
        assert_eq!(s.text(), "qzcustomname");
        s.type_text(" ");
        assert_eq!(s.text(), "qzcustomname ");
        assert_eq!(crate::personal::personal_count("qzcustomname"), Some(count));
    }
    crate::complete::unlist("qzcustomname");
    assert!(crate::personal::completion_words().contains(&"qzcustomname".into()));
    let s = Session::new();
    s.type_text("qzc");
    s.tap(Simulated::SHIFT_RIGHT);
    s.pending();
    s.finish();
    assert_eq!(s.text(), "qzcustomname");
    s.undo();
    s.pending();
    s.finish();
    assert_eq!(s.text(), "qzc");
    assert_eq!(crate::personal::personal_count("qzcustomname"), Some(2));

    // A prefix edit must also be restored exactly by completion undo.
    let s = Session::new();
    s.type_text("keyob");
    s.tap(Simulated::SHIFT_RIGHT);
    s.pending();
    s.finish();
    assert!(s
        .engine
        .lock()
        .cycle
        .as_ref()
        .unwrap()
        .candidates
        .iter()
        .any(|word| word == "keyboard"));
    s.undo();
    s.pending();
    s.finish();
    assert_eq!(s.text(), "keyob");
    assert_eq!(crate::personal::personal_correction("keyob"), None);

    // Hebrew follows the same cycle, character counts, buffer and undo contract.
    SIMULATED_LAYOUT.store(1, Ordering::SeqCst);
    let s = Session::new();
    s.type_text("aku");
    assert_eq!(s.text(), "שלו");
    s.tap(Simulated::SHIFT_RIGHT);
    s.pending();
    s.finish();
    assert!(s.text().starts_with("שלו") && s.text().chars().count() > 3);
    assert_eq!(*s.engine.injector.erasures.lock().unwrap(), [0]);
    assert_eq!(
        reading::<Simulated>(&s.engine.lock().keys, Language::Hebrew),
        s.text()
    );
    let count = s
        .engine
        .lock()
        .cycle
        .as_ref()
        .unwrap()
        .candidates
        .len()
        .min(crate::complete::MAX_CANDIDATES);
    for _ in 0..count {
        s.tap(Simulated::SHIFT_RIGHT);
        s.pending();
        s.finish();
    }
    assert_eq!(s.text(), "שלו");
    s.tap(Simulated::SHIFT_RIGHT);
    s.pending();
    s.finish();
    s.undo();
    s.pending();
    s.finish();
    assert_eq!(s.text(), "שלו");
    s.tap(Simulated::SHIFT_RIGHT);
    s.pending();
    s.finish();
    let accepted = s.text();
    s.type_text(" ");
    assert_eq!(s.text(), format!("{accepted} "));
    assert!(!s.engine.lock().is_replacing);
    assert_eq!(crate::personal::personal_count(&accepted), Some(1));

    let s = Session::new();
    s.type_text("aku");
    s.tap(Simulated::SHIFT_RIGHT);
    s.pending();
    SIMULATED_LAYOUT.store(0, Ordering::SeqCst);
    s.finish();
    assert_eq!(
        s.text(),
        "שלו",
        "layout changes must cancel an in-flight completion"
    );
    assert!(s.engine.lock().pending_learning.is_none());

    // Offers alone never add phrase evidence, even after successful injection.
    let s = Session::new();
    s.type_text("my ");
    for _ in 0..2 {
        s.type_text("keyb");
        s.tap(Simulated::SHIFT_RIGHT);
        s.pending();
        s.finish();
        let offered = reading::<Simulated>(&s.engine.lock().keys, Language::English);
        assert_eq!(s.engine.lock().phrase.boost(&offered), 1.0);
        s.type_text(" my ");
    }
    s.type_text("keyb");
    s.tap(Simulated::SHIFT_RIGHT);
    s.pending();
    s.finish();
    let offered = reading::<Simulated>(&s.engine.lock().keys, Language::English);
    assert!(s.engine.lock().phrase.boost(&offered) > 1.0);
    s.undo();
    s.pending();
    s.finish();
    assert_eq!(s.engine.lock().phrase.boost(&offered), 1.0);

    // Phrase evidence is learned only from finished, retained choices.
    let s = Session::new();
    s.type_text("my keyboard my keyboard my ");
    assert!(s.engine.lock().phrase.boost("keyboard") > 1.0);
    s.engine.mouse_click();
    assert_eq!(s.engine.lock().phrase.boost("keyboard"), 1.0);
    let s = Session::new();
    s.type_text("my keyboard my keyboard my ");
    FOCUS.store(2, Ordering::SeqCst);
    s.type_text("k");
    assert_eq!(s.engine.lock().phrase.boost("keyboard"), 1.0);
    FOCUS.store(1, Ordering::SeqCst);
    let s = Session::new();
    s.type_text("my keyboard my keyboard my ");
    s.type_text("\nmy ");
    assert_eq!(s.engine.lock().phrase.boost("keyboard"), 1.0);
    let s = Session::new();
    s.type_text("my keyboard my keyboard my ");
    s.tap(Simulated::BACKSPACE);
    assert_eq!(s.engine.lock().phrase.boost("keyboard"), 1.0);
    let s = Session::new();
    s.type_text("my keyboard my keyboard my ");
    crate::config::Config::update_live(|cfg| cfg.personal_enabled = false);
    s.type_text("k");
    assert_eq!(s.engine.lock().phrase.boost("keyboard"), 1.0);

    // Saved exceptions remain completion vocabulary when personalization is off.
    crate::config::Config::update_live(|cfg| cfg.personal_enabled = false);
    crate::complete::suppress("qzlearnedname");
    crate::complete::seed_legacy_undo("qzlearnedname");
    crate::complete::seed_legacy_undo("qzlearnedname");
    let s = Session::new();
    s.type_text("qzl");
    s.tap(Simulated::SHIFT_RIGHT);
    s.pending();
    s.finish();
    assert_eq!(s.text(), "qzlearnedname");
    assert_eq!(crate::personal::personal_count("qzlearnedname"), None);

    // A configured expansion cannot appear again as a dictionary offer.
    let path = crate::complete::user_path("abbrev.txt").unwrap();
    std::fs::write(&path, "keyb = keyboard\n").unwrap();
    crate::complete::reload_user_files();
    let words = crate::complete::completions(
        "keyb",
        crate::dictionary::en_dict(),
        crate::dictionary::en_freq(),
    );
    assert_eq!(words.iter().filter(|w| *w == "keyboard").count(), 1);
}

#[test]
fn continued_typing_during_automatic_search_keeps_the_correction() {
    const CHILD: &str = "RECAST_CONTINUED_SEARCH_TEST";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "platform::engine::tests::continued_typing_during_automatic_search_keeps_the_correction",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    for (layout, typo, corrected) in [(0, "recieve", "receive"), (1, "תודדה", "תודה")] {
        SIMULATED_LAYOUT.store(layout, Ordering::SeqCst);
        let physical = |text: &str| {
            if layout == 1 {
                crate::keymap::convert_selection(text)
            } else {
                text.to_owned()
            }
        };
        for phase in ["search", "focus"] {
            for suffix in if layout == 0 {
                vec![
                    "next".to_owned(),
                    "  next".into(),
                    "  ".into(),
                    " ".repeat(32),
                    "keyboad".into(),
                    "x".repeat(32),
                ]
            } else {
                vec!["שלום".to_owned(), "  עולם".into()]
            } {
                let s = Session::new();
                s.type_text(&physical(typo));
                let (ready, reached) = mpsc::sync_channel(0);
                let (proceed, resume) = mpsc::channel();
                *CHECK_GATE.lock().unwrap() = Some(FocusGate {
                    ready,
                    proceed: resume,
                });
                let engine = Arc::clone(&s.engine);
                let worker = thread::spawn(move || engine.key_press(' '));
                reached.recv_timeout(Duration::from_secs(3)).unwrap();
                worker.join().unwrap();
                s.engine.injector.text.lock().unwrap().push(' ');
                s.engine.key_release(' ');
                let focus_gate = if phase == "focus" {
                    let (ready, reached) = mpsc::sync_channel(0);
                    let (proceed_focus, resume) = mpsc::channel();
                    *FOCUS_GATE.lock().unwrap() = Some(FocusGate {
                        ready,
                        proceed: resume,
                    });
                    proceed.send(()).unwrap();
                    reached.recv_timeout(Duration::from_secs(3)).unwrap();
                    Some(proceed_focus)
                } else {
                    None
                };
                s.type_while_checking(&physical(&suffix));
                if let Some(focus_gate) = focus_gate {
                    focus_gate.send(()).unwrap();
                } else {
                    proceed.send(()).unwrap();
                }
                s.pending();
                s.finish();
                assert_eq!(s.text(), format!("{corrected} {suffix}"), "{phase}");
                assert_eq!(
                    reading::<Simulated>(
                        &s.engine.lock().keys,
                        Simulated::current_layout().unwrap()
                    ),
                    suffix.trim_start_matches(' ')
                );
                assert_eq!(s.engine.control.fixed_count(), 1);
                if suffix == "keyboad" {
                    s.type_text(" ");
                    s.pending();
                    s.finish();
                    assert_eq!(s.text(), "receive keyboard ");
                } else {
                    s.undo();
                    s.pending();
                    s.finish();
                    assert_eq!(s.text(), format!("{typo} {suffix}"));
                    crate::complete::unlist(typo);
                }
            }
        }
    }
    SIMULATED_LAYOUT.store(0, Ordering::SeqCst);
}

#[test]
fn finished_word_help_survives_correction_undo_expiry() {
    const CHILD: &str = "RECAST_EXPIRED_UNDO_HELP_TEST";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "platform::engine::tests::finished_word_help_survives_correction_undo_expiry",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    ALLOW_LAYOUT_SWITCH.store(true, Ordering::SeqCst);
    for (layout, typo, corrected, prefix) in [
        (0, "recieve", "receive", "please update the documentation "),
        (1, "תודדה", "תודה", "בבקשה לעדכן את המסמך ולשלוח לי "),
    ] {
        let physical = |text: &str| {
            if layout == 1 {
                crate::keymap::convert_selection(text)
            } else {
                text.to_owned()
            }
        };
        for action in ["completion", "spelling", "convert"] {
            let (word, wanted, gesture) = match (layout, action) {
                (0, "completion") => ("keyb", "keyboard", Simulated::SHIFT_RIGHT),
                (0, "spelling") => ("keyboad", "keyboard", Simulated::SHIFT_LEFT),
                (0, _) => ("akuo", "שלום", Simulated::CTRL_LEFT),
                (_, "completion") => ("מחש", "מחשב", Simulated::SHIFT_RIGHT),
                (_, "spelling") => ("תודדה", "תודה", Simulated::SHIFT_LEFT),
                _ => ("יקךךם", "hello", Simulated::CTRL_LEFT),
            };
            let prefixes = if layout == 0 && action == "completion" {
                vec![
                    prefix.to_owned(),
                    format!("{} ", "x".repeat(26)),
                    format!("{} ", "x".repeat(27)),
                ]
            } else {
                vec![prefix.to_owned()]
            };
            for prefix in prefixes {
                for spaces in [1, 3] {
                    SIMULATED_LAYOUT.store(layout, Ordering::SeqCst);
                    let s = Session::new();
                    s.type_text(&physical(&format!("{typo} ")));
                    s.pending();
                    let separators = " ".repeat(spaces);
                    let later = format!("{prefix}{word}{separators}");
                    assert!(later.chars().count() <= crate::types::MAX_WORD_KEYS);
                    let expired = later.chars().count() > super::word_buffer::MAX_UNDO_SUFFIX;
                    s.type_text(&physical(&later));
                    s.finish();
                    assert_eq!(s.text(), format!("{corrected} {later}"));
                    if expired {
                        assert!(matches!(
                            s.engine.lock().last_action,
                            Some(LastAction::Unchanged { .. })
                        ));
                        s.undo();
                        assert!(
                            s.ready.try_recv().is_err(),
                            "undo-only must not convert unchanged text"
                        );
                    } else {
                        assert!(matches!(
                            s.engine.lock().last_action,
                            Some(LastAction::Fixed(_))
                        ));
                    }
                    s.tap(gesture);
                    if action == "convert" {
                        s.tap(gesture);
                    }
                    s.pending();
                    s.finish();
                    assert_eq!(
                        s.text(),
                        format!("{corrected} {prefix}{wanted}{separators}"),
                        "{action}"
                    );
                    s.undo();
                    s.pending();
                    s.finish();
                    assert_eq!(s.text(), format!("{corrected} {later}"));
                    if action == "convert" {
                        assert!(!crate::complete::suppressed(word));
                    }
                    crate::complete::unlist(word);
                }
            }
        }
    }
    SIMULATED_LAYOUT.store(0, Ordering::SeqCst);
    // An undo queued before the correction lands must not consume an unchanged-word action.
    let s = Session::new();
    s.type_text("recieve ");
    s.pending();
    s.type_text("please update the documentation keyb ");
    s.undo();
    assert!(s.engine.lock().pending_undo);
    s.finish();
    assert_eq!(s.engine.control.undo_count(), 0);
    assert!(matches!(
        s.engine.lock().last_action,
        Some(LastAction::Unchanged { .. })
    ));
    s.tap(Simulated::SHIFT_RIGHT);
    s.pending();
    s.finish();
    assert_eq!(
        s.text(),
        "receive please update the documentation keyboard "
    );
    std::env::set_var("RECAST_PREVIEW_TEST", "1");
    let s = Session::new();
    s.type_text("recieve ");
    s.pending();
    s.type_text("please update the documentation keyb ");
    s.finish();
    s.tap(Simulated::SHIFT_RIGHT);
    assert!(s
        .engine
        .lock()
        .cycle
        .as_ref()
        .is_some_and(|cycle| cycle.preview));
    assert_eq!(s.text(), "receive please update the documentation keyb ");
    s.type_text(" ");
    s.pending();
    s.finish();
    assert_eq!(
        s.text(),
        "receive please update the documentation keyboard  "
    );
}

#[test]
fn expired_undo_word_help_stays_with_the_caret_and_focus() {
    const CHILD: &str = "RECAST_EXPIRED_UNDO_SCOPE_TEST";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "platform::engine::tests::expired_undo_word_help_stays_with_the_caret_and_focus",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    for cancel in [
        "typing",
        "enter",
        "navigation",
        "focus",
        "secure",
        "excluded",
    ] {
        let s = Session::new();
        s.type_text("recieve ");
        s.pending();
        s.type_text("please update the documentation keyb ");
        s.finish();
        assert!(matches!(
            s.engine.lock().last_action,
            Some(LastAction::Unchanged { .. })
        ));
        match cancel {
            "typing" => s.type_text("x"),
            "enter" => s.type_text("\n"),
            "navigation" => s.tap('\x10'),
            "focus" => FOCUS.store(2, Ordering::SeqCst),
            "secure" => INPUT_ALLOWED.store(false, Ordering::SeqCst),
            _ => *lock_forgiving(&s.engine.control.excluded_apps) = vec!["editor".into()],
        }
        let expected = s.text();
        s.tap(Simulated::SHIFT_RIGHT);
        assert!(s.ready.try_recv().is_err(), "{cancel}");
        assert_eq!(s.text(), expected, "{cancel}");
        assert_eq!(s.engine.control.fixed_count(), 1);
        FOCUS.store(1, Ordering::SeqCst);
        INPUT_ALLOWED.store(true, Ordering::SeqCst);
    }
    ALLOW_LAYOUT_SWITCH.store(true, Ordering::SeqCst);
    let s = Session::new();
    s.type_text("recieve ");
    s.pending();
    s.type_text("please update the documentation keyb x");
    s.finish();
    assert!(s.engine.lock().last_action.is_none());
    s.tap(Simulated::CTRL_LEFT);
    s.tap(Simulated::CTRL_LEFT);
    s.pending();
    s.finish();
    assert_eq!(s.text(), "receive please update the documentation keyb ס");
}

#[test]
fn capitalizing_the_next_word_keeps_automatic_correction() {
    const CHILD: &str = "RECAST_CAPITAL_CONTINUATION_TEST";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "platform::engine::tests::capitalizing_the_next_word_keeps_automatic_correction",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    for phase in ["search", "focus"] {
        for shift in [Simulated::SHIFT_LEFT, Simulated::SHIFT_RIGHT] {
            for release_after_search in [false, true] {
                let s = Session::new();
                s.type_text("recieve");
                let (ready, reached) = mpsc::sync_channel(0);
                let (proceed, resume) = mpsc::channel();
                *CHECK_GATE.lock().unwrap() = Some(FocusGate {
                    ready,
                    proceed: resume,
                });
                s.type_while_checking(" ");
                reached.recv_timeout(Duration::from_secs(3)).unwrap();
                let focus_gate = if phase == "focus" {
                    let (ready, reached) = mpsc::sync_channel(0);
                    let (proceed_focus, resume) = mpsc::channel();
                    *FOCUS_GATE.lock().unwrap() = Some(FocusGate {
                        ready,
                        proceed: resume,
                    });
                    proceed.send(()).unwrap();
                    reached.recv_timeout(Duration::from_secs(3)).unwrap();
                    Some(proceed_focus)
                } else {
                    None
                };
                s.engine.key_press(shift);
                s.type_while_checking("n");
                if !release_after_search {
                    s.engine.key_release(shift);
                    s.type_while_checking("ext");
                }
                if let Some(focus_gate) = focus_gate {
                    focus_gate.send(()).unwrap();
                } else {
                    proceed.send(()).unwrap();
                }
                s.pending();
                if release_after_search {
                    s.engine.key_release(shift);
                }
                s.finish();
                if release_after_search {
                    s.type_text("ext");
                }
                assert_eq!(s.text(), "receive Next", "{phase}, {shift:?}");
                assert_eq!(
                    reading::<Simulated>(&s.engine.lock().keys, Language::English),
                    "Next"
                );
                assert!(
                    s.engine.lock().cycle.is_none(),
                    "capitalization must not request suggestions"
                );
                s.undo();
                s.pending();
                s.finish();
                assert_eq!(s.text(), "recieve Next");
                crate::complete::unlist("recieve");
            }
        }
    }
    // Releasing capitalization during the first letter's focus query is harmless too.
    let s = Session::new();
    s.engine.key_press(Simulated::SHIFT_LEFT);
    let (ready, reached) = mpsc::sync_channel(0);
    let (proceed, resume) = mpsc::channel();
    *FOCUS_GATE.lock().unwrap() = Some(FocusGate {
        ready,
        proceed: resume,
    });
    let engine = Arc::clone(&s.engine);
    let worker = thread::spawn(move || engine.key_press('r'));
    reached.recv_timeout(Duration::from_secs(3)).unwrap();
    s.engine.injector.text.lock().unwrap().push('R');
    s.engine.key_release('r');
    s.engine.key_release(Simulated::SHIFT_LEFT);
    proceed.send(()).unwrap();
    worker.join().unwrap();
    s.type_text("ecieve ");
    s.pending();
    s.finish();
    assert_eq!(s.text(), "Receive ");
}

#[test]
fn deliberate_shift_taps_still_cancel_automatic_search() {
    const CHILD: &str = "RECAST_SHIFT_SEARCH_PRIORITY_TEST";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "platform::engine::tests::deliberate_shift_taps_still_cancel_automatic_search",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    for preview in [false, true] {
        if preview {
            std::env::set_var("RECAST_PREVIEW_TEST", "1");
        } else {
            std::env::remove_var("RECAST_PREVIEW_TEST");
        }
        for binding in ["spelling", "completion", "action", "unbound"] {
            crate::config::Config::update_live(|config| {
                config.action_shortcut = if binding == "action" {
                    "left_shift"
                } else {
                    "ctrl"
                }
                .into();
                config.correction_shortcut = if binding == "spelling" {
                    "left_shift"
                } else {
                    "none"
                }
                .into();
                config.completion_shortcut = if binding == "completion" {
                    "left_shift"
                } else {
                    "none"
                }
                .into();
            });
            let s = Session::new();
            s.type_text("recieve");
            let (ready, reached) = mpsc::sync_channel(0);
            let (proceed, resume) = mpsc::channel();
            *CHECK_GATE.lock().unwrap() = Some(FocusGate {
                ready,
                proceed: resume,
            });
            s.type_while_checking(" ");
            reached.recv_timeout(Duration::from_secs(3)).unwrap();
            s.tap(Simulated::SHIFT_LEFT);
            if !preview && matches!(binding, "spelling" | "completion") {
                s.pending();
                s.finish();
            }
            let chosen = s.text();
            proceed.send(()).unwrap();
            s.checks_ready();
            if binding == "unbound" {
                s.pending();
                s.finish();
                assert_eq!(s.text(), "receive ");
            } else {
                assert_eq!(s.text(), chosen, "{binding}");
                assert!(
                    s.ready.try_recv().is_err(),
                    "stale automatic replacement after {binding}"
                );
                assert_eq!(
                    s.engine.control.fixed_count(),
                    u64::from(binding != "action" && !preview)
                );
            }
        }
    }
}

#[test]
fn replacement_replay_keeps_the_latest_finished_word_available() {
    const CHILD: &str = "RECAST_REPLAY_FINISHED_WORD_TEST";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "platform::engine::tests::replacement_replay_keeps_the_latest_finished_word_available"])
            .env(CHILD, "1").output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    EMPTY_INPUT_SUPPORTED.store(false, Ordering::SeqCst);
    for (layout, typo, corrected, next, completed) in [
        (0, "recieve", "receive", "keyb", "keyboard"),
        (1, "תודדה", "תודה", "מחש", "מחשב"),
    ] {
        SIMULATED_LAYOUT.store(layout, Ordering::SeqCst);
        let physical = |text: &str| {
            if layout == 1 {
                crate::keymap::convert_selection(text)
            } else {
                text.to_owned()
            }
        };
        for buffered_space_only in [false, true] {
            for spaces in [1, 3] {
                for action in ["complete", "backspace", "undo"] {
                    let s = Session::new();
                    if buffered_space_only {
                        s.type_text(&physical(typo));
                        let (ready, reached) = mpsc::sync_channel(0);
                        let (proceed, resume) = mpsc::channel();
                        *CHECK_GATE.lock().unwrap() = Some(FocusGate {
                            ready,
                            proceed: resume,
                        });
                        s.type_while_checking(" ");
                        reached.recv_timeout(Duration::from_secs(3)).unwrap();
                        s.type_while_checking(&physical(next));
                        proceed.send(()).unwrap();
                        s.pending();
                        s.type_text(&" ".repeat(spaces));
                    } else {
                        s.type_text(&physical(&format!("{typo} ")));
                        s.pending();
                        s.type_text(&physical(&format!("{next}{}", " ".repeat(spaces))));
                    }
                    s.finish();
                    assert_eq!(
                        s.text(),
                        format!("{corrected} {next}{}", " ".repeat(spaces))
                    );
                    if action == "undo" {
                        s.undo();
                        s.pending();
                        s.finish();
                        assert_eq!(s.text(), format!("{typo} {next}{}", " ".repeat(spaces)));
                        crate::complete::unlist(typo);
                        continue;
                    }
                    if action == "backspace" {
                        for _ in 0..spaces {
                            s.backspace();
                        }
                        assert_eq!(
                            reading::<Simulated>(
                                &s.engine.lock().keys,
                                Simulated::current_layout().unwrap()
                            ),
                            next
                        );
                        assert!(!s.engine.lock().no_fix);
                    }
                    s.tap(Simulated::SHIFT_RIGHT);
                    s.pending();
                    s.finish();
                    let separators = if action == "complete" {
                        " ".repeat(spaces)
                    } else {
                        String::new()
                    };
                    assert_eq!(s.text(), format!("{corrected} {completed}{separators}"));
                    s.undo();
                    s.pending();
                    s.finish();
                    assert_eq!(s.text(), format!("{corrected} {next}{separators}"));
                    crate::complete::unlist(next);
                }
            }
        }
    }
    SIMULATED_LAYOUT.store(0, Ordering::SeqCst);
    let s = Session::new();
    s.type_text("recieve ");
    s.pending();
    s.type_text("keyboad ");
    s.finish();
    s.tap(Simulated::SHIFT_LEFT);
    s.pending();
    s.finish();
    assert_eq!(s.text(), "receive keyboard ");
    // Space can finish a completion before its replacement has landed.
    for preview in [false, true] {
        if preview {
            std::env::set_var("RECAST_PREVIEW_TEST", "1");
        }
        let s = Session::new();
        s.type_text("keyb");
        s.tap(Simulated::SHIFT_RIGHT);
        if !preview {
            s.pending();
        }
        s.type_text(" ");
        if preview {
            s.pending();
        }
        s.finish();
        s.backspace();
        assert_eq!(s.text(), "keyboard");
        assert_eq!(
            reading::<Simulated>(&s.engine.lock().keys, Language::English),
            "keyboard"
        );
        assert!(!s.engine.lock().no_fix);
    }
    std::env::remove_var("RECAST_PREVIEW_TEST");
    ALLOW_LAYOUT_SWITCH.store(true, Ordering::SeqCst);
    let s = Session::new();
    s.type_text("akuo");
    s.tap(Simulated::CTRL_LEFT);
    s.tap(Simulated::CTRL_LEFT);
    s.pending();
    s.type_text(" ");
    s.finish();
    s.backspace();
    assert_eq!(s.text(), "שלום");
    assert_eq!(
        reading::<Simulated>(&s.engine.lock().keys, Language::Hebrew),
        "שלום"
    );
    assert!(!s.engine.lock().no_fix);
}

#[test]
fn replacement_replay_does_not_recover_unknown_or_overlong_fragments() {
    const CHILD: &str = "RECAST_REPLAY_WORD_SAFETY_TEST";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "platform::engine::tests::replacement_replay_does_not_recover_unknown_or_overlong_fragments"])
            .env(CHILD, "1").output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    EMPTY_INPUT_SUPPORTED.store(false, Ordering::SeqCst);
    for suffix in ["more ", "more keyb "] {
        let s = Session::new();
        s.type_text("recieve ");
        s.pending();
        s.finish();
        s.type_text("next");
        s.undo();
        s.pending();
        s.type_text(suffix);
        s.finish();
        assert_eq!(s.text(), format!("recieve next{suffix}"));
        if suffix == "more " {
            assert!(s.engine.lock().deletion_tail.is_none());
            s.backspace();
            assert!(s.engine.lock().keys.is_empty());
            assert!(s.engine.lock().no_fix);
        } else {
            s.backspace();
            assert_eq!(
                reading::<Simulated>(&s.engine.lock().keys, Language::English),
                "keyb"
            );
            s.tap(Simulated::SHIFT_RIGHT);
            s.pending();
            s.finish();
            assert_eq!(s.text(), "recieve nextmore keyboard");
        }
        crate::complete::unlist("recieve");
    }
    let s = Session::new();
    s.type_text("recieve");
    let (ready, reached) = mpsc::sync_channel(0);
    let (proceed, resume) = mpsc::channel();
    *CHECK_GATE.lock().unwrap() = Some(FocusGate {
        ready,
        proceed: resume,
    });
    s.type_while_checking(" ");
    reached.recv_timeout(Duration::from_secs(3)).unwrap();
    s.type_while_checking("next");
    proceed.send(()).unwrap();
    s.pending();
    let suffix = "x".repeat(crate::types::MAX_WORD_KEYS + 1 - "next".len());
    s.type_text(&format!("{suffix} "));
    s.finish();
    assert_eq!(s.text(), format!("receive next{suffix} "));
    assert!(s.engine.lock().deletion_tail.is_none());
    assert!(s.engine.lock().last_action.is_none());
    s.backspace();
    assert!(s.engine.lock().keys.is_empty());
    assert!(s.engine.lock().no_fix);
}

#[test]
fn verified_edits_preserve_typing_while_waiting_to_inject() {
    const CHILD: &str = "RECAST_VERIFIED_BUFFER_TEST";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "platform::engine::tests::verified_edits_preserve_typing_while_waiting_to_inject",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    for (layout, typo, corrected, next, later) in [
        (0, "recieve", "receive", "next", "more"),
        (1, "תודדה", "תודה", "שלום", "עולם"),
    ] {
        SIMULATED_LAYOUT.store(layout, Ordering::SeqCst);
        let physical = |text: &str| {
            if layout == 1 {
                crate::keymap::convert_selection(text)
            } else {
                text.to_owned()
            }
        };
        for action in ["correction", "undo"] {
            for separator in ["", "  "] {
                let s = Session::new();
                if action == "correction" {
                    s.type_text(&physical(typo));
                    let (ready, reached) = mpsc::sync_channel(0);
                    let (proceed, resume) = mpsc::channel();
                    *CHECK_GATE.lock().unwrap() = Some(FocusGate {
                        ready,
                        proceed: resume,
                    });
                    s.type_while_checking(" ");
                    reached.recv_timeout(Duration::from_secs(3)).unwrap();
                    s.type_while_checking(&physical(next));
                    proceed.send(()).unwrap();
                    s.pending();
                } else {
                    s.type_text(&physical(&format!("{typo} ")));
                    s.pending();
                    s.finish();
                    s.type_text(&physical(next));
                    s.undo();
                    s.pending();
                }
                s.type_text(&physical(&format!("{separator}{later}")));
                s.finish();
                assert_eq!(
                    s.text(),
                    format!(
                        "{} {next}{separator}{later}",
                        if action == "correction" {
                            corrected
                        } else {
                            typo
                        }
                    ),
                    "{action}"
                );
                assert_eq!(
                    s.engine.control.fixed_count(),
                    u64::from(action == "correction")
                );
                if action == "correction" {
                    let unfinished = if separator.is_empty() {
                        format!("{next}{later}")
                    } else {
                        later.to_owned()
                    };
                    assert_eq!(
                        reading::<Simulated>(
                            &s.engine.lock().keys,
                            Simulated::current_layout().unwrap()
                        ),
                        unfinished
                    );
                    s.undo();
                    s.pending();
                    s.finish();
                    assert_eq!(s.text(), format!("{typo} {next}{separator}{later}"));
                }
                assert_eq!(s.engine.control.undo_count(), 1);
                crate::complete::unlist(typo);
            }
        }
    }
    SIMULATED_LAYOUT.store(0, Ordering::SeqCst);
}

#[test]
fn verified_undo_resumes_after_a_tracked_word_boundary() {
    const CHILD: &str = "RECAST_VERIFIED_UNDO_BOUNDARY_TEST";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "platform::engine::tests::verified_undo_resumes_after_a_tracked_word_boundary",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    for space_during_undo in [false, true] {
        let s = Session::new();
        s.type_text("recieve ");
        s.pending();
        s.finish();
        s.type_text(if space_during_undo { "next" } else { "next " });
        s.undo();
        s.pending();
        if space_during_undo {
            s.type_text(" ");
        }
        s.finish();
        assert_eq!(s.text(), "recieve next ");
        s.type_text("keyboad ");
        s.pending();
        s.finish();
        assert_eq!(s.text(), "recieve next keyboard ");
        crate::complete::unlist("recieve");
    }
}

#[test]
fn verified_undo_preserves_the_source_layout_of_buffered_text() {
    const CHILD: &str = "RECAST_VERIFIED_UNDO_LAYOUT_TEST";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "platform::engine::tests::verified_undo_preserves_the_source_layout_of_buffered_text"])
            .env(CHILD, "1").output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    ALLOW_LAYOUT_SWITCH.store(true, Ordering::SeqCst);
    for ending in ["", " "] {
        SIMULATED_LAYOUT.store(0, Ordering::SeqCst);
        let s = Session::new();
        s.type_text("akuo ");
        s.pending();
        s.finish();
        assert_eq!(s.text(), "שלום ");
        s.type_text(&crate::keymap::convert_selection("עוד"));
        let (ready, reached) = mpsc::sync_channel(0);
        let (proceed, resume) = mpsc::channel();
        *LAYOUT_GATE.lock().unwrap() = Some(FocusGate {
            ready,
            proceed: resume,
        });
        s.undo();
        reached.recv_timeout(Duration::from_secs(3)).unwrap();
        s.type_text(&crate::keymap::convert_selection(&format!(" קצת{ending}")));
        proceed.send(()).unwrap();
        s.pending();
        s.finish();
        assert_eq!(s.text(), format!("akuo עוד קצת{ending}"));
        assert_eq!(Simulated::current_layout(), Some(Language::English));
        assert!(s.engine.lock().keys.is_empty());
        assert!(s.engine.lock().deletion_tail.is_none());
        assert_eq!(s.engine.lock().no_fix, ending.is_empty());
        if ending.is_empty() {
            s.type_text(" ");
        }
        s.type_text("keyboad ");
        s.pending();
        s.finish();
        assert_eq!(s.text(), "akuo עוד קצת keyboard ");
        crate::complete::unlist("akuo");
    }
    SIMULATED_LAYOUT.store(0, Ordering::SeqCst);
    let s = Session::new();
    s.type_text("akuo ");
    s.pending();
    s.finish();
    s.type_text(&crate::keymap::convert_selection("עוד"));
    s.undo();
    s.pending();
    // Later input under the restored layout must not be read as old-layout text.
    s.type_text("more");
    s.finish();
    assert_eq!(s.text(), "שלום עודmore");
    assert_eq!(s.engine.control.undo_count(), 0);
}

#[test]
fn verified_edits_reject_stale_buffered_text_and_context() {
    const CHILD: &str = "RECAST_VERIFIED_BUFFER_CANCEL_TEST";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "platform::engine::tests::verified_edits_reject_stale_buffered_text_and_context",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    fn waiting_undo<const VERIFY: bool>() -> SessionWithPolicy<true, VERIFY> {
        let s = SessionWithPolicy::new();
        s.type_text("recieve ");
        s.pending();
        s.finish();
        s.type_text("next");
        s.undo();
        s.pending();
        s.type_text("more");
        s
    }
    for cancel in [
        "mismatch",
        "enter",
        "backspace",
        "click",
        "focus",
        "secure",
        "excluded",
    ] {
        let s = waiting_undo::<true>();
        match cancel {
            "mismatch" => s.engine.injector.text.lock().unwrap().push('!'),
            "enter" => s.type_text("\n"),
            "backspace" => s.backspace(),
            "click" => s.engine.mouse_click(),
            "focus" => FOCUS.store(2, Ordering::SeqCst),
            "secure" => INPUT_ALLOWED.store(false, Ordering::SeqCst),
            _ => *lock_forgiving(&s.engine.control.excluded_apps) = vec!["editor".into()],
        }
        let expected = s.text();
        s.finish();
        assert_eq!(s.text(), expected, "{cancel}");
        assert_eq!(s.engine.control.undo_count(), 0, "{cancel}");
        assert!(!crate::complete::suppressed("recieve"), "{cancel}");
        FOCUS.store(1, Ordering::SeqCst);
        INPUT_ALLOWED.store(true, Ordering::SeqCst);
    }
    let s = waiting_undo::<false>();
    s.finish();
    assert_eq!(s.text(), "receive nextmore");
    assert_eq!(s.engine.control.undo_count(), 0);
}

#[test]
fn automatic_search_cancels_unsafe_suffixes_and_preserves_finished_words() {
    const CHILD: &str = "RECAST_SEARCH_CANCELLATION_TEST";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "platform::engine::tests::automatic_search_cancels_unsafe_suffixes_and_preserves_finished_words",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    for cancel in [
        "enter",
        "navigation",
        "click",
        "backspace",
        "modifier",
        "shift_ctrl",
        "both_shifts",
        "bound",
        "focus",
        "layout",
        "secure",
        "disabled",
        "excluded",
    ] {
        let s = Session::new();
        s.type_text("recieve");
        let (ready, reached) = mpsc::sync_channel(0);
        let (proceed, resume) = mpsc::channel();
        *CHECK_GATE.lock().unwrap() = Some(FocusGate {
            ready,
            proceed: resume,
        });
        s.type_while_checking(" ");
        reached.recv_timeout(Duration::from_secs(3)).unwrap();
        s.type_while_checking("next");
        match cancel {
            "enter" => s.type_while_checking("\n"),
            "navigation" => s.tap('\x10'),
            "click" => s.engine.mouse_click(),
            "backspace" => s.backspace(),
            "modifier" => s.tap(Simulated::CTRL_LEFT),
            "shift_ctrl" | "both_shifts" => {
                s.engine.key_press(Simulated::SHIFT_LEFT);
                s.engine.key_press(if cancel == "both_shifts" {
                    Simulated::SHIFT_RIGHT
                } else {
                    Simulated::CTRL_LEFT
                });
            }
            "bound" => s.type_while_checking(&"x".repeat(29)),
            "focus" => FOCUS.store(2, Ordering::SeqCst),
            "layout" => SIMULATED_LAYOUT.store(1, Ordering::SeqCst),
            "secure" => INPUT_ALLOWED.store(false, Ordering::SeqCst),
            "disabled" => {
                s.engine.control.set_enabled(false);
            }
            _ => *lock_forgiving(&s.engine.control.excluded_apps) = vec!["editor".into()],
        }
        let expected = s.text();
        proceed.send(()).unwrap();
        s.checks_ready();
        assert!(s.ready.try_recv().is_err(), "{cancel}");
        assert_eq!(s.text(), expected, "{cancel}");
        assert_eq!(s.engine.control.fixed_count(), 0, "{cancel}");
        FOCUS.store(1, Ordering::SeqCst);
        SIMULATED_LAYOUT.store(0, Ordering::SeqCst);
        INPUT_ALLOWED.store(true, Ordering::SeqCst);
    }
    for earlier in ["recieve", "keyboard"] {
        let s = Session::new();
        let (ready, reached) = mpsc::sync_channel(0);
        let (proceed, resume) = mpsc::channel();
        *CHECK_GATE.lock().unwrap() = Some(FocusGate {
            ready,
            proceed: resume,
        });
        s.type_while_checking(&format!("{earlier} "));
        reached.recv_timeout(Duration::from_secs(3)).unwrap();
        s.type_while_checking("old keyboad ");
        assert!(!s.engine.lock().word_checks.is_empty());
        proceed.send(()).unwrap();
        s.checks_ready();
        s.pending();
        s.finish();
        let (expected, fixed) = if earlier == "recieve" {
            // Both finished words correct without pausing between them:
            // the first preserves the second verbatim, then the second lands.
            s.pending();
            s.finish();
            ("receive old keyboard ", 2)
        } else {
            ("keyboard old keyboard ", 1)
        };
        assert_eq!(s.text(), expected);
        assert_eq!(s.engine.control.fixed_count(), fixed);
    }
    // A delayed fix requires the native field to match every captured character.
    let s = Session::new();
    let (ready, reached) = mpsc::sync_channel(0);
    let (proceed, resume) = mpsc::channel();
    *CHECK_GATE.lock().unwrap() = Some(FocusGate {
        ready,
        proceed: resume,
    });
    s.type_while_checking("recieve ");
    reached.recv_timeout(Duration::from_secs(3)).unwrap();
    s.type_while_checking("next");
    proceed.send(()).unwrap();
    s.pending();
    s.engine.injector.text.lock().unwrap().push('!');
    s.finish();
    assert_eq!(s.text(), "recieve next!");
    assert_eq!(s.engine.control.fixed_count(), 0);

    for suffix in ["next", "  "] {
        let s = SessionWithPolicy::<false, false>::new();
        let (ready, reached) = mpsc::sync_channel(0);
        let (proceed, resume) = mpsc::channel();
        *CHECK_GATE.lock().unwrap() = Some(FocusGate {
            ready,
            proceed: resume,
        });
        s.type_while_checking("recieve ");
        reached.recv_timeout(Duration::from_secs(3)).unwrap();
        s.type_while_checking(suffix);
        proceed.send(()).unwrap();
        s.checks_ready();
        if suffix.trim().is_empty() {
            s.pending();
            s.finish();
            assert_eq!(s.text(), "receive   ");
        } else {
            assert!(s.ready.try_recv().is_err());
            assert_eq!(s.text(), "recieve next");
            assert_eq!(
                reading::<Simulated>(&s.engine.lock().keys, Language::English),
                "next"
            );
            assert!(!s.engine.lock().no_fix);
        }
    }
}

#[test]
fn late_correction_focus_check_preserves_next_word() {
    const CHILD: &str = "RECAST_LATE_CORRECTION_FOCUS_TEST";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "platform::engine::tests::late_correction_focus_check_preserves_next_word",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    for newer in [
        "word",
        "replacement",
        "enter",
        "secure",
        "focus",
        "excluded",
    ] {
        let s = Session::new();
        s.type_text("recieve");
        let (check_ready, checking) = mpsc::sync_channel(0);
        let (finish_check, resume_check) = mpsc::channel();
        *CHECK_GATE.lock().unwrap() = Some(FocusGate {
            ready: check_ready,
            proceed: resume_check,
        });
        let engine = Arc::clone(&s.engine);
        let worker = thread::spawn(move || engine.key_press(' '));
        checking.recv_timeout(Duration::from_secs(3)).unwrap();
        s.engine.injector.text.lock().unwrap().push(' ');
        s.engine.key_release(' ');
        let (focus_ready, focusing) = mpsc::sync_channel(0);
        let (finish_focus, resume_focus) = mpsc::channel();
        *FOCUS_GATE.lock().unwrap() = Some(FocusGate {
            ready: focus_ready,
            proceed: resume_focus,
        });
        finish_check.send(()).unwrap();
        focusing.recv_timeout(Duration::from_secs(3)).unwrap();
        match newer {
            "word" => s.type_while_checking("recieve"),
            "replacement" => {
                s.type_while_checking("recieve ");
            }
            "enter" => s.type_while_checking("\n"),
            "secure" => INPUT_ALLOWED.store(false, Ordering::SeqCst),
            "focus" => FOCUS.store(2, Ordering::SeqCst),
            _ => *lock_forgiving(&s.engine.control.excluded_apps) = vec!["editor".into()],
        }
        finish_focus.send(()).unwrap();
        worker.join().unwrap();
        s.checks_ready();
        if !matches!(newer, "word" | "replacement") {
            assert!(s.ready.try_recv().is_err());
            assert!(!s.engine.lock().is_replacing);
            assert_eq!(s.engine.control.fixed_count(), 0);
            if matches!(newer, "secure" | "excluded") {
                assert!(s.engine.lock().no_fix);
            }
            assert_eq!(
                s.text(),
                if newer == "enter" {
                    "recieve \n"
                } else {
                    "recieve "
                }
            );
            INPUT_ALLOWED.store(true, Ordering::SeqCst);
            FOCUS.store(1, Ordering::SeqCst);
            continue;
        }
        if newer == "word" {
            s.pending();
            s.finish();
            assert_eq!(s.text(), "receive recieve");
            assert_eq!(
                reading::<Simulated>(&s.engine.lock().keys, Language::English),
                "recieve"
            );
            assert!(!s.engine.lock().no_fix);
            s.type_text(" ");
            s.pending();
        } else {
            // The finished second word is preserved by the first correction,
            // then its own check lands after it.
            s.pending();
            s.finish();
            s.pending();
        }
        s.finish();
        assert_eq!(s.text(), "receive receive ");
        assert_eq!(s.engine.control.fixed_count(), 2);
    }
}

#[test]
fn typing_correction_undo_and_interrupted_replacements() {
    // Undo changes process-wide suppression and learning state. Run this
    // scenario alone so it cannot change the parallel corpus test's results.
    const CHILD: &str = "RECAST_ENGINE_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "platform::engine::tests::typing_correction_undo_and_interrupted_replacements",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    // Search owns a snapshot, not the state lock. Releases stay responsive;
    // input that moves the cursor cancels the snapshot before any edit.
    for cancel in ["release", "click", "typing"] {
        let s = Session::new();
        s.type_text("recieve");
        let (ready, reached) = mpsc::sync_channel(0);
        let (proceed, resume) = mpsc::channel();
        *CHECK_GATE.lock().unwrap() = Some(FocusGate {
            ready,
            proceed: resume,
        });
        let engine = Arc::clone(&s.engine);
        let worker = thread::spawn(move || engine.key_press(' '));
        reached.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(
            s.engine.state.try_lock().is_ok(),
            "search must release typing state"
        );
        s.engine.key_release(' ');
        s.engine.injector.text.lock().unwrap().push(' ');
        if cancel == "click" {
            s.engine.mouse_click();
        }
        if cancel == "typing" {
            s.type_while_checking("x");
        }
        proceed.send(()).unwrap();
        worker.join().unwrap();
        s.checks_ready();
        if cancel == "click" {
            assert!(s.ready.try_recv().is_err());
            assert_eq!(s.text(), "recieve ");
        } else {
            s.pending();
            s.finish();
            assert_eq!(
                s.text(),
                if cancel == "typing" {
                    "receive x"
                } else {
                    "receive "
                }
            );
        }
    }

    // Manual mode preserves commands while retaining explicit repair and undo.
    for word in ["recieve", "akuo", "hellorecieve"] {
        let s = Session::new();
        *lock_forgiving(&s.engine.control.manual_only_apps) = vec!["editor".into()];
        s.type_text(&format!("{word} "));
        assert_eq!(s.text(), format!("{word} "));
        assert!(!s.engine.lock().is_replacing);
        assert_eq!(s.engine.control.fixed_count(), 0);
        assert!(s.engine.lock().last_action.is_some());
    }
    let s = Session::new();
    *lock_forgiving(&s.engine.control.manual_only_apps) = vec!["editor".into()];
    s.type_text("keyb");
    s.tap(Simulated::SHIFT_RIGHT);
    s.pending();
    s.finish();
    assert_ne!(s.text(), "keyb");
    s.undo();
    s.pending();
    s.finish();
    assert_eq!(s.text(), "keyb");
    let s = Session::new();
    s.type_text("recieve ");
    s.pending();
    *lock_forgiving(&s.engine.control.manual_only_apps) = vec!["editor".into()];
    s.finish();
    assert_eq!(s.text(), "recieve ");

    // Physical Enter may submit, move focus, or end a line. Never replay it.
    for text in ["recieve\n", "akuo\n", "old\n"] {
        let s = Session::new();
        s.type_text(text);
        assert_eq!(s.text(), text);
        assert!(!s.engine.lock().is_replacing);
        assert!(s.engine.lock().keys.is_empty());
        assert!(s.engine.lock().last_action.is_none());
        assert_eq!(s.engine.lock().history.run(), Run::default());
        assert_eq!(s.engine.control.fixed_count(), 0);
        s.undo();
        assert!(!s.engine.lock().is_replacing);
        assert!(s.ready.try_recv().is_err());
    }
    let s = Session::new();
    s.type_text("recieve ");
    s.pending();
    s.type_text("\n");
    s.finish();
    assert_eq!(s.text(), "recieve \n");
    assert_eq!(s.engine.control.fixed_count(), 0);
    assert!(s.engine.lock().last_action.is_none());
    s.type_text("recieve ");
    s.pending();
    s.finish();
    assert_eq!(s.text(), "recieve \nreceive ");

    // Editing rules cancels work already queued and never corrects the editor itself.
    let s = Session::new();
    s.type_text("recieve ");
    s.pending();
    s.engine
        .control
        .word_rules_open
        .store(true, Ordering::Relaxed);
    s.finish();
    assert_eq!(s.text(), "recieve ");
    s.type_text("keyboad ");
    assert!(s.engine.lock().keys.is_empty());
    assert!(!s.engine.lock().is_replacing);
    assert_eq!(s.engine.control.fixed_count(), 0);
    s.engine
        .control
        .word_rules_open
        .store(false, Ordering::Relaxed);
    s.type_text(" recieve ");
    s.pending();
    s.finish();
    assert_eq!(s.text(), "recieve keyboad  receive ");
    let evidence = s.engine.control.history()[0].inspection();
    assert!(evidence.contains("receive: score"), "{evidence}");
    assert!(
        evidence.contains("English reading: \"recieve\""),
        "{evidence}"
    );
    crate::config::Config::update_live(|cfg| cfg.spell_enabled = false);
    assert_eq!(
        s.engine.control.history()[0].inspection(),
        evidence,
        "inspecting an applied correction must not recompute it with current settings"
    );
    crate::config::Config::update_live(|cfg| cfg.spell_enabled = true);

    // Focus changes between the two undo taps cannot act on another field.
    let s = Session::new();
    s.type_text("recieve ");
    s.pending();
    s.finish();
    s.tap(Simulated::CTRL_LEFT);
    FOCUS.store(2, Ordering::SeqCst);
    s.tap(Simulated::CTRL_LEFT);
    assert!(!s.engine.lock().is_replacing);
    assert_eq!(s.text(), "receive ");
    assert_eq!(s.engine.control.undo_count(), 0);
    FOCUS.store(1, Ordering::SeqCst);

    // A late second tap starts a new pair; a timely third tap undoes the correction.
    ALLOW_LAYOUT_SWITCH.store(true, Ordering::SeqCst);
    let s = Session::new();
    s.type_text("recieve ");
    s.pending();
    s.finish();
    s.tap(Simulated::CTRL_LEFT);
    s.engine.lock().last_action_tap =
        Some(Instant::now() - DOUBLE_TAP_WINDOW - Duration::from_millis(1));
    s.tap(Simulated::CTRL_LEFT);
    assert!(!s.engine.lock().is_replacing);
    assert_eq!(s.text(), "receive ");
    s.tap(Simulated::CTRL_LEFT);
    s.pending();
    s.finish();
    assert_eq!(s.text(), "recieve ");
    assert_eq!(s.engine.control.undo_count(), 1);
    SIMULATED_LAYOUT.store(0, Ordering::SeqCst);
    ALLOW_LAYOUT_SWITCH.store(false, Ordering::SeqCst);
    crate::complete::unlist("recieve");

    let s = Session::new();
    s.type_text("keyboad ");
    s.pending();
    s.finish();
    assert_eq!(s.text(), "keyboard ");
    assert_eq!(s.engine.control.fixed_count(), 1);
    s.undo();
    s.pending();
    s.finish();
    assert_eq!(s.text(), "keyboad ");
    assert_eq!(s.engine.control.undo_count(), 1);
    assert!(crate::complete::suppressed("keyboad"));
    assert!(s.engine.lock().last_action.is_none());

    // Undo preserves spaces typed after a correction or while it is pending.
    for buffered in [false, true] {
        let s = Session::new();
        s.type_text("recieve ");
        s.pending();
        if !buffered {
            s.finish();
        }
        s.type_text("  ");
        s.undo();
        if buffered {
            assert!(s.engine.lock().pending_undo);
            s.proceed.send(()).unwrap();
        }
        s.pending();
        s.finish();
        assert_eq!(s.text(), "recieve   ");
        assert!(s.engine.lock().keys.is_empty());
        assert_eq!(s.engine.control.undo_count(), 1);
        crate::complete::unlist("recieve");
    }
    let s = Session::new();
    s.type_text("keyb");
    s.tap(Simulated::SHIFT_RIGHT);
    s.pending();
    s.finish();
    s.type_text("  ");
    s.undo();
    s.pending();
    s.finish();
    assert_eq!(s.text(), "keyb  ");
    assert!(s.engine.lock().keys.is_empty());
    crate::complete::unlist("keyb");

    // Cursor/focus changes, Enter, and the space bound still invalidate undo.
    for interrupt in ["enter", "click", "focus", "bound"] {
        let s = Session::new();
        s.type_text("recieve ");
        s.pending();
        s.finish();
        s.type_text("  ");
        match interrupt {
            "enter" => s.type_text("\n"),
            "click" => s.engine.mouse_click(),
            "focus" => {
                FOCUS.store(2, Ordering::SeqCst);
                s.type_text(" ");
            }
            _ => s.type_text(&" ".repeat(MAX_UNDO_SPACES)),
        }
        let text = s.text();
        s.undo();
        s.idle();
        assert_eq!(s.text(), text, "{interrupt}");
        assert!(!s.engine.lock().is_replacing, "{interrupt}");
        assert!(s.engine.lock().last_action.is_none(), "{interrupt}");
        assert_eq!(s.engine.control.undo_count(), 0);
        FOCUS.store(1, Ordering::SeqCst);
    }

    // Typing into the next word keeps one correction undoable: the suffix is
    // restored verbatim after the original word is brought back.
    let s = Session::new();
    s.type_text("recieve ");
    s.pending();
    s.finish();
    assert_eq!(s.text(), "receive ");
    s.type_text("  x");
    let text = s.text();
    assert_eq!(text, "receive   x");
    s.undo();
    s.pending();
    s.finish();
    assert_eq!(s.text(), "recieve   x");
    assert!(s.engine.lock().keys.is_empty());
    s.type_text("y ");
    assert_eq!(s.text(), "recieve   xy ");
    assert!(s.ready.try_recv().is_err());
    assert!(!s.engine.lock().is_replacing);
    assert!(!matches!(
        s.engine.lock().last_action,
        Some(LastAction::Fixed(_))
    ));
    assert_eq!(s.engine.control.undo_count(), 1);
    crate::complete::unlist("recieve");

    for length in [32, 33] {
        let s = Session::new();
        s.type_text("recieve ");
        s.pending();
        s.finish();
        s.type_text(&"x".repeat(length));
        s.undo();
        if length == 32 {
            s.pending();
            s.finish();
        }
        assert_eq!(
            s.text(),
            format!(
                "{} {}",
                if length == 32 { "recieve" } else { "receive" },
                "x".repeat(length)
            )
        );
        assert!(s.ready.try_recv().is_err());
        crate::complete::unlist("recieve");
    }

    // Backspace updates the suffix; native text mismatches refuse extended undo.
    for mismatch in [false, true] {
        let s = Session::new();
        s.type_text("recieve ");
        s.pending();
        s.finish();
        s.type_text("nextx");
        s.backspace();
        if mismatch {
            s.engine.injector.text.lock().unwrap().push('!');
        }
        s.undo();
        s.pending();
        s.finish();
        assert_eq!(
            s.text(),
            if mismatch {
                "receive next!"
            } else {
                "recieve next"
            }
        );
        assert_eq!(s.engine.control.undo_count(), u64::from(!mismatch));
        crate::complete::unlist("recieve");
    }

    // An eager undo during injection waits for the correction to finish;
    // neither Ctrl tap may invalidate a partly written word.
    for shortcut in ["none", "right_ctrl"] {
        crate::config::Config::update_live(|cfg| cfg.undo_shortcut = shortcut.into());
        let s = Session::new();
        s.type_text("acheive ");
        s.pending();
        s.engine.injector.injecting.store(true, Ordering::SeqCst);
        s.undo();
        assert!(s.engine.lock().pending_undo);
        s.proceed.send(()).unwrap();
        s.pending();
        assert_eq!(s.text(), "achieve ");
        s.finish();
        assert_eq!(s.text(), "acheive ");
        assert_eq!(s.engine.control.undo_count(), 1);
        crate::complete::unlist("acheive");
        // A delayed learning write must not resurrect a newer unlist.
        crate::complete::seed_legacy_undo("acheive");
        crate::complete::seed_legacy_undo("acheive");
        assert!(!crate::complete::learned("acheive"));
    }
    crate::config::Config::update_live(|cfg| cfg.undo_shortcut = "none".into());

    // Cursor changes and later typing still cancel an eager undo.
    for click in [false, true] {
        let s = Session::new();
        s.type_text("recieve ");
        s.pending();
        s.undo();
        assert!(s.engine.lock().pending_undo);
        if click {
            s.engine.mouse_click();
        } else {
            s.type_text("next");
        }
        s.finish();
        assert_eq!(s.text(), if click { "recieve " } else { "receive next" });
        assert_eq!(s.engine.control.undo_count(), 0);
        assert!(!s.engine.lock().pending_undo);
    }

    // Pause the worker at injection, then move the cursor, paste, edit, or
    // change focus. None may erase, learn, count a fix, or re-arm undo.
    for interrupt in 0..6 {
        let s = Session::new();
        s.type_text("recieve ");
        s.pending();
        match interrupt {
            0 => s.engine.mouse_click(),
            1 => s.tap('\x1b'),
            2 => {
                s.engine.key_press(Simulated::CTRL_LEFT);
                s.tap('v');
                s.engine.key_release(Simulated::CTRL_LEFT);
            }
            3 => s.tap(Simulated::BACKSPACE),
            4 => {
                FOCUS.store(2, Ordering::SeqCst);
            }
            _ => {
                s.engine.injector.injecting.store(true, Ordering::SeqCst);
                s.type_text("x");
            }
        }
        s.finish();
        assert_eq!(
            s.text(),
            if interrupt == 5 {
                "recieve x"
            } else {
                "recieve "
            },
            "interruption {interrupt}"
        );
        assert_eq!(s.engine.control.fixed_count(), 0);
        assert!(
            s.engine.control.history().is_empty(),
            "canceled injections must not leave correction evidence"
        );
        assert!(s.engine.lock().last_action.is_none());
        if interrupt == 3 {
            assert!(s.engine.lock().no_fix);
            s.type_text(" recieve ");
            s.pending();
            s.finish();
            assert_eq!(s.text(), "recieve  receive ");
        }
        FOCUS.store(1, Ordering::SeqCst);
    }

    // Shortcut letters and Ctrl+Space are not text or word terminators.
    let s = Session::new();
    s.type_text("recie");
    s.engine.key_press(Simulated::CTRL_LEFT);
    s.tap('v');
    s.tap(' ');
    s.engine.key_release(Simulated::CTRL_LEFT);
    s.type_text("ve ");
    assert!(!s.engine.lock().is_replacing);
    assert!(s.engine.lock().keys.is_empty());
    // An empty terminator must clear suppression for the following word.
    s.type_text("recieve ");
    s.pending();
    s.finish();
    assert_eq!(s.text(), "recieve receive ");

    // Fast typing is replayed; undo preserves those later keys.
    let s = Session::new();
    s.type_text("recieve ");
    s.pending();
    s.type_text("next");
    s.finish();
    assert_eq!(s.text(), "receive next");
    s.undo();
    s.pending();
    s.finish();
    assert_eq!(s.text(), "recieve next");
    crate::complete::unlist("recieve");

    // A complete second word arriving during injection survives undo too.
    let s = Session::new();
    s.type_text("recieve ");
    s.pending();
    s.type_text("next ");
    s.finish();
    s.undo();
    s.pending();
    s.finish();
    assert_eq!(s.text(), "recieve next ");
    assert!(!s.engine.lock().is_replacing);
    assert_eq!(s.engine.control.undo_count(), 1);
    crate::complete::unlist("recieve");

    // Pausing or disabling while a worker is waiting cancels that rewrite.
    for pause in [true, false] {
        let s = Session::new();
        s.type_text("recieve ");
        s.pending();
        if pause {
            s.engine.control.pause_for(Duration::from_secs(60));
        } else {
            s.engine.control.set_enabled(false);
        }
        s.finish();
        assert_eq!(s.text(), "recieve ");
        assert_eq!(s.engine.control.fixed_count(), 0);
        assert!(s.engine.lock().last_action.is_none());
    }

    // Focus can move after a successful correction, before an undo gesture.
    let s = Session::new();
    s.type_text("recieve ");
    s.pending();
    s.finish();
    FOCUS.store(2, Ordering::SeqCst);
    s.undo();
    assert_eq!(s.text(), "receive ");
    assert!(!s.engine.lock().is_replacing);
    assert_eq!(s.engine.control.undo_count(), 0);
    FOCUS.store(1, Ordering::SeqCst);

    // A programmatic focus change between typing and the terminator is
    // caught even without a mouse or navigation-key event.
    let s = Session::new();
    s.type_text("recieve");
    FOCUS.store(2, Ordering::SeqCst);
    s.type_text(" ");
    assert!(!s.engine.lock().is_replacing);
    assert_eq!(s.text(), "recieve ");
    FOCUS.store(1, Ordering::SeqCst);

    // Backspacing within a word must not suspend its correction.
    let s = Session::new();
    s.type_text("recievex");
    s.backspace();
    s.type_text(" ");
    s.pending();
    s.finish();
    assert_eq!(s.text(), "receive ");
    s.type_text("recieve ");
    s.pending();
    s.finish();
    assert_eq!(s.text(), "receive receive ");

    // Erasing a fully tracked word restores correction immediately.
    let s = Session::new();
    s.type_text("bad");
    for _ in 0..3 {
        s.backspace();
    }
    s.type_text("recieve ");
    s.pending();
    s.finish();
    assert_eq!(s.text(), "receive ");

    // A finished word remains known through its separator and correction.
    for original in ["old ", "recieve ", "old  ", "old\n"] {
        let s = Session::new();
        s.type_text(original);
        if original == "recieve " {
            s.pending();
            s.finish();
            assert_eq!(s.text(), "receive ");
        }
        let count = s.text().chars().count();
        for _ in 0..count {
            s.backspace();
        }
        s.type_text("recieve ");
        s.pending();
        s.finish();
        assert_eq!(s.text(), "receive ", "after erasing {original:?}");
    }

    // Real deletion often includes repeats after the field is already empty.
    // A click or a delete shortcut also loses context; an empty field is safe.
    let s = Session::new();
    s.engine.mouse_click();
    for _ in 0..3 {
        s.type_text("recieve ");
        s.pending();
        s.finish();
        assert_eq!(s.text(), "receive ");
        for _ in 0..12 {
            s.backspace();
        }
    }
    s.type_text("old");
    s.engine.key_press(Simulated::CTRL_LEFT);
    s.tap(Simulated::BACKSPACE);
    s.engine.injector.text.lock().unwrap().clear();
    s.engine.key_release(Simulated::CTRL_LEFT);
    s.type_text("recieve ");
    s.pending();
    s.finish();
    assert_eq!(s.text(), "receive ");

    // Arrows and forward delete suppress automatic rewriting. An explicit
    // request may still replace the freshly tracked suffix, preserving older text.
    for reset in '\x10'..='\x14' {
        let s = Session::new();
        s.type_text("old ");
        s.tap(reset);
        s.type_text("x");
        s.backspace();
        s.type_text("recieve");
        s.tap(Simulated::SHIFT_RIGHT);
        s.pending();
        s.finish();
        s.type_text(" ");
        assert!(!s.engine.lock().is_replacing);
        s.type_text("recieve ");
        s.pending();
        s.finish();
        assert_eq!(s.text(), "old receive receive ");
    }

    // Backspacing into an unknown prefix suspends correction until a boundary.
    let s = Session::new();
    s.type_text("old ");
    s.backspace();
    s.backspace();
    s.type_text("recieve ");
    assert!(!s.engine.lock().is_replacing);
    assert_eq!(s.text(), "olrecieve ");
    s.type_text("recieve ");
    s.pending();
    s.finish();
    assert_eq!(s.text(), "olrecieve receive ");

    // Explicit completion still works on a fully tracked, edited prefix.
    let s = Session::new();
    s.type_text("helx");
    s.backspace();
    s.tap(Simulated::SHIFT_RIGHT);
    s.pending();
    s.finish();
    assert!(s.text().starts_with("hel") && s.text().len() > 3);
    assert_eq!(
        *s.engine.injector.erasures.lock().unwrap(),
        [0],
        "first completion must append without deleting the prefix"
    );
    let count = s
        .engine
        .lock()
        .cycle
        .as_ref()
        .unwrap()
        .candidates
        .len()
        .min(crate::complete::MAX_CANDIDATES);
    for _ in 0..count {
        s.tap(Simulated::SHIFT_RIGHT);
        s.pending();
        s.finish();
    }
    assert_eq!(
        s.text(),
        "hel",
        "completion cycle restores the exact prefix"
    );

    // Adjacent offers can share more text than the original input. Preserve it
    // while cycling, but still undo to the original input after every offer.
    for (typed, offers, erasures) in [
        ("keyb", ["keyboard", "keyboards", "keyboard"], [0, 0, 1]),
        ("recie", ["receive", "receiver", "receive"], [2, 0, 1]),
    ] {
        for undo_at in 0..3 {
            let cycle_session = Session::new();
            cycle_session.type_text(typed);
            {
                let mut st = cycle_session.engine.lock();
                st.cycle = Some(Cycle {
                    preview: false,
                    preview_ready: false,
                    typed: st.keys.to_vec(),
                    trailing_spaces: Vec::new(),
                    candidates: offers.iter().map(|text| (*text).into()).collect(),
                    candidate_langs: vec![Language::English; offers.len()],
                    correction: false,
                    index: offers.len(),
                    resume_index: 0,
                    on_screen: typed.len(),
                    lang: Language::English,
                    original_lang: Language::English,
                });
            }
            for expected in offers.iter().take(undo_at + 1) {
                cycle_session.tap(Simulated::SHIFT_RIGHT);
                cycle_session.pending();
                cycle_session.finish();
                assert_eq!(cycle_session.text(), *expected);
            }
            assert_eq!(
                *cycle_session.engine.injector.erasures.lock().unwrap(),
                erasures[..=undo_at]
            );
            cycle_session.undo();
            cycle_session.pending();
            cycle_session.finish();
            assert_eq!(cycle_session.text(), typed);
            crate::complete::unlist(typed);
        }
    }

    // Live settings must also stop an already cached completion cycle.
    s.tap(Simulated::SHIFT_RIGHT);
    s.pending();
    s.finish();
    let completed = s.text();
    crate::config::Config::update_live(|cfg| cfg.complete_enabled = false);
    s.tap(Simulated::SHIFT_RIGHT);
    assert!(!s.engine.lock().is_replacing);
    assert_eq!(s.text(), completed);
    assert!(s.engine.lock().cycle.is_none());
    // Turning completion off must still let the user undo its last offer.
    s.undo();
    s.pending();
    s.finish();
    assert_eq!(s.text(), "hel");
    crate::config::Config::update_live(|cfg| cfg.complete_enabled = true);

    // Expansions keep their full text across cycles even though the live
    // word buffer may contain only the last word. Undo restores the prefix.
    let s = Session::new();
    s.type_text("btw");
    {
        let mut st = s.engine.lock();
        st.cycle = Some(Cycle {
            preview: false,
            preview_ready: false,
            typed: st.keys.to_vec(),
            trailing_spaces: Vec::new(),
            candidates: vec!["by the way".into(), "between".into()],
            candidate_langs: vec![Language::English; 2],
            correction: false,
            index: 2,
            resume_index: 0,
            on_screen: 3,
            lang: Language::English,
            original_lang: Language::English,
        });
    }
    for expected in ["by the way", "between", "btw", "by the way"] {
        s.tap(Simulated::SHIFT_RIGHT);
        s.pending();
        s.finish();
        assert_eq!(s.text(), expected);
    }
    s.undo();
    s.pending();
    s.finish();
    assert_eq!(s.text(), "btw");

    // Exclusions stop capture before any planner, debug log or learning call.
    // A new exclusion also cancels a correction planned before the UI change.
    let s = Session::new();
    s.type_text("recieve ");
    s.pending();
    *lock_forgiving(&s.engine.control.excluded_apps) = vec!["editor".into()];
    s.finish();
    assert_eq!(s.text(), "recieve ");
    lock_forgiving(&s.engine.control.excluded_apps).clear();
    s.type_text(" recieve ");
    s.pending();
    s.finish();
    assert_eq!(s.text(), "recieve  receive ");

    for focus in [0, 2] {
        let s = Session::new();
        *lock_forgiving(&s.engine.control.excluded_apps) = vec!["secret.exe".into()];
        FOCUS.store(focus, Ordering::SeqCst);
        s.type_text("recieve hello akuo ");
        s.type_text("hel");
        s.tap(Simulated::SHIFT_RIGHT);
        s.undo();
        let st = s.engine.lock();
        assert!(st.keys.is_empty() && st.buffered_keys.is_empty());
        assert!(!st.is_replacing && st.last_action.is_none());
        assert!(st.held_keys.is_empty());
        assert_eq!(s.engine.control.fixed_count(), 0);
    }
    FOCUS.store(1, Ordering::SeqCst);
    let s = Session::new();
    *lock_forgiving(&s.engine.control.excluded_apps) = vec!["secret.exe".into()];
    s.type_text("recieve ");
    s.pending();
    // A focus change while the injection worker waits still cancels it.
    FOCUS.store(2, Ordering::SeqCst);
    s.finish();
    assert_eq!(s.text(), "recieve ");
    FOCUS.store(1, Ordering::SeqCst);
    s.type_text(" ");
    s.type_text("recieve ");
    s.pending();
    s.finish();
    assert_eq!(s.text(), "recieve  receive ");

    let s = Session::new();
    *lock_forgiving(&s.engine.control.excluded_apps) = vec!["secret.exe".into()];
    s.type_text("recieve ");
    s.pending();
    // With exclusions active, new typing cancels pending work immediately
    // instead of delaying buffered-key accounting behind an app query.
    s.type_text("x");
    s.finish();
    assert_eq!(s.text(), "recieve x");
    assert_eq!(s.engine.control.fixed_count(), 0);

    // Temporary app pause cancels pending injection and blocks all capture.
    let s = Session::new();
    s.type_text("recieve ");
    s.pending();
    s.engine.control.pause_in_app("EDITOR");
    s.finish();
    assert_eq!(s.text(), "recieve ");
    for focus in [1, 0, 3, 1] {
        FOCUS.store(focus, Ordering::SeqCst);
        s.type_text("recieve hel");
        s.tap(Simulated::SHIFT_RIGHT);
        s.undo();
        let st = s.engine.lock();
        assert!(st.keys.is_empty() && st.buffered_keys.is_empty());
        assert!(!st.is_replacing && st.last_action.is_none());
        assert_eq!(s.engine.control.paused_app().as_deref(), Some("editor"));
    }
    assert_eq!(s.engine.control.fixed_count(), 0);
    s.engine
        .control
        .listener_ready
        .store(true, Ordering::Relaxed);
    assert!(
        crate::platform::status_for::<Simulated>(&s.engine.control, None)
            .starts_with("Paused in editor")
    );
    // The existing status poll notices leaving even without a keystroke.
    FOCUS.store(2, Ordering::SeqCst);
    assert!(
        crate::platform::status_for::<Simulated>(&s.engine.control, None).starts_with("Active")
    );
    assert!(s.engine.control.paused_app().is_none());
    FOCUS.store(1, Ordering::SeqCst);
    s.type_text(" recieve ");
    s.pending();
    s.finish();
    assert!(s.text().ends_with(" receive "));
    // Expiration and manual resume never remove saved restrictions.
    *lock_forgiving(&s.engine.control.excluded_apps) = vec!["editor".into()];
    s.engine.control.pause_in_app("editor");
    FOCUS.store(2, Ordering::SeqCst);
    s.type_text(" ");
    assert!(s.engine.control.paused_app().is_none());
    FOCUS.store(1, Ordering::SeqCst);
    s.engine.control.pause_in_app("editor");
    s.engine.control.resume_app();
    s.type_text("recieve ");
    assert!(!s.engine.lock().is_replacing);
    assert!(s.text().ends_with("recieve "));
    assert_eq!(
        s.engine.control.app_mode(Some("editor")),
        Some(crate::config::AppMode::Off)
    );

    // Switching to layout-only cancels an already-planned spelling fix.
    let s = Session::new();
    s.type_text("recieve ");
    s.pending();
    *lock_forgiving(&s.engine.control.layout_only_apps) = vec!["editor".into()];
    s.finish();
    assert_eq!(s.text(), "recieve ");
    s.type_text(" recieve keyb");
    s.tap(Simulated::SHIFT_RIGHT);
    assert!(!s.engine.lock().is_replacing);
    assert_eq!(s.text(), "recieve  recieve keyb");
    s.engine
        .control
        .listener_ready
        .store(true, Ordering::Relaxed);
    let application = ("Text Editor".into(), "editor".into());
    assert!(
        crate::platform::status_for::<Simulated>(&s.engine.control, Some(&application))
            .starts_with("Active in Text Editor · Layout only —")
    );
    let stale_application = ("Other App".into(), "other".into());
    assert!(
        crate::platform::status_for::<Simulated>(&s.engine.control, Some(&stale_application))
            .starts_with("Active in Editor · Layout only —")
    );
    SIMULATED_LAYOUT.store(2, Ordering::SeqCst);
    assert!(
        crate::platform::status_for::<Simulated>(&s.engine.control, None)
            .starts_with("Keyboard layout unavailable")
    );
    SIMULATED_LAYOUT.store(0, Ordering::SeqCst);
    FOCUS.store(0, Ordering::SeqCst);
    assert!(
        crate::platform::status_for::<Simulated>(&s.engine.control, None)
            .starts_with("Text focus unavailable —")
    );
    assert!(
        crate::platform::status_for::<Simulated>(&s.engine.control, Some(&application))
            .starts_with("Enabled in Text Editor · Layout only —")
    );
    lock_forgiving(&s.engine.control.layout_only_apps).clear();
    assert!(
        crate::platform::status_for::<Simulated>(&s.engine.control, Some(&application))
            .starts_with("Enabled in Text Editor · Full correction —")
    );
    s.type_text(" recieve ");
    assert!(
        !s.engine.lock().is_replacing,
        "status must not bypass missing focus"
    );
    *lock_forgiving(&s.engine.control.excluded_apps) = vec!["editor".into()];
    assert!(
        crate::platform::status_for::<Simulated>(&s.engine.control, Some(&application))
            .starts_with("Paused in Text Editor —")
    );
    lock_forgiving(&s.engine.control.excluded_apps).clear();
    // Knowing the frontmost app is not permission to bypass missing focus
    // or end a pause whose actual focused application is still unknown.
    s.engine.control.pause_in_app("secret.exe");
    assert!(
        crate::platform::status_for::<Simulated>(&s.engine.control, Some(&application))
            .starts_with("Paused in secret.exe")
    );
    assert_eq!(s.engine.control.paused_app().as_deref(), Some("secret.exe"));
    s.engine.control.resume_app();
    FOCUS.store(1, Ordering::SeqCst);

    // Single-tap undo uses the same focus and modifier-chord guards.
    INPUT_ALLOWED.store(false, Ordering::SeqCst);
    assert!(
        crate::platform::status_for::<Simulated>(&s.engine.control, None).contains("Secure Input")
    );
    INPUT_ALLOWED.store(true, Ordering::SeqCst);
    s.engine.control.pause_for(Duration::from_secs(60));
    assert!(
        crate::platform::status_for::<Simulated>(&s.engine.control, None)
            .starts_with("Paused · 1 min remaining —")
    );
    let disabled = AppControl::new_with_config_and_state(crate::config::Config::global(), false);
    disabled.pause_for(Duration::from_secs(60));
    assert!(crate::platform::status_for::<Simulated>(&disabled, None)
        .starts_with("Disabled until you enable it —"));
    s.engine.control.resume();
    *lock_forgiving(&s.engine.control.excluded_apps) = vec!["editor".into()];
    assert!(
        crate::platform::status_for::<Simulated>(&s.engine.control, None)
            .starts_with("Paused in Editor —")
    );

    // Layout-only still fixes the layout, but never expands abbreviations.
    ALLOW_LAYOUT_SWITCH.store(true, Ordering::SeqCst);
    let s = Session::new();
    *lock_forgiving(&s.engine.control.layout_only_apps) = vec!["editor".into()];
    s.type_text(" akuo ");
    s.pending();
    s.finish();
    assert_eq!(s.text(), " שלום ");
    SIMULATED_LAYOUT.store(0, Ordering::SeqCst);
    ALLOW_LAYOUT_SWITCH.store(false, Ordering::SeqCst);
    let abbrev = crate::complete::user_path("abbrev.txt").unwrap();
    std::fs::create_dir_all(abbrev.parent().unwrap()).unwrap();
    std::fs::write(&abbrev, "zzpractice = should not expand\n").unwrap();
    crate::complete::reload_user_files();
    s.type_text("zzpractice teh ");
    assert!(!s.engine.lock().is_replacing);
    assert!(s.text().ends_with("zzpractice teh "));
    std::fs::remove_file(&abbrev).unwrap();
    crate::complete::reload_user_files();

    crate::config::Config::update_live(|cfg| cfg.undo_shortcut = "right_ctrl".into());
    let s = Session::new();
    s.type_text("acheive ");
    s.pending();
    s.finish();
    s.tap(Simulated::CTRL_RIGHT);
    s.pending();
    s.finish();
    assert_eq!(s.text(), "acheive ");
    assert_eq!(s.engine.control.undo_count(), 1);
    let s = Session::new();
    s.type_text("recieve ");
    s.pending();
    s.finish();
    s.engine.key_press(Simulated::CTRL_RIGHT);
    s.tap('v');
    s.engine.key_release(Simulated::CTRL_RIGHT);
    assert!(!s.engine.lock().is_replacing);
    assert_eq!(s.engine.control.undo_count(), 0);
    let s = Session::new();
    s.type_text("recieve ");
    s.pending();
    s.finish();
    FOCUS.store(2, Ordering::SeqCst);
    s.tap(Simulated::CTRL_RIGHT);
    assert!(!s.engine.lock().is_replacing);
    assert_eq!(s.engine.control.undo_count(), 0);
    crate::config::Config::update_live(|cfg| cfg.undo_shortcut = "none".into());

    // Practice runs actual layout correction, undo, and completion without learning.
    FOCUS.store(3, Ordering::SeqCst);
    ALLOW_LAYOUT_SWITCH.store(true, Ordering::SeqCst);
    let s = Session::new();
    s.engine
        .control
        .practice_open
        .store(true, Ordering::Relaxed);
    s.type_text("akuo ");
    s.pending();
    s.finish();
    assert_eq!(s.text(), "שלום ");
    assert_eq!(s.engine.control.practice_stage.load(Ordering::Relaxed), 1);
    s.undo();
    s.pending();
    s.finish();
    assert_eq!(s.text(), "akuo ");
    assert_eq!(s.engine.control.practice_stage.load(Ordering::Relaxed), 2);
    assert!(!crate::complete::suppressed("akuo"));
    assert!(!crate::complete::learned("akuo"));
    s.engine.mouse_click();
    s.engine.injector.text.lock().unwrap().clear();
    s.type_text(" keyb");
    s.tap(Simulated::SHIFT_RIGHT);
    s.pending();
    s.finish();
    assert_eq!(s.text(), " keyboard");
    assert_eq!(s.engine.control.practice_stage.load(Ordering::Relaxed), 3);
    assert_eq!(s.engine.control.fixed_count(), 0);
    assert_eq!(s.engine.control.undo_count(), 0);
    ALLOW_LAYOUT_SWITCH.store(false, Ordering::SeqCst);
    FOCUS.store(1, Ordering::SeqCst);

    // Undo layout confirmation runs on the worker, without the typing
    // lock. A click while it stalls must prevent destructive injection.
    ALLOW_LAYOUT_SWITCH.store(true, Ordering::SeqCst);
    SIMULATED_LAYOUT.store(0, Ordering::SeqCst);
    let s = Session::new();
    s.type_text("akuo ");
    s.pending();
    s.finish();
    let (ready, waiting) = mpsc::sync_channel(1);
    let (proceed, reply) = mpsc::sync_channel(1);
    *LAYOUT_GATE.lock().unwrap() = Some(FocusGate {
        ready,
        proceed: reply,
    });
    s.undo();
    waiting.recv_timeout(Duration::from_secs(3)).unwrap();
    assert!(
        s.engine.state.try_lock().is_ok(),
        "layout wait holds the typing lock"
    );
    s.engine.mouse_click();
    proceed.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while s.engine.lock().is_replacing {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(s.text(), "שלום ");
    assert_eq!(s.engine.control.undo_count(), 0);
    assert!(s.ready.try_recv().is_err());
    ALLOW_LAYOUT_SWITCH.store(false, Ordering::SeqCst);
    SIMULATED_LAYOUT.store(0, Ordering::SeqCst);

    // Hold the OS reply indefinitely: releases and clicks still run, and
    // the late reply must not resurrect the canceled word. This used to
    // hold state.lock() for the entire focus query (up to 250 ms on Linux).
    let s = Session::new();
    *lock_forgiving(&s.engine.control.excluded_apps) = vec!["secret.exe".into()];
    let (ready, waiting) = mpsc::sync_channel(1);
    let (proceed, reply) = mpsc::sync_channel(1);
    *FOCUS_GATE.lock().unwrap() = Some(FocusGate {
        ready,
        proceed: reply,
    });
    let engine = Arc::clone(&s.engine);
    let worker = thread::spawn(move || engine.key_press('r'));
    waiting.recv_timeout(Duration::from_secs(3)).unwrap();
    assert!(
        s.engine.state.try_lock().is_ok(),
        "OS query holds the typing lock"
    );
    let started = Instant::now();
    s.engine.key_release('r');
    s.engine.mouse_click();
    eprintln!(
        "release + cancellation during stalled focus query: {:?}",
        started.elapsed()
    );
    proceed.send(()).unwrap();
    worker.join().unwrap();
    assert!(s.engine.lock().keys.is_empty());
    assert!(s.engine.lock().held_keys.is_empty());

    // Exercise both completed and in-flight corrections through real key
    // events. Undo must preserve prior text, separators, and original layout.
    ALLOW_LAYOUT_SWITCH.store(true, Ordering::SeqCst);
    for (typed, completion, corrected) in [
        ("recieve ", false, "receive "),
        ("akuo ", false, "שלום "),
        ("keyb", true, "keyboard"),
    ] {
        for queued in [false, true] {
            crate::complete::unlist(typed.trim());
            SIMULATED_LAYOUT.store(0, Ordering::SeqCst);
            let s = Session::new();
            s.type_text("old ");
            s.type_text(typed);
            if completion {
                s.tap(Simulated::SHIFT_RIGHT);
            }
            s.pending();
            if !queued {
                s.finish();
                assert_eq!(s.text(), format!("old {corrected}"));
            }
            s.tap(Simulated::CTRL_LEFT);
            s.tap(Simulated::CTRL_LEFT);
            if queued {
                assert!(s.engine.lock().pending_conversion);
                s.proceed.send(()).unwrap();
            }
            s.pending();
            s.finish();
            assert_eq!(s.text(), format!("old {typed}"));
            assert_eq!(SIMULATED_LAYOUT.load(Ordering::SeqCst), 0);
            assert!(s.engine.lock().cycle.is_none());
            assert!(s.engine.lock().pending_learning.is_none());
            assert_eq!(s.engine.control.undo_count(), 1);
            crate::complete::unlist(typed.trim());
        }
    }

    // Undo after cycling restores the typed prefix, not the previous offer.
    SIMULATED_LAYOUT.store(0, Ordering::SeqCst);
    let s = Session::new();
    s.type_text("keyb");
    for _ in 0..2 {
        s.tap(Simulated::SHIFT_RIGHT);
        s.pending();
        s.finish();
    }
    assert_ne!(s.text(), "keyb");
    s.tap(Simulated::CTRL_LEFT);
    s.tap(Simulated::CTRL_LEFT);
    s.pending();
    s.finish();
    assert_eq!(s.text(), "keyb");
    assert_eq!(s.engine.control.undo_count(), 1);
    crate::complete::unlist("keyb");

    // A bounded unchanged word after a correction is preserved during undo.
    SIMULATED_LAYOUT.store(0, Ordering::SeqCst);
    let s = Session::new();
    s.type_text("recieve ");
    s.pending();
    s.finish();
    s.type_text("do ");
    s.tap(Simulated::CTRL_LEFT);
    s.tap(Simulated::CTRL_LEFT);
    s.pending();
    s.finish();
    assert_eq!(s.text(), "recieve do ");
    assert_eq!(s.engine.control.undo_count(), 1);
    crate::complete::unlist("recieve");

    // A dedicated undo key never converts an unchanged or ignored word.
    crate::config::Config::update_live(|cfg| {
        cfg.action_shortcut = "left_ctrl".into();
        cfg.undo_shortcut = "right_ctrl".into();
    });
    for word in ["do", "recieve"] {
        SIMULATED_LAYOUT.store(0, Ordering::SeqCst);
        if word == "recieve" {
            crate::complete::suppress(word);
        }
        let s = Session::new();
        s.type_text(&format!("{word} "));
        s.tap(Simulated::CTRL_RIGHT);
        assert_eq!(s.text(), format!("{word} "));
        assert!(!s.engine.lock().is_replacing);
        if word == "recieve" {
            assert!(crate::complete::suppressed(word));
        }
    }
    crate::complete::unlist("recieve");
    crate::config::Config::update_live(|cfg| {
        cfg.action_shortcut = "ctrl".into();
        cfg.undo_shortcut = "none".into();
    });

    // A shared single-tap undo key must undo exactly once on a double tap.
    SIMULATED_LAYOUT.store(0, Ordering::SeqCst);
    crate::config::Config::update_live(|cfg| cfg.undo_shortcut = "left_ctrl".into());
    let s = Session::new();
    s.type_text("recieve ");
    s.pending();
    s.finish();
    s.tap(Simulated::CTRL_LEFT);
    assert!(!s.engine.lock().is_replacing);
    s.tap(Simulated::CTRL_LEFT);
    s.pending();
    s.finish();
    thread::sleep(DOUBLE_TAP_WINDOW + Duration::from_millis(20));
    assert_eq!(s.text(), "recieve ");
    assert_eq!(s.engine.control.undo_count(), 1);
    assert!(!s.engine.lock().is_replacing);
    crate::config::Config::update_live(|cfg| cfg.undo_shortcut = "none".into());

    crate::complete::unlist("recieve");

    // Punctuation and wrappers must not become part of the undo exception.
    for token in ["recieve.", "(recieve)"] {
        SIMULATED_LAYOUT.store(0, Ordering::SeqCst);
        crate::complete::unlist("recieve");
        let s = Session::new();
        s.type_text(&format!("{token} "));
        s.pending();
        s.finish();
        s.undo();
        s.pending();
        s.finish();
        assert_eq!(s.text(), format!("{token} "));
        assert!(crate::complete::suppressed("recieve"));
        s.type_text("recieve ");
        assert_eq!(s.text(), format!("{token} recieve "));
        assert!(!s.engine.lock().is_replacing);
        crate::complete::unlist("recieve");
    }

    // Double taps undo the entire expansion, including multiple words.
    SIMULATED_LAYOUT.store(0, Ordering::SeqCst);
    let abbrev = crate::complete::user_path("abbrev.txt").unwrap();
    std::fs::write(&abbrev, "zzconvert = hello world\n").unwrap();
    crate::complete::reload_user_files();
    assert_eq!(
        crate::complete::expand("ZZCONVERT").as_deref(),
        Some("hello world")
    );
    let s = Session::new();
    s.type_text("zzconvert ");
    s.pending();
    s.finish();
    assert_eq!(s.text(), "hello world ");
    s.tap(Simulated::CTRL_LEFT);
    s.tap(Simulated::CTRL_LEFT);
    s.pending();
    s.finish();
    assert_eq!(s.text(), "zzconvert ");
    assert_eq!(s.engine.control.undo_count(), 1);
    std::fs::remove_file(&abbrev).unwrap();
    crate::complete::reload_user_files();

    // A rule owns its punctuation; additional sentence punctuation survives.
    std::fs::write(
        &abbrev,
        "zzpunct = fallback\nzzpunct. = exact\nc++ = c plus plus\n++ = plus\n() = pair\n(++) = wrapped\n",
    )
    .unwrap();
    crate::complete::reload_user_files();
    for (typed, expected, protected) in [
        ("zzpunct.", "exact", "zzpunct"),
        ("zzpunct.!", "exact!", "zzpunct"),
        ("zzpunct?", "fallback?", "zzpunct"),
        ("c++", "c plus plus", "c"),
        ("++", "plus", "++"),
        ("()", "pair", "()"),
        ("(++)", "wrapped", "(++)"),
    ] {
        crate::complete::unlist(protected);
        let s = Session::new();
        s.type_text(&format!("{typed} "));
        s.pending();
        s.finish();
        assert_eq!(s.text(), format!("{expected} "));
        s.undo();
        s.pending();
        s.finish();
        assert_eq!(s.text(), format!("{typed} "));
        assert!(crate::complete::suppressed(protected));
        s.type_text(&format!("{typed} "));
        assert_eq!(s.text(), format!("{typed} {typed} "));
        assert!(!s.engine.lock().is_replacing);
        crate::complete::unlist(protected);
    }
    std::fs::remove_file(&abbrev).unwrap();
    crate::complete::reload_user_files();

    // Hebrew rules must behave identically through automatic expansion and undo.
    std::fs::write(&abbrev, "קבד = שלום עולם\n").unwrap();
    crate::complete::reload_user_files();
    SIMULATED_LAYOUT.store(1, Ordering::SeqCst);
    let s = Session::new();
    let keys = crate::keymap::hebrew_text_to_physical_english("קבד").unwrap();
    s.type_text(&format!("{keys}/ "));
    s.pending();
    s.finish();
    assert_eq!(s.text(), "שלום עולם. ");
    s.undo();
    s.pending();
    s.finish();
    assert_eq!(s.text(), "קבד. ");
    std::fs::remove_file(&abbrev).unwrap();
    crate::complete::reload_user_files();
    SIMULATED_LAYOUT.store(0, Ordering::SeqCst);

    // An eager double tap undoes the correction after injection finishes.
    SIMULATED_LAYOUT.store(0, Ordering::SeqCst);
    let s = Session::new();
    s.type_text("recieve ");
    s.pending();
    s.tap(Simulated::CTRL_LEFT);
    s.tap(Simulated::CTRL_LEFT);
    assert!(s.engine.lock().pending_conversion);
    s.proceed.send(()).unwrap();
    s.pending();
    s.finish();
    assert_eq!(s.text(), "recieve ");
    assert_eq!(s.engine.control.undo_count(), 1);
    crate::complete::unlist("recieve");

    // A queued double tap loses permission to undo after a click or later
    // typing, just like an immediate gesture on stale text.
    for click in [false, true] {
        SIMULATED_LAYOUT.store(0, Ordering::SeqCst);
        let s = Session::new();
        s.type_text("recieve ");
        s.pending();
        s.tap(Simulated::CTRL_LEFT);
        s.tap(Simulated::CTRL_LEFT);
        assert!(s.engine.lock().pending_undo);
        if click {
            s.engine.mouse_click();
        } else {
            s.type_text("next");
        }
        s.finish();
        assert_eq!(s.text(), if click { "recieve " } else { "receive next" });
        assert_eq!(s.engine.control.undo_count(), 0);
        assert!(!s.engine.lock().pending_undo);
        assert!(s.ready.try_recv().is_err());
    }

    // Manual conversion never removes an exception or needs a dictionary entry.
    SIMULATED_LAYOUT.store(0, Ordering::SeqCst);
    crate::complete::suppress("qzxx");
    let s = Session::new();
    s.type_text("qzxx ");
    s.tap(Simulated::CTRL_LEFT);
    s.tap(Simulated::CTRL_LEFT);
    s.pending();
    s.finish();
    assert_eq!(s.text(), "/זסס ");
    assert!(crate::complete::suppressed("qzxx"));

    // A refused layout switch must not erase anything.
    SIMULATED_LAYOUT.store(0, Ordering::SeqCst);
    REFUSE_LAYOUT_SWITCH.store(true, Ordering::SeqCst);
    let s = Session::new();
    s.type_text("qzxx ");
    s.tap(Simulated::CTRL_LEFT);
    s.tap(Simulated::CTRL_LEFT);
    let deadline = Instant::now() + Duration::from_secs(3);
    while s.engine.lock().is_replacing {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(s.text(), "qzxx ");
    assert!(s.ready.try_recv().is_err());
    REFUSE_LAYOUT_SWITCH.store(false, Ordering::SeqCst);
    // All ambiguous words use the same explicit conversion rule, before
    // or after a terminator, and in either direction.
    ALLOW_LAYOUT_SWITCH.store(true, Ordering::SeqCst);
    for (english, hebrew) in [
        ("do", "גם"),
        ("go", "עם"),
        ("to", "אם"),
        ("zzzzqqqq", "זזזז////"),
    ] {
        for reverse in [false, true] {
            for suffix in ["", " "] {
                SIMULATED_LAYOUT.store(usize::from(reverse), Ordering::SeqCst);
                let s = Session::new();
                s.type_text(&format!("{english}{suffix}"));
                let (before, after) = if reverse {
                    (hebrew, english)
                } else {
                    (english, hebrew)
                };
                *s.engine.injector.text.lock().unwrap() = format!("{before}{suffix}");
                assert!(!s.engine.lock().is_replacing);
                s.tap(Simulated::CTRL_LEFT);
                s.tap(Simulated::CTRL_LEFT);
                s.pending();
                s.finish();
                assert_eq!(s.text(), format!("{after}{suffix}"));
                s.tap(Simulated::CTRL_LEFT);
                s.tap(Simulated::CTRL_LEFT);
                s.pending();
                s.finish();
                assert_eq!(s.text(), format!("{before}{suffix}"));
                assert!(!crate::complete::suppressed(before));
            }
        }
    }
    SIMULATED_LAYOUT.store(0, Ordering::SeqCst);
    crate::config::Config::update_live(|cfg| cfg.undo_shortcut = "left_ctrl".into());
    let s = Session::new();
    s.type_text("do ");
    s.tap(Simulated::CTRL_LEFT);
    assert!(!s.engine.lock().is_replacing);
    s.tap(Simulated::CTRL_LEFT);
    s.pending();
    s.finish();
    assert_eq!(s.text(), "גם ");
    s.tap(Simulated::CTRL_LEFT);
    s.pending();
    s.finish();
    assert_eq!(s.text(), "do ");
    crate::config::Config::update_live(|cfg| cfg.undo_shortcut = "none".into());
    for interruption in ["click", "focus", "disabled", "secure"] {
        let s = Session::new();
        s.type_text(if interruption == "none" {
            "zzzzqqqq "
        } else {
            "do "
        });
        let original = s.text();
        match interruption {
            "click" => s.engine.mouse_click(),
            "focus" => {
                FOCUS.store(2, Ordering::SeqCst);
            }
            "disabled" => s.engine.control.set_enabled(false),
            "secure" => INPUT_ALLOWED.store(false, Ordering::SeqCst),
            _ => {}
        }
        s.tap(Simulated::CTRL_LEFT);
        s.tap(Simulated::CTRL_LEFT);
        // No native selection exists, so a selection query is harmless.
        let deadline = Instant::now() + Duration::from_secs(3);
        while s.engine.lock().is_replacing {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(s.text(), original);
        assert!(s.ready.try_recv().is_err());
        FOCUS.store(1, Ordering::SeqCst);
        INPUT_ALLOWED.store(true, Ordering::SeqCst);
    }
    for interruption in ["none", "click", "typing", "focus", "selection"] {
        let s = Session::new();
        let original = "prefix AKUO GUKO 🙂 suffix";
        *s.engine.injector.text.lock().unwrap() = original.into();
        *SELECTION.lock().unwrap() = Some(Selection {
            start: 7,
            length: 9,
            text: "AKUO GUKO".into(),
        });
        s.engine.mouse_click();
        s.tap(Simulated::CTRL_LEFT);
        s.tap(Simulated::CTRL_LEFT);
        s.pending();
        match interruption {
            "click" => s.engine.mouse_click(),
            "typing" => s.engine.key_press('x'),
            "focus" => {
                FOCUS.store(2, Ordering::SeqCst);
            }
            "selection" => {
                *SELECTION.lock().unwrap() = None;
            }
            _ => {}
        }
        s.finish();
        if interruption == "none" {
            assert_eq!(s.text(), "prefix שלום עולם 🙂 suffix");
            s.tap(Simulated::CTRL_LEFT);
            s.tap(Simulated::CTRL_LEFT);
            s.pending();
            s.finish();
            assert_eq!(s.text(), original);
        } else {
            assert_eq!(s.text(), original);
        }
        FOCUS.store(1, Ordering::SeqCst);
        *SELECTION.lock().unwrap() = None;
    }
    // Rebinding exercises the same screen/injection path as the defaults.
    for (binding, action_key, completion_binding, completion_key) in [
        (
            "left_shift",
            Simulated::SHIFT_LEFT,
            "right_ctrl",
            Simulated::CTRL_RIGHT,
        ),
        (
            "right_shift",
            Simulated::SHIFT_RIGHT,
            "left_ctrl",
            Simulated::CTRL_LEFT,
        ),
        (
            "left_ctrl",
            Simulated::CTRL_LEFT,
            "left_shift",
            Simulated::SHIFT_LEFT,
        ),
        (
            "right_ctrl",
            Simulated::CTRL_RIGHT,
            "right_shift",
            Simulated::SHIFT_RIGHT,
        ),
    ] {
        crate::config::Config::update_live(|cfg| {
            cfg.correction_shortcut = "none".into();
            cfg.action_shortcut = binding.into();
            cfg.completion_shortcut = completion_binding.into();
            cfg.undo_shortcut = "none".into();
            cfg.correction_shortcut = "none".into();
        });
        SIMULATED_LAYOUT.store(0, Ordering::SeqCst);
        let s = Session::new();
        s.type_text("keyb");
        // A held key, a repeated press, or a chord must not complete.
        s.engine.key_press(completion_key);
        s.engine.lock().completion_down = Some(Instant::now() - TAP_MAX - Duration::from_millis(1));
        s.engine.key_release(completion_key);
        assert!(!s.engine.lock().is_replacing);
        s.engine.key_press(completion_key);
        s.engine.key_press(completion_key);
        s.engine.key_release(completion_key);
        assert!(!s.engine.lock().is_replacing);
        s.tap(completion_key);
        s.pending();
        s.finish();
        assert_eq!(s.text(), "keyboard");
        s.tap(action_key);
        assert!(!s.engine.lock().is_replacing);
        s.tap(action_key);
        s.pending();
        s.finish();
        assert_eq!(s.text(), "keyb");
        assert_eq!(s.engine.control.undo_count(), 1);
        crate::complete::unlist("keyb");
        SIMULATED_LAYOUT.store(0, Ordering::SeqCst);

        let s = Session::new();
        s.type_text("do ");
        s.engine.key_press(action_key);
        s.engine.lock().action_down = Some(Instant::now() - TAP_MAX - Duration::from_millis(1));
        s.engine.key_release(action_key);
        s.tap(action_key);
        assert!(!s.engine.lock().is_replacing);
        s.tap(action_key);
        s.pending();
        s.finish();
        assert_eq!(s.text(), "גם ");
        s.tap(action_key);
        s.tap(action_key);
        s.pending();
        s.finish();
        assert_eq!(s.text(), "do ");

        let s = Session::new();
        s.type_text("keyb");
        s.engine.key_press(completion_key);
        s.engine.key_press(action_key);
        s.engine.key_release(action_key);
        s.engine.key_release(completion_key);
        assert!(!s.engine.lock().is_replacing);
        assert!(s.ready.try_recv().is_err());
    }
    // Disable both shortcuts, then ensure changing a binding mid-tap
    // cannot reinterpret an old key-down or the first half of a pair.
    crate::config::Config::update_live(|cfg| {
        cfg.action_shortcut = "none".into();
        cfg.completion_shortcut = "none".into();
    });
    let s = Session::new();
    s.type_text("keyb");
    for key in [
        Simulated::CTRL_LEFT,
        Simulated::CTRL_RIGHT,
        Simulated::SHIFT_LEFT,
        Simulated::SHIFT_RIGHT,
    ] {
        s.tap(key);
        s.tap(key);
    }
    assert!(!s.engine.lock().is_replacing);
    s.engine.key_press(Simulated::SHIFT_LEFT);
    crate::config::Config::update_live(|cfg| cfg.completion_shortcut = "left_shift".into());
    s.engine.key_release(Simulated::SHIFT_LEFT);
    assert!(!s.engine.lock().is_replacing);
    crate::config::Config::update_live(|cfg| cfg.action_shortcut = "ctrl".into());
    let s = Session::new();
    s.type_text("do ");
    s.tap(Simulated::CTRL_LEFT);
    crate::config::Config::update_live(|cfg| cfg.action_shortcut = "left_ctrl".into());
    s.tap(Simulated::CTRL_LEFT);
    assert!(!s.engine.lock().is_replacing);
    s.tap(Simulated::CTRL_LEFT);
    s.pending();
    s.finish();
    assert_eq!(s.text(), "גם ");
    crate::config::Config::update_live(|cfg| {
        cfg.action_shortcut = "ctrl".into();
        cfg.completion_shortcut = "right_shift".into();
        cfg.correction_shortcut = "left_shift".into();
    });
    ALLOW_LAYOUT_SWITCH.store(false, Ordering::SeqCst);
}

#[test]
fn explicit_correction_is_separate_and_restores_original_text() {
    const CHILD: &str = "RECAST_EXPLICIT_CORRECTION_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "platform::engine::tests::explicit_correction_is_separate_and_restores_original_text", "--nocapture"])
            .env(CHILD, "1")
            .output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    crate::config::Config::update_live(|config| {
        config.complete_enabled = false;
        config.spell_enabled = false;
        config.spell_min_len = 10;
        config.spell_max_dist = 0;
        config.correction_shortcut = "left_shift".into();
    });
    for (original, wanted) in [
        ("hte", "the"),
        ("form", "from"),
        ("recieve's.", "receive's."),
    ] {
        let session = Session::new();
        session.type_text(original);
        session.tap(Simulated::SHIFT_LEFT);
        session.pending();
        let choices = session
            .engine
            .lock()
            .cycle
            .as_ref()
            .unwrap()
            .candidates
            .clone();
        let index = choices
            .iter()
            .position(|word| word == wanted)
            .unwrap_or_else(|| panic!("{wanted} missing from {choices:?}"));
        session.finish();
        for _ in 0..index {
            session.tap(Simulated::SHIFT_LEFT);
            session.pending();
            session.finish();
        }
        assert_eq!(session.text(), wanted);
        // Enter accepts the selected word, then clears the field context without replay.
        session.type_text("\n");
        assert_eq!(session.text(), format!("{wanted}\n"));
        assert!(!session.engine.lock().is_replacing);
        assert!(session.engine.lock().last_action.is_none());
        assert!(session.engine.lock().pending_learning.is_none());
    }
    let session = Session::new();
    session.type_text("hte");
    session.tap(Simulated::SHIFT_LEFT);
    session.pending();
    let choices = session
        .engine
        .lock()
        .cycle
        .as_ref()
        .unwrap()
        .candidates
        .len();
    session.finish();
    let restore_taps = choices + (choices - 1) / crate::complete::MAX_CANDIDATES;
    for _ in 0..restore_taps {
        session.tap(Simulated::SHIFT_LEFT);
        session.pending();
        session.finish();
    }
    assert_eq!(session.text(), "hte");
    assert!(session.engine.lock().last_action.is_none());
    let session = Session::new();
    session.type_text("hte");
    session.tap(Simulated::SHIFT_RIGHT);
    assert!(
        !session.engine.lock().is_replacing,
        "disabled completion must stay separate"
    );
    session.tap(Simulated::SHIFT_LEFT);
    session.pending();
    session.finish();
    session.tap(Simulated::SHIFT_RIGHT);
    assert!(session.engine.lock().cycle.as_ref().unwrap().correction);
    session.undo();
    session.pending();
    session.finish();
    assert_eq!(session.text(), "hte");
    crate::config::Config::update_live(|config| {
        config.complete_enabled = true;
        config.complete_min_len = 100;
    });
    let session = Session::new();
    session.type_text("hte");
    session.tap(Simulated::SHIFT_LEFT);
    session.pending();
    session.finish();
    let selected = session.text();
    session.engine.lock().completion_use = Some(true);
    session.tap(Simulated::SHIFT_RIGHT);
    assert!(!session.engine.lock().is_replacing);
    assert!(session.engine.lock().cycle.as_ref().unwrap().correction);
    assert_eq!(
        session.engine.lock().pending_learning.as_ref().unwrap().to,
        selected
    );
    assert_eq!(session.engine.lock().completion_use, Some(true));
    session.type_text(" ");
    assert_eq!(session.text(), format!("{selected} "));
    assert!(!session.engine.lock().is_replacing);
    crate::config::Config::update_live(|config| {
        config.complete_min_len = crate::config::DEFAULT_COMPLETE_MIN_LEN;
        config.personal_enabled = true;
    });
    for _ in 0..2 {
        let session = Session::new();
        session.type_text("form");
        session.tap(Simulated::SHIFT_LEFT);
        session.pending();
        let index = session
            .engine
            .lock()
            .cycle
            .as_ref()
            .unwrap()
            .candidates
            .iter()
            .position(|word| word == "from")
            .unwrap();
        session.finish();
        for _ in 0..index {
            session.tap(Simulated::SHIFT_LEFT);
            session.pending();
            session.finish();
        }
        session.type_text(" ");
    }
    assert_eq!(crate::personal::personal_correction("form"), None);
    crate::config::Config::update_live(|config| config.personal_enabled = false);
    for (original, first, second) in [
        ("keyb", Simulated::SHIFT_RIGHT, Simulated::SHIFT_LEFT),
        ("hte", Simulated::SHIFT_LEFT, Simulated::SHIFT_RIGHT),
    ] {
        let session = Session::new();
        session.type_text(original);
        session.tap(first);
        session.pending();
        session.finish();
        session.tap(second);
        session.pending();
        let choices = session
            .engine
            .lock()
            .cycle
            .as_ref()
            .unwrap()
            .candidates
            .len()
            .min(crate::complete::MAX_CANDIDATES);
        session.finish();
        for _ in 0..choices {
            session.tap(second);
            session.pending();
            session.finish();
        }
        assert_eq!(
            session.text(),
            original,
            "switching gestures must preserve {original}"
        );
    }
    // Accepting a phrase honors the explicit choice instead of correcting its tail.
    crate::config::Config::update_live(|config| {
        config.spell_enabled = true;
        config.spell_min_len = crate::config::DEFAULT_SPELL_MIN_LEN;
        config.spell_max_dist = crate::config::DEFAULT_SPELL_MAX_DIST;
        config.rule_stats_enabled = true;
    });
    let session = Session::new();
    session.type_text("btw");
    {
        let mut state = session.engine.lock();
        state.cycle = Some(Cycle {
            preview: false,
            preview_ready: false,
            typed: state.keys.to_vec(),
            trailing_spaces: Vec::new(),
            candidates: vec!["by the waay".into()],
            candidate_langs: vec![Language::English],
            index: 1,
            resume_index: 0,
            on_screen: 3,
            lang: Language::English,
            original_lang: Language::English,
            correction: false,
        });
    }
    let accepted = crate::personal::usefulness_counts()[2];
    session.tap(Simulated::SHIFT_RIGHT);
    session.pending();
    session.finish();
    session.type_text(" ");
    assert_eq!(session.text(), "by the waay ");
    assert!(!session.engine.lock().is_replacing);
    assert_eq!(crate::personal::usefulness_counts()[2], accepted + 1);
    session.undo();
    session.pending();
    session.finish();
    assert_eq!(session.text(), "btw ");
}

#[test]
fn suggestion_search_keeps_capture_responsive_and_cancels_stale_work() {
    const CHILD: &str = "RECAST_SUGGESTION_WORKER_TEST";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "platform::engine::tests::suggestion_search_keeps_capture_responsive_and_cancels_stale_work"])
            .env(CHILD, "1").output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    crate::config::Config::update_live(|cfg| {
        cfg.action_shortcut = "left_ctrl".into();
        cfg.undo_shortcut = "right_ctrl".into();
        cfg.completion_shortcut = "right_shift".into();
        cfg.correction_shortcut = "left_shift".into();
    });
    for cancel in [
        "typing",
        "navigation",
        "click",
        "focus",
        "space",
        "binding",
        "none",
    ] {
        let s = Session::new();
        s.type_text(if cancel == "space" { "keyb " } else { "keyb" });
        s.engine.key_press(Simulated::SHIFT_RIGHT);
        let (ready, waiting) = mpsc::sync_channel(1);
        let (proceed, reply) = mpsc::sync_channel(1);
        *FOCUS_GATE.lock().unwrap() = Some(FocusGate {
            ready,
            proceed: reply,
        });
        // This must return before the focus service replies, on every backend.
        s.engine.key_release(Simulated::SHIFT_RIGHT);
        waiting.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(s.engine.state.try_lock().is_ok());
        // A second tap must not invalidate the first search or disappear.
        s.engine.key_press(Simulated::SHIFT_RIGHT);
        s.engine.key_release(Simulated::SHIFT_RIGHT);
        match cancel {
            "typing" => s.type_text("x"),
            "navigation" => s.engine.key_press('\x10'),
            "click" => s.engine.mouse_click(),
            "focus" => {
                FOCUS.store(2, Ordering::SeqCst);
            }
            "space" => s.type_text(" "),
            "binding" => {
                crate::config::Config::update_live(|cfg| cfg.completion_shortcut = "none".into());
                s.engine
                    .lock()
                    .sync_shortcuts(&crate::config::Config::global());
            }
            _ => (),
        }
        proceed.send(()).unwrap();
        s.suggestions_ready();
        if cancel == "none" {
            s.pending();
            assert_eq!(s.engine.lock().cycle.as_ref().unwrap().index, 0);
            s.proceed.send(()).unwrap();
            s.pending();
            assert_eq!(s.engine.lock().cycle.as_ref().unwrap().index, 1);
            s.finish();
            s.undo();
            s.pending();
            s.finish();
            assert_eq!(s.text(), "keyb");
        } else {
            assert!(!s.engine.lock().is_replacing, "stale suggestion: {cancel}");
            assert!(s.ready.try_recv().is_err(), "stale injection: {cancel}");
            assert_eq!(
                s.text(),
                if cancel == "typing" {
                    "keybx"
                } else if cancel == "space" {
                    "keyb  "
                } else {
                    "keyb"
                }
            );
        }
        FOCUS.store(1, Ordering::SeqCst);
        crate::config::Config::update_live(|cfg| cfg.completion_shortcut = "right_shift".into());
    }
}

#[test]
fn cycling_to_original_keeps_that_word_without_disabling_the_next() {
    const CHILD: &str = "RECAST_KEEP_ORIGINAL_TEST";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "platform::engine::tests::cycling_to_original_keeps_that_word_without_disabling_the_next",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    for key in [Simulated::SHIFT_LEFT, Simulated::SHIFT_RIGHT] {
        for boundary in ["unfinished", "finished", "buffered"] {
            let s = Session::new();
            crate::config::Config::update_live(|config| {
                config.spell_enabled = boundary != "finished";
            });
            s.type_text(if boundary == "finished" {
                "recieve "
            } else {
                "recieve"
            });
            crate::config::Config::update_live(|config| config.spell_enabled = true);
            s.tap(key);
            s.pending();
            s.finish();
            let choices = s
                .engine
                .lock()
                .cycle
                .as_ref()
                .unwrap()
                .candidates
                .len()
                .min(crate::complete::MAX_CANDIDATES);
            for step in 0..choices {
                s.tap(key);
                s.pending();
                if boundary == "buffered" && step + 1 == choices {
                    // Replay a Space that reached the field while the original was returning.
                    s.engine.injector.text.lock().unwrap().push(' ');
                    s.engine.lock().buffered_keys.push(Typed {
                        key: ' ',
                        shift: false,
                    });
                }
                s.finish();
            }
            assert_eq!(
                s.text(),
                if boundary == "unfinished" {
                    "recieve"
                } else {
                    "recieve "
                }
            );
            assert!(!crate::complete::suppressed("recieve"));
            assert!(!crate::complete::learned("recieve"));
            if boundary == "unfinished" {
                s.type_text(" ");
                assert!(
                    !s.engine.lock().is_replacing,
                    "Space overrode the original choice"
                );
                assert!(s.ready.try_recv().is_err());
                assert_eq!(s.text(), "recieve ");
            }
            // Keeping one occurrence must not suppress automatic repair elsewhere.
            s.type_text("recieve ");
            s.pending();
            s.finish();
            assert_eq!(s.text(), "recieve receive ");
        }
    }
}

#[test]
fn spelling_preview_is_passive_and_commits_only_on_trusted_typing() {
    const CHILD: &str = "RECAST_PREVIEW_TEST";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "platform::engine::tests::spelling_preview_is_passive_and_commits_only_on_trusted_typing"])
            .env(CHILD, "1").output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    crate::config::Config::update_live(|cfg| {
        cfg.action_shortcut = "left_ctrl".into();
        cfg.undo_shortcut = "right_ctrl".into();
        cfg.correction_shortcut = "left_shift".into();
        cfg.completion_shortcut = "right_shift".into();
    });
    let preview = |s: &Session| {
        s.tap(Simulated::SHIFT_LEFT);
        let deadline = Instant::now() + Duration::from_secs(3);
        while s.engine.lock().suggestion_worker {
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        assert!(s.engine.lock().cycle.as_ref().unwrap().preview_ready);
    };
    // Completion may be bound to Left Shift even on a preview-capable backend.
    crate::config::Config::update_live(|cfg| {
        cfg.correction_shortcut = "right_shift".into();
        cfg.completion_shortcut = "left_shift".into();
    });
    let completion = Session::new();
    completion.type_text("ther");
    for _ in 0..24 {
        preview(&completion);
        assert_eq!(completion.text(), "ther");
        assert_eq!(completion.engine.control.fixed_count(), 0);
        assert!(completion.ready.try_recv().is_err());
    }
    // Choose an actual offer, then accept exactly once and restore the prefix.
    loop {
        let original = {
            let st = completion.engine.lock();
            let cycle = st.cycle.as_ref().unwrap();
            cycle.index >= cycle.candidates.len()
        };
        if !original {
            break;
        }
        preview(&completion);
    }
    let expected = {
        let st = completion.engine.lock();
        let cycle = st.cycle.as_ref().unwrap();
        cycle.candidates[cycle.index].clone()
    };
    completion.type_text(" ");
    completion.pending();
    completion.finish();
    assert_eq!(completion.text(), format!("{expected} "));
    completion.undo();
    completion.pending();
    completion.finish();
    assert_eq!(completion.text(), "ther ");
    crate::complete::unlist("ther");
    crate::config::Config::update_live(|cfg| {
        cfg.correction_shortcut = "left_shift".into();
        cfg.completion_shortcut = "right_shift".into();
    });
    for suffix in [" ", "x", ".", "  "] {
        let s = Session::new();
        s.type_text("helo");
        preview(&s);
        assert_eq!(s.text(), "helo");
        assert_eq!(s.engine.control.fixed_count(), 0);
        assert!(s.engine.lock().pending_learning.is_none());
        preview(&s);
        let chosen = {
            let st = s.engine.lock();
            let cycle = st.cycle.as_ref().unwrap();
            cycle.candidates[cycle.index].clone()
        };
        assert_eq!(s.text(), "helo");
        s.type_text(suffix);
        s.pending();
        s.finish();
        assert_eq!(s.text(), format!("{chosen}{suffix}"));
        assert_eq!(s.engine.control.fixed_count(), 1);
        if suffix.chars().all(char::is_whitespace) {
            s.undo();
            s.pending();
            s.finish();
            assert_eq!(s.text(), format!("helo{suffix}"));
        }
    }
    crate::complete::unlist("helo");
    let s = Session::new();
    s.type_text("helo");
    preview(&s);
    loop {
        let original = {
            let st = s.engine.lock();
            let c = st.cycle.as_ref().unwrap();
            c.index == c.candidates.len()
        };
        if original {
            break;
        }
        preview(&s);
    }
    assert_eq!(s.text(), "helo");
    s.type_text("x");
    assert_eq!(s.text(), "helox");
    assert_eq!(s.engine.control.fixed_count(), 0);
    assert!(s.engine.lock().pending_learning.is_none());
    assert!(!crate::complete::suppressed("helo"));
    assert!(!crate::complete::learned("helo"));
    assert!(s.ready.try_recv().is_err());
    let original_space = Session::new();
    original_space.type_text("helo");
    preview(&original_space);
    loop {
        let original = {
            let st = original_space.engine.lock();
            let c = st.cycle.as_ref().unwrap();
            c.index >= c.candidates.len()
        };
        if original {
            break;
        }
        preview(&original_space);
    }
    original_space.type_text(" ");
    assert_eq!(original_space.text(), "helo ");
    assert!(original_space.ready.try_recv().is_err());
    assert_eq!(original_space.engine.control.fixed_count(), 0);
    for cancel in ['\n', '\x1b', '\x10', '\x08', Simulated::CTRL_LEFT] {
        let s = Session::new();
        s.type_text("helo");
        preview(&s);
        s.tap(cancel);
        assert!(s.engine.lock().cycle.is_none());
        assert_eq!(s.text(), "helo");
        assert!(s.ready.try_recv().is_err());
    }
    let s = Session::new();
    s.type_text("helo");
    preview(&s);
    let generation = s.engine.lock().generation;
    s.engine.mouse_click();
    assert!(!s.engine.preview_ready(generation));
    assert_eq!(s.text(), "helo");
    let spaced = Session::new();
    spaced.type_text("hello  ");
    preview(&spaced);
    let chosen = {
        let st = spaced.engine.lock();
        let c = st.cycle.as_ref().unwrap();
        c.candidates[c.index].clone()
    };
    assert_eq!(spaced.text(), "hello  ");
    spaced.type_text("x");
    spaced.pending();
    spaced.finish();
    assert_eq!(spaced.text(), format!("{chosen}  x"));
    // Rapid requests are drained in order, without replacing the source.
    let rapid = Session::new();
    rapid.type_text("helo");
    preview(&rapid);
    let (mut expected_index, mut expected_resume, count) = {
        let st = rapid.engine.lock();
        let cycle = st.cycle.as_ref().unwrap();
        (cycle.index, cycle.resume_index, cycle.candidates.len())
    };
    for _ in 0..8 {
        if expected_index >= count {
            expected_index = expected_resume;
        } else if expected_index + 1 == count
            || (expected_index + 1) % crate::complete::MAX_CANDIDATES == 0
        {
            expected_resume = (expected_index + 1) % count;
            expected_index = count;
        } else {
            expected_index += 1;
        }
        rapid.tap(Simulated::SHIFT_LEFT);
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    while rapid.engine.lock().suggestion_worker {
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    assert!(rapid.engine.lock().cycle.as_ref().unwrap().preview);
    assert_eq!(
        rapid.engine.lock().cycle.as_ref().unwrap().index,
        expected_index
    );
    assert_eq!(rapid.text(), "helo");
    assert_eq!(rapid.engine.control.fixed_count(), 0);
    assert!(rapid.ready.try_recv().is_err());
    for blocked in [false, true] {
        let s = Session::new();
        s.type_text("helo");
        preview(&s);
        if blocked {
            INPUT_ALLOWED.store(false, Ordering::SeqCst);
        } else {
            s.engine.control.pause_for(Duration::from_secs(60));
        }
        s.type_text("x");
        assert_eq!(s.text(), "helox");
        assert!(s.ready.try_recv().is_err());
        assert!(s.engine.lock().cycle.is_none());
        INPUT_ALLOWED.store(true, Ordering::SeqCst);
    }
    let s = Session::new();
    s.type_text("helo");
    preview(&s);
    FOCUS.store(2, Ordering::SeqCst);
    s.type_text("x");
    assert_eq!(s.text(), "helox");
    assert!(s.ready.try_recv().is_err());
}

mod dictionary_typing;

#[test]
fn preparation_and_unedited_failure_do_not_leave_layout_only_corrections() {
    const CHILD: &str = "RECAST_PREPARATION_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "platform::engine::tests::preparation_and_unedited_failure_do_not_leave_layout_only_corrections"])
            .env(CHILD, "1").output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    ALLOW_LAYOUT_SWITCH.store(true, Ordering::SeqCst);
    for preparation in [false, true] {
        PREPARE_ALLOWED.store(preparation, Ordering::SeqCst);
        EDIT_STARTED.store(false, Ordering::SeqCst);
        SIMULATED_LAYOUT.store(0, Ordering::SeqCst);
        let s = Session::new();
        s.type_text("akuo ");
        s.checks_ready();
        s.idle();
        assert_eq!(s.text(), "akuo ");
        assert_eq!(SIMULATED_LAYOUT.load(Ordering::SeqCst), 0);
        assert!(s.engine.injector.erasures.lock().unwrap().is_empty());
        assert!(matches!(
            s.engine.lock().last_action,
            Some(LastAction::Unchanged { .. })
        ));
        assert!(!s.engine.lock().no_fix);
    }
    // A proven pre-edit failure must not suppress the next ordinary word.
    PREPARE_ALLOWED.store(true, Ordering::SeqCst);
    EDIT_STARTED.store(true, Ordering::SeqCst);
    let s = Session::new();
    s.type_text("recieve ");
    s.pending();
    s.finish();
    assert_eq!(s.text(), "receive ");
}

#[cfg(target_os = "linux")]
#[test]
fn fetched_input_batches_must_finish_before_replay_is_frozen() {
    const CHILD: &str = "RECAST_BATCH_CAPTURE_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "platform::engine::tests::fetched_input_batches_must_finish_before_replay_is_frozen"])
            .env(CHILD, "1").output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let s = Session::new();
    s.engine.lock().focus = Some(1);
    let generation = s.engine.lock().generation;
    assert!(s.engine.replay_valid(&[], generation, 0));
    CAPTURE_PENDING.store(true, Ordering::SeqCst);
    assert!(!s.engine.wait_for_capture(generation, Duration::ZERO));
    assert!(s
        .engine
        .replay_snapshot(&[], generation, Duration::ZERO)
        .is_none());
    assert!(!s.engine.replay_valid(&[], generation, 0));
    // Finish dispatching keys that the app already received before the reader woke.
    s.engine.lock().buffered_keys.push(Typed {
        key: ' ',
        shift: false,
    });
    CAPTURE_PENDING.store(false, Ordering::SeqCst);
    assert!(s.engine.wait_for_capture(generation, Duration::ZERO));
    let snapshot = s
        .engine
        .replay_snapshot(&[], generation, Duration::ZERO)
        .unwrap();
    assert_eq!(snapshot.len(), 1);
    assert_eq!(snapshot[0].key, ' ');
    assert!(s.engine.replay_valid(&[], generation, 1));
    s.engine.forget_everything();
    assert!(!s.engine.wait_for_capture(generation, Duration::ZERO));
}

#[test]
fn automatic_correction_waits_for_a_partially_dispatched_space() {
    const CHILD: &str = "RECAST_PARTIAL_SPACE_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "platform::engine::tests::automatic_correction_waits_for_a_partially_dispatched_space"])
            .env(CHILD, "1")
            .env("RECAST_INJECT_HELD_TIMEOUT", "2000000")
            .output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    for during_focus in [false, true] {
        ALLOW_LAYOUT_SWITCH.store(true, Ordering::SeqCst);
        SIMULATED_LAYOUT.store(0, Ordering::SeqCst);
        let s = Session::new();
        s.type_text("akuo");
        let (ready, reached) = mpsc::sync_channel(0);
        let (proceed, resume) = mpsc::channel();
        *CHECK_GATE.lock().unwrap() = Some(FocusGate {
            ready,
            proceed: resume,
        });
        s.type_while_checking(" ");
        reached.recv_timeout(Duration::from_secs(3)).unwrap();
        let worker_resume = if during_focus {
            let (ready, reached) = mpsc::sync_channel(0);
            let (focus_proceed, resume) = mpsc::channel();
            *FOCUS_GATE.lock().unwrap() = Some(FocusGate {
                ready,
                proceed: resume,
            });
            proceed.send(()).unwrap();
            reached.recv_timeout(Duration::from_secs(3)).unwrap();
            focus_proceed
        } else {
            proceed
        };
        CAPTURE_PENDING.store(true, Ordering::SeqCst);
        CAPTURE_OBSERVED.store(false, Ordering::SeqCst);
        let (focus_ready, focus_reached) = mpsc::sync_channel(0);
        let (focus_proceed, focus_resume) = mpsc::channel();
        *FOCUS_GATE.lock().unwrap() = Some(FocusGate {
            ready: focus_ready,
            proceed: focus_resume,
        });
        let engine = Arc::clone(&s.engine);
        let writer = thread::spawn(move || {
            // The physical reader has counted this press but is still waiting
            // on focus before adding its Space to the tracked deletion tail.
            engine.key_press(' ');
            engine.injector.text.lock().unwrap().push(' ');
            engine.key_release(' ');
            CAPTURE_PENDING.store(false, Ordering::SeqCst);
        });
        focus_reached.recv_timeout(Duration::from_secs(3)).unwrap();
        worker_resume.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while !CAPTURE_OBSERVED.load(Ordering::SeqCst) {
            assert!(
                Instant::now() < deadline,
                "search discarded the partially captured Space"
            );
            thread::sleep(Duration::from_millis(1));
        }
        focus_proceed.send(()).unwrap();
        writer.join().unwrap();
        s.checks_ready();
        s.pending();
        s.finish();
        assert_eq!(s.text(), "שלום  ");
    }
}

#[test]
fn repeated_undos_do_not_save_word_exceptions() {
    const CHILD: &str = "RECAST_NO_UNDO_LEARNING_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "platform::engine::tests::repeated_undos_do_not_save_word_exceptions",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        return;
    }
    let path = crate::complete::user_path("learned.txt").unwrap();
    for _ in 0..3 {
        crate::complete::unlist("recieve");
        let before = std::fs::read(&path).ok();
        let s = Session::new();
        s.type_text("recieve ");
        s.pending();
        s.finish();
        s.undo();
        s.pending();
        s.finish();
        assert_eq!(s.text(), "recieve ");
        assert!(crate::complete::suppressed("recieve"));
        assert!(!crate::complete::learned("recieve"));
        assert_eq!(std::fs::read(&path).ok(), before);
    }
}

#[test]
fn rapid_finished_words_each_get_checked() {
    const CHILD: &str = "RECAST_RAPID_WORDS_TEST";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "platform::engine::tests::rapid_finished_words_each_get_checked",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    for (first, middle) in [
        ("keyboard", String::new()),
        ("recieve", String::new()),
        ("keyboard", "a ".repeat(40)),
    ] {
        let s = Session::new();
        let (ready, reached) = mpsc::sync_channel(0);
        let (proceed, resume) = mpsc::channel();
        *CHECK_GATE.lock().unwrap() = Some(FocusGate {
            ready,
            proceed: resume,
        });
        s.type_while_checking(&format!("{first} "));
        reached.recv_timeout(Duration::from_secs(3)).unwrap();
        s.type_while_checking(&format!("{middle}keyboad recieve "));
        assert!(s.engine.lock().word_checks.len() <= super::word_buffer::MAX_UNDO_SUFFIX);
        proceed.send(()).unwrap();
        let count = if first == "recieve" { 3 } else { 2 };
        for _ in 0..count {
            s.pending();
            s.proceed.send(()).unwrap();
        }
        s.idle();
        s.checks_ready();
        assert_eq!(
            s.text(),
            format!(
                "{} {middle}keyboard receive ",
                if first == "recieve" { "receive" } else { first }
            )
        );
        assert_eq!(s.engine.control.fixed_count(), count);
    }
    for cancel in [
        "enter",
        "click",
        "navigation",
        "focus",
        "layout",
        "secure",
        "disabled",
    ] {
        let s = Session::new();
        let (ready, reached) = mpsc::sync_channel(0);
        let (proceed, resume) = mpsc::channel();
        *CHECK_GATE.lock().unwrap() = Some(FocusGate {
            ready,
            proceed: resume,
        });
        s.type_while_checking("recieve ");
        reached.recv_timeout(Duration::from_secs(3)).unwrap();
        s.type_while_checking("keyboad recieve ");
        match cancel {
            "enter" => s.type_while_checking("\n"),
            "click" => s.engine.mouse_click(),
            "navigation" => s.tap('\x10'),
            "focus" => FOCUS.store(2, Ordering::SeqCst),
            "layout" => SIMULATED_LAYOUT.store(1, Ordering::SeqCst),
            "secure" => INPUT_ALLOWED.store(false, Ordering::SeqCst),
            _ => {
                s.engine.control.set_enabled(false);
            }
        }
        let expected = s.text();
        proceed.send(()).unwrap();
        s.checks_ready();
        assert!(s.ready.try_recv().is_err(), "{cancel}");
        assert_eq!(s.text(), expected, "{cancel}");
        assert_eq!(s.engine.control.fixed_count(), 0, "{cancel}");
        FOCUS.store(1, Ordering::SeqCst);
        SIMULATED_LAYOUT.store(0, Ordering::SeqCst);
        INPUT_ALLOWED.store(true, Ordering::SeqCst);
    }
}
