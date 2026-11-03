import Gio from 'gi://Gio';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Keyboard from 'resource:///org/gnome/shell/ui/status/keyboard.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';

const XML = `<node><interface name="org.recast.Gnome">
<method name="Snapshot"><arg type="s" direction="out"/></method>
<method name="Switch"><arg type="u" direction="in"/><arg type="s" direction="out"/></method>
</interface></node>`;

export default class ReCast extends Extension {
    enable() {
        this._object = Gio.DBusExportedObject.wrapJSObject(XML, this);
        this._object.export(Gio.DBus.session, '/org/recast/Gnome');
        this._owner = Gio.bus_own_name_on_connection(Gio.DBus.session, 'org.recast.Gnome',
            Gio.BusNameOwnerFlags.NONE, null, null);
    }
    disable() {
        if (this._owner) Gio.bus_unown_name(this._owner);
        this._owner = null;
        this._object?.unexport();
        this._object = null;
    }
    Snapshot() {
        if (Main.sessionMode.isLocked || Main.sessionMode.isGreeter) return 'null';
        const manager = Keyboard.getInputSourceManager();
        const sources = Object.values(manager.inputSources).sort((a, b) => a.index - b.index);
        return JSON.stringify({
            names: sources.map(source => source.type === 'xkb' ? source.id : `ibus:${source.id}`),
            current: sources.indexOf(manager.currentSource),
        });
    }
    Switch(index) {
        if (Main.sessionMode.isLocked || Main.sessionMode.isGreeter) return 'false';
        const sources = Object.values(Keyboard.getInputSourceManager().inputSources)
            .sort((a, b) => a.index - b.index);
        const source = sources[index];
        if (!source || source.type !== 'xkb') return 'false';
        source.activate(true);
        return 'true';
    }
}
