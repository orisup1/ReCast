#!/usr/bin/env bash
#
# Build ReCast and install it into /Applications as a .app bundle.
#
# The bundle in exec/ is scratch space, not a committed artifact: `make bundle`
# assembles it from the release binary, an Info.plist generated from Cargo.toml
# and assets/recast.icns. This script builds it and puts the result where macOS
# expects to find it.
#
# The tccutil reset at the end is the part that is easy to leave out and then
# spend an afternoon on. macOS keys Input Monitoring and Accessibility grants to
# the bundle's code signature, not to its path: replace the executable inside a
# bundle that already has them and TCC keeps saying yes while quietly handing
# the app nothing. No keystrokes arrive, nothing is logged, and the permission
# checkbox is still ticked. Resetting forces the prompt to come back, which is
# the only way to re-grant against the new signature.
#
# Run it from anywhere; paths are resolved from the script's own location.

set -euo pipefail

BUNDLE_ID="com.recast.app"
APP_NAME="ReCast.app"
INSTALL_DIR="/Applications"

# ─── 1. macOS only ───────────────────────────────────────────────────────────
# Everything below this line is Darwin-specific: the .app layout, ditto,
# tccutil. On anything else there is nothing to do but say so.
if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "This script only does anything on macOS (found $(uname -s))." >&2
    echo "  Linux:   make install" >&2
    echo "  Windows: deploy.ps1" >&2
    exit 1
fi

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SRC_BUNDLE="$REPO_ROOT/exec/$APP_NAME"
BINARY="$REPO_ROOT/target/release/recast"

# ─── 2. Build the bundle ─────────────────────────────────────────────────────
# `make bundle` builds the release binary and assembles the whole .app around
# it — executable, Info.plist, icon. Nothing here assumes exec/ already holds
# anything, because since the binaries were untracked it usually does not.
echo "==> Building (release) and assembling the bundle"
make -C "$REPO_ROOT" bundle

if [[ ! -x "$BINARY" ]]; then
    echo "Build reported success but $BINARY is not there." >&2
    exit 1
fi

# ─── 3. Install the bundle ───────────────────────────────────────────────────
# /Applications is group-writable by admins on most machines but not all, so
# fall back to sudo rather than failing halfway through with a copied binary and
# no installed app.
SUDO=""
if [[ ! -w "$INSTALL_DIR" ]]; then
    echo "==> $INSTALL_DIR is not writable; using sudo"
    SUDO="sudo"
fi

# A running copy holds its own executable open, and TCC keeps bookkeeping for a
# live process. Stop it before the files move under it. `quit` first because
# ReCast is an LSUIElement with no window to close politely otherwise; the
# pkill is for the case where it is wedged or was never registered.
if pgrep -f "$INSTALL_DIR/$APP_NAME" >/dev/null 2>&1; then
    echo "==> Stopping the running ReCast"
    osascript -e 'quit app "ReCast"' >/dev/null 2>&1 || true
    sleep 1
    pkill -f "$INSTALL_DIR/$APP_NAME" >/dev/null 2>&1 || true
fi

# Remove before copying rather than copying over the top: ditto merges, so a
# file dropped from the bundle between versions would live on in the installed
# copy forever.
echo "==> Installing to $INSTALL_DIR/$APP_NAME"
$SUDO rm -rf "${INSTALL_DIR:?}/$APP_NAME"
$SUDO ditto "$SRC_BUNDLE" "$INSTALL_DIR/$APP_NAME"

# ─── 4. Reset the privacy grants ─────────────────────────────────────────────
# Non-fatal: tccutil exits non-zero when the bundle id has no records yet, which
# is exactly the state a first install is in and is not a problem.
echo "==> Resetting privacy permissions for $BUNDLE_ID"
if ! tccutil reset All "$BUNDLE_ID"; then
    echo "    (nothing to reset — first install, or the id was never registered)"
fi

echo
echo "Installed: $INSTALL_DIR/$APP_NAME"
echo "Launch it once and re-grant Input Monitoring and Accessibility when asked;"
echo "until you do, ReCast sees no keystrokes."
