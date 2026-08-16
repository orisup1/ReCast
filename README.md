<p align="center">
  <img src="assets/icons/icon-256.png" width="128" height="128" alt="ReCast logo">
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
  current layout is left untouched — including prefixed Hebrew forms, one prefix or two
  where Hebrew genuinely stacks them (`והבית`, `כשבית`, `שהשלום`), and words whose
  other-layout reading happens to also be a dictionary word. It only switches when the
  *other* layout yields a confident word and the current one yields nothing real.
  This is what stops valid (and nested/prefixed) words from being mangled.
- It **reads the words around the one it is deciding**. Language comes in runs — nobody
  writes one Hebrew word between two English ones — so a key sequence that is a real word
  in *both* layouts, which no amount of dictionary work can resolve on its own, is settled
  by what you were writing a moment ago. The run only ever tips a genuine tie; it can never
  make ReCast switch on a reading that is not a word at all.
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
| Linux   | `evdev`         | `uinput` keycodes, one batch     | Hyprland / sway / KDE / GNOME / any X11 (see below) |
| macOS   | `CGEventTap`    | `CGEvent` Unicode string         | Carbon `TISSelectInputSource`                       |
| Windows | `rdev`          | one `SendInput` Unicode batch    | `LoadKeyboardLayoutW` + `WM_INPUTLANGCHANGEREQUEST` |

Linux additionally requires the user to be in the `input` group (for `evdev` read access)
and creates a `uinput` virtual device named `recast-injector` to replay corrected words.

## Requirements

- **Rust** toolchain (`rustup`, `cargo`).
- Both **English and Hebrew layouts installed** in your OS keyboard settings. Their order
  does not matter — ReCast looks up which position each one is in.
- **Linux only:** membership in the `input` group (for `evdev` read access). ReCast creates
  a `uinput` virtual device named `recast-injector` to replay corrected words.

### Switching the layout on Linux

There is no single way to do this on Linux: X11 lets anyone set the XKB group,
and Wayland deliberately does not — the compositor owns the keymap and each one
exposes its own way to ask. So ReCast probes the session once at startup and
picks one of five backends:

| Session                             | How                                    |
| ----------------------------------- | -------------------------------------- |
| Hyprland                            | its control socket                     |
| sway (or any i3-IPC compositor)     | the `$SWAYSOCK` socket                 |
| KDE Plasma (Wayland or X11)         | `org.kde.KeyboardLayouts` over D-Bus   |
| GNOME (Wayland or X11)              | `org.gnome.desktop.input-sources`      |
| Any X11 session — i3, xfce, MATE, … | the XKB group, via `libX11`            |

The first two talk to the compositor over its socket rather than shelling out
to `hyprctl`/`swaymsg`: that was measured at 6.3 ms against 0.11 ms for the same
question, and a correction that changes layout asks twice. X11 uses `libX11`,
loaded with `dlopen` so a pure Wayland machine that has no X libraries installed
still runs. KDE and GNOME go through `gdbus`/`busctl`/`qdbus` and `gsettings`
respectively, which is a process per query — the 300 ms cache is what makes that
survivable.

`recast --status` prints which backend was chosen and the layouts it found:

```
  layout switch:  Hyprland (control socket) — layouts: us, il
```

If the probe guesses wrong, `layout_backend` in `config.toml` (or
`RECAST_LAYOUT_BACKEND`) forces one: `hyprland`, `sway`, `kde`, `gnome`, `x11`
or `none`.

Four of the five are also *watched* rather than only asked: Hyprland's event
socket, sway's i3-IPC `input` subscription, `gsettings monitor` on GNOME and
`gdbus monitor` on KDE all say when the layout changes, so the answer is already
in hand by the time a word ends. That matters most for GNOME and KDE, where
asking means a subprocess — it now happens on a thread nobody is waiting on, and
only when something has actually changed. X11 keeps polling, because it answers
over a connection ReCast already holds open and the query it would save costs a
round trip rather than a process. ReCast also drops its cached answer whenever
you release something shaped like a layout hotkey (two modifiers, or a modifier
with space, or Caps Lock), so a hand-switched layout is never one word behind.

