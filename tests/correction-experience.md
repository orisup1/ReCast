# Correction experience review

Reviewed the shared typing engine, correction planner, replacement and replay path,
layout handling, shortcuts, undo, setup documentation, and regression coverage.
This is a code and simulated-typing review, not an observed user study or a native
application acceptance test.

## Everyday typing

The core experience should require only ordinary typing. Space already triggers
layout and spelling checks, including punctuation-framed words. Search runs away
from capture, and same-layout spelling replacements retain the unchanged prefix.
These choices reduce typing stalls and visible deletion without teaching a new habit.

A reproducible gap made this experience depend on timing: only one waiting word
check survived while another search ran. Typing `keyboard keyboad recieve ` during
a delayed first search dropped the check for `keyboad`. The last typo still
corrected, making the behavior appear inconsistent. Waiting between words hid it.

The engine now queues finished-word checks in order and pauses that worker during
replacement. A check racing with a replacement is deferred when its captured text
still matches. The queue is bounded; existing limits on following text, layout,
focus, input permission, and exact native text matching remain in force. Regression
coverage includes two and three corrections in an uninterrupted burst, plus
cancellation by submission, navigation, click, focus, layout, secure input, and
app disablement. No settings or shortcuts change.

## Other correction experiences

- **Wrong keyboard layout:** automatic conversion also switches the keyboard for
  subsequent typing. Ambiguous valid words remain protected. Once following text
  arrives, a delayed layout-changing fix is still declined because physical keys
  alone cannot prove which language that text used. Making this seamless needs a
  verified native text-and-caret snapshot, rather than weaker confidence gates.
- **Spelling:** confidence, known-word, case, punctuation, and saved-word guards
  prevent disruptive rewrites. This review does not relax thresholds to create
  more visible demonstrations of activity. A missed fix is less disruptive than
  changing a correctly typed name or technical term.
- **Suggestions and completion:** existing explicit shortcuts, original-text
  cycling, and passive previews remain optional. Automatic correction must not
  depend on learning these gestures. No new popup, acceptance key, or forced
  suggestion interaction is introduced.
- **Undo:** existing correction undo preserves bounded subsequent typing and
  rejects stale targets. An unwanted correction is suppressed for the session.
  The fix retains these mechanisms; it does not repurpose Backspace or the host
  application's undo shortcut.
- **Editing existing text:** clicks, navigation, selection changes, paste, and
  untracked deletion can invalidate captured word boundaries. An empty supported
  field already allows immediate recovery. Nonempty fields still need a trusted
  boundary. A future native word-before-caret query could remove this limitation
  without asking the user to type an extra space or select a word.
- **Enter and Tab:** these keys can submit text, execute commands, or move focus.
  Replaying them after asynchronous correction would change ordinary app behavior.
  Enter stays a context boundary, not an automatic correction trigger.
- **Setup:** practice is optional; ordinary applications are the intended path.
  Keyboard and accessibility requirements remain necessary. No added training,
  practice requirement, configuration step, or habit change.

## Remaining limits

The queue improves words finished during search. Whole words typed during the
native replacement itself still use the existing replay bookkeeping; this change
does not add automatic checks for every replayed word. Delayed spelling fixes also
require supported native verification once non-space text follows, and abandon
checks beyond the existing 32-character suffix limit. Unsupported controls and
layout-changing delayed fixes can therefore still miss corrections.

Next useful work is application-specific verification of fast typing, manual edits,
and caret recovery in real editors, browsers, and messaging fields. Do not ask users
to pause, add separators, select text, or rehearse example mistakes to compensate
for unsupported native behavior.

## Iteration log

### Pass 1: uninterrupted typing

Reproduced a dropped middle-word check before changing code. Replaced the single
waiting slot with a bounded FIFO, pausing search during replacement and deferring
matching work that raced with injection. Verified ordinary typing, overflow,
and stale-context cancellation. The unit suite passed (422 passed, 12 ignored),
Clippy passed with warnings denied, and formatting and whitespace checks passed.
The final overflow regression also passed separately. Existing unrelated Windows
and layout work was excluded from this change.

### Pass 2: backlog latency

Repeated the correction-path review with a burst longer than the verification
window. Although expired checks were safely rejected, they still performed their
dictionary searches before rejection, delaying fresh words. The worker now skips
checks from an obsolete generation or beyond the existing suffix limit before
search. The overflow regression covers bounded storage, expired work, and two
recent corrections. No timing requirement or additional user action is introduced.
Validation: the rapid-word, overflow, and cancellation regression passed.

### Pass 3: undo after a correction burst

Repeated the review around recovery from unwanted fixes. Added an end-to-end
assertion that, after two or three queued corrections, undo restores only the
latest original word and leaves earlier corrected words intact, including the
long-burst overflow case. Existing verified-undo tests also passed for continued
typing, word-boundary recovery, and text typed under a different layout. No new
undo gesture or acceptance action is needed. Validation: the burst regression
and both `verified_undo` tests passed.
