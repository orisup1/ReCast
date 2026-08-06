<p align="center">
  <img src="assets/recast-icon.svg" width="128" height="128" alt="ReCast logo">
</p>

<h1 align="center">ReCast</h1>

ReCast is a small background helper that watches what you type, checks each finished word
against language dictionaries, and automatically switches the keyboard layout between
English and Hebrew when it looks like you are typing in the wrong layout — then retypes the
mistyped word in the correct layout. It also autocorrects English typos, so a word that is
merely misspelled gets fixed in place instead — and completes words you are still typing.

## How it works

- Captures global key events on every supported platform.
- Builds up the current word from key presses, remembering the Shift / Caps Lock state of
  each one so a correction comes back capitalized the way you typed it; resets the buffer on
  cursor / focus-shifting keys (Tab, Escape, arrows, Home/End, PgUp/PgDn, Insert, Delete)
  and on mouse clicks (macOS / Windows).
- When you press Space or Enter, it interprets the typed key sequence as both an English
  and a Hebrew word and looks each up in the matching dictionary.
- **Punctuation is not part of the word.** A word you finished with `.`, `,`, `?`, `)` or
  a quote is looked up without it — the end of a clause or a sentence is where words most
  often end, and a correction that only fired before a bare space missed most of them.
  Whatever you typed comes back with the correction (`recieve,` → `receive,`), and
  punctuation *inside* a word stays part of it (`don't`).
- It **anchors on your live keyboard layout** (queried from the OS, with any English or
  Hebrew regional variant recognised). A sequence that already reads as a real word in your
  current layout is left untouched — including prefixed Hebrew forms (ו/ה/ל/ב/כ/מ/ש) and
  words whose other-layout reading happens to also be a dictionary word. It only switches
  when the *other* layout yields a confident word and the current one yields nothing real.
  This is what stops valid (and nested/prefixed) words from being mangled.
- On a switch it erases the mistyped word and puts the corrected one back **in one shot**,
  the way a paste lands rather than the way typing does — followed by the original
  Space/Enter. On macOS and Windows the word is inserted as text in a single event, so it
  appears at once and does not depend on the layout change having propagated; on Linux the
  whole erase + retype sequence goes to the virtual keyboard as one batch.
