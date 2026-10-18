#!/usr/bin/env python3
"""Check native replay in a disposable GTK field on Linux/Hyprland.

Requires PyGObject/GTK 3, libevdev, readable input devices, and /dev/uinput.
The candidate replaces a standalone daemon through its normal startup path.
--restore must name the installed standalone binary to restart afterward.
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
    parser.add_argument("--restore", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--rounds", type=int, default=10)
    parser.add_argument("--cold-starts", type=int, default=1)
    parser.add_argument("--queued", action="store_true",
                        help="Pause only the test daemon while typing each word.")
    parser.add_argument("--word", default="recieve")
    parser.add_argument("--expected", default="receive")
    parser.add_argument("--spaces", type=int, default=3)
    args = parser.parse_args()
    args.binary = args.binary.resolve(strict=True)
    args.restore = args.restore.resolve(strict=True)
    if args.binary.name != "recast" or min(args.rounds, args.cold_starts, args.spaces) < 1:
        parser.error("Use a binary named recast and positive trial counts/spacing.")
    codes = dict(zip("qwertyuiop", range(16, 26)))
    codes.update(zip("asdfghjkl", range(30, 39)))
    codes.update(zip("zxcvbnm", range(44, 51)))
    codes[" "] = 57
    if any(char not in codes for char in args.word):
        parser.error("The synthetic input supports lowercase English letters only.")
    if args.queued and len(args.word) + args.spaces > 15:
        parser.error("Queued trials must fit below the keyboard's 64-event input buffer.")

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
            emit(code, 1)
            try:
                time.sleep(.006)
            finally:
                emit(code, 0)
            time.sleep(.015)

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
                                       RECAST_LAYOUT_BACKEND="hyprland")
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
                        hypr("switchxkblayout", "all", "0")
                        time.sleep(.1)
                        tap(1)
                        tap(57)
                        on_main(lambda: buffer.set_text(""))
                        expected = args.expected + " " * args.spaces
                        for trial in range(args.rounds):
                            assert_focus()
                            if args.queued:
                                os.kill(candidate.pid, signal.SIGSTOP)
                            try:
                                for char in args.word + " " * args.spaces:
                                    tap(codes[char])
                            finally:
                                if args.queued:
                                    os.kill(candidate.pid, signal.SIGCONT)
                            deadline = time.monotonic() + 1
                            actual = text()
                            while actual != expected and time.monotonic() < deadline:
                                time.sleep(.005)
                                actual = text()
                            results.append({"cold_start": cold, "round": trial,
                                            "actual": actual, "corrected": actual == expected})
                            for _ in actual:
                                tap(14)
                            if text():
                                raise RuntimeError("Exact deletion did not empty the test field")
            except Exception as error:
                errors.append(str(error))
            finally:
                cleanup = [stop_candidate,
                           lambda: hypr("switchxkblayout", "all", str(old_layout))]
                if old_focus:
                    cleanup.append(lambda: hypr("focus", "address:" + old_focus))
                if started_candidate:
                    cleanup.append(lambda: subprocess.run(
                        [str(args.restore)], check=True, timeout=8,
                        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
                for action in cleanup:
                    try:
                        action()
                    except Exception as error:
                        errors.append("Cleanup: " + str(error))
                try:
                    args.output.write_text(json.dumps({"queued": args.queued, "results": results,
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
