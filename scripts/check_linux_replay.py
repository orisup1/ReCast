#!/usr/bin/env python3
"""Check native replay in a disposable GTK field on Linux/Hyprland.

Requires PyGObject/GTK 3, libevdev, readable input devices, and /dev/uinput.
The candidate replaces a standalone daemon through its normal startup path.
--restore can name the installed standalone binary to restart afterward.
"""

import argparse
import ctypes
import ctypes.util
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import threading
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--restore", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--rounds", type=int, default=10)
    parser.add_argument("--cold-starts", type=int, default=1)
    parser.add_argument("--queued", action="store_true",
                        help="Pause only the test daemon while typing each word.")
    parser.add_argument("--helper", choices=['0', '1'], default='1')
    parser.add_argument("--wayland", choices=['0', '1'], default='1')
    parser.add_argument("--suffix", default='')
    parser.add_argument("--layout", type=int, choices=[0, 1], default=0)
    parser.add_argument("--expected-layout", type=int, choices=[0, 1])
    parser.add_argument("--injector-layout", type=int, choices=[0, 1],
                        help="Force a stale injector group before each trial.")
    parser.add_argument("--undo-expected", help="Exact field text expected after double Ctrl.")
    parser.add_argument("--word", default="recieve")
    parser.add_argument("--expected", default="receive")
    parser.add_argument("--spaces", type=int, default=3)
    parser.add_argument("--caps", action="store_true", help="Check replacement with Caps Lock enabled.")
    parser.add_argument("--max-latency-ms", type=float, help="Optional end-to-end replacement budget.")
    parser.add_argument("--space-hold-ms", type=float, default=6,
                        help="Physical Space hold duration for native latency trials (0–300 ms).")
    args = parser.parse_args()
    if not 0 <= args.space_hold_ms <= 300:
        parser.error("Space hold must be between 0 and 300 ms to avoid auto-repeat.")
    args.binary = args.binary.resolve(strict=True)
    args.restore = args.restore.resolve(strict=True) if args.restore else None
    if args.undo_expected is not None and args.rounds != 1:
        parser.error("Undo suppresses the word for this session; use --rounds 1 for undo trials.")
    if args.binary.name != "recast" or min(args.rounds, args.cold_starts, args.spaces) < 1:
        parser.error("Use a binary named recast and positive trial counts/spacing.")
    codes = dict(zip("qwertyuiop", range(16, 26)))
    codes.update(zip("asdfghjkl", range(30, 39)))
    codes.update(zip("zxcvbnm", range(44, 51)))
    codes[" "] = 57
    if any(char not in codes for char in args.word + args.suffix):
        parser.error("The synthetic input supports lowercase English letters only.")
    if args.queued and len(args.word) + args.spaces + len(args.suffix) > 15:
        parser.error("Queued trials must fit below the keyboard's 64-event input buffer.")

    if args.helper == '1':
        os.environ.pop('NO_AT_BRIDGE', None)
    import gi
    gi.require_version("Gtk", "3.0")
    from gi.repository import GLib, Gtk

    lib = ctypes.CDLL(ctypes.util.find_library("evdev"))
    lib.libevdev_new.restype = ctypes.c_void_p
    lib.libevdev_set_name.argtypes = [ctypes.c_void_p, ctypes.c_char_p]
    lib.libevdev_enable_event_type.argtypes = [ctypes.c_void_p, ctypes.c_uint]
    lib.libevdev_enable_event_code.argtypes = [ctypes.c_void_p, ctypes.c_uint,
                                             ctypes.c_uint, ctypes.c_void_p]
    lib.libevdev_uinput_create_from_device.argtypes = [ctypes.c_void_p, ctypes.c_int,
                                                      ctypes.POINTER(ctypes.c_void_p)]
    lib.libevdev_uinput_write_event.argtypes = [ctypes.c_void_p, ctypes.c_uint,
                                               ctypes.c_uint, ctypes.c_int]
    lib.libevdev_uinput_destroy.argtypes = [ctypes.c_void_p]
    lib.libevdev_free.argtypes = [ctypes.c_void_p]

    def hypr(*command):
        # Hyprland's Lua dispatcher replaces the older focuswindow dispatcher.
        if command[0] == "focus":
            command = ("eval", "return hl.dispatch(hl.dsp.focus({ window = "
                       + json.dumps(command[1]) + " }))")
        return subprocess.check_output(["hyprctl", *command], text=True)

    old_focus = json.loads(hypr("-j", "activewindow")).get("address")
    keyboards = json.loads(hypr("-j", "devices"))["keyboards"]
    old_layout = next(key["active_layout_index"] for key in keyboards if key["main"])
    old_caps = next(key.get("capsLock", False) for key in keyboards if key["main"])
    device = lib.libevdev_new()
    ui = ctypes.c_void_p()
    try:
        lib.libevdev_set_name(device, b"recast-replay-test")
        lib.libevdev_enable_event_type(device, 1)
        for key in range(1, 250):
            lib.libevdev_enable_event_code(device, 1, key, None)
        if lib.libevdev_uinput_create_from_device(device, -2, ctypes.byref(ui)) < 0:
            raise RuntimeError("Cannot create the synthetic keyboard")

        title = f"ReCast replay test {os.getpid()}"
        window = Gtk.Window(title=title)
        window.set_default_size(600, 200)
        view = Gtk.TextView()
        window.add(view)
        window.show_all()
        view.grab_focus()
        results = []
        changes = []
        errors = []
        cancelled = threading.Event()

        def cancel(*_):
            cancelled.set()
            return True

        window.connect("delete-event", cancel)
        signal.signal(signal.SIGINT, cancel)
        signal.signal(signal.SIGTERM, cancel)
        buffer = view.get_buffer()
        buffer.connect("changed", lambda value: changes.append([
            time.monotonic(), value.get_text(value.get_start_iter(), value.get_end_iter(), True)
        ]))

        def on_main(function):
            done = threading.Event()
            result = []

            def invoke():
                try:
                    result.append(function())
                finally:
                    done.set()
                return False

            GLib.idle_add(invoke)
            if not done.wait(3):
                raise RuntimeError("GTK main loop timed out")
            return result[0]

        def text():
            return on_main(lambda: buffer.get_text(buffer.get_start_iter(),
                                                  buffer.get_end_iter(), True))

        def assert_focus():
            if cancelled.is_set():
                raise RuntimeError("Test cancelled")
            if json.loads(hypr("-j", "activewindow")).get("title") != title:
                raise RuntimeError("Test field lost focus; stopping synthetic input")

        def emit(code, value):
            for kind, key, state in [(1, code, value), (0, 0, 0)]:
                if lib.libevdev_uinput_write_event(ui, kind, key, state) < 0:
                    raise RuntimeError("Synthetic keyboard write failed")

        def tap(code):
            assert_focus()
            pressed_at = time.monotonic()
            emit(code, 1)
            try:
                time.sleep(args.space_hold_ms / 1000 if code == 57 else .006)
            finally:
                emit(code, 0)
            time.sleep(.015)
            return pressed_at

        def set_caps(enabled):
            keys = json.loads(hypr("-j", "devices"))["keyboards"]
            current = next(key.get("capsLock", False) for key in keys
                           if key["name"] == "recast-replay-test")
            if current != enabled:
                emit(58, 1)
                emit(58, 0)
                time.sleep(.02)
                keys = json.loads(hypr("-j", "devices"))["keyboards"]
                actual = next(key.get("capsLock", False) for key in keys
                              if key["name"] == "recast-replay-test")
                if actual != enabled:
                    raise RuntimeError("Caps Lock key is remapped; this caps trial is unavailable")

        def worker():
            candidate = None
            started_candidate = False

            def stop_candidate():
                if candidate is None or candidate.poll() is not None:
                    return
                try:
                    os.kill(candidate.pid, signal.SIGCONT)
                    candidate.terminate()
                except ProcessLookupError:
                    pass
                try:
                    candidate.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    candidate.kill()
                    candidate.wait(timeout=3)

            try:
                with tempfile.TemporaryDirectory(prefix="recast-replay-") as config:
                    environment = {key: value for key, value in os.environ.items()
                                   if not key.startswith("RECAST_")}
                    environment.update(XDG_CONFIG_HOME=config,
                                       RECAST_LAYOUT_BACKEND="hyprland",
                                       RECAST_DESKTOP_HELPER=args.helper,
                                       RECAST_WAYLAND_INPUT=args.wayland)
                    on_main(window.present)
                    time.sleep(.1)
                    hypr("focus", "title:^" + title + "$")
                    for cold in range(args.cold_starts):
                        assert_focus()
                        stop_candidate()
                        candidate = subprocess.Popen(
                            [str(args.binary), "--foreground"], env=environment,
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                        started_candidate = True
                        deadline = time.monotonic() + 8
                        while True:
                            if cancelled.is_set():
                                raise RuntimeError("Test cancelled")
                            if candidate.poll() is not None:
                                raise RuntimeError("Test daemon exited during startup")
                            status = subprocess.run([str(args.binary), "--status"],
                                                    env=environment, capture_output=True,
                                                    text=True, timeout=4)
                            if "listener:       ready" in status.stdout:
                                break
                            if time.monotonic() > deadline:
                                raise RuntimeError("Test listener did not become ready")
                            time.sleep(.1)

                        hypr("focus", "title:^" + title + "$")
                        hypr("switchxkblayout", "all", str(args.layout))
                        time.sleep(.1)
                        set_caps(args.caps)
                        tap(1)
                        tap(57)
                        on_main(lambda: buffer.set_text(""))
                        expected = args.expected + " " * args.spaces + args.suffix
                        for trial in range(args.rounds):
                            assert_focus()
                            if args.injector_layout is not None:
                                hypr("switchxkblayout", "recast-injector", str(args.injector_layout))
                            if args.queued:
                                os.kill(candidate.pid, signal.SIGSTOP)
                            change_start = len(changes)
                            trigger = None
                            try:
                                for char in args.word + " " * args.spaces + args.suffix:
                                    pressed_at = tap(codes[char])
                                    if trigger is None and char == ' ':
                                        trigger = pressed_at
                            finally:
                                if args.queued:
                                    os.kill(candidate.pid, signal.SIGCONT)
                            deadline = time.monotonic() + 1
                            actual = text()
                            while actual != expected and time.monotonic() < deadline:
                                time.sleep(.005)
                                actual = text()
                            current_keys = json.loads(hypr("-j", "devices"))["keyboards"]
                            physical_layout = next(key["active_layout_index"] for key in current_keys
                                                   if key["name"] == "recast-replay-test")
                            layout_ok = (args.expected_layout is None or physical_layout == args.expected_layout)
                            visible_at = next((stamp for stamp, value in changes[change_start:]
                                               if value == expected and stamp >= trigger), None)
                            result = {"cold_start": cold, "round": trial, "actual": actual,
                                      "layout": physical_layout, "corrected": actual == expected and layout_ok,
                                      "replacement_ms": ((visible_at - trigger) * 1000
                                                         if visible_at is not None else None)}
                            if args.max_latency_ms is not None:
                                result['corrected'] &= (result['replacement_ms'] is not None
                                                        and result['replacement_ms'] <= args.max_latency_ms)
                            if args.undo_expected is not None and result['corrected']:
                                tap(29)
                                tap(29)
                                deadline = time.monotonic() + 1
                                actual = text()
                                while actual != args.undo_expected and time.monotonic() < deadline:
                                    time.sleep(.005)
                                    actual = text()
                                result['undo_actual'] = actual
                                restored_keys = json.loads(hypr("-j", "devices"))["keyboards"]
                                result['undo_layout'] = next(key["active_layout_index"] for key in restored_keys
                                                             if key["name"] == "recast-replay-test")
                                result['corrected'] &= (actual == args.undo_expected
                                                        and result['undo_layout'] == args.layout)
                            results.append(result)
                            for _ in actual:
                                tap(14)
                            if text():
                                raise RuntimeError("Exact deletion did not empty the test field")
                            hypr("switchxkblayout", "all", str(args.layout))
                            # External IPC switches do not generate modifier
                            # releases. Allow the documented 300 ms cache TTL.
                            time.sleep(.35)
                            tap(57)
                            on_main(lambda: buffer.set_text(""))
            except Exception as error:
                errors.append(str(error))
            finally:
                cleanup = [stop_candidate, lambda: set_caps(old_caps),
                           lambda: hypr("switchxkblayout", "all", str(old_layout))]
                if old_focus:
                    cleanup.append(lambda: hypr("focus", "address:" + old_focus))
                if started_candidate and args.restore:
                    restore_environment = dict(os.environ)
                    # The fixture may run inside a systemd-hosted terminal.
                    # Restoring a standalone app must not inherit service mode.
                    restore_environment.pop("INVOCATION_ID", None)
                    cleanup.append(lambda: subprocess.run(
                        [str(args.restore)], check=True, timeout=8, env=restore_environment,
                        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
                for action in cleanup:
                    try:
                        action()
                    except Exception as error:
                        errors.append("Cleanup: " + str(error))
                try:
                    args.output.write_text(json.dumps({"queued": args.queued, "space_hold_ms": args.space_hold_ms, "results": results,
                                                       "changes": changes, "errors": errors}, indent=2))
                except Exception as error:
                    errors.append("Saving results: " + str(error))
                finally:
                    GLib.idle_add(Gtk.main_quit)

        thread = threading.Thread(target=worker)
        thread.start()
        Gtk.main()
        thread.join()
        passed = sum(result["corrected"] for result in results)
        print(f"{passed}/{len(results)} passed; details: {args.output}")
        for error in errors:
            print(error)
        return 0 if not errors and passed == args.rounds * args.cold_starts else 1
    finally:
        if ui.value:
            lib.libevdev_uinput_destroy(ui)
        lib.libevdev_free(device)


if __name__ == "__main__":
    raise SystemExit(main())
