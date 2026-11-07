# Correction experience review

This review follows the shared typing engine, native adapter contracts, startup,
practice, and shortcut help. Findings describe code behavior, not a live usability
study. The desired experience is to keep writing in an ordinary application and
occasionally undo a correction, without preparing a demonstration.

## Findings and changes

- **Starting work:** the listener already corrects the first captured word after a
  click or focus change. Opening a scripted practice exercise on first launch
  nevertheless frames correction as something to perform. Practice is now opt-in.
  Missing-keyboard setup in the tray and required permission setup remain available.
- **Learning the behavior:** practice asked for a leading Space even after clicking
  into its field. That unnecessary ritual is removed. After clearing the exercise,
  explicit completion can use the freshly typed prefix without another click.
  Practice remains a diagnostic exercise, not a prerequisite for everyday use.
- **Automatic correction:** Space checks layout and spelling; uncertain text stays
  unchanged. Ordinary typing needs no gesture. The first correction notification
  now teaches only that behavior and undo, instead of introducing completion and
  settings at the same time.
- **Recovering a word:** Ctrl taps previously required every typing key to have
  been released first. This made a quick request silently fail during natural
  rollover. Action taps now tolerate an already-held letter or Space outside an
  active replacement. New keypresses under Ctrl still cancel the gesture, so
  normal shortcuts retain their behavior. Injection keeps its stricter rules.
- **Undo:** double-Ctrl is overloaded: it undoes a correction but converts an
  unchanged word. A separate, optional undo key already exists. Help now leads
  with undo and explains conversion separately. It also reflects existing support
  for preserving up to 32 following characters in supported macOS/Windows fields;
  the old help incorrectly said all further typing ended undo.
- **Explicit spelling and completion:** macOS offers passive previews; Windows
  and Linux cycle actual replacements. These remain deliberate requests. Help
  now groups their common browsing behavior instead of repeating lengthy rules.
- **Trust and exceptions:** undo suppresses a word for the session; repeated undo
  can persist an exception. This behavior stays documented. Recent corrections
  and word rules already offer inspection and reversal. No new automatic learning,
  broader text collection, or correction-threshold changes were introduced.

## Remaining friction and next steps

1. **Sentence-final words without Space:** Enter cancels pending work because it
   may send a message or execute a command. Do not change this by erasing/replaying
   Enter or guessing during unfinished words. A future native edit path would
   need to validate and commit before submission, with application-specific tests.
2. **Cursor edits and pasted text:** navigation or edits into untracked content
   suppress automatic correction until a boundary. Explicit correction can use a
   freshly typed suffix; selected-text layout rescue works only in supported
   controls. Extending this requires reliable caret context, not lower confidence
   thresholds or treating an arbitrary fragment as a whole word.
3. **Fast continuous typing:** a new press during automatic dictionary search
   invalidates that result. This protects text but can cause missed corrections.
   Improving it requires a validated suffix-preserving commit path and race tests
   for typing, focus changes, and submission. Simply removing the revision check
   would risk editing the wrong characters.
4. **Different suggestion experiences:** bring passive previews to other platforms
   only where safe caret anchoring and nonactivating UI are available. Repeated
   replacement while browsing remains visible friction there.
5. **Recall in everyday writing:** synthetic dictionary examples cannot establish
   how often real users benefit. Evaluate mixed-language sentences, intentional
   jargon, common typos, immediate submission, navigation, and rapid typing in
   ordinary editors. Use voluntary local examples with expected output; distinguish
   missed fixes from wrong replacements. Relaxing thresholds to make a demo fire
   more often would undermine trust.

## Verification

Regression coverage exercises Ctrl rollover after a letter and Space, including
manual conversion, separator preservation, and rejection of a real Ctrl chord.
Existing engine tests cover fresh-field correction, deletion/retyping, undo,
preview acceptance, focus changes, and cancellation. Native application timing,
permission dialogs, and Windows/Linux UI still need real desktop validation.

## Follow-up pass 1: suggestions after ordinary editing

The extended undo window kept an earlier correction available while the user
finished another word. Suggestions could consequently reuse that earlier word.
Explicit suggestions now use the latest tracked finished word when an earlier
correction has a non-space suffix. Its visible text must match the tracked suffix;
unavailable or inconsistent context is refused. Undo retains its existing meaning.

Backspacing an extra space also removed it from the visible suffix without removing
it from the separator count used by later suggestions. The count now shrinks with
the suffix. The regression reproduced an attempted edit whose expected suffix had
three spaces while the document had two. Tests now cover correction followed by
another word, completion of that word, undo, and removal of extra spaces.

## Follow-up pass 2: release timing during focus checks

Every key release previously advanced the event revision. Releasing Space while
the accessibility focus query was pending therefore canceled correction even
though the text and target were unchanged. Ordinary releases now update held-key
state without invalidating that query. Modifier releases still invalidate it,
because they can trigger gestures or layout changes.

A deterministic test pauses the focus query, releases Space, and confirms that
correction completes. Companion cases confirm that a click, new text, or a Ctrl
gesture still cancels the pending check. The release case failed before the fix
with no correction scheduled, then passed after it.

Both passes preserve shortcuts, acceptance rules, correction thresholds, and
submission behavior. They improve existing interactions without asking users to
pause, add separators, or learn another workflow. Concurrent text during dictionary
search remains a separate limitation; this change only removes cancellation caused
by harmless releases. Native desktop timing still requires live validation.

## Follow-up pass 3: changing fields during a slow desktop reply

The renewed review followed automatic correction, explicit spelling and completion,
manual layout recovery, undo, and editing after navigation. A shared context query
could undo the benefit of immediate fresh-field capture: after a click and new
typing, a late focus or empty-field reply from the old cursor context cleared the
new word buffer and suppressed correction. It could also cancel a newer correction
that was already queued. Users would then have to reach another boundary and try
again, despite having typed a complete word in the new field.

Both query paths now discard replies whose cursor-context generation has changed
before touching current state. This covers focus validation used by typing,
undo, and preview acceptance, plus the empty-field check used to resume capture.
The regression failed before the fix, then passed for both query paths with a new
word still being typed, a correction already pending, and navigation suppression
already established. It also checks that overlapping keys without a new context
still suppress automatic correction: a delayed first key cannot turn an unknown
prefix into a trusted word.

This removes an avoidable retry without changing correction confidence, shortcuts,
or the set of supported native controls. It does not solve sentence submission,
untracked pasted text, or text arriving during dictionary search. Those still need
validated native context rather than another demo gesture. The tests simulate
delayed desktop replies; they do not establish live application timing or correction
recall during everyday use.
