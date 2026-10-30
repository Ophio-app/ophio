# SPDX-License-Identifier: Apache-2.0
"""Rescale the disposable GNOME guest's existing logical monitor."""

from pathlib import Path
import sys
import gi

gi.require_version("Gio", "2.0")
from gi.repository import Gio, GLib  # noqa: E402 (select Gio before import)

if (
    Path("/etc/companion-lab").read_text(encoding="utf-8").strip()
    != "disposable-desktop-lab"
):
    raise SystemExit("Disposable guest required")
scale = float(sys.argv[1])
if scale not in (1.0, 1.25):
    raise SystemExit("Expected the qualification scales 1 or 1.25")
proxy = Gio.DBusProxy.new_for_bus_sync(
    Gio.BusType.SESSION,
    Gio.DBusProxyFlags.NONE,
    None,
    "org.gnome.Mutter.DisplayConfig",
    "/org/gnome/Mutter/DisplayConfig",
    "org.gnome.Mutter.DisplayConfig",
    None,
)
serial, monitors, logical, _ = proxy.call_sync(
    "GetCurrentState", None, Gio.DBusCallFlags.NONE, 5000, None
).unpack()
if len(monitors) != 1 or len(logical) != 1:
    raise SystemExit("This check requires the single lab monitor")
identity, modes, _ = monitors[0]
mode = next(mode[0] for mode in modes if mode[6].get("is-current"))
x, y, _, transform, primary, _, _ = logical[0]
params = GLib.Variant(
    "(uua(iiduba(ssa{sv}))a{sv})",
    (
        serial,
        1,
        [(x, y, scale, transform, primary, [(identity[0], mode, {})])],
        {"layout-mode": GLib.Variant("u", 1)},
    ),
)
proxy.call_sync("ApplyMonitorsConfig", params, Gio.DBusCallFlags.NONE, 5000, None)
print(f"Applied scale {scale} to the current physical mode")
