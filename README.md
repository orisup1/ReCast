<p align="center">
  <img src="assets/recast-icon.svg" width="128" height="128" alt="ReCast logo">
</p>

<h1 align="center">ReCast</h1>

ReCast runs in the background and fixes English/Hebrew keyboard-layout mistakes
as you type. It switches layouts and replaces the mistyped word, corrects English
spelling (`recieve` → `receive`), and offers word completion and custom abbreviations.
Everything runs locally, with dictionaries embedded in the executable. Your
clipboard is never touched.

## How it works

ReCast checks words when you finish them with Space, preserving trailing punctuation.
It uses the current keyboard layout, dictionary matches, and word frequency to decide
whether to rewrite them. Each word gets one rewrite or none; capitalization and
the terminator are preserved. Same-layout corrections keep an unchanged prefix
on screen and rewrite only the remaining suffix, reducing visible deletion.
Enter accepts an already selected suggestion and clears typing context. It never
starts an automatic rewrite or replays Enter, because that key may send a message
or execute a command instead of inserting a line break.
A confident English or Hebrew spelling fix can also correct a
wrong-layout word in the same replacement. Combined Hebrew repairs preserve
source punctuation and protect known English words, common names, ALL CAPS,
and saved exceptions. A confident English spelling repair takes precedence.

Spelling also checks missing, extra, or wrong first letters in longer words,
including combined opening mistakes (`xocmputer` → `computer`) and additional
mistakes later in the word (`ommunicaton` → `communication`). Missing, repeated,
extra, swapped, and wrong letters are checked throughout the word. A single
stray ending letter can recover the unchanged base (`computerx` → `computer`).
Words of eight or more letters can use the three-edit budget; six-letter words
can recover multiple cheap slips. Shorter tokens keep their existing limits.
These fixes also work when typed in the Hebrew layout.
Layout correction also recognizes English contractions and possessives such as
`don't`, `I'm`, and `keyboard's`, preserving capitalization and punctuation.
Layout correction recognizes unlisted Hebrew words with supported
one- through four-letter prefixes, such as `במקלדת`, `ושלמקלדת`, and `וכשהמקלדת`,
when the stem is a ranked dictionary word and the English reading is not a known word or common name.
Conservative spelling keeps its existing single-edit limit.

Spelling corrects recognized apostrophe forms by checking their stem and preserving
the suffix (`recieve's` → `receive's`). Valid forms and saved word exceptions remain
protected; arbitrary internal punctuation and digits are not spelling inputs.

Spelling correction protects frequency-attested dictionary words, curated
technical terms, ALL CAPS, and tokens containing
digits or internal punctuation. It uses word frequency and weighted typo costs;
the previous finished word can resolve close candidates using bounded phrase
evidence. Keeping the original token also competes with a replacement: frequency
evidence, short unknown words, and trusted compound components can prevent an
otherwise unique suggestion from winning. Reviewed nonword misspellings remain
repairable even when the broad layout dictionary contains them; valid rare words
and regional variants are not added to that reviewed list. Unfamiliar names or
jargon can still get unwanted fixes. When competing candidates remain too close,
ReCast leaves the word unchanged. Undo, ignored words,
and Conservative spelling give you control.
Navigation, mouse clicks, and focus changes cancel stale corrections where detected.

## Install

Enable **English and Hebrew keyboards** in your OS settings. Their order does not matter.

