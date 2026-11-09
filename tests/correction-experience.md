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
- **Editing existing text:** clicks and detected changes to the focused target
  already start a fresh capture immediately, including nonempty fields. Deleting
  known text also recovers without an empty-field probe. Navigation, paste, and
  untracked deletion can still invalidate captured word boundaries and require
  a trusted boundary. A future native word-before-caret query could improve those
  cases without asking the user to type an extra space or select a word.
- **Enter and Tab:** these keys can submit text, execute commands, or move focus.
  Replaying them after asynchronous correction would change ordinary app behavior.
  Enter stays a context boundary, not an automatic correction trigger.
- **Setup:** practice is optional; ordinary applications are the intended path.
  Keyboard and accessibility requirements remain necessary. No added training,
  practice requirement, configuration step, or habit change.

## Remaining limits

The queue now also checks finished words preserved by a successful replacement,
including words started before that replacement. Explicitly chosen text and
unknown editing fragments are not automatically reconsidered on acceptance.
Delayed spelling fixes still require supported native verification once non-space
text follows, and abandon
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

### Pass 4: punctuation and capitalization

Repeated the review for sentence-like typing rather than bare demonstration
words. Inspected punctuation framing and unchanged-prefix replacement, then ran
13 punctuation tests and the capitalized-continuation engine regression; all
passed. Quotes, brackets, apostrophes, and shifted punctuation survive supported
corrections. Capitalizing the next word with either Shift keeps a pending fix
alive and does not request suggestions. No additional production change was
justified; punctuation-triggered asynchronous edits would need their own safety
contract, so Space remains the automatic boundary.

### Pass 5: first words and ordinary editing

Repeated the review around clicking into a field and deleting/retyping text.
Verified that first words already correct after clicks, focus changes, and layout
shortcuts even when an empty-field probe is unavailable. Backtracking into the
previous tracked word also preserves correction. The delete/retype regression
exercises 240 correction/deletion cycles without an empty-field probe. All three
regressions passed. Clarified the review's distinction between fresh capture after
a click and unknown text after navigation/paste; no warm-up word, extra space,
or new editing habit is introduced.

### Pass 6: bilingual correction and failed switches

Repeated the review for layout correction, same-layout spelling, combined repairs,
and restoration of the original language. Inspected deferred layout confirmation
and pre-injection failure recovery. Both retained-bilingual-repair and failed-
preparation regressions passed: successful repairs keep the intended language;
a proven failure before editing restores the layout and allows the next ordinary
word to correct. Delayed layout-changing fixes still abstain after following text,
because silently reinterpreting those keys would risk corrupting user text. No
manual keyboard-switching ritual was added; native support remains the next step.

### Pass 7: optional help without gesture interference

Repeated the review around manual correction, completion, passive previews, and
returning to original text. Inspected gesture rollover/chord distinction and
preview acceptance. Three regressions passed: suggestion taps tolerate typing
rollover but reject actual chords; cycling to original text protects that choice
without disabling the next word; previews leave text unchanged until accepted
and reject stale context. Ordinary Shift capitalization remains covered by pass 4.
No new shortcut, automatic completion, or forced acceptance step was added.

### Pass 8: slow replies and worker ownership

Repeated the review with delayed focus responses and overlapping replacements.
A targeted run exposed a test waiting for an idle gap between consecutive edits;
consecutive queued edits need no such gap, so those tests now release the first
edit and await the second explicitly. Code inspection also found that a deferred
check could publish worker availability before its worker exited. Removed that
early release: only the worker loop relinquishes ownership under the state lock.
This prevents a replacement restart from creating two active search workers.
Validation: stale-query and responsive-suggestion regressions passed; delayed
focus, automatic cancellation, and burst/undo regressions each passed 10 runs.

### Pass 9: submission safety and habit-neutral guidance

Repeated the review for Enter, cursor changes, exclusions, secure input, and what
help tells users to do. Verified-edit cancellation and the broader interrupted-
replacement regression passed, including submission without replaying Enter.
Found misleading guidance: practice demanded immediate undo before more typing,
and shortcut help, practice, and the terminal dashboard still claimed undo could
save exceptions. Removed the unnecessary urgency and aligned all those surfaces
with session-only suppression and explicit ignoring. README undo guidance now
matches. Practice progression passed unchanged; shortcuts, correction thresholds,
and optional practice remain unchanged. Native verification limits still apply.