ReCast used to support Hyprland alone, and hardcoded *layout 0 is English,
layout 1 is Hebrew* — so anyone with a third layout got switched into it, and
everyone else got no layout correction at all while the speller kept working,
which is a confusing way for a feature to be missing.

### Prebuilt binaries

The [Releases page](https://github.com/orisup1/recast/releases) has ready-to-run builds if
you would rather not install a toolchain. They are self-contained — the dictionaries are
inside the executable — so there is nothing to unpack alongside them:

| Asset            | Target                                                     |
| ---------------- | ---------------------------------------------------------- |
| `recastLinux`    | Linux x86-64                                               |
| `ReCast.exe`     | Windows x86-64 (no runtime DLLs needed; UCRT, Windows 10+) |
| `recastMac`      | macOS universal (arm64 + x86-64)                           |
| `ReCast.app.zip` | macOS bundle, same universal binary inside                 |

They are built by the **Binaries** workflow (`.github/workflows/binaries.yml`, run from
the Actions tab), which compiles each one on its own runner and attaches the results to a
release. They were committed into `exec/` until that had cost the repository ~45 MB per
refresh, permanently — a release asset can be replaced, a git object cannot. `exec/` is
still where a local `make bundle` stages the macOS `.app`, but nothing in it is tracked.
Building yourself is still the recommended path; these exist so you can try it in one step.

#### macOS: "ReCast is damaged and can't be opened"

If you see this, nothing is damaged. macOS attaches a `com.apple.quarantine` flag to
anything that arrives from a browser, an AirDrop, a zip or a USB stick, and Gatekeeper
then judges the download by its code signature. A build with **no** signature comes back
as *damaged* rather than as *unsigned* — the same misleading wording either way. The
machine that built it never sees this, because files it produced itself are not
quarantined; that is why the message appears only on the other side of a transfer,
including a transfer back to the machine the build came from.

The downloads are ad-hoc signed, which is enough to clear that. Because the signature
carries no Apple-issued certificate, macOS still asks once before the first launch —
right-click the app → **Open**, or *System Settings → Privacy & Security → Open Anyway*.
For the bare `recastMac` binary, or if you would rather skip the prompt:

```bash
xattr -dr com.apple.quarantine ReCast.app     # or: recastMac
```

Building it yourself sidesteps all of this, since a local build is never quarantined.
`make bundle` ad-hoc signs the bundle it assembles; `make notarize
CODESIGN_ID="Developer ID Application: …"` produces one that opens with no prompt at all,
which needs a paid Apple Developer account.

To move a build you made to another Mac, use **`make dist`** rather than compressing
`exec/ReCast.app` by hand. A `.app` is a directory, and its signature depends on the exact
file layout and modes inside it; most zip tools and most non-APFS filesystems do not
preserve them, and a bundle whose signature no longer matches its contents is reported as
damaged in the same words as one with no signature at all. `make dist` verifies the
signature and packages it with `ditto`, which keeps that structure intact. That is also the only way ReCast's Input
Monitoring and Accessibility grants survive an upgrade: macOS keys them to the signature,
and an ad-hoc signature is a fresh identity on every build, so each new version has to be
granted permission again.

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

### Linux

One-shot setup: adds your user to the `input` group, builds in release mode, installs the
binary to `~/.local/bin/recast`, and registers a `systemd --user` unit that starts ReCast
at login.

```bash
sudo usermod -aG input $USER && exec newgrp input <<< 'make service'
```

`newgrp` applies the new group to the current shell so you don't have to log out; omit it
and re-login instead if you prefer. Ensure `~/.local/bin` is on your `PATH`.

Manage the service:

```bash
systemctl --user status  recast    # health check
systemctl --user restart recast    # apply a rebuild
journalctl  --user -u    recast -f  # follow logs
make service-uninstall             # stop and remove the unit
```

Common Make targets:

| Target             | Action                                    |
| ------------------ | ----------------------------------------- |
| `make`             | `cargo build --release`                   |
| `make install`     | build + copy binary to `~/.local/bin`     |
| `make deploy`      | clean + build + install                   |
| `make uninstall`   | remove the installed binary               |
| `make run ARGS=-g` | `cargo run` with a flag (here, the TUI)   |
| `make help`        | full target list                          |

Override the install root with `PREFIX=`, e.g. `make install PREFIX=/opt/recast`.

### macOS

```bash
make service
```

This writes a launchd LaunchAgent at `~/Library/LaunchAgents/org.recast.plist` and starts
it. The first time it runs, grant the binary **Input Monitoring** and **Accessibility**
permissions in *System Settings → Privacy & Security*.

To reset ReCast's permissions:

```bash
tccutil reset All com.recast.app
```

Once installed, `recast` is on your `PATH` and supports the same flags as every other
platform (see [Usage](#usage)) — e.g. `recast -h`, `recast -g`, `recast -s`. Note that the
LaunchAgent runs with `KeepAlive`, so `recast -s` stops a manually-launched instance but the
*service* will be relaunched by launchd; to stop the service, run `make service-uninstall`.

### Windows

In PowerShell:

```powershell
.\deploy.ps1 -Target service
```

This builds, installs to `%USERPROFILE%\.local\bin`, and registers a Scheduled Task that
runs ReCast at logon. Run `.\deploy.ps1 -Target help` to list every target.

## Usage

You don't need the service — you can run the binary (or `cargo run --release`) directly
from any shell. On Linux it daemonizes into the background by default (under systemd this
is detected automatically); on macOS and Windows it lives in the tray/menubar.

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
  config.toml:    /home/you/.config/recast/config.toml
  abbrev.txt:     3 abbreviation(s)
  ignore.txt:     7 word(s)
  memory (this):  8.9 MB

  settings (config.toml and RECAST_* applied):
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

The **TUI** (`-g`, Linux/Windows) shows the enabled state, the fixed-word counter, and a
live log; press `e` or `Space` to toggle correction on and off, and `q` to quit. The
**control window** (`-w`) offers the same toggle and counter in a small GUI window. On
macOS, use the menubar menu instead.

### config.toml

Every setting can be written down instead of exported. `recast --write-config`
creates `<config dir>/recast/config.toml` with all of them in it, commented out
and showing their defaults; uncomment what you want to change:

```toml
spell_dist = 1        # cap the autocorrect at single-edit typos
complete_min = 4      # shortest prefix Right Shift will complete
inject_batch_gap = 0  # send a correction as one write (Linux)
```

The keys are the environment names with `RECAST_` dropped and the rest
lowercased — `RECAST_SPELL_DIST` is `spell_dist` — and the environment still
wins where both are set. It is a flat `key = value` file with `#` comments: a
subset of TOML, so an editor's TOML mode works, but there are no tables and no
arrays because every setting here is a bool or a number.

**This is the one that works under a service.** ReCast is started by systemd,
launchd or a Scheduled Task on the three platforms, none of which pass your
shell environment through — so a `RECAST_*` variable set in `.bashrc` reaches
a hand-launched ReCast and nothing else. The file is read whichever way it
started.

Unlike `abbrev.txt` and `ignore.txt`, it is read once, at startup: changing it
takes a restart. Anything in it that ReCast could not use — a misspelled key, a
line with no `=`, a number that is not a number — is named at startup and again
under `--status`, rather than being ignored in silence.

### Environment variables

| Variable            | Effect                                                          |
| ------------------- | -------------------------------------------------------------- |
| `RECAST_DEBUG=1`    | Print every word check and switch decision                     |
| `RECAST_SPLIT=1`    | Enable the opt-in missing-space splitting fallback             |
| `RECAST_SHORT=0`    | Never auto-switch on short (≤3 char) words                      |

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
not fixing short mistyped words. It is not a bare length cutoff: length was only
ever standing in for "probably an accidental dictionary hit", and the frequency
lists answer that directly. So even with the gate on, a short reading that is one
of the 500 most common words in its language — `the`, `של`, `זה` — still
switches, because those are the short words people actually mistype the layout
of rather than the collisions the gate is for.

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
- it is not listed in your own `ignore.txt`, and not a word you have taken the
  same correction back from twice (see [Undo](#undo)),
- the correction is inside the edit budget for a word that length: one edit up to
  6 characters, two from 7, three from 10, capped by `RECAST_SPELL_DIST`
  (default 3 — set it to 1 for single-typo fixes only),
- the correction is a common word, within `RECAST_SPELL_RANK` (default 20000),
  halved past one edit and divided by five past two — except for a word of 9
  characters or more whose two edits were both the *cheap*, well-explained kind
  (a doubled letter, a neighbouring key, a spelling confusion people share),
  which keeps the full budget.

That last exception is what the corpus asked for. `occurance` → `occurrence` is a
doubled letter and a known suffix confusion, and `occurrence` sits at rank 19205
— just outside the halved budget, so the obvious fix used to be declined.
`restaraunt` reaches `restraint` (rank 14085) in the same band and the same
number of edits, but by moving letters that were not wrong, and it is not the
word anyone meant. Length alone cannot tell those apart; what the edits *were*
can, which is why the exception is conditioned on both.

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
first-letter edit carries a heavy surcharge and a first letter from the other
side of the keyboard is out of reach entirely. Three things can still reach an
opening, and all three are events rather than guesses: a transposed opening
(`hte` → `the`), a word-initial spelling rule (`fone` → `phone`), and the key
physically *next to* the one meant (`vecause` → `because`, `fovernment` →
`government`). The last of those costs the neighbouring-key price plus the
first-letter surcharge, which needs a two-edit budget — so it only ever happens
to a word long enough to afford one, and never to the short tokens that are
overwhelmingly names.

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

What the *speller* deliberately does not do is look at the surrounding words.
(The layout pipeline does — see the language run above — but that is a different
question: which of two real words you meant, not which real word to invent.)
Every published evaluation of this kind of corrector puts the ceiling for
single-word correction well below what context-aware models reach, and correcting
a word that is *already* a real word (`from` for `form`) needs that context.
ReCast never touches a word the dictionary knows, so it stays on the safe side of
that line.

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
the fast path for the `hostname` → `hostage` case.

**Undo the same word twice and it stays retired**, across restarts, without your
having to go and write anything down. Once is a reflex and is worth a session;
twice, on two separate occasions, is a decision — the correction is not a one-off
annoyance but something that will keep happening to a word you keep typing.
ReCast counts undos in `<config dir>/recast/learned.txt` and stops at two, which
reaches the same place `ignore.txt` does by hand. `recast --status` reports how
many words are retired that way, and the un-retire gesture below clears the count
along with everything else.

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
exits — it is a glance, not a log, and it is never persisted. The exceptions are
two files, both of which gain a word only through a deliberate gesture of yours,
and both of which are plain text you can read, edit or delete (see
[Your files](#your-files)):

- `ignore.txt`, when you double-tap Ctrl on a correction or click one in
  `Recent`.
- `learned.txt`, when you undo a correction — the word you typed, and how many
  times you have taken that correction back. Nothing else about the word is
  recorded: not what it was corrected *to*, not when, not what surrounded it.

Both grow only by your undoing or listing something, so what they contain is the
set of words you have explicitly told ReCast to stop touching — never a
transcript. If you would rather ReCast learned nothing, delete `learned.txt`; it
is rewritten only at the next undo.

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

`abbrev.txt` and `ignore.txt` are yours: both optional, both absent by default —
nothing is created for you — and both re-read within about two seconds of being
edited, so adding an abbreviation and typing it are the same action rather than a
restart apart. The rest of the table is ReCast's own bookkeeping, written when
there is something to write and read at startup. Everything lives under the OS
config directory:

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
| `learned.txt` | Written by ReCast: `word<TAB>count` of the corrections you have undone. Two and the word is retired for good. Editable — a bare word on a line counts as one. |
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
release build on Linux, macOS and Windows runners. Nothing below `src/platform/` has
tests, so "it builds on its own OS" is the whole guarantee there.

The three platform modules used to be near-identical copies of one another — the word
buffer, both gestures, undo, the completion cycle and the replacement planning were
written out three times, and a change made in one and forgotten in the other two was the
most likely regression in the codebase. That state machine now lives once in
`src/platform/engine.rs`, generic over a `Platform` trait, and each OS module supplies
only what is genuinely its own: how keystrokes arrive, how a replacement is put on screen,
and how the process starts up. `src/platform/textkeys.rs` holds the further half that
macOS and Windows share, since both capture `rdev::Key` and both insert corrections as
text. Per-platform code went from ~3400 lines to ~1450.

## License

Apache-2.0 — see [LICENSE](LICENSE).
