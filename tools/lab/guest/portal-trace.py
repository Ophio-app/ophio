#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Guest-only portal message metadata; never record arguments or restore tokens."""

import json
from pathlib import Path
import time

if (
    Path("/etc/companion-lab").read_text(encoding="utf-8").strip()
    != "disposable-desktop-lab"
):
    raise SystemExit("Requires the disposable guest")

import dbus
from dbus.mainloop.glib import DBusGMainLoop
from gi.repository import GLib

DBusGMainLoop(set_as_default=True)
connection = dbus.SessionBus(private=True)


def message(_, value):
    interface = value.get_interface()
    if interface and not any(name in interface for name in ("portal", "Mutter")):
        return
    record = {
        "at": time.monotonic_ns(),
        "type": value.get_type(),
        "sender": value.get_sender(),
        "destination": value.get_destination(),
        "serial": value.get_serial(),
        "reply_serial": value.get_reply_serial(),
        "interface": interface,
        "member": value.get_member(),
        "error": value.get_error_name(),
    }
    if (
        interface == "org.freedesktop.portal.Request"
        and value.get_member() == "Response"
    ):
        arguments = value.get_args_list()
        record["response"] = int(arguments[0])
        record["result_keys"] = list(arguments[1].keys())
    print(json.dumps(record), flush=True)


connection.add_message_filter(message)
connection.call_blocking(
    "org.freedesktop.DBus",
    "/org/freedesktop/DBus",
    "org.freedesktop.DBus.Monitoring",
    "BecomeMonitor",
    "asu",
    ([], 0),
)
GLib.MainLoop().run()
