//! Every delay the correction path takes, in one place.
//!
//! Injecting a correction is not one action but a short sequence — wait for the
//! user to lift the key that triggered it, erase what they typed, type the
//! replacement, replay anything they got in meanwhile, then stop ignoring our
//! own output — and every step needs the one before it to have landed. The
//! operating system offers no way to be told that it has, so each gap is a
//! guess: long enough that the event is not dropped or coalesced, short enough
//! that the user does not watch their word being rewritten.
//!
//! Those guesses used to be five unrelated literals spread across three
//! platform modules — 1 ms here, 2 ms there, a bare `from_micros(100)` in a
//! loop — with no way to tell which were measured and which were the first
//! number that worked. Whoever wanted to make corrections feel faster had to
//! find them all and could not try a value without a rebuild.
//!
//! So they are named, gathered, documented with what is known about their
//! floor, and overridable from the environment. Tuning is now a restart rather
//! than a build, which is what makes the honest answer to "how small can these
//! be?" — measure on the machine that matters — actually available.
//!
//! # What is actually slow
//!
//! Not, mostly, the numbers here. Linux (`uinput`) and Windows (`SendInput`)
//! both hand the whole correction to the OS as one batch, so their entire
//! per-key cost is zero and only [`Injection::settle`] is paid once. macOS is
//! the outlier: `CGEvent`s posted too close together get coalesced or dropped,
//! so backspaces are paced individually and an eight-letter word spends
//! `8 × (press_gap + inter_key_gap)` before the replacement even starts.
//!
//! The other real cost is [`Injection::held_release_timeout`], and it is not a
//! delay so much as a ceiling: the wait ends the moment the user lifts the key,
//! and only a user still leaning on space pays it in full.

use std::time::Duration;

/// The timing policy for injecting a correction.
///
/// Read once at startup ([`injection`]) rather than per correction: these are
/// consulted inside the loop that paces individual keystrokes, and an
/// environment lookup there would cost more than the gap being measured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Injection {
    /// Gap between a synthetic key-down and its matching key-up.
    ///
    /// macOS only — the other two platforms submit press and release together
    /// and never sleep between them. 50 µs was measured to be too short: a
    /// backspace would go missing (leaving the original first letter behind) or
    /// a retyped key would be lost entirely. The floor is somewhere between
    /// that and here, and is a property of the machine, not of the code.
    pub press_gap: Duration,

    /// Gap between one injected key and the next, for the same reason as
    /// [`press_gap`](Self::press_gap).
    pub inter_key_gap: Duration,

    /// Grace period between the last injected event and letting the listener
    /// look at keystrokes again.
    ///
    /// Both `CGEventPost` and `SendInput` return once the event is *queued*,
    /// not once it has been delivered, so un-gating immediately can let the
    /// tail of our own correction arrive as though the user had typed it — the
    /// replacement lands, and then its last characters land again in the buffer
    /// behind it.
    pub settle: Duration,

    /// Longest the injector will wait for the user to physically release a key
    /// it is about to press synthetically.
    ///
    /// A synthetic press of a key the keyboard is already holding down is
    /// discarded by the compositor as a duplicate, which is how the word's last
    /// letter sometimes went missing. This is a ceiling, not a cost: the wait
    /// ends as soon as the key comes up.
    ///
    /// It covers the letters of the replacement and the shift keys — keys the
    /// user has usually let go of long before the word ends. The one key that
    /// is *always* still down has its own, much longer ceiling below.
    pub held_release_timeout: Duration,

    /// Longest the injector will wait for the key that *ended* the word —
    /// space or enter — to come back up.
    ///
    /// Its own setting because it is the one key guaranteed to still be held:
    /// pressing it is what asked for the correction. Nothing can be injected
    /// around that. A release sent from the injector is dropped by the kernel
    /// before any compositor sees it (key state is per input device, and the
    /// injector never pressed the key), and a press sent while the real key is
    /// down stays swallowed no matter how many times it is repeated — measured
    /// against Hyprland. So the terminator is simply not typeable until the
    /// user lifts their finger, and giving up early means the corrected word
    /// silently loses the space after it.
    ///
    /// An ordinary keypress lasts 50–150 ms, so this has to be several times
    /// that to be a ceiling rather than a deadline. Reaching it means the user
    /// is leaning on space, which is already producing auto-repeat the
    /// correction cannot be reconciled with; the replacement goes out anyway,
    /// space or no space.
    pub terminator_release_timeout: Duration,

    /// How often that wait re-checks whether the key has come up. Straight
    /// latency — the correction cannot start until a poll notices — so it is
    /// deliberately far tighter than any of the gaps above.
    pub held_poll: Duration,

    /// How long to let the compositor notice the injector device after creating
    /// it, before listening starts.
    ///
    /// Linux only, paid once at startup and never again. Wayland compositors
    /// enumerate input devices asynchronously; injecting into a device the
    /// compositor has not finished setting up drops the events on the floor.
    pub device_settle: Duration,

    /// Longest to wait for a layout switch to actually take effect before
    /// giving up on the correction.
    ///
    /// Switching is asynchronous on every platform: the call returns when the
    /// request is *accepted*, not when the keymap is live. Injecting into that
    /// gap types the corrected word out under the old layout, which is the
    /// exact garbage the correction existed to prevent. Measured at ~0.3–0.9 ms
    /// on Hyprland, so this is a ceiling for a loaded machine rather than a
    /// cost — the wait ends as soon as the compositor confirms.
    pub layout_confirm: Duration,

    /// How often that confirmation is re-checked. Cheap on Linux (~0.11 ms per
    /// query over the Hyprland socket), so it is polled tightly.
    pub layout_poll: Duration,

    /// Gap between one write of injected events and the next.
    ///
    /// Linux only, and not a pacing gap like the two above — it is back
    /// pressure. The kernel gives every reader of an input device a fixed
    /// 64-event ring buffer (`EVDEV_BUF_PACKETS` × the device's
    /// events-per-packet hint, measured at exactly 64 here). A correction is
    /// four events per keystroke — press, SYN, release, SYN — over the word,
    /// the erasure and the terminator, so it is `8 × word length + 8` events:
    /// a seven-letter word already fills the buffer in a single write.
    ///
    /// Whether that matters is up to the reader. Measured against a compositor
    /// that keeps up, 168 events in one write arrive intact; against one that
    /// is 5 ms behind, the kernel drops the overflow, posts `SYN_DROPPED`, and
    /// what survives is arbitrary — including, because it is emitted last, the
    /// space after the corrected word.
    ///
    /// So the write is split and this is what separates the pieces: enough for
    /// a reader that is merely behind to catch up, nothing at all for one that
    /// is keeping up. Set it to 0 to go back to one unbroken write.
    pub batch_gap: Duration,
}

