# Delete/retype benchmark investigation

Tested on 2026-10-17 using the release build on the user's Linux/Hyprland desktop.
The final binary was installed at `~/.local/bin/recast` and the daemon restarted.

## Confirmed cause

The correction engine cleared its current-word buffer when Space finished a
word. The first Backspace after that saw an empty buffer, assumed deletion had
entered unknown text, and set `no_fix`. The first Space after retyping then
cleared this flag without correcting the word. Deleting that attempt and
repeating the cycle set the flag again.

Existing deletion tests hid this behavior: their simulated platform could
confirm that the input field was empty. Linux and Windows use the default
`Platform::input_empty`, which cannot confirm this. Disabling that simulated
probe reproduced the failure on the second iteration immediately.

## Fix

The engine now retains a bounded tail of the finished text and its separators,
independently of undo state. Successful replacement records the text actually
inserted, including changes in word length and layout. Backspace can therefore
remove known spaces, re-enter the known word, and recognize when the tracked
word has been fully deleted. Extra Backspaces do not suppress the next freshly
captured word in that same context. Accepting a completion also records this
known tail.

Navigation, focus changes, unknown text, and shortcuts still invalidate this
knowledge. A failed replacement restores its previous deletion context only
when the existing retry checks permit it.

## Experiments

Native trials used a disposable GTK text view and a virtual keyboard that feeds
real evdev events through the daemon. They did not call the planner directly or
use a Wayland typing tool that bypasses evdev capture. Each trial typed the
misspelling and Space, observed the field, deleted its contents, then typed the
same misspelling again without resetting focus or adding a recovery Space.
The test restored the previous window focus and keyboard layout afterward.

| Scenario | Before | After the deletion fix |
| --- | --- | --- |
| `recieve `, exact Backspaces | 1/5 correct | 10/10 correct |
| `recieve `, four extra Backspaces | 1/5 correct | 10/10 correct |
| `recieve `, Ctrl+Backspace | 1/5 correct | 1/10 correct; shortcut protection remains |
| `recieve `, select-all then Backspace | 1/5 correct | 1/10 correct; shortcut protection remains |

A further final-release run used `adress` with three trailing spaces. A clean
repeat passed 10/10 exact deletions, 10/10 extra-Backspace deletions, and 10/10
held-Backspace deletions. Three additional daemon restarts each passed 5/5 exact
deletion cycles with that word and spacing, for 15/15 cold-start trials.

An earlier startup stress run produced corrupt replacement text in its first
iteration (`adrdress`) and incorrect subsequent replacements during exact
deletion. The same run's extra-Backspace and held-Backspace scenarios passed
20/20, and the corruption did not recur in the subsequent 45 trials described
above. This is an unresolved native input/injection anomaly; these results do
not establish its cause or claim that the deletion-state fix eliminates it.
Raw observations are retained in `/tmp/recast-native-final.json`,
`/tmp/recast-native-final-recheck.json`, and
`/tmp/recast-native-cold-{0,1,2}.json`.

## Regression coverage and checks

`delete_and_retype_corrects_without_an_empty_field_probe` runs 240 automatic
correction/deletion cycles with the empty-field probe disabled. It covers
English spelling, merged-token replacement, both layout directions, combined
layout/spelling, one or three spaces, exact deletion, and four extra
Backspaces. It also checks deletion after accepting a completion and retains
navigation suppression.

Run the regression with:

```sh
cargo test delete_and_retype_corrects_without_an_empty_field_probe -- --nocapture
```

The full suite passed 327 unit tests and 6 CLI tests, with 7 tests ignored.
Formatting, diff checks, and Clippy for Linux, macOS, and Windows passed. Native
runtime experiments were performed on Linux only.

## Remaining limits

Ctrl+Backspace and select-all/delete still discard context because their effects
vary by application and the engine cannot confirm an empty field on Linux or
Windows. Their next Space establishes a new boundary instead of applying an
automatic correction. The Backspace fix does not change this shortcut policy.
The separate native corruption incident above also remains unresolved.
