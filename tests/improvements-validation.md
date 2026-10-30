# Selected improvements: implementation and verification

Implemented items 3, 4, 5, 6, 8, and 9 from the improvement review:

- Manual-only app mode is available in settings and native preferences, with live
  persistence and automatic rewrites disabled while explicit gestures remain.
- macOS checks actual key output against capture mappings; Linux preserves XKB
  variants and declines unknown mappings. Linux name checks cannot detect custom
  remaps that retain standard layout names.
- Optional Linux AT-SPI focus/selection helper and GNOME Shell layout extension
  support accessible GNOME/KDE Wayland applications. Setup is in
  [the integration guide](../integrations/README.md).
- Hebrew transposition plus final-form repair retains frequency and ambiguity
  gates. The authored conversation corpus covers 30 protected tokens and 12
  intended repairs: 11 repairs, one abstention, zero wrong replacements. These
  are synthetic examples, not collected messages or a real-world accuracy estimate.
- Windows native Edit/RichEdit selection rescue uses UI Automation and selected
  replacement messages. Linux uses AT-SPI. Both preserve the clipboard and reuse
  the shared exact-text undo path. Unsupported controls are declined.
- The [native protocol](native-ui.md), [browser fixture](native-editing.html), and
  ignored Windows native Edit test cover editing/replay acceptance. The existing
  Linux/Hyprland evdev replay runner remains available.

## Checks executed

- macOS Rust unit suite: 360 passed, 9 ignored. Two local-socket tests initially
  failed under sandbox restrictions and passed when rerun with loopback access.
- CLI integration suite: 6 passed.
- macOS Carbon main-thread/worker wrapper test passed. The sandbox emitted an
  input-service connection warning; this is not an editing acceptance result.
- Clippy with warnings denied: macOS, x86_64 Linux, and x86_64 Windows targets.
  Windows resource embedding could not run because the MinGW resource compiler
  is unavailable; cross-target analysis still succeeded.
- Python suite: 18 passed, including failed insertion, rollback, stale selection,
  focus interruption, and exact Unicode text round trip.
- Browser fixture: seven allowed-outcome checks passed.
- GNOME protocol fixtures: input-source ordering, IBus rejection, invalid index,
  and locked-session guards passed.
- Formatting and whitespace checks passed.

Frozen independent evaluation remains unchanged: 0 unwanted changes among 96
protected cases; 11 misses and 3 wrong replacements among 62 desired repairs.
Those known errors remain visible in the existing evaluation; no thresholds or
frozen expectations were loosened to conceal them.

## Native acceptance still required

No Windows/Linux desktop is available here. The new Windows native Edit test,
GNOME/KDE sessions, AT-SPI application compatibility, and end-to-end physical
editing/replay trials were not run. No extension/helper was installed or enabled,
and the installed ReCast app was not replaced or restarted.

AT-SPI has no atomic selected-text replacement. An interruption after insertion
can leave both original and inserted text; rollback only deletes verified inserted
text. The helper is opt-in. Exact text undo does not promise rich-formatting undo.