/// The shipped policy for this platform.
///
/// The per-key gaps are macOS's, because macOS is the only platform that paces
/// keys at all; the other two leave them unread rather than pretend to a
/// different value.
pub const DEFAULTS: Injection = Injection {
    press_gap: Duration::from_micros(700),
    inter_key_gap: Duration::from_micros(700),
    settle: Duration::from_millis(1),
    // Linux can afford a much tighter ceiling than the other two: `uinput`
    // events are submitted as one batch to the kernel rather than posted
    // through a compositor's queue, so a slightly early injection is recovered
    // from instead of scrambled.
    #[cfg(target_os = "linux")]
    held_release_timeout: Duration::from_millis(40),
    #[cfg(not(target_os = "linux"))]
    held_release_timeout: Duration::from_millis(150),
    // Long enough to outlast a deliberate keypress rather than a brisk one.
    // Only Linux ever waits on this: macOS and Windows insert the trailing
    // space as *text* alongside the corrected word, so there is no key to be
    // swallowed and nothing to wait for.
    terminator_release_timeout: Duration::from_millis(600),
    held_poll: Duration::from_micros(100),
    device_settle: Duration::from_millis(300),
    layout_confirm: Duration::from_millis(180),
    layout_poll: Duration::from_micros(500),
    // Measured against a reader deliberately held behind, 50 corrections of a
    // sixteen-letter word at each setting:
    //
    //   reader 0.5 ms behind:  250 µs → 3 drops,  500 µs → none
    //   reader 2 ms behind:    250 µs → 4 of 50 corrections lost their space,
    //                          500 µs → none lost, though events still dropped
    //
    // 500 µs was better than 250 at every lag tried and never worse. It costs a
    // six-letter word — two writes, one gap — half a millisecond, against the
    // ~13 ms the layout query used to spend on subprocesses.
    batch_gap: Duration::from_micros(500),
};

