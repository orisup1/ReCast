# Native UI smoke test

Run these checks on macOS and Windows. Cross-target Clippy verifies compilation;
it does not verify native drawing, privacy grants, focus, or display scaling.

1. Launch from Finder/Explorer and a terminal. Confirm the menu has Status, current-app mode, quick app pause, Pause,
   Recent corrections, Language preference, Settings, Help, and Quit. On macOS, the icon should remain
   compact in light and dark appearances, with no terminal banner in its title.
2. Pause, resume, and disable correction. Check distinct icon shapes, matching
   status text, and the pause countdown. Re-enable correction after this check.
3. Use the visible navigation to open each Settings section. Change spelling, sensitivity, and shortcuts; verify
   immediate behavior and persistence after restarting. A conflicting shortcut or
   environment override should show an explanation and restore the previous value.
   Select Balanced, Prefer English, and Prefer Hebrew in both the menu and General;
   check matching selection, persistence, and rollback when RECAST_PREFER is set.
4. Choose an application and change its mode. Switch applications, choose Use last
   application, and confirm the identifier before changing its mode. Restore the
   original mode afterward. Open and cancel each existing word-rule editor.
5. Open practice from Help. Verify one exercise step at a time, actual correction,
   undo, completion, Done, and Start over. Start over must focus the enabled field.
   On Windows, missing keyboards should show a Settings link and update on return.
   On a fresh Mac account, check the staged Accessibility and keyboard setup.
6. Generate several corrections, including Hebrew, and open history. Verify original
   and replacement columns, timestamps, Details, and Ignore. New corrections must
   not shift the displayed snapshot. Refresh and paginate through the history.
7. On Windows, check 100%, 125%, 150%, and 200% display scaling. Move each window
   between monitors with different scales, resize it to its minimum, and check
   readable fonts, unclipped controls, and reachable history pagination. Include
   the log window and modal word-rule editors.
8. Resize Settings and practice to minimum height. Scroll to every control without
   shrinking text or hit targets. Tab to offscreen controls and confirm they become
   visible. Switch sections after scrolling; verify navigation remains reachable.
   Preview English `recieve` and Hebrew `תודדה` with the matching keyboard choice.
   Preview must not type, switch the OS layout, learn, or increment correction counts.
   Change sensitivity and preview again. Verify selection and results remain stable
   across tray refreshes. Check original/replacement alignment separately with
   `(שלום!)`, `(hello!)`, digits, and mixed text; timestamps must remain separate.
9. In practice, select an undo-only key. Verify unchanged/ignored words remain
   unchanged, exceptions survive, and corrected words restore. Existing shortcuts
   must survive unless explicitly edited. Shared keys retain the double-tap delay.
10. Navigate with Tab, Shift+Tab, Space, and arrow keys. Check VoiceOver/Narrator
   labels. Minimize and reopen each window from the tray. Closing a secondary
   window must leave correction running; closing the log must stop logging.
11. On Windows, rapidly scroll Settings past partially clipped controls, tab to
    offscreen fields, switch sections after scrolling, and resize repeatedly.
    Cover and uncover the window. Vacated control areas must repaint with the
    system form background; no stale text trails or duplicated controls remain.

The Windows GDI regression test checks background erasure and movement of clipped
and fully offscreen controls without taking focus:

```powershell
cargo test native_form_erases_stale_pixels_and_moves_offscreen_controls
```

## Editing and replay acceptance

Use disposable documents and default US/Hebrew layouts. Record the commit/build,
OS, compositor, application/version, mode, layouts, and result for each trial.
Run every scenario in Full and Manual only modes; in Manual only, automatic edits
must never occur. Explicit conversion/completion/undo must remain available.

Open `tests/native-editing.html` locally for a browser fixture with a textarea,
rich-text chat, a second focus target, password and read-only probes. It provides
seven scenarios and exports bounded event traces locally. Type physically:
JavaScript keyboard events do not exercise OS capture, and Windows ignores
foreign injected keyboard events. An unchanged selection counts as unsupported,
not a passing conversion test. Exported traces contain fixture text; use sample
text only. Test native applications as well:

| Platform | Everyday typing | Coding / terminals | Chat / mixed text |
| --- | --- | --- | --- |
| macOS | TextEdit plain and rich documents | VS Code, Terminal | Safari/Chrome fixture and a disposable chat draft |
| Windows | Notepad/native Edit, WordPad or another RichEdit host | VS Code, Windows Terminal | Edge/Chrome fixture and a disposable chat draft |
| Linux X11 | GTK text editor | VS Code, terminal | Firefox/Chromium fixture |
| GNOME Wayland | GTK editor with AT-SPI helper and Shell extension | terminal, VS Code | browser fixture; record accessibility support |
| KDE Wayland | Kate/Qt editor with AT-SPI bridge/helper | Konsole, VS Code | browser fixture; record accessibility support |

