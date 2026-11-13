# Cross-platform verification — 2026-11-13

This checks the current working tree, including the word-replacement fixes.
Passing tests establish the covered behavior, not perfect compatibility with
every application, keyboard mapping, compositor, or operating-system version.

## Fixes from this pass

- Windows native Edit/RichEdit controls no longer fall back to counted Backspaces
  after an exact native replacement is refused. Password, read-only, and disabled
  native controls are excluded from printable-key capture.
- Windows records the source layout before a replacement, rejects buffered letters
  that cannot be preserved across a layout change, and marks editing as started
  only when a native mutation or input batch begins. Unedited failures can restore
  the previous layout.
- All adapters share the buffered-layout safety rule. The engine checks again
  after layout confirmation, covering typing that arrives while switching blocks.
  Verified native edits may preserve text from the original layout.
- Linux no longer treats a helper's explicit password refusal as missing native
  support and then proceeds to compositor or kernel keyboard injection.
- Linux verified native undo can restore a layout while preserving buffered text
  in its known source language.

## Feature coverage

| Feature | Automated evidence | Native acceptance still needed |
| --- | --- | --- |
| Layout conversion, English/Hebrew spelling, punctuation and capitalization | Shared engine, dictionary, and key-map tests; target-specific compilation | Physical layouts and replacement delivery in real applications |
| Completion, spelling alternatives, cycling, abbreviations, exact undo | Shared typing tests, including continued typing, spaces, and delayed searches | macOS caret strip; Windows/Linux direct cycling and native undo behavior |
| Focus changes, navigation, submission, stale workers, held keys, input replay | Engine race tests, native event construction, platform-specific capture tests compiled for each target | Actual window servers, device hotplug, compositor scheduling, and chat submission |
| Full, Layout only, Manual only, Off, app exclusions and pause | Engine/configuration tests and CLI integration tests | Native menus, application identity, and focus transitions |
| Word rules, preferences, personalization and statistics | Persistence, rollback, rule-editor, learning and privacy-boundary tests | Real filesystem failure modes and native editor workflows |
| Startup, service registration, diagnostics and CLI | Registration/rollback tests, status transport tests, debug/release CLI tests | Actual login/logout and service recovery on each OS |
| Practice, history, settings, accessibility and display scaling | Practice progression, layout queries and native form code compilation | Interactive UI, screen readers, multi-monitor placement, and Windows scaling |
| GNOME integration and AT-SPI editing | Python transaction tests and GNOME protocol fixtures | GNOME/KDE sessions, accessibility providers and extension installation |

## Checks executed

- macOS Rust suite: **430 passed, 12 ignored**. This includes the shared typing
  engine, dictionaries, configuration, persistence, startup and diagnostics tests.
- CLI integration: **6 passed** in each of the debug and release profiles.
- Real Carbon input-source queries passed on the main thread and a worker.
- The normally ignored macOS event-construction test passed. It verifies tags and
  modifier flags without posting keyboard events or touching user text.
- Python script suite: **29 passed**.
- GNOME protocol fixtures and **7 browser fixture outcome checks** passed. These
  are fixture checks, not real browser typing or compositor acceptance.
- Clippy with warnings denied passed for macOS ARM64, macOS Intel, Linux x86-64
  and Windows x86-64 after the platform changes. Test code for the remote targets was compiled,
  not executed on those operating systems.
- Windows x86-64 GNU executable cross-build passed with MinGW. Its `.rsrc` section
  was confirmed, so icon/version resource embedding was included. An initial cached
  warning about a missing resource compiler disappeared after rerunning the build
  script with the installed tools.
- Sampled real-dictionary typing audits passed **20 scenarios** across English
  `keyboard` and Hebrew `מקלדת`, including labeled layout/spelling controls, word
  preservation, punctuation, newline handling, deletion/retyping and completion.
- The macOS release app was rebuilt and its ad-hoc signature verified at
  `target/fixed/ReCast.app`. The running `target/bundle/ReCast.app` copy reports
  an enabled correction listener; this status check does not test live typing.

## Known limits

- Native Linux and Windows desktops were unavailable. Docker's daemon was not
  running, no linked remote-desktop tool was available, and Windows binaries cannot
  run directly on this macOS host. The new hidden-window Windows protection test
  was cross-compiled; native Windows CI must execute it.
- The macOS UI automation service timed out twice with
  `Computer Use server error -10005: timeoutReached`. Native settings/history and
  live replacement acceptance were therefore not verified. No settings, privacy
  grants or installed services were changed. The rebuilt candidate is separate
  from the running `target/bundle/ReCast.app` copy.
- Password protection depends on the operating system and accessibility provider
  exposing protected-field information. Custom controls and Linux sessions without
  the optional accessibility helper are not certified by these checks.
- AT-SPI replacement is not atomic. Existing insert-first validation and rollback
  remain necessary; interrupted edits can still require inspection in the host app.
- The exhaustive dictionary typing audit and performance benchmarks were not run
  in full. Fixture assertions do not establish end-to-end typing latency.
- Independent frozen accuracy evaluation still has **11 missed corrections and
  3 wrong replacements among 62 desired repairs**, with **0 unwanted changes among
  96 protected cases**. Test expectations, spelling thresholds and frozen labels
  were not weakened to make these results disappear. Passing this regression test
  means matching its recorded baseline, not error-free language inference.

Use [the native acceptance matrix](native-ui.md) before certifying a specific
desktop/application combination. In particular, repeat rapid typing, host
autocorrection, layout restoration, password fields, submission, focus changes,
selection rescue, and exact undo in disposable documents.
