// Loaded only by scripts/wayland.sh into a private headless GNOME Shell.
import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Meta from 'gi://Meta';
import Shell from 'gi://Shell';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';

const IFACE = `<node><interface name="org.ideae.Harness">
  <method name="Step"><arg type="s" direction="in"/><arg type="s" direction="out"/></method>
</interface></node>`;

function maximized(w) {
    if (typeof w.is_maximized === 'function')
        return w.is_maximized();
    return w.get_maximized() === Meta.MaximizeFlags.BOTH;
}

function describe(w) {
    const f = w.get_frame_rect();
    const b = w.get_buffer_rect();
    return `"${w.get_title()}" frame=${f.x},${f.y} ${f.width}x${f.height} ` +
        `buffer=${b.x},${b.y} ${b.width}x${b.height} max=${maximized(w)} full=${w.is_fullscreen()}`;
}

function appWindows() {
    return global.display.list_all_windows()
        .filter(w => w.get_window_type() === Meta.WindowType.NORMAL);
}

export default class Harness extends Extension {
    enable() {
        this._log = GLib.getenv('IDEAE_WL_LOG');
        this._pointer = global.stage.context.get_backend().get_default_seat()
            .create_virtual_device(Clutter.InputDeviceType.POINTER_DEVICE);
        this._ids = [];
        this._ids.push([global.display, global.display.connect('window-created', (_d, w) => this._watch(w))]);
        for (const w of global.display.list_all_windows())
            this._watch(w);
        if (Main.layoutManager._startingUp)
            this._ids.push([Main.layoutManager, Main.layoutManager.connect('startup-complete', () => Main.overview.hide())]);
        else
            Main.overview.hide();
        this._dbus = Gio.DBusExportedObject.wrapJSObject(IFACE, this);
        this._dbus.export(Gio.DBus.session, '/org/ideae/Harness');
        this.write('ready');
    }

    disable() {
        this._dbus?.unexport();
        for (const [obj, id] of this._ids)
            obj.disconnect(id);
        this._ids = [];
    }

    write(line) {
        if (!this._log)
            return;
        const t = (GLib.get_monotonic_time() / 1e6).toFixed(3);
        const out = Gio.File.new_for_path(this._log).append_to(Gio.FileCreateFlags.NONE, null);
        out.write_all(new TextEncoder().encode(`${t} ${line}\n`), null);
        out.close(null);
    }

    _watch(w) {
        if (w.get_window_type() !== Meta.WindowType.NORMAL)
            return;
        const report = what => this.write(`${what} ${describe(w)}`);
        report('created');
        for (const signal of ['position-changed', 'size-changed', 'notify::maximized-horizontally',
            'notify::maximized-vertically', 'notify::fullscreen'])
            this._ids.push([w, w.connect(signal, () => report(signal))]);
        this._ids.push([w, w.connect('unmanaged', () => this.write(`unmanaged "${w.get_title()}"`))]);
    }

    _move(x, y) {
        this._pointer.notify_absolute_motion(GLib.get_monotonic_time(), x, y);
    }

    _button(button, pressed) {
        this._pointer.notify_button(GLib.get_monotonic_time(), button,
            pressed ? Clutter.ButtonState.PRESSED : Clutter.ButtonState.RELEASED);
    }

    // Screen coordinates, or with @ relative to the first app window's frame.
    _point(spec) {
        let [x, y] = spec.replace(/^@/, '').split(',').map(Number);
        if (spec.startsWith('@')) {
            const w = appWindows()[0];
            if (!w)
                throw new Error('no app window');
            const f = w.get_frame_rect();
            x += f.x;
            y += f.y;
        }
        return [x, y];
    }

    // One step of scripts/wayland.sh; the answer goes to its stdout.
    StepAsync([step], invocation) {
        this._step(step).then(
            text => invocation.return_value(new GLib.Variant('(s)', [text])),
            e => invocation.return_dbus_error('org.ideae.Harness.Error', `${e}`));
    }

    // Events a few milliseconds apart, as a hand makes them: clients see the
    // pointer arrive before it presses.
    async _pause(ms = 30) {
        await new Promise(resolve => GLib.timeout_add(GLib.PRIORITY_DEFAULT, ms, () => {
            resolve();
            return GLib.SOURCE_REMOVE;
        }));
    }

    async _step(step) {
        const [name, ...rest] = step.split(':');
        const arg = rest.join(':');
        const button = {click: 1, dclick: 1, rclick: 3, mclick: 2}[name];
        if (name === 'move') {
            this._move(...this._point(arg));
            await this._pause();
            return '';
        }
        if (button) {
            if (arg) {
                this._move(...this._point(arg));
                await this._pause(100);
            }
            for (let i = 0; i < (name === 'dclick' ? 2 : 1); i++) {
                this._button(button, true);
                await this._pause();
                this._button(button, false);
                await this._pause();
            }
            return '';
        }
        if (name === 'press' || name === 'release') {
            this._button(Number(arg || 1), name === 'press');
            await this._pause();
            return '';
        }
        if (name === 'windows')
            return appWindows().map(describe).join('\n');
        if (name === 'log') {
            this.write(arg);
            return '';
        }
        if (name === 'shot') {
            const stream = Gio.File.new_for_path(arg).replace(null, false, Gio.FileCreateFlags.NONE, null);
            await new Shell.Screenshot().screenshot(false, stream);
            stream.close(null);
            return arg;
        }
        throw new Error(`unknown step ${step}`);
    }
}

