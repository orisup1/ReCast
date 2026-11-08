# Linux desktop integration

The binary embeds `scripts/linux_desktop.py`; no Python source installation is
needed. The helper runs under your desktop user over private parent-owned pipes.
It reads focus/application metadata and verifies only the bounded suffix captured
from typing before a native correction or undo. Selected text is read only for an
explicit rescue gesture. It never reads the entire document or clipboard.
It does not open a network listener or write text logs.

Install your distribution's Python 3, PyGObject, AT-SPI 2 introspection data, and
accessibility service. Debian/Ubuntu package names include `python3-gi` and
`gir1.2-atspi-2.0`. Enable desktop accessibility and the application's accessibility
support. Qt applications may also need their distribution's Qt accessibility bridge.

ReCast automatically tries the helper when these dependencies are available.
Set `desktop_helper = false` or `RECAST_DESKTOP_HELPER=0` to disable it. Existing
explicit settings remain authoritative. The helper starts asynchronously and seeds
already-focused controls from active windows with a bounded, metadata-only scan.
Applications must expose accessibility: `NO_AT_BRIDGE=1` disables the GTK bridge;
restart affected applications with accessibility enabled to use native suffix edits. Missing dependencies, unavailable
focus, inaccessible applications, and timeouts cause requests to be declined.
The helper retries startup after failures. `recast --status` identifies the
backend and whether helper support was enabled.

## Replacement delivery

ReCast checks native edit capability before waiting for physical keys. Supported
AT-SPI fields can therefore correct while Space is still held. The control and
captured text are verified again before editing; a failed or uncertain native
request never falls through to keyboard deletion.

In a single-seat Wayland session advertising `zwp_virtual_keyboard_manager_v1`,
ReCast can deliver literal English/Hebrew text directly through the compositor.
This route also works without application accessibility. It assigns text only to
printable key positions that are not physically held, preserves lock indicators,
and confirms its keymap before sending events. It uses neither the clipboard nor
an external typing command. The session must provide `XDG_SESSION_TYPE=wayland`
and a working `WAYLAND_DISPLAY` connection to the daemon.

This protocol is optional and is generally available on Hyprland/Sway rather than
GNOME/KDE. Unsupported sessions, multiple seats, or unavailable connections retain
`uinput` replay with physical-release waits and bounded writes. Disable compositor
input with `wayland_input = false` or `RECAST_WAYLAND_INPUT=0`. Disabling only
`desktop_helper` still permits compositor input. Focus, layout confirmation, and
stale-input cancellation apply to every route. Keyboard delivery does not provide
verified extended undo or an atomic application text edit.

KDE keeps its existing D-Bus layout backend. GNOME Wayland also needs this Shell
extension (GNOME on X11 keeps using live XKB):

```sh
mkdir -p /tmp/recast-extension
cd integrations/gnome/recast@orisupino
gnome-extensions pack --force --out-dir=/tmp/recast-extension
gnome-extensions install --force /tmp/recast-extension/recast@orisupino.shell-extension.zip
```

Log out and back in if GNOME does not yet list the extension, then run:

```sh
gnome-extensions enable recast@orisupino
```

The extension returns a single live snapshot of input-source names and active
index. Switching is confirmed by reading the live state again. IBus sources are
not treated as XKB layouts. Locked/greeter sessions refuse queries and switches.
The declared Shell versions share this API; they still require native acceptance
checks before release. This change has not been installed or enabled automatically.
To remove it, disable/uninstall `recast@orisupino` with `gnome-extensions`, and
remove the helper setting.

## Focus and selection limits

GNOME/KDE app rules match the focused application's executable basename. Existing
Hyprland/Sway/X11 identifiers remain intact. With the helper, pending changes also
track accessibility focus revisions so moving between controls cancels them.
GNOME/KDE Wayland requires focus information even without per-app rules; without
it, corrections pause. Supported controls must expose a focused editable AT-SPI
text object with an active ancestor window. Flatpak restrictions or incomplete
accessibility implementations can prevent access.

Selection rescue requires one nonempty selection, at most 64 KiB, in a writable
non-password control. Offsets use Unicode characters, not UTF-8 bytes. Replacement
inserts first, verifies the affected text, then removes the original. If insertion
fails, the original is never deleted. If deletion fails, the helper attempts to
remove only the verified insertion. Focus changes, concurrent editing, process
failure, or timeout can leave both texts; AT-SPI has no atomic replace-selection
operation. Do not repeat blindly after an interrupted edit. Native undo grouping
and rich formatting depend on the application. ReCast's second gesture restores
exact text only after a successfully verified edit.

Windows rescue similarly depends on UI Automation and native Edit/RichEdit
messages. Custom editors, browsers without native edit controls, elevated windows,
and unsupported accessibility providers are skipped. Windows native checks are
available as an ignored test; see `tests/native-ui.md`.

## Verification

```sh
python3 -m unittest discover -s scripts -p test_linux_desktop.py
node scripts/test_gnome_extension.cjs
node scripts/test_native_editing.cjs
```

These check failure handling and protocol fixtures, not a running compositor.
Run the native matrix before advertising a desktop/application combination as tested.
API contracts: [AT-SPI insertion](https://gnome.pages.gitlab.gnome.org/at-spi2-core/libatspi/method.EditableText.insert_text.html)
and [Windows selected replacement](https://learn.microsoft.com/en-us/windows/win32/controls/em-replacesel).
