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