/// Overrides, all in microseconds, from the environment or `config.toml` (see
/// [`crate::settings`]) — the file names them without the `RECAST_` prefix and
/// in lowercase, so `RECAST_INJECT_BATCH_GAP` is `inject_batch_gap`:
///
/// * `RECAST_INJECT_PRESS_GAP`
/// * `RECAST_INJECT_KEY_GAP`
/// * `RECAST_INJECT_SETTLE`
/// * `RECAST_INJECT_HELD_TIMEOUT`
/// * `RECAST_INJECT_TERM_TIMEOUT`
/// * `RECAST_INJECT_HELD_POLL`
/// * `RECAST_INJECT_DEVICE_SETTLE`
/// * `RECAST_INJECT_LAYOUT_CONFIRM`
/// * `RECAST_INJECT_LAYOUT_POLL`
/// * `RECAST_INJECT_BATCH_GAP`
///
/// Microseconds rather than milliseconds because the interesting range for the
/// first two is below a millisecond, and an integer setting whose useful values
/// are all `0` is not a setting.
///
/// The same ten names are listed in `config::NUMERIC_KEYS` (so a typoed value
/// is complained about rather than silently ignored) and in `main`'s `--help`.
/// A test in `config` keeps the three lists in step.
pub fn injection() -> &'static Injection {
    static POLICY: std::sync::OnceLock<Injection> = std::sync::OnceLock::new();
    POLICY.get_or_init(|| Injection {
        press_gap: micros("RECAST_INJECT_PRESS_GAP", DEFAULTS.press_gap),
        inter_key_gap: micros("RECAST_INJECT_KEY_GAP", DEFAULTS.inter_key_gap),
        settle: micros("RECAST_INJECT_SETTLE", DEFAULTS.settle),
        held_release_timeout: micros("RECAST_INJECT_HELD_TIMEOUT", DEFAULTS.held_release_timeout),
        terminator_release_timeout: micros(
            "RECAST_INJECT_TERM_TIMEOUT",
            DEFAULTS.terminator_release_timeout,
        ),
        held_poll: micros("RECAST_INJECT_HELD_POLL", DEFAULTS.held_poll),
        device_settle: micros("RECAST_INJECT_DEVICE_SETTLE", DEFAULTS.device_settle),
        layout_confirm: micros("RECAST_INJECT_LAYOUT_CONFIRM", DEFAULTS.layout_confirm),
        layout_poll: micros("RECAST_INJECT_LAYOUT_POLL", DEFAULTS.layout_poll),
        batch_gap: micros("RECAST_INJECT_BATCH_GAP", DEFAULTS.batch_gap),
    })
}

/// How many events go out in one write to the injector, and the buffer that
/// number has to respect.
///
/// The kernel gives each reader of an input device a ring buffer of
/// [`KERNEL_BUFFER_EVENTS`] — `EVDEV_BUF_PACKETS` times the device's
/// events-per-packet hint, measured at exactly 64 for a keyboard. Overflow is
/// not queued and not retried: the kernel drops what does not fit, posts
/// `SYN_DROPPED`, and the reader resynchronises. Half the buffer per write
/// leaves room for a reader that is one write behind.
///
/// Checked below at compile time rather than in a test, because the cost of
/// getting it wrong is not a failure anyone would recognise — it is corrections
/// that come out missing their last keystroke, sometimes.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub const EVENTS_PER_WRITE: usize = 32;

/// The kernel's per-reader event ring, measured on this platform.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub const KERNEL_BUFFER_EVENTS: usize = 64;

const _: () = {
    assert!(
        EVENTS_PER_WRITE <= KERNEL_BUFFER_EVENTS / 2,
        "a write this big can fill the reader's buffer on its own"
    );
    // One keystroke is press, SYN, release, SYN. A write ending part-way
    // through one would leave a key pressed and uncommitted for the whole gap.
    assert!(
        EVENTS_PER_WRITE.is_multiple_of(4),
        "a write must not end in the middle of a keystroke"
    );
};

/// A duration override given in microseconds, falling back to `default` when
/// unset or unparsable.
///
/// Zero is honoured rather than rejected: "do not sleep here at all" is a
/// legitimate thing to want to measure, and on a platform that batches its
/// events it is very likely correct.
fn micros(key: &str, default: Duration) -> Duration {
    crate::settings::get(key)
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_micros)
        .unwrap_or(default)
}

/// Sleep, unless the policy asked for no gap at all.
///
/// `thread::sleep(0)` is not free — it is a syscall, and on most platforms it
/// yields the rest of the timeslice, which is the opposite of what someone
/// setting a gap to zero is asking for.
#[inline]
pub fn pause(gap: Duration) {
    if !gap.is_zero() {
        std::thread::sleep(gap);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_gaps_are_ordered_the_way_the_comments_claim() {
        let t = DEFAULTS;
        // The poll has to be tighter than the wait it is polling, or the
        // timeout is decided by the poll rather than by the timeout.
        assert!(t.held_poll < t.held_release_timeout);
        assert!(t.held_poll < t.terminator_release_timeout);
        // The key that triggered the correction is still down by definition,
        // and an ordinary keypress outlasts the general ceiling. If this were
        // not the longer of the two, the trailing space would go on being
        // dropped for everyone who does not type in taps.
        assert!(t.terminator_release_timeout > t.held_release_timeout);
        // Settling happens once per correction; the per-key gaps happen once
        // per character. Whichever way they are tuned, a per-key gap that
        // exceeded the one-off one would be the wrong shape.
        assert!(t.press_gap <= t.settle);
        assert!(t.inter_key_gap <= t.settle);
    }

    #[test]
    fn an_unset_override_leaves_the_default_alone() {
        // Nothing sets this, so it must read back as what was passed in.
        assert_eq!(
            micros("RECAST_INJECT_NOTHING_SETS_THIS", Duration::from_micros(42)),
            Duration::from_micros(42)
        );
    }

    #[test]
    fn a_zero_gap_is_a_real_setting_and_costs_no_syscall() {
        // The distinction `pause` exists to make: zero means "do not sleep",
        // not "sleep for the smallest amount the OS will give you".
        assert!(Duration::ZERO.is_zero());
        pause(Duration::ZERO);
    }
}
