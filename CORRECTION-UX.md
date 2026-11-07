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

## Follow-up pass 4: requesting help without waiting for key releases

Reviewing finished-word suggestions exposed another unnecessary timing ritual.
Tapping Shift immediately after Space could silently do nothing while Space was
still held. Rebinding spelling or completion to Ctrl also made a tap fail when
the previous letter was still being released. The shared engine accepted letter
overlap only for Shift suggestions and omitted Space from that allowance.

Suggestions now reuse the typing-key eligibility already used by action taps,
while retaining the existing stricter Ctrl rules during replacement. A new key
pressed after the modifier still cancels the tap as a chord. No new gesture or
setting is needed.

The simulated keyboard incorrectly classified Space as a letter, unlike the
native key maps, which hid the first failure. That fixture now matches native
separator behavior. The new regression failed with no suggestion scheduled before
the engine fix. It passes for spelling and completion on all four Shift/Ctrl
bindings, with either a held letter or Space, checks that chords do not inject,
and verifies undo preserves the original word and separator. Existing safeguards
for injection, navigation, focus changes, and submission remain in place. Live
desktop timing and the remaining limitations above still need separate validation.

## Follow-up pass 5: backspacing into the preceding word

Ordinary editing exposed a gap between deleting a finished word immediately and
returning to it after starting the next word. Typing the next letter discarded
the known finished-word tail. Erasing that new word then recorded an empty tail,
so backspacing through the separator could not recover the preceding word for
further correction. A user could see the entire word while ReCast tracked only
the newly appended fragment.

The engine now retains its existing bounded tail while the following word is
unfinished and preserves that tail when the following word is erased. Backspace
can then reuse the existing recovery path. Changes in focus identity, including
loss of focus information, discard the tail, as do cursor invalidation and word
buffer overflow. No extra history buffer, context reads, or correction thresholds
were added. Backtracking remains limited to the last tracked finished word.

The regression failed before the fix because the recovered word buffer was empty.
It now covers a corrected or unchanged preceding word, one or three separators,
and ordinary correction after editing inside the recovered word, with empty-field
probing disabled. Negative cases verify navigation, focus loss, and overlong text
cannot reuse the older word. Live desktop timing, deeper edits, pasted context,
and the submission/search limitations above remain outside this change.

## Follow-up pass 6: keeping an original suggestion choice

The direct replacement cycle offered the original text between pages but did not
protect that choice from automatic correction. After browsing spelling or
completion suggestions and returning to `recieve`, Space could immediately change
it to `receive` again. Rejecting a suggestion therefore required another action.
The passive macOS preview already protected an accepted original choice.

A successful direct cycle now suppresses automatic correction for the current
unfinished word when the user returns to its original text. This decision is made
after replay restores the tracked word, so a Space already present or replayed
during replacement does not suppress the next word instead. Choosing another
candidate still establishes trusted text. No saved exception, learning change,
confidence adjustment, or new gesture is involved.

The regression reproduced the unwanted replacement on Space before the fix. It
passes for spelling and completion with the original chosen before Space, after
Space, or with Space replayed during replacement. Each case also verifies the next
occurrence still autocorrects and no ignore or learned exception was saved. These
are shared-engine simulations; native desktop behavior and the earlier remaining
limitations still need separate validation.

## Follow-up pass 7: continuous typing during final correction validation

The automatic path reused physical-event focus validation after dictionary
search. If the user started the next word during that final focus query, its
revision change made the old check clear the newer word buffer and suppress
correction. It could also cancel a correction for the next word that had already
been queued. Unlike the earlier cross-field case, this occurred in the same
cursor context, so the context-generation guard did not prevent it.

Post-search validation now observes focus and checks snapshot freshness before
changing state. An obsolete outcome is discarded without touching newer typing
or replacement state; already obsolete searches also skip the extra focus query.
Current mode, focus, layout, input revision, replacement state, and enablement
checks still gate any edit. A current result in an unsafe or changed application
mode still clears context, and practice accounting remains intact.

The regression pauses search and then final focus validation. It failed before
the fix because the next tracked word disappeared. It now verifies both ongoing
typing and an already queued newer correction survive, while Enter, focus changes,
secure input, and application exclusions prevent stale edits. The first word can
still miss correction when typing cancels its search; this change prevents that
miss from disrupting the next word. Native timing and the earlier submission and
context limitations still require separate validation.

## Follow-up pass 8: continuing after conversion, Space, and undo

Manually converting an unfinished word saved its original keys for undo, as
expected. Adding Space afterward updated the visible undo suffix but left those
keys marked as an unfinished prefix. Undo restored the right document text but
also put the already finished word into the active typing buffer. The next word
could then be checked as if it were concatenated with the restored word, despite
the separator still being visible.

The shared word-ending path now clears that saved prefix when Space finishes a
conversion whose active buffer is already empty. Completion acceptance and spaces
buffered during replacement already performed the same cleanup. Undo still
restores the original layout, text, and exact spaces; no native edit or injection
policy changed.

The regression failed before the fix because undo left a finished word in the
active buffer. It now covers conversion before or after the boundary, one or three
spaces, undo, and automatic correction of the next word without touching the
restored word. It also checks that manual conversion undo saves no exception.
Native desktop timing and the earlier remaining limitations still need separate
validation.