The release workflow produces these self-contained downloads; check
[GitHub Releases](https://github.com/orisup1/recast/releases) for available builds:

| File | Platform |
| --- | --- |
| `recastLinux` | Linux x86-64 |
| `ReCast.exe` | Windows x86-64, Windows 10+ |
| `recastMac` | macOS, Intel and Apple Silicon |
| `ReCast.app.zip` | macOS app bundle, Intel and Apple Silicon |
| `SHA256SUMS` | Download checksums |

For a source build, install Rust/Cargo, clone this repository, and run the commands
below from its root. No separate dictionary installation is needed.

### macOS

For a downloaded app, extract `ReCast.app.zip`, move the app to `/Applications`,
and open it from Finder. To build and install the app from source:

```bash
make app
```

Setup opens on first launch or whenever a requirement is missing. It checks
**Accessibility** and both keyboards, links to System Settings, and offers
**Check again**. Keyboard capture uses an active session event tap authorized by
Accessibility, so a separate Input Monitoring grant is not required.
If ReCast is missing from Accessibility, click **+**, then **Cmd+Shift+G**, and enter
the exact path shown in setup. **Show ReCast in Finder** reveals that copy.
Quit and reopen ReCast if permission changes require a relaunch.

For a release smoke test, use a fresh macOS account, install the downloaded app
in `/Applications`, and grant only Accessibility. Leave Input Monitoring off.
Enable English and Hebrew keyboards, reopen ReCast, and verify correction, undo,
and completion in TextEdit. Quit and reopen once more to check that access persists.
Automated tests cannot verify macOS privacy grants for a downloaded app.

Use **Settings > General > Start at login** in the menubar app for autostart. Alternatively,
`make service` installs a bare binary with a launchd LaunchAgent;
`make service-uninstall` removes that service. Terminal-launched binaries may have
permissions attributed to the terminal rather than ReCast.

After the keyboard listener is ready, **Practice correction and undo** opens once
in the tray app or Linux control window. Close it to skip, or reopen it from
**Help > Practice correction and undo** in the tray or the Linux controls.
On macOS and Windows, practice shows one step at a time and checks both installed
keyboards. Windows offers a link to Language settings; the checklist updates when
you return. The exercise uses a real local text field: try layout correction,
undo it, then complete `keyb` with Right Shift. Successful steps are confirmed by
the keyboard engine. Practice does not train word exceptions or personalization,
consume the first-correction hint, or change correction counts. Saved word rules
and your enabled settings still apply. Linux practice requires a desktop that
can identify the active application; its field is disabled otherwise.

### Linux

ReCast reads keyboards through `evdev` and writes corrections through `uinput`.
Your user needs access to both devices. Add yourself to the `input` group, then
log out and back in before installing the user service:

```bash
sudo usermod -aG input "$USER"
# After logging back in, from the repository:
make service
```

This builds and installs `~/.local/bin/recast` and starts a systemd user service.
Ensure `~/.local/bin` is on your `PATH`.

```bash
systemctl --user status recast
journalctl --user -u recast -f
systemctl --user restart recast
make service-uninstall
```

Layout backends support Hyprland, Sway, KDE, GNOME, and X11. Native GNOME/KDE
Wayland cannot identify the focused application; see [Application exclusions](#application-exclusions).
Disconnected keyboards are detected automatically; ReCast waits for reconnection.

### Windows

Run `ReCast.exe` for the tray app. **Settings > General > Start at login** registers it in the per-user
Run key. To build, install, and register a logon Scheduled Task from source:

```powershell
.\deploy.ps1 -Target service
```

The default install directory is `%USERPROFILE%\.local\bin`.
Use `-Target service-uninstall` to remove the task, or `-Target help` for all targets.

## Controls

The gestures below are defaults. In **Settings**, choose **Double-tap action
(undo / convert)**, **Completion (single tap)**, and **Spelling suggestions (single
tap)** to use Left/Right Ctrl or Left/Right Shift, or disable any gesture.
Double-tap actions also offer
**Either Ctrl**. Changes save and apply immediately; help, status, and practice
show your current bindings. Completion and spelling suggestions need separate keys
from each other, the double-tap action, and extra single-tap undo. Disable or move
the conflicting binding first.
Only bare taps shorter than 300 ms count; double taps must finish within 500 ms
of each other. Holding a modifier or using it in a chord keeps its normal behavior.

| Action | Gesture or control |
| --- | --- |
| Request spelling alternatives | Tap **Left Shift** mid-word; repeat to cycle choices and back to your original text |
| Complete an English or Hebrew word | Tap **Right Shift** mid-word; tap again to cycle through suggestions and back to your prefix |
| Reconsider an unchanged word | Tap **Ctrl twice** immediately, before or after its Space |
| Rescue selected text (macOS) | Select text in an editable field, then tap **Ctrl twice**; repeat to restore it |
| Convert an unchanged last word to the other layout | Tap **Ctrl twice within half a second**, even for unknown words |
| Undo the latest correction or completion | Tap **Ctrl twice** immediately, or use the optional single-Ctrl undo shortcut |
| Allow an ignored word again | Use the configured single-Ctrl undo shortcut after the word, or edit your word lists |
| Enable/disable or pause | Tray/menubar, Linux control window, or terminal dashboard |
| Inspect or ignore a correction | Open **Recent corrections…** in the tray, then choose **Details** or **Ignore**; in the Linux window, expand a correction and choose **Ignore this word** |
| Inspect live activity | **Help > Start live log…** in the tray/menubar, or **Live log…** in the Linux control window |
| Review gestures | **Help > Typing shortcuts** in the tray, or **Typing shortcuts** in the Linux control window |

Double-Ctrl undoes the latest automatic correction, abbreviation expansion, or
completion, restoring the original text or prefix. If the last word was unchanged,
it converts the word to the other keyboard layout, even if neither reading is a
dictionary word. Conversion works before or after Space and preserves the
separator. Repeating a manual conversion converts the word back without changing
saved exceptions or training spelling replacements. Up to 32 extra spaces preserve
correction undo and are restored with the original word. Other typing, Enter, or
moving the cursor ends the opportunity to act on a finished word.

The optional single-Ctrl shortcut handles undo/unlisting. If it shares the action
key, the single-tap action waits half a second so double taps take priority.
The double-tap action can still be remapped or disabled in Settings.

On macOS, selected-text rescue converts physical English/Hebrew key positions,
without dictionary checks. The first English or Hebrew letter determines the
source layout; select one language at a time for predictable results. Spaces,
line breaks, digits, and unmapped characters are preserved. Converted text stays
selected, and double-Ctrl again restores the exact original, including case.
It uses Accessibility selection editing, leaves the clipboard and keyboard layout
untouched, and requires an editable control that exposes writable selected-text
and selection-range attributes. Unsupported controls are left unchanged. Selections
are limited to 64 KiB of UTF-8 text. Windows/Linux selected-text rescue is not yet
available; manual word conversion works through the shared engine on all platforms.
App exclusions, pause/disabled state, and Secure Input still apply.

Holding Shift for capitals or Ctrl for shortcuts does not trigger these gestures.
**Settings → Extra undo shortcut** can additionally enable a single tap of
Left Ctrl or Right Ctrl. The configured double-tap action remains available unless disabled. Holding Ctrl or using
it in a chord never triggers the extra shortcut; the same cursor/focus safeguards
apply to both gestures.
Extra spaces preserve undo. Other typing, Enter, or cursor movement ends the undo
opportunity. Undo restores the original text and, when changed, the previous layout.
One undo suppresses that word
for the session; undo counts are saved in `learned.txt`, and two occasions make the
exception survive restarts. Allowing the word again clears its saved exception.
On macOS, an undo gesture completed while a correction is landing waits for that
correction to finish; later typing or cursor movement still cancels the queued undo.
During the destructive part of a macOS rewrite, keyboard events and mouse button
events wait briefly until the replacement is complete, then replay in their original
order. This keeps continued typing from canceling a partly erased word. Unsafe queued
input clears correction and undo context; bare modifier taps and spaces retain it.

The macOS/Windows tray opens a native **Settings** window with General, Shortcuts,
Applications, and Word rules sections. General offers language preference,
spelling, completion, spelling sensitivity, and Start at login. Sensitivity ranges from no automatic
spelling edits to Conservative (one edit), Balanced (two), and Flexible (three);
shorter words still have stricter limits. The status submenu suggests Conservative
spelling when many corrections have been undone. The Linux control window offers
language preference and live toggles for spelling, completion, and Conservative spelling. The
enabled/disabled switch survives restarts.

### Command line

```bash
recast                 # Linux: background daemon; macOS/Windows: tray app
recast -f              # Linux: stay in the foreground
recast -g              # Terminal dashboard (Linux/Windows)
recast -w              # Control window (Linux only)
recast --stop          # Linux/macOS; on Windows, quit from the tray
recast --status        # Running state and configuration diagnostics
recast --explain "recieve" --layout en  # Preview a correction without typing
recast --explain "יקךךם" --layout he    # Preview visible Hebrew-layout text
recast --write-config  # Create a commented config without overwriting one
recast --help          # All options and environment settings
```

The tray and Linux control window also offer **Pause in [application] until I
switch away** for the last detected external app. Switching to another identified
app ends this temporary pause automatically; **Resume in [application]** ends it
early. Unknown focus and ReCast's own controls preserve the pause. Saved app modes
remain unchanged and apply when correction resumes.

The Linux control window offers **Pause for 30 minutes** / **Resume** and
**Recent corrections**. Expand a correction to see its captured decision evidence,
then choose **Ignore this word** to save an exception. On macOS and Windows,
**Recent corrections…** opens a window with separate Original and Replacement
columns, timestamps, **Details**, and **Ignore**. It shows a snapshot of up to 20
corrections, up to five per page depending on window height. Choose **Refresh** to include newer corrections; rows
stay stable while you inspect or ignore a word. Closing Settings, history, or
practice leaves ReCast running in the tray. Undone corrections remain visible and marked. Details
include both layout readings, dictionary/form matches, frequency ranks, and spelling
scores and competing candidates when spelling was evaluated. Evidence is captured
when the decision is made, stays in the bounded in-memory history, and is not
recomputed using later settings. Inspecting a correction does not add a word rule.

`--explain` accepts one visible word with optional trailing punctuation and requires
an explicit `en` or `he` layout. It reports the replacement, planner operation,
layout evidence, spelling candidates, and protection or rejection reasons,
using this invocation's settings and saved lists. It never captures keys, changes
the OS layout, types text, or stops a running instance. It has no prior-word
history or live application checks; unsupported characters produce an error.

In the terminal dashboard, `e`/Space toggles correction, `p` pauses for 30 minutes,
`r` reloads lists, and `q` quits. Closing the control window or quitting the dashboard
ends ReCast. On macOS, use the menubar instead of the dashboard.

Starting ReCast normally replaces an existing instance. If a service manager would
immediately restart the old copy, ReCast explains how to stop that service first.
`--keep-others` bypasses replacement, but concurrent copies can correct text twice.
`--status` queries each running copy for its live settings, memory, listener health,
active application mode, and pause/disabled reason, including on Windows. An older
or unresponsive copy is reported as running with live status unavailable. If no
copy is running, it prints clearly labeled offline configuration diagnostics.

## Configuration and files

| OS | Configuration directory |
| --- | --- |
| Linux | `~/.config/recast/` (or `$XDG_CONFIG_HOME/recast/`) |
| macOS | `~/Library/Application Support/recast/` |
| Windows | `%APPDATA%\recast\` |

Live UI settings save to `config.toml` and apply immediately. Manual edits to that
file require a restart. Environment variables override the file; the UI explains
when an override prevents a change. Run `recast --write-config` for every supported
setting, including layout backends and injection timings.

The file accepts flat `key = value` lines and `#` comments, without tables or arrays.
Keys are environment names lowercased without `RECAST_`; for example,
`RECAST_SPELL_DIST=1` is `spell_dist = 1`.

```toml
# Example overrides; everything else uses its default.
prefer = "balanced" # balanced (default), english, or hebrew
spell_dist = 1      # Single-edit spelling fixes (default ceiling: 3)
complete = true    # Completion and abbreviations (default: true)
split = false      # Missing-space splitting (default: false)
personal = false   # Persistent personalization (default: false)
```

Spelling defaults to words of at least 4 characters and suggestions ranked within
50,000 for words of at least eight letters (20,000 for shorter words). Completion
defaults to prefixes of at least 3 characters and rank 30,000.
Short layout switches accept ranks up to 30,000; `short = false` restricts them
to ranks up to 500. Homograph overrides accept alternate readings ranked up to
5,000, requiring a tenfold frequency advantage without language history;
`freq = false` disables the frequency tie-break between valid readings in both layouts.

Choose **Balanced**, **Prefer English**, or **Prefer Hebrew** from **Language
preference** in the tray/menubar, **Settings > General**, or the Linux window.
In the terminal dashboard (`recast -g`), press **b** to cycle through these modes.
Changes save and apply immediately. For a command-line launch, use
`RECAST_PREFER=english recast` (or `hebrew` / `balanced`); services use
`prefer = "english"` in `config.toml` after a restart. `recast --status` shows
the active preference; `--explain` includes its decision checks.

**Balanced** retains the existing symmetric rules. A preference adds checks only
to automatic switches into the other language: exact targets of 2–3 letters must
rank within 500, longer targets within 5,000. A recognized preferred-language
word stays unchanged without an established run in the other language. After two
confirmed words in that language, the limits relax to 5,000 and 20,000 respectively.
Combined layout and spelling repairs require at least five target letters and
rank within 5,000 (20,000 with an established run). Supported Hebrew prefixes can
use their ranked dictionary stem. Unranked targets are left unchanged.
These conservative heuristics trade some automatic corrections for fewer unwanted
switches; they are not measured confidence probabilities. Switching toward the
preferred language retains all Balanced protections. Explicit conversion,
completion, abbreviations, and spelling within the current language work as usual.
Automatic learned layout replacements also pass the preference checks.

An absent config uses defaults. Invalid values produce diagnostics and fall back;
an unreadable config stops startup so exclusions are not silently discarded.

| File | Purpose |
| --- | --- |
| `config.toml` | Settings; optional until created or saved from the UI |
| `abbrev.txt` | Custom `abbr = expansion` rules; `#` comments allowed |
| `ignore.txt` | Words to leave alone, one per line; `#` comments allowed |
| `learned.txt` | Undo counts used for persistent exceptions |
| `state.txt` | Saved enabled/disabled state |
| `welcomed` | Marker for the one-time correction hint |
| `setup-complete` | macOS setup marker; missing requirements still reopen setup |
| `practice-offered` | Marker for the optional first-run practice window |
| `personal/` | Opt-in personalization files, `rules.txt` rule counts, and `usefulness.txt` usage counts |

`abbrev.txt` and `ignore.txt` reload within about two seconds of edits.
**Settings > General > Advanced settings** opens the config file in your editor,
creating it if needed without overwriting existing content. **Settings > Word
rules** opens the native editors for ignored words, learned exceptions, and
abbreviations. **Reload lists** is available in that section.

For abbreviations, add rules to `abbrev.txt`:

```text
btw = by the way
addr = 1 Main Street, Tel Aviv
```

Rules expand when you finish the word and take priority over automatic corrections.
Right Shift also offers an expansion. Capitalization follows your input:
`Btw` becomes `By the way`. No abbreviations ship enabled; `complete = false`
disables both abbreviations and word completion.

The separate spelling-suggestion gesture (Left Shift by default) requests
alternatives in the current language, including short misspellings (`hte` → `the`)
and valid English words (`form` / `from`). It works with automatic spelling and
completion disabled, never switches layout, and uses the same cycling and undo.
Requests stay bounded to four choices and preserve capitalization and trailing
punctuation. Choosing an alternative to a valid dictionary word does not train
an automatic replacement for that word.

Word completion uses the active English or Hebrew layout. It first offers exact
prefix matches; if there are none, it tries one missing, extra, wrong, or swapped
prefix letter. Cycling and undo always restore your original prefix. Prefix
length limits count characters in either language. English apostrophe prefixes
such as `don'` can complete to `don't`; saved apostrophe words are also candidates.
Saved words in `ignore.txt` and persistent undo exceptions are also completion candidates, including names
and technical terms absent from the bundled dictionary.

### Application exclusions

On macOS and Windows, open **Settings > Applications** and choose the last detected
app or a saved app. **Use last application** refreshes that list after switching
apps. On Linux, use **Application modes** in the control window:

- **Full correction** follows the global spelling/completion settings.
- **Layout only** keeps layout correction and undo, with spelling, abbreviations,
  completion, and personalization disabled in that app.
- **Off** excludes the app from processing.

Choose a saved application and set its mode to **Full correction** to restore it. Changes save together and apply
immediately; an in-flight replacement is canceled if its app mode changes.
You can also edit these values and restart:

```toml
# Exact IDs, comma-separated and case-insensitive; no wildcards.
exclude_apps = "Alacritty, org.keepassxc.KeePassXC"
layout_only_apps = "code, org.gnome.TextEditor"
undo_shortcut = "right_ctrl" # optional single-tap undo; none (default), left_ctrl, right_ctrl
# Shortcut defaults; these settings are global, not per-application.
action_shortcut = "ctrl" # ctrl, left_ctrl, right_ctrl, left_shift, right_shift, none
completion_shortcut = "right_shift" # left_ctrl, right_ctrl, left_shift, right_shift, none
correction_shortcut = "left_shift" # explicit spelling alternatives; same choices, separate key
# macOS uses bundle IDs; Windows uses executable filenames including .exe.
```

A conflicting shortcut configuration keeps action/undo bindings first, then completion,
and disables conflicting completion or spelling-suggestion gestures at startup.
Diagnostics report the disabled gesture. Abbreviation expansion is unaffected.

Excluded apps receive no corrections, expansions, completion, undo rewrites,
word logging, or learning. Global events still track key releases. Off takes
precedence if an app appears in both lists. With any application restrictions
configured, an unknown active application also suspends processing.
On **native GNOME/KDE Wayland**, this means correction pauses throughout the session;
the Linux window disables adding restrictions when detection is unavailable.
After returning to an allowed app, finish the current word with Space/Enter to resume.

The compact tray menu contains Status, Pause, Recent corrections, Language preference,
Settings, Help, and Quit. Status includes the current application mode, recovery details, and
correction counts. **Pause correction** groups temporary pauses and **Disable until
I enable it again**, which persists across restarts. The icon distinguishes active,
paused, disabled, and attention states by shape. macOS uses a monochrome template
icon that adapts to the menubar appearance. The Linux window uses the same status
wording and groups pause controls.

The status submenu and Linux window explain Active, Disabled, Paused,
Excluded application, Secure Input, unavailable keyboard capture, and unknown
application identity, with a suggested recovery action. Routine corrections stay
silent. `--status` exposes the same status from the running process.

### Word-rule editor

Open **Settings** in the tray/menubar and select the **Word rules** section, or
expand **Settings > Word rules** in the Linux control window.
Choose **Ignored words**, **Learned exceptions**, or **Abbreviations**. Add or
remove words one per line; write abbreviations as `shortcut = expansion`.
Learned exceptions shows saved exceptions from repeated undos; single-undo
session exceptions are not listed. Other lists still apply when you remove a word.

**Save** applies changes immediately. **Cancel** leaves the rules unchanged.
Correction pauses while the editor is open and returns to its previous enabled
or paused state when you close it. Invalid entries and failed saves keep your
edits open. If rules changed on disk while you were editing, copy your edits and
reopen the editor before saving, so newer changes are not overwritten.

## Privacy

ReCast processes global keyboard events locally: no telemetry, remote dictionaries,
or update checks. Recent corrections stay in memory. Undo counts and explicitly
ignored words are saved locally; debug logging and personalization are off by default.
Synthetic input replaces text without using the clipboard.

The live log starts only when you choose **Help > Start live log…** in the tray/menubar
or press **Start logging** in the Linux control window. It shows timestamps,
correction and undo text, and Accessibility, listener, or keyboard setup issues
as they appear.
Closing its window stops recording and clears the entries; **Stop live log**
stops it while keeping the window open for review. Entries stay in memory only,
up to 500 lines. A new session clears earlier entries. Leave it off when typing
sensitive text.

Live status uses an authenticated connection on localhost only. Its random access
token is stored under the user's cache directory (owner-only on Unix); status
responses contain configuration and application identity, never typed words or
recent corrections. Nothing is sent to a remote service.

On macOS, ReCast suspends processing while the OS reports **Secure Input**.
Linux and Windows have no equivalent check: exclusions can protect a whole app,
but cannot identify individual password fields in an allowed browser or application.

`RECAST_DEBUG=1` prints checked words and may expose sensitive text in service logs.
Set `rule_stats = true` in `config.toml` (or `RECAST_RULE_STATS=1`) to save
automatic correction and undo counts in `personal/rules.txt`. Rows cover layout,
missing-space split, spelling or learned replacement, abbreviation, and combined
layout plus spelling. They contain no typed words, application names, or key
timings. Manual conversions and completion taps are excluded. Counts update on
successful text replacements and flush about every 30 seconds. This setting is
independent of personalization and is off by default.

The same setting saves aggregate usefulness counters in `personal/usefulness.txt`:
completion sessions, extra cycling taps, accepted choices, first-choice acceptance,
abandoned offers, automatic corrections, and correction undos. One session begins
with a successfully inserted offer; acceptance requires finishing the chosen word.
Returning to the original prefix, continuing to edit, undo, or losing context ends
an offer as abandoned. Unresolved offers are not counted as accepted or abandoned.
These counters contain no words or application IDs, flush about every 30 seconds,
and appear in `recast --status`. The counts support acceptance and undo ratios;
they do not automatically retune ranking. `--clear-personal-data` removes them.

`RECAST_PERSONAL=1` saves word/correction data and aggregate key timings under
`personal/`; on Linux/Windows, this may include password-field text. Personal files
are user-only on Unix. Automatic corrections train word frequency and replacement
pairs only after subsequent typing retains the result; undo, deletion, and detected
focus changes cancel pending learning. Completion offers do not train replacement
pairs merely by being displayed: only a retained choice, finished with Space or
Enter, can train word frequency or a manual spelling repair.
Completion ranks exact and single-typo prefix matches together, penalizing edits
and accounting for the taps needed to reach each choice. Frequency and context
lead; saved keystrokes add at most a 60% ranking benefit, so long words cannot
dominate merely by having a long suffix. One-letter completions retain their
frequency weight. Otherwise equal capped scores prefer the shorter word.
For three-letter prefixes,
typo recovery runs only when no exact offer exists. Configured abbreviations still
come first. Retained word frequency influences ranking when personalization is on.

Completion and manually requested spelling offers use bundled English and
Hebrew phrase priors immediately, even with personalization off. Curated weights
are supplemented by 994 measured pairs from two public Leipzig news samples,
each containing 10,000 sentences. Measured pairs require at least five occurrences,
conditional frequency of 5%, and association of at least three times the independent
word-frequency expectation. Weights remain bounded to four; counts and pinned source
checksums are in `src/phrase_counts.tsv`. Only the previous finished word
is needed. Personalization additionally boosts pairs retained at least twice in the
current typing context; these pair counts remain in memory. Focus changes, cursor
moves, deletion, undo, and Enter clear phrase context; punctuation ends the
previous-word link. Disabling personalization discards learned pair counts while
keeping the public phrase priors available.

Tap the completion shortcut (Right Shift by default) before Space to request an
English or Hebrew spelling suggestion when the prefix has no exact completion. Repeated taps
cycle alternatives and return to the original text. Phrase context ranks these
manual offers; automatic spelling retains its confidence gates. Existing words
are not reconsidered by the manual spelling list. Explicit abbreviations retain
priority. Finishing a selected spelling repair of the same or shorter length can
train its correction pair when personalization is enabled; unaccepted offers and
longer prefix completions do not.

Automatic English spelling also uses phrase evidence when it would otherwise
abstain because candidates are too close. Only candidates within four score units
of the original best are reconsidered; a phrase weight of at least two can lower
a candidate's score by at most eight units. The winner must still lead by at least
four units. Phrase context never changes an already confident spelling decision
or widens edit-distance and frequency limits. Word protection, ALL CAPS, ignored
words, and undo exceptions still apply. This also supports a combined layout and
spelling repair. Recent correction details include the phrase weight and discount.

Hebrew spelling corrections and manual offers search one letter insertion, deletion,
substitution, or transposition, favor missing/extra ו and י, enforce final-letter
positions, and recognize the layout planner's supported prefix stacks. Candidates
need a ranked dictionary word or stem. Existing words and saved exceptions are
preserved; the spelling enable, minimum-length, and rank settings still apply.
Automatic Hebrew corrections require at least four letters and a 20-point lead
over both the next candidate and keeping the original; phrase context contributes
at most eight points to candidate ranking. Neighboring physical keys and repeated
letters receive lower costs. These cheap repairs use the configured spelling rank
ceiling; speculative edits retain a ceiling of 20,000. Inferred prefixes receive
a score surcharge rather than inheriting the stem's full confidence. Correction
details show candidates, costs, frequency rejections, prefix evidence, learned
discounts, and the keep-original comparison.
Manual spelling search normalizes niqqud and Hebrew
or ASCII quotes for lookup, preserving them attached to their original letters
in offered repairs. A decorated letter cannot be deleted. Automatic Hebrew spelling
accepts plain letters only. Live keyboard capture
still supports the existing unshifted Hebrew keymap; this does not add niqqud input
capture or new Linux injection mappings.
Ambiguous candidates remain available through the completion gesture. Hebrew
repairs preserve the current layout and punctuation, replace only the changed
suffix, and support the same undo and retained-correction learning as English.

Before a combined layout and spelling repair, both language assessments compete.
A candidate rejected by its own confidence gate can still veto an ambiguous switch.
Reading source letter keys as alternate-language punctuation adds uncertainty;
English punctuation is preserved when evaluating a Hebrew repair. Exact layout
decisions retain their existing word, frequency, and history guards. A supported
phrase in the preceding word's language can resolve the language choice, without
bypassing either speller's acceptance gates. Scores are heuristics, not confidence
percentages.

When the current layout has no exact completion, known word, or explicit
abbreviation, the completion gesture also checks the other layout. An exact
alternate-layout prefix match can replace weak current-layout offers. Acceptance
by the completion tap switches layout and inserts the completion together. Cycling
back or undo restores the original text and requests the original layout. A refused
initial layout switch cancels insertion. No automatic layout change happens from
these completion suggestions before the gesture.

With personalization enabled, repeatedly retained English and Hebrew correction pairs also
adjust the ranking cost of transpositions, neighboring-key substitutions, and
missing letters (including extra letters in Hebrew). Each language trains its own
counts. The discount is capped at 15 score units, does not widen edit-distance or frequency
limits, and never trains from a single observation. Undo withdraws one vote for the
rejected correction, weakening its learned edit-class evidence.

Learned exact layout pairs switch in either direction, including in layout-only
mode, and skip spelling search. Configured abbreviations take precedence, unknown
layouts remain untouched, and single-letter inputs never trigger learned repairs.
Repeated Hebrew spelling pairs stay in Hebrew. Learning requires `personal = true`.

Learned replacements require at least two retained votes, twice the votes for all
competing replacements combined, and a lead of at least two votes. Ties and weak
leads do not trigger a learned replacement. Correction-pair votes halve every
30 days, including their contribution to learned typo costs. New observations do
not reset the aging period. The local `personal/confusions.txt` file stores the
decay timestamp in a fourth column; existing three-column files remain readable
and start aging when first loaded by this version.

With personalization enabled, words retained at least twice also become completion
candidates even when absent from the bundled dictionary. To clear these files,
**stop ReCast first**, then run:

```bash
recast --clear-personal-data
```

This removes ReCast's personalization and rule-statistics files, not your ignored
words or undo exceptions.

## Troubleshooting

- **No corrections:** check the enabled state, installed keyboards, permissions,
  exclusions, and `recast --status`. On Linux, verify `input` membership and `uinput`
  access, then inspect the service journal.
- **Wrong Linux backend:** set `layout_backend = "hyprland"` (or `sway`, `kde`,
  `gnome`, `x11`) and restart. Hyprland needs the current
  `HYPRLAND_INSTANCE_SIGNATURE` and `XDG_RUNTIME_DIR` in the process environment.
- **macOS permissions broken after reinstall:** remove the old Accessibility entry
  and add the running copy shown in setup. `make app` resets privacy grants;
  for a manual reinstall, `tccutil reset All com.recast.app` resets them before you
  grant access again. Relaunch afterward if needed.
- **Unwanted spelling fixes:** undo immediately, ignore the word, enable
  Conservative spelling, or disable spelling in Settings.

## Development

`cargo test independent_correction_evaluation -- --nocapture` reports unwanted
changes, missed fixes, wrong fixes, precision, and recall on independently authored
evaluation cases in `tests/data/evaluation.tsv`. Standalone cases were frozen before
the Hebrew and phrase changes, with exact baselines for existing failures. Expected
results still count toward accuracy; a passing baseline test does not imply perfect
correction. These synthetic cases are not a random sample of real user typing and
must not be used to tune thresholds. The original regression corpus remains separate.
Reviewed baseline changes are recorded separately in
`tests/data/evaluation_updates.tsv`, leaving the frozen examples unchanged. Each
update must name the exact old and new result and a review reason; stale updates,
new errors, and unreviewed changes fail the test. Replacing a wrong correction
with abstention is allowed only as an explicit update and still counts as a miss.

Regenerate the measured phrase priors by downloading the two public archives named
in `src/phrase_counts.tsv`, then running:

```bash
python3 scripts/build_phrases.py /path/to/eng_news_2020_10K.tar.gz /path/to/heb_news_2020_10K.tar.gz
python3 -m unittest discover -s scripts -p 'test_*.py'
```

The builder verifies pinned SHA256 checksums, respects sentence and punctuation
boundaries, and keeps at most 512 pairs per language. Add `--check` to verify the
committed output without changing it. Only aggregate phrase counts are bundled;
the app never downloads corpora or sends typed text to a server.

```bash
cargo build --locked --release
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --release --test cli
cargo test correction_accuracy_corpus -- --nocapture
make bench
```

See [PERFORMANCE.md](PERFORMANCE.md) for search optimizations, benchmark scope,
and further latency opportunities.

The shared correction planner is in [src/dictionary/mod.rs](src/dictionary/mod.rs), with
spelling in [src/spell/mod.rs](src/spell/mod.rs) and completion/lists in
[src/complete.rs](src/complete.rs). [src/platform/engine.rs](src/platform/engine.rs)
owns typing, cancellation, completion cycling, and undo across all platforms.
Native adapters handle capture/injection; [src/layout](src/layout) handles layout backends.

Add real correction reports to [tests/data/corrections.tsv](tests/data/corrections.tsv).
Optional sequence and application-mode columns exercise language history across
words, switches between English and Hebrew, and layout-only behavior. A different
sequence or application mode resets history, matching the engine's cancellation.
The corpus separates unwanted, missed, and wrong corrections; it is a regression
check, not an estimate of accuracy for all typing. Engine tests use a simulated
screen; focus benchmarks require a desktop session.

[CI](.github/workflows/ci.yml) checks formatting, tests, Clippy, release builds, and
release CLI behavior on Linux, macOS, and Windows. The manual
[Binaries workflow](.github/workflows/binaries.yml) publishes release assets and
checksums, with signing/notarization when repository credentials are configured.
`make help` and `.\deploy.ps1 -Target help` list build, install, and service targets.

## Dictionary expansion

`en_dict.txt` and `en_tech.txt` include curated programming terms, tools, formats,
and infrastructure names. `en_tech.txt` is merged at build time and explicitly
protected from spelling changes even when a term is absent from the frequency list.
These additions enable wrong-layout recovery without inventing frequency rankings
or promoting jargon into general-purpose spelling suggestions. Their unchanged English readings are covered by the correction corpus.

Spelling correction also considers missing, extra, or wrong first letters
on longer words, including later typos when the edit budget permits. Layout
correction recognizes supported Hebrew prefix stacks when the dictionary stem
has at least three letters and frequency rank at most 50,000; the full word must
have at least five letters. These inferred forms never override an exact
current-layout word when the layout is known.

Build-time preprocessing merges, deduplicates, and sorts dictionary entries for
binary search. Source dictionary order therefore does not affect lookup speed.
Do not reorder `en_freq.txt` or `he_freq.txt`: their line order defines frequency
rank, which controls correction decisions. Generated frequency blobs are sorted
by word while preserving those ranks.

English additions are imported automatically from the SCOWL-derived
[wooorm English dictionary](https://github.com/wooorm/dictionaries/tree/8cfea406b505e4d7df52d5a19bce525df98c54ab/dictionaries/en).
Only explicit lowercase entries of at least four letters that also occur in
`en_freq.txt` are added. This cross-check avoids importing raw subtitle noise,
capitalized names, or short layout collisions. Hunspell affixes are not expanded.
Existing entries and frequency rankings are preserved. Hebrew already contains
all pure Hebrew words of at least four letters in its bundled frequency list.

To repeat the import (Python 3, no packages required):

```bash
curl -fsSL https://raw.githubusercontent.com/wooorm/dictionaries/8cfea406b505e4d7df52d5a19bce525df98c54ab/dictionaries/en/index.dic -o /tmp/recast-en.dic
python3 scripts/expand_dictionary.py /tmp/recast-en.dic
python3 scripts/expand_dictionary.py /tmp/recast-en.dic --check
```

The importer verifies the pinned source checksum and adds only missing entries.
These additions are a filtered extract; SCOWL's notices and redistribution terms
are retained in [licenses/scowl.txt](licenses/scowl.txt).

## License

Application code: Apache-2.0 — see [LICENSE](LICENSE).
