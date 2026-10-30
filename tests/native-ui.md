# Native UI smoke test

Run these checks on macOS and Windows. Cross-target Clippy verifies compilation;
it does not verify native drawing, privacy grants, focus, or display scaling.

1. Launch from Finder/Explorer and a terminal. Confirm the menu has Status, Pause,
   Recent corrections, Language preference, Settings, Help, and Quit. On macOS, the icon should remain
   compact in light and dark appearances, with no terminal banner in its title.
2. Pause, resume, and disable correction. Check distinct icon shapes, matching
   status text, and the pause countdown. Re-enable correction after this check.
3. Open each Settings section. Change spelling, sensitivity, and shortcuts; verify
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
8. Navigate with Tab, Shift+Tab, Space, and arrow keys. Check VoiceOver/Narrator
   labels. Minimize and reopen each window from the tray. Closing a secondary
   window must leave correction running; closing the log must stop logging.

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

An optional Windows native integration test opens its own disposable Edit control,
takes focus, checks cancellation, replacement, stale selection, exact undo, and
read-only rejection. Run in a logged-in Windows desktop and keep it foreground:

```powershell
cargo test native_selection_round_trip -- --ignored --nocapture
```

The existing Linux/Hyprland replay runner can exercise real evdev/uinput replay:

```sh
python3 scripts/check_linux_replay.py --binary /path/to/candidate/recast \
  --restore /path/to/installed/recast --output /tmp/recast-replay.json --rounds 20
```

It replaces a standalone daemon for the trial and restores the specified installed
binary afterward. Read its help and use a disposable desktop session. It is not a
GNOME/KDE harness. macOS Carbon main-thread/worker access is checked separately by
`cargo test --test macos_layout`; that test does not prove editing correctness.

No new native Windows/Linux editing trials were executed during this implementation.
Cross-target compilation, engine simulations, and fixture checks must not be
reported as native acceptance or measured end-to-end typing latency.
