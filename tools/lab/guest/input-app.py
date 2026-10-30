#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Guest-only input witness: text, pointer, scrolling and held input state."""

import json
import os
from pathlib import Path
import time
import sys

if (
    Path("/etc/companion-lab").read_text(encoding="utf-8").strip()
    != "disposable-desktop-lab"
    or os.geteuid() == 0
):
    raise SystemExit("Requires an ordinary disposable guest user")

import gi

gi.require_version("Gtk", "3.0")
gi.require_version("Gdk", "3.0")
from gi.repository import Gdk, GLib, Gtk  # noqa: E402 (select GTK before importing it)

evidence = Path.home() / "evidence"
state = {
    "keys": [],
    "buttons": [],
    "clicks": 0,
    "scrolls": 0,
    "motion": None,
    "text": "",
}
window = Gtk.Window(title="Desktop input witness")
window.set_default_size(1000, 600)
window.connect("destroy", Gtk.main_quit)
box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=8)
window.add(box)
label = Gtk.Label(label="Guest desktop input witness")
box.pack_start(label, False, False, 0)
clock = Gtk.Label()
scroll = Gtk.ScrolledWindow()
view = Gtk.TextView()
scroll.add(view)
box.pack_start(scroll, True, True, 0)
# Keep the changing capture witness clear of desktop notification banners.
box.pack_start(clock, False, False, 0)
buffer = view.get_buffer()


def save(event):
    state["monotonic_ns"] = time.monotonic_ns()
    state["realtime_ns"] = time.time_ns()
    state["event"] = event
    temporary = evidence / "input-state.tmp"
    temporary.write_text(json.dumps(state, ensure_ascii=False), encoding="utf-8")
    temporary.replace(evidence / "input-state.json")
    with (evidence / "input-events.jsonl").open("a", encoding="utf-8") as stream:
        stream.write(json.dumps(state, ensure_ascii=False) + "\n")


def key(_, event):
    code = Gdk.keyval_name(event.keyval)
    if event.type == Gdk.EventType.KEY_PRESS and code not in state["keys"]:
        state["keys"].append(code)
    elif event.type == Gdk.EventType.KEY_RELEASE and code in state["keys"]:
        state["keys"].remove(code)
    save("key")
    return False


def pointer(_, event):
    state["motion"] = [event.x, event.y]
    state["screen_point"] = [event.x_root, event.y_root]
    state["view_size"] = [view.get_allocated_width(), view.get_allocated_height()]
    if event.type == Gdk.EventType.BUTTON_PRESS:
        state["clicks"] += 1
        if event.button not in state["buttons"]:
            state["buttons"].append(event.button)
    elif (
        event.type == Gdk.EventType.BUTTON_RELEASE and event.button in state["buttons"]
    ):
        state["buttons"].remove(event.button)
    elif event.type == Gdk.EventType.SCROLL:
        state["scrolls"] += 1
    save("pointer")
    return False


def changed(_):
    state["text"] = buffer.get_text(
        buffer.get_start_iter(), buffer.get_end_iter(), True
    )
    save("text")


def tick():
    clock.set_text(f"Frame witness: {time.monotonic_ns()}")
    return True


view.add_events(
    Gdk.EventMask.POINTER_MOTION_MASK
    | Gdk.EventMask.SCROLL_MASK
    | Gdk.EventMask.BUTTON_PRESS_MASK
    | Gdk.EventMask.BUTTON_RELEASE_MASK
)
for name in ("key-press-event", "key-release-event"):
    view.connect(name, key)
for name in (
    "motion-notify-event",
    "button-press-event",
    "button-release-event",
    "scroll-event",
):
    view.connect(name, pointer)
buffer.connect("changed", changed)
GLib.timeout_add(50, tick)
window.show_all()
if "--fullscreen" in sys.argv:
    window.fullscreen()
view.grab_focus()
save("start")
Gtk.main()
