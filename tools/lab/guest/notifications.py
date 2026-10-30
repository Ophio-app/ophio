#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Disposable guest notification service with real, visible action buttons."""

import os
from pathlib import Path

if (
    Path("/etc/companion-lab").read_text(encoding="utf-8").strip()
    != "disposable-desktop-lab"
    or os.geteuid() == 0
):
    raise SystemExit("Requires an ordinary disposable guest user")

import gi

gi.require_version("Gtk", "3.0")
from gi.repository import Gio, GLib, Gtk  # noqa: E402

XML = """<node><interface name="org.freedesktop.Notifications">
<method name="GetCapabilities"><arg type="as" direction="out"/></method>
<method name="GetServerInformation"><arg type="s" direction="out"/>
<arg type="s" direction="out"/><arg type="s" direction="out"/>
<arg type="s" direction="out"/></method>
<method name="Notify"><arg type="s" direction="in"/><arg type="u" direction="in"/>
<arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="s" direction="in"/>
<arg type="as" direction="in"/><arg type="a{sv}" direction="in"/>
<arg type="i" direction="in"/><arg type="u" direction="out"/></method>
<method name="CloseNotification"><arg type="u" direction="in"/></method>
<signal name="ActionInvoked"><arg type="u"/><arg type="s"/></signal>
<signal name="NotificationClosed"><arg type="u"/><arg type="u"/></signal>
</interface></node>"""
BUS_PATH = "/org/freedesktop/Notifications"
INTERFACE = "org.freedesktop.Notifications"
connection = Gio.bus_get_sync(Gio.BusType.SESSION, None)
reply = connection.call_sync(
    "org.freedesktop.DBus",
    "/org/freedesktop/DBus",
    "org.freedesktop.DBus",
    "RequestName",
    GLib.Variant("(su)", (INTERFACE, 4)),
    GLib.VariantType.new("(u)"),
    Gio.DBusCallFlags.NONE,
    -1,
    None,
)
if reply.unpack()[0] != 1:
    raise SystemExit("A notification service already owns the guest session")
windows = {}
next_id = 1


def close(identifier, reason):
    window = windows.pop(identifier, None)
    if window is not None:
        window.destroy()
        connection.emit_signal(
            None,
            BUS_PATH,
            INTERFACE,
            "NotificationClosed",
            GLib.Variant("(uu)", (identifier, reason)),
        )


def action(_button, identifier, key):
    connection.emit_signal(
        None,
        BUS_PATH,
        INTERFACE,
        "ActionInvoked",
        GLib.Variant("(us)", (identifier, key)),
    )


def method(_connection, _sender, _path, _interface, name, parameters, invocation):
    global next_id
    if name == "GetCapabilities":
        invocation.return_value(
            GLib.Variant("(as)", (["actions", "body", "persistence"],))
        )
    elif name == "GetServerInformation":
        invocation.return_value(
            GLib.Variant("(ssss)", ("Guest action witness", "Lab", "1", "1.2"))
        )
    elif name == "CloseNotification":
        close(parameters.unpack()[0], 3)
        invocation.return_value(None)
    elif name == "Notify":
        _app, replace, _icon, summary, body, actions, _hints, _timeout = (
            parameters.unpack()
        )
        identifier = replace if replace in windows else next_id
        if identifier == next_id:
            next_id += 1
        previous = windows.pop(identifier, None)
        if previous is not None:
            previous.destroy()
        window = Gtk.Window(title="Guest notification")
        window.set_default_size(360, 180)
        window.set_keep_above(True)
        window.set_accept_focus(False)
        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=8)
        box.set_border_width(12)
        window.add(box)
        for text in (summary, body):
            label = Gtk.Label(label=text)
            label.set_line_wrap(True)
            box.pack_start(label, False, False, 0)
        for key, label in zip(actions[::2], actions[1::2]):
            button = Gtk.Button(label=label)
            button.connect("clicked", action, identifier, key)
            box.pack_start(button, False, False, 0)
        window.connect("delete-event", lambda _w, _e: True)
        windows[identifier] = window
        window.show_all()
        invocation.return_value(GLib.Variant("(u)", (identifier,)))
    else:
        invocation.return_dbus_error("org.freedesktop.DBus.Error.UnknownMethod", name)


connection.register_object(
    BUS_PATH, Gio.DBusNodeInfo.new_for_xml(XML).interfaces[0], method, None, None
)
Gtk.main()