For each supported combination:

1. Type `recieve` followed by Space, three rapid Spaces, and rapidly Space/Enter.
   The result must contain intact corrected or untouched words, every intended
   space, and exactly one Enter. Repeat 20 times, including typing the next word
   before correction finishes. Record corruption separately from abstention.
2. Type a typo and immediately click another field or switch applications. The
   destination field must remain unchanged. Repeat with cursor navigation,
   select-all/delete, native undo, and a layout switch during pending correction.
3. Try `const recieve = "שלום";`, `API_URL`, a URL, Hebrew with niqqud, emoji,
   and intentional slang. Manual only must preserve exact text until a gesture.
4. Select `akuo` within `left akuo שלום🙂 right`. Double-Ctrl must change only
   the selection to `שלום`; repeat to restore exact original text. Change the
   selection or focus before completion: no stale replacement may land. Test
   readonly/password controls, an empty selection, and a selection over 64 KiB:
   all must be declined. The clipboard must remain unchanged.
5. Test Dvorak, Colemak, a dead-key layout, standard US and Hebrew. macOS/Linux
   must pause on incompatible mappings and resume on supported layouts. Linux
   custom mappings using unchanged standard names are outside name validation.
6. Disable/re-enable the helper or GNOME extension, switch windows, and test
   lock/unlock. Missing required layout/focus information must decline edits.
7. On macOS, bind completion to Left Shift, type `ther`, and cycle through at
   least 24 offers, including original-text checkpoints. Repeat with quick taps
   and deliberate taps. The source must stay `ther` while a small strip changes its highlighted offer.
   Verify the filled highlight remains legible in light and dark appearance,
   including the original-text choice. Check the shortcut hints, long English
   and Hebrew choices, and a caret near each screen edge (including a secondary
   display): the strip must stay on screen and open above a low caret.
   Space accepts exactly once; no leftover first letters such as `wtthrough`.
   Repeat in Spotlight with its inline autocomplete visible; earlier query text
   must survive acceptance, and Enter must cancel without a duplicate launch. Include a word before the prefix
   and trailing spaces; undo must restore `ther` and preserve surrounding text.
8. On Windows, Linux, and macOS, request spelling and completion while typing
   quickly. Rapid taps must continue cycling without freezing input. During a
   slow lookup, type another letter, add spaces, navigate, click another field,
   or change the shortcut: no stale replacement may appear. Repeat with app
   rules enabled. In Recent corrections, verify the original/replacement are
   identifiable and Ignore word explains that it affects future corrections.
9. In Recent corrections, ignore a capitalized word, then choose Allow again.
   The button and protection status must update immediately; unrelated word
   rules must survive. Check a failed save shows an error and leaves protection
   unchanged. With only single-tap undo enabled, history and practice must name
   that key; shared undo/action keys must mention the 500 ms delay.

An optional Windows native integration test opens its own disposable Edit control,
takes focus, checks cancellation, replacement, stale selection, exact undo, and
read-only rejection. Run in a logged-in Windows desktop and keep it foreground:

```powershell
cargo test native_selection_round_trip -- --ignored --nocapture
```

The layout regression test opens a host that ignores layout requests and an
embedded Edit control that accepts them. With English and Hebrew installed, it
checks that layout reads and switches follow the focused control. Keep its window
foreground until it closes:

```powershell
cargo test layout_switch_reaches_focused_control_instead_of_host -- --ignored --nocapture
```

The Linux/Hyprland replay runner exercises real capture and replacement delivery:

```sh
python3 scripts/check_linux_replay.py --binary /path/to/candidate/recast \
  --restore /path/to/installed/recast --output /tmp/recast-replay.json --rounds 20
```

It replaces a standalone daemon for the trial and restores the specified installed
binary afterward. Read its help and use a disposable desktop session. It is not a
GNOME/KDE harness. macOS Carbon main-thread/worker access is checked separately by
`cargo test --test macos_layout`; that test does not prove editing correctness.

Linux Hyprland/GTK trials are recorded in `CORRECTION-UX.md` and `PERFORMANCE.md`.
Other native desktops and Windows still require runtime checks. Cross-target
compilation, engine simulations, and fixture checks must not be reported as
native acceptance or measured end-to-end typing latency.

## Fluid correction regression checks

- While a correction is pending, type a short sentence ending in `keyb ` so the
  following text exceeds 32 characters. Completion and spelling alternatives must
  still act on the latest word; double Ctrl converts that word when the older
  undo has expired. Undo-only must remain a no-op. Repeat at exactly 32 and 33
  following characters, with extra spaces and Hebrew. New typing, navigation,
  Enter, secure input, exclusions, and a focus change must not revive an old word.
