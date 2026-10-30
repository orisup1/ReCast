#!/usr/bin/env python3
"""Optional ReCast desktop adapter. JSON lines on private parent-owned pipes only.

Requires PyGObject and the Atspi 2.0 introspection package. No text is read until
an explicit selection request; snapshots contain application/focus metadata only.
"""
import json
import os
import secrets
import sys
import time

MAX_TEXT = 65536
MAX_REQUEST = 524288


def replace_selection(target, expected, replacement, valid):
    """Insert before deleting: failed insertion can never erase the original."""
    # shortcut: AT-SPI edits are not atomic; use a native transaction if one becomes available.
    start, length, original = expected
    if not valid() or target.selection() != expected:
        return False
    if not target.insert(start, replacement):
        return False
    inserted = len(replacement)
    # Check the exact affected range before any deletion. A concurrent editor
    # change or failed read stops here, retaining the original text.
    if not valid() or target.text(start, start + inserted + length) != replacement + original:
        return False
    if not target.delete(start + inserted, start + inserted + length):
        # Roll back only the verified inserted prefix, never guessed offsets.
        if valid() and target.text(start, start + inserted + length) == replacement + original:
            target.delete(start, start + inserted)
            target.select(start, start + length)
        return False
    return valid() and target.select(start, start + inserted)


def main():
    import gi
    gi.require_version('Atspi', '2.0')
    from gi.repository import Atspi, Gio, GLib
    Atspi.init()
    Atspi.set_timeout(50, 100)
    bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
    focus = None
    revision = 0
    session = secrets.token_hex(16)
    deadline = 0.0

    def changed(event, *_args):
        nonlocal focus, revision
        revision += 1
        focus = event.source if event.detail1 else None

    listener = Atspi.EventListener.new(changed)
    listener.register('object:state-changed:focused')

    def gnome(method, argument=None):
        value = bus.call_sync('org.recast.Gnome', '/org/recast/Gnome',
                              'org.recast.Gnome', method, argument,
                              GLib.VariantType('(s)'), Gio.DBusCallFlags.NO_AUTO_START,
                              50, None)
        return json.loads(value.unpack()[0])

    def current():
        if focus is None:
            return None
        states = focus.get_state_set()
        if not states.contains(Atspi.StateType.FOCUSED) or states.contains(Atspi.StateType.DEFUNCT):
            return None
        parent = focus
        active = False
        for _ in range(32):
            if parent is None:
                break
            active = parent.get_state_set().contains(Atspi.StateType.ACTIVE)
            if active:
                break
            parent = parent.get_parent()
        if not active:
            return None
        return focus

    def snapshot():
        node = current()
        if node is None:
            return None
        pid = node.get_process_id()
        app = os.path.basename(os.readlink(f'/proc/{pid}/exe'))
        return {'id': f'a11y:{session}:{revision}', 'app': app}

    class Target:
        def __init__(self, node):
            self.node = node
            states = node.get_state_set()
            if node.get_role() == Atspi.Role.PASSWORD_TEXT or not states.contains(Atspi.StateType.EDITABLE):
                raise ValueError('Not an editable non-password field')
            self.reader = node.get_text_iface()
            self.writer = node.get_editable_text_iface()
            if self.reader is None or self.writer is None:
                raise ValueError('Editable text unavailable')

        def selection(self):
            if self.reader.get_n_selections() != 1:
                return None
            selected = self.reader.get_selection(0)
            start, end = selected.start_offset, selected.end_offset
            if start < 0 or not 0 < end - start <= MAX_TEXT:
                return None
            text = self.text(start, end)
            if len(text.encode('utf-8')) > MAX_TEXT or len(text) != end - start:
                return None
            return start, end - start, text

        def text(self, start, end):
            return self.reader.get_text(start, end)

        def insert(self, start, text):
            return self.writer.insert_text(start, text, len(text.encode('utf-8')))

        def delete(self, start, end):
            return self.writer.delete_text(start, end)

        def select(self, start, end):
            if self.reader.get_n_selections():
                return self.reader.set_selection(0, start, end)
            return self.reader.add_selection(start, end)

    def dispatch(request):
        nonlocal deadline
        deadline = request['deadline']
        if time.time() >= deadline:
            return None
        action = request['action']
        if action == 'layout':
            return gnome('Snapshot')
        if action == 'switch':
            return gnome('Switch', GLib.Variant('(u)', (request['index'],)))
        if action == 'focus':
            return snapshot()
        expected_id = request['focus']
        before = snapshot()
        if before is None or before['id'] != expected_id:
            return None
        target = Target(current())
        if action == 'empty':
            return target.reader.get_character_count() == 0
        selected = target.selection()
        if action == 'selection':
            return dict(zip(('start', 'length', 'text'), selected)) if selected else None
        if action != 'replace' or selected is None:
            return None
        expected = request['expected']
        expected = (expected['start'], expected['length'], expected['text'])
        replacement = request['text']
        if not replacement or len(replacement.encode('utf-8')) > MAX_TEXT or '\x00' in replacement:
            return None
        valid = lambda: snapshot() == before and time.time() < deadline
        if not replace_selection(target, expected, replacement, valid):
            return None
        selected = target.selection()
        if selected != (expected[0], len(replacement), replacement):
            return None
        return dict(zip(('start', 'length', 'text'), selected))

    def incoming(_source, condition):
        if condition & (GLib.IO_HUP | GLib.IO_ERR):
            loop.quit()
            return False
        line = sys.stdin.buffer.readline(MAX_REQUEST + 1)
        if not line or len(line) > MAX_REQUEST:
            loop.quit()
            return False
        request = {}
        try:
            request = json.loads(line)
            answer = dispatch(request)
        except (Exception,):
            answer = None
        print(json.dumps({'sequence': request.get('sequence'), 'answer': answer}, ensure_ascii=True), flush=True)
        return True

    loop = GLib.MainLoop()
    GLib.io_add_watch(sys.stdin.fileno(), GLib.IO_IN | GLib.IO_HUP | GLib.IO_ERR, incoming)
    print('{"ready":true}', flush=True)
    loop.run()
    listener.deregister('object:state-changed:focused')
    Atspi.exit()


if __name__ == '__main__':
    main()
