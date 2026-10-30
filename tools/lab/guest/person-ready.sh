#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Session start for the qualification user. Records the desktop's endpoints as a
# terminal opened in this session would inherit them, then opens the witness.
set -euo pipefail
[[ $(cat /etc/companion-lab 2>/dev/null) == disposable-desktop-lab && $EUID != 0 ]]
[[ -n ${WAYLAND_DISPLAY:-} && -n ${XDG_RUNTIME_DIR:-} ]]
umask 077
state=$XDG_RUNTIME_DIR/desktop-lab
mkdir -p "$state" "$HOME/evidence"
# Only these variables are saved, never the complete process environment.
for variable in PATH WAYLAND_DISPLAY DISPLAY DBUS_SESSION_BUS_ADDRESS XDG_RUNTIME_DIR \
  XDG_CURRENT_DESKTOP XDG_SESSION_DESKTOP XDG_SESSION_TYPE XDG_SESSION_ID \
  XDG_DATA_DIRS XDG_CONFIG_DIRS DESKTOP_SESSION HYPRLAND_INSTANCE_SIGNATURE; do
  if [[ -n ${!variable:-} ]]; then
    printf '%s=%s\0' "$variable" "${!variable}"
  fi
done > "$state/session.env.new"
mv "$state/session.env.new" "$state/session.env"
# Idle locking would otherwise interrupt a long watch in this unattended guest.
case ${XDG_CURRENT_DESKTOP:-} in
  *GNOME*) gsettings set org.gnome.desktop.session idle-delay 0 ;;
  *KDE*) kwriteconfig6 --file kscreenlockerrc --group Daemon --key Autolock false ;;
esac
nohup python3 /usr/local/lib/desktop-lab/input-app.py > "$state/input-app.log" 2>&1 < /dev/null &