- If no layout switch applies and you are typing in English, a second pipeline compares the
  word *within* English and fixes near-miss typos in place (see
  [English autocorrect](#english-autocorrect)). Only one of the two ever acts on a word.
- Tapping **Right Shift** mid-word completes it — tap again to cycle through the other
  guesses — and abbreviations you define expand when the word is finished (see
  [Auto-complete](#auto-complete)).
- Tapping **Ctrl twice** right after a correction puts back what you typed and stops that
  word being corrected again (see [Undo](#undo)).
- Missing-space splitting (carving `helloעולם` into two words) is **opt-in** via
  `RECAST_SPLIT=1`; it is off by default because it cannot reliably tell a word we simply
  don't have in the dictionary from two run-together words.

## Supported platforms

| OS      | Capture         | Injection                        | Layout switch                                       |
| ------- | --------------- | -------------------------------- | --------------------------------------------------- |
| Linux   | `evdev`         | `uinput` keycodes, one batch     | `hyprctl switchxkblayout` (Hyprland only)           |
| macOS   | `CGEventTap`    | `CGEvent` Unicode string         | Carbon `TISSelectInputSource`                       |
| Windows | `rdev`          | one `SendInput` Unicode batch    | `LoadKeyboardLayoutW` + `WM_INPUTLANGCHANGEREQUEST` |

Linux additionally requires the user to be in the `input` group (for `evdev` read access)
and creates a `uinput` virtual device named `recast-injector` to replay corrected words.

## Setup

1. Install Rust (`rustup`, `cargo`).
2. Make sure both English and Hebrew layouts are installed in your OS keyboard settings.
   On Linux/Hyprland the xkb config must list English as layout 0 and Hebrew as layout 1.

### Prebuilt binaries

`exec/` holds ready-to-run builds if you would rather not install a toolchain. They are
self-contained — the dictionaries are inside the executable — so there is nothing to
unpack alongside them:

| File               | Target                                                     |
| ------------------ | ---------------------------------------------------------- |
| `exec/recastLinux` | Linux x86-64                                               |
| `exec/ReCast.exe`  | Windows x86-64 (no runtime DLLs needed; UCRT, Windows 10+) |
| `exec/recastMac`   | macOS arm64                                                |
| `exec/ReCast.app`  | macOS bundle                                               |

They are committed artifacts rather than build output, so they are only as current as the
last time someone refreshed them. That refresh is now a button: the **Binaries** workflow
(`.github/workflows/binaries.yml`, run from the Actions tab) builds all three on their own
runners and commits the results back here, so they no longer drift apart one platform at a
time. The next run also replaces the arm64-only macOS builds above with universal
(arm64 + x86-64) ones, which an Intel Mac can actually run. Building yourself is still the
recommended path; these exist so you can try it in one step.

The English and Hebrew dictionaries are baked into the binary at compile time, so the
executable is self-contained and runs identically from any working directory — no data
files or wrapper scripts to install. They are baked in *sorted*, so a lookup is a binary
search over the embedded bytes: nothing is parsed at startup and the daemon idles at a
few MB of memory instead of ~100 MB.

### Memory

ReCast is meant to start at login and still be running weeks later, which makes memory
growth a different kind of bug — there is no end of the run to hide it behind. So nothing
in it grows with use. The word buffer, the list of words you have undone and the
corrections history are all bounded at their source; the ~11 MB of dictionaries are
read-only pages of the executable rather than heap, so they are shared, never copied and
reclaimable by the OS under pressure.

Measured, not asserted: `cargo test` includes a check that ten times the work adds no
meaningful memory, and that the total stays under a 50 MB ceiling. On this machine it
settles at **about 9 MB and grows by hundredths of one** across the tenfold increase. `recast --status`
prints the figure so you can check a daemon that has been up for a month against it.

## Linux: full install + autostart

One-shot setup. Adds your user to the `input` group (required for `evdev` access),
builds in release mode, installs the binary to `~/.local/bin/recast`, and registers a
`systemd --user` unit that starts ReCast at login:

```bash
sudo usermod -aG input $USER && exec newgrp input <<< 'make service'
```

`newgrp` applies the new group to the current shell so you don't have to log out;
omit the `newgrp` part and re-login instead if you prefer. Make sure `~/.local/bin`
is on your `PATH`.

Manage the service:

```bash
systemctl --user status  recast       # health check
systemctl --user restart recast       # apply a rebuild
journalctl  --user -u    recast -f    # logs
make service-uninstall                  # stop + remove the unit
```

### Other Make targets

```bash
make              # cargo build --release
make install      # build + copy bin to ~/.local/bin
make deploy       # clean + build + install
make uninstall    # remove the installed bin
make run ARGS=-g  # cargo run with the GUI flag
make help         # full target list
```

Override the install root with `PREFIX=`, e.g. `make install PREFIX=/opt/recast`.

## macOS

```bash
make service
```

Writes a launchd LaunchAgent at `~/Library/LaunchAgents/org.recast.plist` and starts it.
You will need to grant the binary **Input Monitoring** and **Accessibility** permissions
in System Settings → Privacy & Security the first time it runs.

reset app permision with 
```bash
tccutil reset All com.recast.app
```

## Windows

PowerShell:

```powershell
.\deploy.ps1 -Target service
```

Builds, installs to `%USERPROFILE%\.local\bin`, and registers a Scheduled Task that
runs ReCast at logon. `.\deploy.ps1 -Target help` lists every target.

## Running directly

If you don't want a service, just run the binary (or `cargo run --release`) from any
shell. On Linux it daemonizes into the background by default (`-f`/`--foreground`
keeps it attached; under systemd this is detected automatically); on macOS/Windows
it stays in the tray/menubar.

```bash
recast          # start (Linux: forks into the background and writes a pidfile)
recast -s       # stop a running daemon (Linux only)
recast -g       # foreground with a terminal dashboard (TUI): status, log, toggle
recast -w       # foreground with a small control window (Linux only)
recast --status # what is running, and what is configured
recast -v       # version
recast -h       # full option list
```

**Starting ReCast replaces the ReCast that is already running.** Two at once is
never what you want: both see the same keystroke, both decide the same word
needs fixing, and both retype it — over the top of each other. So a new instance
stops the old one and waits for it to actually be gone before it starts. Pass
`--keep-others` if you really do want a second copy.

The exception is an instance held up by a service manager set to restart it
whenever it dies — the macOS LaunchAgent written by "Start at login" does this.
Stopping that one would only make launchd start it again, and the copy it starts
would stop *this* one, so ReCast says how to stop the service properly and exits
instead. On Linux, systemd is told to restart ReCast only when it *fails*, so the
service can be replaced; ReCast prints the `systemctl --user start recast` needed
to bring it back afterwards.

The TUI (`-g`, Linux/Windows) shows the enabled state, the counters and the
corrections themselves as they happen; `e`/`Space` toggles correction on and
off, `p` pauses it for half an hour, `r` re-reads your files, `q` quits. The
control window (`-w`) offers the toggle and counters in a tiny GUI window. On
macOS use the menubar menu instead.

The enabled/disabled switch is **remembered across restarts** — turning
correction off is a decision about the machine, not about one run of the
process — and `--status` reports it whether or not anything is running:

```
recast 0.7.0
  running:        yes (pid 4821)
  correction:     enabled
  start at login: yes
  config dir:     /home/you/.config/recast
  abbrev.txt:     3 abbreviation(s)
  ignore.txt:     7 word(s)
  memory (this):  8.9 MB

  settings (with any RECAST_* override applied):
    short words          on
    missing-space split  off
    frequency tie-break  on
    spelling             on  (min length 4, max rank 20000, max distance 3)
    auto-complete        on  (min prefix 3, max rank 30000)
```

The settings block reads back what the program actually resolved, which is the
only way to tell an override that was applied from one that was not. A value it
could not parse is reported rather than swallowed — `RECAST_SPELL_DIST=l` used
to fall back silently to the default 3, the *loosest* setting, from someone
plainly trying to tighten it:

```
  ! RECAST_SPELL_DIST="l" is not a number — using the default instead.
```

Environment variables:

```bash
RECAST_DEBUG=1 recast   # print every word check and switch decision
RECAST_SPLIT=1 recast   # opt-in missing-space splitting (off by default)
RECAST_SHORT=0 recast   # never auto-switch on short (≤3 char) words
RECAST_FREQ=0  recast   # disable the homograph frequency tie-break (on by default)
RECAST_SPELL=0 recast   # disable the English spelling autocorrect (on by default)

RECAST_SPELL_MIN=5    recast  # shortest word the autocorrect may fix (default 4)
RECAST_SPELL_RANK=10000 recast  # how common a suggestion must be (default 20000)
RECAST_SPELL_DIST=1   recast  # cap the autocorrect at single-edit typos (default 3)

RECAST_COMPLETE=0 recast        # disable auto-complete entirely (on by default)
RECAST_COMPLETE_MIN=4 recast    # shortest prefix Right Shift will complete (default 3)
RECAST_COMPLETE_RANK=10000 recast  # how common a completion must be (default 30000)
```

`RECAST_DEBUG=1` prints **every word you type** — under a service that means into your
system log, password fields included, since ReCast cannot tell one text field from
another. Use it while diagnosing something, not as a standing setting.

When a key sequence spells a real word in **both** layouts (a homograph
collision), ReCast normally keeps whatever layout you are in. The frequency
tie-break overrides that in the lopsided case: if the *other* layout's reading
is a genuinely common word (top ~2000 by usage) while your current reading is
rare or unlisted, it switches to the common one — so an accidental obscure
homograph still gets corrected. Set `RECAST_FREQ=0` to always keep the current
layout on homographs. Frequency ranks come from compact OpenSubtitles wordlists
embedded in the binary, consulted only for this tie-break (never as a switch
trigger on their own).

Short words are the most collision-prone (many 2–3 letter abbreviations are
valid in one dictionary while spelling a real word in the other layout), so
`RECAST_SHORT=0` is the knob to reach "never wrongly switch" at the cost of
not fixing short mistyped words.

## English autocorrect

Alongside the between-language comparison, ReCast also compares *within* English:
a word that is not a wrong-layout mistype but is a near-miss of a common English
word gets retyped as that word (`recieve` → `receive`, `helo` → `hello`,
`goverment` → `government`). It runs only when your live layout is English —
corrections are injected as keystrokes, so they only come out right there.

Because a wrong spelling fix silently rewrites your text, the bar is high. A
word is only corrected when all of this holds:

- it is not in the 370k-word English dictionary, and not a token the frequency
  corpus sees often (that is what protects names and handles — `sami`, `ori`,
  `github` are left alone),
- it is at least `RECAST_SPELL_MIN` characters (default 4), all letters — no
  digits and no internal punctuation, so identifiers, paths and `github.com`
  survive (the punctuation you *ended* the word with is set aside first, and
  typed back with the correction),
- it is not typed in ALL CAPS — acronyms are not misspellings,
- it is not listed in your own `ignore.txt` (see below),
- the correction is inside the edit budget for a word that length: one edit up to
  6 characters, two from 7, three from 10, capped by `RECAST_SPELL_DIST`
  (default 3 — set it to 1 for single-typo fixes only),
- the correction is a common word, within `RECAST_SPELL_RANK` (default 20000),
  halved past one edit and divided by five past two.

### How it picks

It is a noisy-channel corrector, the standard formulation from Kernighan, Church
and Gale: the user knew the word they wanted and their hands (or their memory of
the spelling) turned it into what we saw, so the answer maximises
`P(word) × P(typo | word)` — how likely anyone was to want that word at all,
times how likely that word was to come out looking like this. **Both** matter.
Ranking by edit distance and using frequency only to break ties, the way most
simple correctors do, quietly makes the second factor infinitely more important
than the first; here the two are added in log space, so a candidate can win
either by being the likelier slip or by being the far likelier word.

Distance is *weighted*, not counted: the mistakes people actually make cost less
than a full edit. Dropping or doubling one half of a double letter is the
cheapest, then a transposition, then hitting the key physically next to the right
one, then a vowel-for-vowel swap; anything else is a plain edit. So `helo` →
`hello` beats `helo` → `help` even though `help` is the more common word — but
make `help` a hundred times more common and it wins after all.

### What the keyboard explains

Where the keys sit is what separates a slip of the *hand* from a slip of the
*memory*, and it is used in three places:

- **The wrong key.** A letter next to the one meant is a fat finger, not a
  misspelling. Sliding one key along a row (`wprk` → `work`) is the likeliest
  slip there is; reaching onto the row above or below (`g` for `t`) is a
  deliberate movement that goes wrong less often, and costs a little more.
- **An extra key.** A letter that is a *neighbour of the letter beside it* is
  the hand catching two keys on the way past — `worjk` → `work`, `mnake` →
  `make`, `tjhat` → `that`. Those used to cost a full edit, which put them out
  of reach of anything but the longest words; now the hand is a cheaper
  explanation than the writer having believed in the letter. A stray letter
  from the other side of the keyboard (`worqk`) still pays full price, because
  nothing about the hand explains it.
- **Nothing about a missing key.** The discount is deliberately one-directional:
  adjacency explains keys that were *hit*, and there is no sense in which a
  letter is missing because of where its key sits. Only the double-letter
  discount applies on that side.

The geometry is QWERTY, including the half-key stagger between rows, and it is
about the *physical* keyboard: this is the same reasoning for a word typed under
the Hebrew layout, since the keys have not moved.

On top of single letters there is a table of **whole-string confusions** —
`ant`↔`ent`, `ance`↔`ence`, `ie`↔`ei`, `able`↔`ible`, `f`↔`ph`, `n`↔`kn`,
`r`↔`wr`, `c`↔`k`, `i`↔`y` and a few dozen more. This is Brill and Moore's error
model: a writer who types `apparant` made *one* decision about how the word is
spelled, not three unrelated slips, and pricing it as one is what brings it into
range. It is also what lets a phonetic respelling be found at all — `fisical` and
`physical` share barely half their letters, but only one rule apart.

Edits are priced by *where* in the word they land, too. Getting the opening of a
word wrong is rare and rewriting it is the most damaging thing this can do, so a
first-letter edit carries a heavy surcharge and a plain wrong first letter is out
of reach entirely — with two exceptions, both of which keep the letters you
typed: a transposed opening (`hte` → `the`) and a word-initial spelling rule
(`fone` → `phone`).

Together that is what makes badly mangled words reachable — `recieveing` →
`receiving`, `beutifull` → `beautiful`, `maintainance` → `maintenance`,
`restaraunt` → `restaurant` — where a strict one-edit speller had to give up.

The wider budget has a cost: an 8-letter piece of jargon two slips from a common
word is exactly the shape this is built to fix, so `hostname` → `hostage` and
`postgres` → `posters` are the sort of thing that can happen (as `impl` → `imply`
already could at one edit). Ways out: double-tap Ctrl on the spot (see
[Undo](#undo)); list the words you type in `<config dir>/recast/ignore.txt`, one
per line; set `RECAST_SPELL_DIST=1` for the old single-edit behaviour; or
`RECAST_SPELL=0` to turn the pipeline off. The double-tap is also how a word
comes *back off* either list, so nothing you retire is retired for good.

What it deliberately does not do is look at the surrounding words. Every
published evaluation of this kind of corrector puts the ceiling for single-word
correction well below what context-aware models reach, and correcting a word that
is *already* a real word (`from` for `form`) needs that context. ReCast never
touches a word the dictionary knows, so it stays on the safe side of that line.

## Auto-complete

Two ways to type less, both off the same word buffer and both English-only (the
result is injected as English text or keystrokes, so it only comes out right
under an English layout).

**Tap Right Shift** mid-word and ReCast finishes it (`recei` → `received`,
`tomo` → `tomorrow`). Right Shift is the trigger because it is the only key on
every keyboard that types nothing and means nothing to the app you are in: a tap
of it can't move focus, indent a line or open your editor's own completion popup
the way Tab would, so nothing has to be undone when ReCast declines. Holding it
to type a capital is unaffected — only a tap with nothing pressed in between
counts.

**Tap it again to cycle.** The first guess is not always the word you meant, so
the next tap swaps in the next candidate, and tapping past the end of the short
list hands back exactly what you typed, capitalization included. That is what
makes guessing affordable: a wrong completion costs one more tap, not a word's
worth of deletion — and it is why the completer is allowed to guess at all.

Candidates are ranked by **what the tap saves you**, not by raw frequency: the
value of an offer is the letters it fills in weighted by how likely that word is
(`P ∝ 1/rank`), so completing a five-letter prefix by one letter loses to a word
that finishes it outright. Frequency still dominates a lopsided pair — `tomo`
completes to `tomorrow`, not to a longer, rarer relative — it only decides
between candidates that were already close.

**Abbreviations** you define expand when you finish the word — and the first tap
of Right Shift offers one too, since a rule you wrote by hand beats anything
guessed from a corpus. Put them in `<config dir>/recast/abbrev.txt`, one per
line:

```
# comments start with #
btw = by the way
addr = 1 Main Street, Tel Aviv
ty	thank you
```

Nothing is built in: the file starts absent and the feature stays inert until you
put something in it. The expansion takes priority over every other pipeline —
you wrote the rule, so nothing overrules it — and it follows the capitalization
you typed (`Btw` → `By the way`, `BTW` → `BY THE WAY`).

`RECAST_COMPLETE=0` turns both off.

## Undo

**Tap Ctrl twice, quickly**, right after a correction and ReCast puts back what
you actually typed — and switches the layout back too, if that is what the
correction changed. Ctrl is the second gesture key for the same reason Right
Shift is the first: on its own it types nothing and means nothing to the app you
are in, so the gesture can't leak a keystroke into your document. Holding Ctrl
for a shortcut is unaffected; only two bare press-and-release pairs inside half a
second count.

Undo erases backwards from the cursor, so it is only offered for the correction
the cursor is **still sitting on**. Type anything else — even a space — and that
correction is final and the next double-tap does nothing. This is the same
bargain macOS and iOS make, and it is what stops a mistimed double-tap from
eating text further back.

Putting the letters back is only half of it. A correction is a *function* of what
you typed, so retyping the same word reaches the same conclusion — an undo that
only rewrote the screen would put you on a treadmill. So undoing a word also
**retires** it: nothing corrects that word again until ReCast restarts. That is
the fast path for the `hostname` → `hostage` case, and `ignore.txt` is still how
you make it permanent.

A completion can be taken back the same way, though tapping Right Shift around
the cycle gets you there without the gesture.

### …and the same gesture puts a word back in play

The double-tap is a **toggle**, so it also works in the other direction. Type a
word you have retired — one you undid earlier, or one sitting in `ignore.txt` —
and nothing happens to it, as you asked. Double-tap Ctrl right there and ReCast
takes it off the list and corrects it after all:

```
hostname ⇥                 you undid this earlier, so nothing happens
Ctrl Ctrl                  → hostage      (and hostname is off the list again)
```

Coming off the list means coming off it properly: the entry is removed from
`ignore.txt` on disk as well as from memory, so it does not come back at the next
restart. Only lines that *are* that word are removed — your comments, spacing and
every other entry are copied through byte for byte, and the file is replaced by
rename so an interrupted write can't leave you with half a list.

Which direction the gesture takes is decided by what happened to the word, never
by how you tap: a word that was just corrected gets the correction taken back, a
word that was just passed over because it is listed gets un-listed. A word that
is simply spelled correctly arms nothing, and the gesture does nothing at all.

## One fix per word

The pipelines are mutually exclusive: each finished word gets **one** correction
or none. An abbreviation expansion goes first (you defined it by hand), then the
layout switch, because it is exact — the keystrokes literally spell a real word
in the other language — and only if that declines does the speller get a look. A word that the speller fixes is typed as
its corrected self and never re-examined, so it is not then flipped to the other
layout even if its keys happen to spell a Hebrew word too.

## The tray / menubar menu

On macOS and Windows everything below is in the menu behind the icon:

| Item                    | What it does                                                                                                     |
| ----------------------- | ---------------------------------------------------------------------------------------------------------------- |
| `Fixed: N · M taken back` | The two counters. See [Is it set right for you?](#is-it-set-right-for-you).                                      |
| `Disable` / `Enable`    | The switch, remembered across restarts.                                                                            |
| `Pause for 30 minutes`  | Stops correcting, then starts again by itself. Counts down in the menu, and the same row ends it early.            |
| `Recent`                | The last five corrections, `typed → result (pipeline)`. **Click one and that word goes into `ignore.txt`.**        |
| `Reload lists`          | Re-reads `abbrev.txt` / `ignore.txt` now (they are picked up within a couple of seconds anyway).                    |
| `Start at login`        | Registers ReCast with launchd / the per-user `Run` key, without going back to `make service` or `deploy.ps1`.       |

The recent list is there because silent text replacement is the whole premise
of this program: "what did it just change?" is the question a counter cannot
answer, and clicking the answer is how you say *not this word, ever*.

## Is it set right for you?

The menu and the TUI show **two** numbers — corrections that stuck, and
corrections you took back with the undo gesture. The pair is the reading. A
correction you were happy with is invisible by design, so the count on its own
cannot tell working well from working badly; the ratio can. Once five have been
taken back and they are a third or more of the total, both UIs say so and
suggest `RECAST_SPELL_DIST=1`, which is the knob that turns the badly-mangled
cases back off.

## Privacy

ReCast reads every key you press. It cannot work otherwise — deciding that
`akuo` was meant to be `שלום` means seeing `akuo` first — so the only questions
worth answering are what it does with that, and what it doesn't. This section is
the answer, and everything in it is checkable against the source.

**Nothing leaves the machine.** There is no network code in this program and no
networking crate in its dependency list: no telemetry, no analytics, no crash
reporting, no update check, no remote dictionary. The word lists are compiled
into the executable by `build.rs`, so even the lookups are local — there is
nothing to fetch and nothing to ask.

**Nothing you type is written to disk, except the words you ask it to keep.**
The word being typed lives in memory and is dropped at every word boundary, on
Tab / Escape / an arrow key, and on a mouse click. The recent-corrections list
the tray and TUI show is the last 20, held in RAM and gone when the process
exits — it is a glance, not a log, and it is never persisted. The one exception
is `ignore.txt`, which gains a word only when you put it there yourself, by
double-tapping Ctrl on a correction or clicking one in `Recent`. That is your
file, in plain text, and you can read or edit it (see
[Your files](#your-files)).

**Your clipboard is never touched.** A corrected word is injected as synthetic
input carrying the characters directly — `CGEventKeyboardSetUnicodeString` on
macOS, a `KEYEVENTF_UNICODE` `SendInput` batch on Windows, `uinput` keycodes on
Linux. Nothing is copied, so what you had on the clipboard is still there
afterwards. (This is why corrections land at once, the way a paste does, without
being one.)

**The one notification quotes nothing.** The first-correction hint deliberately
does not name the word it is about: a notification is a copy of text that
outlives the moment it belonged to, sitting in a notification centre after the
window it came from is closed.

**Logging is off, and turning it on is the one thing to be careful with.**
Normal operation writes no record of what you type. `RECAST_DEBUG=1` prints
every word it checks to stdout — useful at a terminal, and a transcript of your
typing anywhere else. Where that goes depends on how ReCast was started: the
Linux daemon sends stdout to `/dev/null`, the systemd user unit sends it to the
journal, and the macOS LaunchAgent writes `/tmp/recast.out.log` and
`/tmp/recast.err.log`, which are readable by other users on the machine. Don't
leave debug on under a service on any platform.

**What it needs from the OS**, for the same reason, is the permission to see all
of this: membership of the `input` group on Linux (plus a `uinput` device to
type corrections back), Input Monitoring and Accessibility on macOS, and a
low-level keyboard hook on Windows. Those are the real trust you are extending;
the rest of this section is about what is done with it.

### Passwords

On macOS ReCast stops watching the keyboard entirely while a password field has
focus, using the same signal the OS gives every application
(`IsSecureEventInputEnabled`): the word buffer is dropped, nothing is checked
and nothing is corrected until focus moves on. The event tap is listen-only and
macOS withholds those characters from it in any case — but not being in the
loop is a stronger promise than not having been given the data, and it also
stops a correction from firing *inside* the field.

There is no equivalent signal on Linux or Windows, so the same guarantee cannot
be made there.

## Your files

Both are optional and absent by default — nothing is created for you. They are
re-read within about two seconds of being edited, so adding an abbreviation and
typing it are the same action rather than a restart apart. They live under the
OS config directory:

| OS      | Directory                               |
| ------- | --------------------------------------- |
| Linux   | `~/.config/recast/`                     |
| macOS   | `~/Library/Application Support/recast/` |
| Windows | `%APPDATA%\recast\`                     |

| File         | What it holds                                                                                                            |
| ------------ | ------------------------------------------------------------------------------------------------------------------------ |
| `abbrev.txt` | `abbr = expansion` per line, `#` comments. Expands when you finish the word, and is offered by the first Right Shift tap. |
| `ignore.txt` | One word per line, `#` comments. Words the autocorrect must never touch.                                                  |
| `state.txt`  | Written by ReCast: the Enable/Disable switch, so it survives a restart.                                                   |
| `welcomed`   | Written by ReCast: a marker saying the one-time hint below has been shown. Delete it to see it again.                     |

`ignore.txt` is the only file of *yours* that ReCast writes. Double-tapping Ctrl on a
word that was skipped *because* it is listed takes that line back out — comments,
spacing and every other entry copied through untouched, and the file replaced by
rename — and clicking a correction in the tray's `Recent` list appends one.

The first time a correction ever lands, ReCast says so once with a desktop
notification, because the gestures below are otherwise invisible: there is no
window to discover them in, and the README is not where you are when your word
is rewritten for the first time. Once, ever — the `welcomed` marker is what
makes sure of it.

## Development

```bash
cargo build --release     # or `make` — release is the meaningful profile (LTO + strip)
cargo test                # 109 tests, all pure: dictionaries, speller, completer, keymaps, counters
RECAST_DEBUG=1 cargo run  # log every word check and switch decision
```

The correction pipeline is platform-agnostic and lives in `src/dictionary.rs` (the
decision core), `src/spell.rs` (the English speller) and `src/complete.rs` (completion,
abbreviations, the ignore and undo lists, and the watcher that reloads them).
`src/types.rs` holds the shared counters and the corrections history the UIs read,
`src/prefs.rs` the state kept between runs, and `src/notify.rs` the one-time hint.
Only capture and injection are per-OS, in
`src/platform/{linux,macos,windows}.rs`, and each of those owns its entire startup path.
The word lists are preprocessed by `build.rs` into sorted blobs the binary embeds, so
there is nothing to install alongside the executable.

Both cross-targets compile from Linux, and are worth checking before a release since
neither is exercised by `cargo test`:

```bash
cargo check --target x86_64-pc-windows-gnu
cargo check --target x86_64-apple-darwin
```

CI (`.github/workflows/ci.yml`) runs `cargo test`, `cargo clippy -- -D warnings` and a
release build on Linux, macOS and Windows runners for every push. The three platform
modules are near-identical copies that nothing keeps in step, so a change made in one and
forgotten in the other two is the most likely regression here — building each on its own
OS is what catches it.

## License

Apache-2.0 — see [LICENSE](LICENSE).
