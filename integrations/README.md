# Optional Linux desktop integration

The binary embeds `scripts/linux_desktop.py`; no Python source installation is
needed. The helper runs under your desktop user over private parent-owned pipes.
It reads focus/application metadata during typing. It reads selected text only
for an explicit rescue gesture and never reads the entire document or clipboard.
It does not open a network listener or write text logs.

Install your distribution's Python 3, PyGObject, AT-SPI 2 introspection data, and
accessibility service. Debian/Ubuntu package names include `python3-gi` and
`gir1.2-atspi-2.0`. Enable desktop accessibility and the application's accessibility
support. Qt applications may also need their distribution's Qt accessibility bridge.

Set `desktop_helper = true` in ReCast's configuration file, or
`RECAST_DESKTOP_HELPER=1` in its environment, then
restart ReCast. The helper starts asynchronously; focus another window/control
after startup so it receives a focus event. Missing dependencies, unavailable
focus, inaccessible applications, and timeouts cause requests to be declined.
The helper retries startup after failures. `recast --status` identifies the
backend and whether helper support was enabled.

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