- Type `recieve Next` continuously, capitalizing with each Shift key in separate
  trials. The first word's correction must not require waiting for Shift release,
  and undo must preserve `Next` exactly. Repeat with an initially capitalized
  `Recieve `. A bare configured Shift tap must still request suggestions and
  prevent an older automatic result from changing the preview. Ctrl+Shift,
  navigation, and real layout changes must continue canceling stale work.
- While a correction is landing, finish another word with one or three spaces.
  Request spelling or completion immediately afterward, then try Backspace into
  that word. Both actions must use the latest word and preserve the earlier
  corrected text. Separately, undo the earlier correction: later text must survive.
  Repeat in English and Hebrew, and with Space accepting a completion while its
  replacement is pending. Navigation, unknown fragments, and layout restoration
  must not revive older or incomplete word tracking.
- Type a mixed-language draft continuously, including `recieve next` and several
  extra spaces after a typo. Do not pause to let correction finish. In supported
  native fields, a delayed same-layout spelling fix must preserve the following
  text and remain undoable. Check Hebrew spelling too. Repeat in unsupported
  fields: a missed fix is acceptable; blind deletion of later text is not.
  Enter, navigation, focus changes, and layout changes during lookup must cancel
  stale edits. Record correction misses separately from corruption and latency.
- After `recieve ` becomes `receive `, type `next`, request undo, and keep typing
  without waiting. Supported native fields should preserve the added text while
  restoring `recieve`. Repeat with a Space before the undo gesture and a Space
  typed while undo is pending; the next typo should correct without a warmup.
  Repeat after a layout correction, including text typed before layout restoration:
  its characters must survive verbatim. Text that cannot be verified, Enter,
  focus changes, and navigation must prevent a stale edit.
- In TextEdit plain and rich text, correct a word in the middle of a document.
  Check that surrounding formatting, text, and caret survive native replacement.
  Repeat in a control without writable Accessibility text ranges; keyboard fallback
  must preserve the same text. Check host Undo as well as ReCast undo.
- After `recieve ` becomes `receive `, type `next` then use ReCast undo.
  Supported macOS fields and Windows native Edit/RichEdit controls must contain
  `recieve next`. Repeat with Backspace, 32/33 suffix characters, multiple spaces,
  Hebrew, and input arriving during replacement. Unsupported controls must decline
  extended undo without deleting anything; spaces-only undo remains available.
- Change text externally, move the caret, or switch focus before extended undo.
  Stale text must never be rewritten. Verify separate single-Ctrl undo acts without
  the shared-key 500 ms wait, and Settings explains shared bindings.


### Linux continuous typing and injector layout

The disposable GTK harness requires a foreground Hyprland session, PyGObject,
GTK 3, libevdev, and input/uinput access. It takes focus temporarily, injects
only into its own field, and restores the prior layout/window. Stop any supervised
instance first; use `--restore /absolute/path/to/recast` only if a standalone
instance should be restarted afterward. The helper-enabled trial enables the GTK
accessibility bridge in its own process, even if the invoking shell disables it.

```sh
python3 scripts/check_linux_replay.py --binary target/release/recast --output /tmp/recast-suffix.json --rounds 6 --queued --suffix next --spaces 1
python3 scripts/check_linux_replay.py --binary target/release/recast --output /tmp/recast-undo.json --rounds 1 --queued --suffix next --spaces 1 --undo-expected 'recieve next'
python3 scripts/check_linux_replay.py --binary target/release/recast --output /tmp/recast-injector.json --rounds 6 --queued --helper 0 --wayland 0 --injector-layout 1 --expected-layout 0
python3 scripts/check_linux_replay.py --binary target/release/recast --output /tmp/recast-layout.json --rounds 6 --queued --helper 0 --word akuo --expected שלום --spaces 1 --expected-layout 1
python3 scripts/check_linux_replay.py --binary target/release/recast --output /tmp/recast-speed.json --rounds 8 --helper 0 --spaces 1 --space-hold-ms 125 --max-latency-ms 100
```

`--helper 0 --wayland 0` explicitly tests kernel replay. `--helper 0` alone
allows compositor input. `replacement_ms` measures the first trigger Space press
to the expected GTK text-buffer change, not display paint. Latency checks are
opt-in; queued trials and following text measure different workloads. Set
`GDK_BACKEND=x11` to exercise GTK through XWayland in the same Wayland session.
`--caps` requires an actual Caps Lock binding and rejects remapped keys.

Undo deliberately suppresses a rejected word for the session, so undo trials use
one round. Run separate trials for actual native GNOME/KDE Wayland, Sway and X11,
and for native macOS/Windows apps; cross-compilation cannot validate their UI.