### Pass 10: final behavior and scope review

Rechecked the complete change against the original constraint: ordinary typing,
existing shortcuts, optional practice, conservative correction gates, and native
text verification are preserved. The production changes retain pending words,
avoid expired searches, serialize worker ownership, and remove misleading undo
pressure. Additional passes recorded passing behavior rather than inventing new
features or changing user habits.

Final validation passed:

- `cargo test --locked`: 422 unit tests passed, 12 ignored; all 6 CLI tests passed;
  macOS input-source main-thread and worker-thread checks passed.
- `cargo clippy --locked --all-targets -- -D warnings`.
- `cargo fmt --check` and `git diff --check`.
- Delayed-focus, automatic-cancellation, and rapid-burst/undo tests each passed
  10 consecutive targeted runs after the worker-handoff change.

The full suite includes correction accuracy and independent evaluation regression
checks; passing them does not establish real-world accuracy. Live typing in native
applications, downloaded-app privacy grants, and Windows/Linux runtime behavior
were not exercised. Existing unrelated layout and Windows edits remain outside
these commits. No app installation, deployment, or remote push was performed.

### Pass 11: continuous typing during replacement

Reproduced a second timing gap: `recieve keyboad recieve ` corrected only the
first word when the other words finished while its replacement was pending.
Those Space presses buffered text but did not request word checks. The successful
replacement now returns finished replayed words to the same bounded FIFO. One
shared focus snapshot covers the batch; native queries remain outside the typing
lock. Checks retain their original ordering and exact following-text verification.

Regression coverage includes English and Hebrew, words split between search and
replacement, punctuation, extra spaces, unfinished subsequent text, latest-only
undo, and keeping an original suggestion while the next typo corrects. Pending
replay checks cancel on submission, click, navigation, changed focus/layout,
secure input, disablement, and exclusions. Existing tests continue to cover
unknown fragments, overlong input, and preserving buffered text across layout undo.

Validation: `cargo test --locked` passed (423 unit tests, 12 ignored; 6 CLI tests;
macOS input-source checks on main and worker threads). Clippy with warnings denied,
formatting, and whitespace checks passed. This remains simulated typing coverage;
the installed app was not replaced and live native editing was not exercised.

Native follow-up: in disposable editor and chat drafts, type several typos at
ordinary speed, continue into the next word, and undo the latest correction.
Repeat after clicking into existing text and during manual editing. Record missed
corrections separately from text damage. No deliberate delay, extra separator,
or rehearsed mistake should be required.

## Ten-pass follow-up review

These ten passes review distinct parts of the correction experience. Changes are
grouped by related behavior rather than committing every test run. Evidence is
source inspection and simulated typing; this is not a native usability study.

### 1. Recovery after a failed correction

A deterministic preparation gate reproduced a stalled queue: the first edit failed
before touching text, but the next typo waited for another keystroke to restart.
Failure recovery now closes its replacement gate under the state lock before
waking queued checks. The regression verifies continued correction without more
input, latest-only undo, and cancellation by click, Enter, focus change, secure
input, disablement, and exclusions.

The full suite also exposed an older explicit-help fixture racing the automatic
checks added in the preceding review. That fixture now disables automatic spelling
while testing explicit spelling, uses unknown words for explicit conversion, and
waits for replay searches to finish. Automatic repair remains covered separately.

### 2. Context during fast typing

Pausing the check for `data` reproduced `data baye ` missing the existing contextual
repair to `data base `. Queued words had captured context before earlier checks
finished. The FIFO worker now reads phrase and language history when it starts each
check. No confidence threshold changes. A regression verifies correction, undo,
and no context leakage across Enter, clicks, or focus changes.

Validation for passes 1–2: 425 unit tests passed, 12 ignored; all 6 CLI tests and
the macOS input-source wrapper passed. Clippy with warnings denied, formatting,
and whitespace checks passed. Live application editing was not exercised.
