# Correction experience review

This review follows automatic layout and spelling correction, explicit spelling
and completion, conversion, selected-text rescue, undo, ordinary editing, startup,
and feedback. It evaluates code paths and simulated interactions, not a live
usability study. The intended experience is to keep writing in an ordinary app
without preparing an example or waiting for correction.

## Experience and decisions

- **Ordinary typing:** Space checks a finished word. Confidence protections keep
  valid words, names, saved exceptions, and ambiguous text unchanged. Slow search
  previously ran inside capture; incoming text could also discard its result.
  This timing dependence was the main remaining reason to pause or stage an
  example. Automatic search now runs on a worker, with bounded continuation
  described below.
- **Visible replacement:** keeping a shared prefix already reduces deletion.
  On macOS, typing during a replacement previously ruled out the native edit
  path and forced paced erasure/replay. Native replacement can now include that
  tracked suffix. Windows also prefers its existing verified Edit/RichEdit
  replacement before keyboard fallback. Exact text matching protects surrounding
  text; unsupported controls retain their fallback behavior.
- **Undo and trust:** double Ctrl restores a correction, but converts an unchanged
  word. That overload remains a discovery cost. Existing help leads with undo;
  a separate undo-only key remains optional. Undo preserves tracked spaces and,
  in supported native fields, up to 32 following characters. One rejection
  suppresses the word for the session; two occasions can save an exception.
  Recent corrections provides inspection, Ignore word, and Allow again.
- **Spelling alternatives and completion:** these are explicit requests, not a
  prerequisite for automatic correction. macOS previews choices without changing
  the document. Windows/Linux cycle actual replacements, which remains visible
  friction. Returning to the original choice protects that occurrence from
  autocorrection. Existing rollover support avoids requiring the preceding letter
  or Space to be released first.
- **Manual layout recovery:** double Ctrl can convert an unknown word before or
  after Space. Selection rescue covers supported editable controls. Repeating a
  conversion restores it without saving an exception. Neither feature requires
  lowering automatic confidence or collecting more typing data.
- **Editing:** fresh words after clicks or focus changes need no warmup word.
  Backspacing into the last tracked word, accepting original suggestions, and
  continuing after conversion/undo retain their existing regression coverage.
  Navigation into unknown text still requires a boundary; correcting an arbitrary
  captured fragment as if it were a complete word could damage existing text.
- **Startup and feedback:** practice is already opt-in. Required permission and
  keyboard setup stay available. The first correction hint teaches normal typing
  and undo once; ordinary corrections do not open dialogs or notify repeatedly.
  No new tutorial, gesture, setting, notification, or dependency was added.

## Continuous typing changes

Automatic dictionary work uses one worker and at most one waiting word. Capture
continues tracking text and cancellation while search runs. A newer finished word
replaces waiting work that can no longer safely target the caret. The tracked
unchanged word is available immediately for explicit gestures, without waiting
for dictionary search.

Extra spaces no longer discard an otherwise current result. On supported macOS
and Windows native fields, a same-layout spelling correction can also survive up
to 32 freshly typed following characters, including spaces. The complete expected
suffix must match before a native edit. The later text stays intact, its unfinished
word remains tracked, and undo restores the original correction while preserving
that suffix. Native layout queries run outside the typing-state lock.

Deletion, command/modifier presses, navigation, clicks, Enter, focus/layout
changes, disabled correction, secure input, application restrictions, and suffix
overflow still reject stale results. A native text mismatch never falls back to
blind deletion for a delayed correction. Confidence thresholds and saved learning
policy are unchanged.

## Limits that still matter

1. **Submission:** Enter may send a message or execute a command. It cancels
   pending work and is never erased or replayed. A final word submitted without
   Space may remain uncorrected.
2. **Unavailable native context:** delayed edits across following letters require
   verified native replacement. Linux and unsupported native controls decline
   them. Ordinary correction and spaces-only replay retain their existing paths.
3. **Later layout changes or completed words:** following letters are not
   speculatively reinterpreted under another layout. Once another word has
   finished, an old search is discarded rather than rewriting earlier text.
   Extremely fast input or slow accessibility services can still cause misses.
4. **Pasted or pre-existing text:** selected-text rescue is explicit and bounded.
   Reliable caret context is needed before expanding automatic edits into unknown
   text. No document scanning was added.
5. **Everyday recall and native feel:** synthetic examples do not establish how
   often real users benefit or how native applications redraw. Further evaluation
   should use voluntary ordinary mixed-language drafts, jargon, immediate
   submission, navigation, and continuous typing. Record misses separately from
   wrong replacements; do not lower confidence merely to make demonstrations fire.

## Verification

The new continuation regression failed before the fix with no correction scheduled.
It now covers English and Hebrew, delayed search and final focus replies, extra
spaces, the 32-character bound, suffix-preserving undo, and correction of the next
word. Capture returns while a search is deliberately stalled. Companion cases
exercise stale-result cancellation, native text mismatch, an unverified backend,
and replacement of queued work by the latest finished word.

Existing tests cover automatic layout/spelling, explicit suggestions, passive
preview acceptance, manual conversion, selection rescue, exclusions, learning,
editing, and replay. The full run passed 406 unit tests and six CLI tests, with
12 tests intentionally ignored; the macOS main/worker input-source check also
passed. Clippy passed for macOS, Linux, and Windows with Rust warnings denied.
Windows resource embedding could not run because the resource compiler is absent;
the Windows check is static analysis, not an executable or desktop trial.
Formatting and whitespace checks passed.

Native desktop trials remain necessary: simulations and
cross-target checks do not establish application compatibility or typing latency.
The installed app was not replaced or restarted during this review.
