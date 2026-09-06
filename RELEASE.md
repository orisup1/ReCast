# ReCast v0.8.0

## Release notes

- Shared correction, completion, and undo engine with regression coverage for
  interrupted typing, focus changes, shortcuts, and deletion.
- Improved English/Hebrew correction context and optional local personalization.
  Personalization remains off by default; its files can contain typed words.
- Backspace keeps correction enabled while canceling stale pending rewrites.
- Linux starts list/layout watchers and the personalization flusher after
  daemonization, so they survive startup. Failed injection no longer counts as a
  successful correction or arms undo.
- Windows attaches the parent console before processing CLI options and preserves
  redirected output. Punctuation typed during a correction replays with the proper
  virtual key codes (a period previously mapped to Delete).
- `--write-config` cannot overwrite a file created concurrently or follow a dangling
  symlink to create another file.
- Removed the committed app bundle and the entire `exec` directory. Build tools now
  assemble macOS bundles under ignored `target/bundle/`.
- Automatic CI and release checks cover Linux, macOS, and Windows. Release tags must
  match the crate version and the commit used to build the assets.

## Local validation

- Linux: 177 unit tests and 2 CLI tests passed; 3 opt-in benchmarks ignored.
  The live Hyprland test passed with desktop socket access.
- Linux release build and both release CLI tests passed.
- Formatting and Clippy passed for Linux, `x86_64-apple-darwin`, and
  `x86_64-pc-windows-gnu` (all targets, warnings denied).
- Workflow YAML and Bash syntax passed. A mocked publishing check covered absent,
  matching, and mismatched tags. Generated bundle metadata reports 0.8.0.
- Cross-check limitations: the local Windows resource compiler is unavailable;
  the macOS dependency `block 0.1.6` reports a future Rust compatibility warning.
  Native builds must validate resources, linking, and signing.

## Before publishing

1. Run CI on the intended release commit. All three native jobs must pass.
2. On native desktops, verify layout correction in both directions, spelling,
   Right Shift completion, Ctrl double-tap undo, and cancellation after changing
   focus. Check Linux list reload after daemonized startup and Windows release
   `--version`, `--help`, and redirected output.
3. Run **Binaries** on that same commit with tag `v0.8.0` and draft enabled.
   A pre-existing tag must point to that commit; a new tag is created there.
4. Download the draft assets, verify `SHA256SUMS`, and check startup on each OS.
   Confirm macOS signing/notarization and Windows signing if certificates are configured.
5. Use the notes above in the draft release and publish after those checks pass.

Cross-target Clippy from Linux checks compilation; native runtime and signing checks
require their respective operating systems. No release has been published by preparing
these files.
