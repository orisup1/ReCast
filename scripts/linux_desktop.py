#!/usr/bin/env python3
"""Optional ReCast desktop adapter. JSON lines on private parent-owned pipes only.

Requires PyGObject and the Atspi 2.0 introspection package. Text reads are bounded
to an explicit selection or the expected captured suffix; focus snapshots contain
application/focus metadata only.
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


def replace_suffix(target, expected, replacement, valid, begin=lambda: None):
    """Replace only captured text immediately before an unselected caret.

    AT-SPI has no atomic replace operation. Insert first, then verify the exact
    range and caret again before deleting. An interrupted transaction must never
    fall back to blind key replay, even if it reports failure.
    """
    caret = target.caret()
    start = caret - len(expected)
    if (not expected or not replacement or start < 0 or target.has_selection()
            or not valid() or target.text(start, caret) != expected):
        return False
    if not valid() or target.caret() != caret or target.has_selection():
        return False
    begin()
    if not target.insert(start, replacement):
        return False
    inserted = len(replacement)
    # Providers differ in whether programmatic insertion advances the caret.
    # Accept both unchanged and shifted positions, but never a third position.
    if (not valid() or target.has_selection()
            or target.caret() not in (caret, caret + inserted)
            or target.text(start, caret + inserted) != replacement + expected):
        # Continued typing can move the caret past the inserted range. Roll
        # back only our still-verifiable prefix, preserving the original and
        # anything the user added. Never delete against a changed focus/range.
        if valid() and target.text(start, caret + inserted) == replacement + expected:
            target.delete(start, start + inserted)
        return False
    if not target.delete(start + inserted, caret + inserted):
        if valid() and target.text(start, caret + inserted) == replacement + expected:
            target.delete(start, start + inserted)
        return False
    return valid() and target.set_caret(start + inserted)


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
        if event.detail1:
            focus = event.source
        elif focus == event.source:
            focus = None

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
        # Focus events are dispatched by this same main loop. Synchronous edit
        # requests must query current state rather than an unprocessed cache.
        focus.clear_cache()
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
            if node.get_role() == Atspi.Role.PASSWORD_TEXT:
                raise PermissionError('Password fields cannot be edited')
            if not states.contains(Atspi.StateType.EDITABLE):
                raise ValueError('Not an editable field')
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

        def caret(self):
            return self.reader.get_caret_offset()

        def set_caret(self, offset):
            return self.reader.set_caret_offset(offset)

        def has_selection(self):
            return self.reader.get_n_selections() != 0

        def text(self, start, end):
            return Atspi.Text.get_text(self.reader, start, end)

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
        if action == 'replacement_target':
            before = snapshot()
            if before is None:
                return None
            # Capability checks only: no text, selection, or caret reads.
            Target(current())
            return before if snapshot() == before else None
        expected_id = request['focus']
        before = snapshot()
        if before is None or before['id'] != expected_id:
            return None
        try:
            target = Target(current())
        except PermissionError:
            return {'status': 'unchanged'} if action == 'replace_suffix' else None
        except ValueError:
            return {'status': 'unsupported'} if action == 'replace_suffix' else None
        if action == 'replace_suffix':
            expected, replacement = request['expected'], request['text']
            if (not isinstance(expected, str) or not isinstance(replacement, str)
                    or max(len(expected.encode('utf-8')), len(replacement.encode('utf-8'))) > MAX_TEXT
                    or '\x00' in expected + replacement):
                return {'status': 'rejected'}
            valid = lambda: snapshot() == before and time.time() < deadline
            started = False
            def begin():
                nonlocal started
                started = True
            applied = replace_suffix(target, expected, replacement, valid, begin)
            return {'status': 'applied' if applied else ('rejected' if started else 'unchanged')}
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

    # A listener receives future changes, not the already-focused control.
    # Seed once from active windows so starting ReCast mid-session needs no
    # focus-changing warmup. Read states/identity only, never document text.
    seed_deadline = time.monotonic() + 0.5
    try:
        desktop = Atspi.get_desktop(0)
        pending = []
        for app_index in range(desktop.get_child_count()):
            if time.monotonic() >= seed_deadline:
                break
            app = desktop.get_child_at_index(app_index)
            for window_index in range(app.get_child_count()):
                window = app.get_child_at_index(window_index)
                if window.get_state_set().contains(Atspi.StateType.ACTIVE):
                    pending.append(window)
        visited = 0
        while pending and visited < 512 and time.monotonic() < seed_deadline:
            node = pending.pop()
            visited += 1
            states = node.get_state_set()
            if states.contains(Atspi.StateType.FOCUSED):
                focus = node
                break
            if not states.contains(Atspi.StateType.DEFUNCT):
                pending.extend(node.get_child_at_index(index)
                               for index in range(min(node.get_child_count(), 128)))
    except (Exception,):
        pass

    loop = GLib.MainLoop()
    GLib.io_add_watch(sys.stdin.fileno(), GLib.IO_IN | GLib.IO_HUP | GLib.IO_ERR, incoming)
    print('{"ready":true}', flush=True)
    loop.run()
    listener.deregister('object:state-changed:focused')
    Atspi.exit()


if __name__ == '__main__':
    main()
