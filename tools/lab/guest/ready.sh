#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
set -euo pipefail
[[ $(cat /etc/companion-lab 2>/dev/null) == disposable-desktop-lab && $EUID != 0 ]]
[[ -n ${WAYLAND_DISPLAY:-} && -n ${DBUS_SESSION_BUS_ADDRESS:-} ]]
if [[ -z ${XDG_SESSION_ID:-} ]]; then
  # GDM's systemd-launched autostart can omit this variable. Select only this
  # guest user's active local graphical session, never an SSH login.
  lab_session=$(loginctl show-user "$(id -u)" -p Display --value)
  if [[ -n $lab_session &&
        $(loginctl show-session "$lab_session" -p Active --value) == yes &&
        $(loginctl show-session "$lab_session" -p Remote --value) == no &&
        $(loginctl show-session "$lab_session" -p Type --value) == wayland ]]; then
    export XDG_SESSION_ID="$lab_session"
  fi
fi
umask 077
# Only graphical endpoints are saved, never the complete process environment.
for variable in WAYLAND_DISPLAY DBUS_SESSION_BUS_ADDRESS XDG_RUNTIME_DIR XDG_CURRENT_DESKTOP XDG_SESSION_TYPE XDG_SESSION_ID HYPRLAND_INSTANCE_SIGNATURE; do
  printf 'export %s=%q\n' "$variable" "${!variable:-}"
done > "$HOME/lab/session.env"
dbus-update-activation-environment --systemd WAYLAND_DISPLAY XDG_CURRENT_DESKTOP XDG_SESSION_TYPE QT_LINUX_ACCESSIBILITY_ALWAYS_ON
mkdir -p "$HOME/.config/systemd/user"
cat > "$HOME/.config/systemd/user/desktop-lab.target" <<'EOF'
[Unit]
Description=Disposable graphical session
Requires=graphical-session.target
After=graphical-session.target
EOF
systemctl --user daemon-reload
systemctl --user start desktop-lab.target
systemctl --user start pipewire.service wireplumber.service
systemctl --user start xdg-desktop-portal.service
python3 "$HOME/lab/input-app.py" > "$HOME/evidence/input-app.log" 2>&1 &
if [[ -x $HOME/bin/ophio ]]; then
  # A delegated scope, as the installed user service has (Delegate=yes), so each
  # run gets a group of its own and Stop can end everything the run started.
  systemd-run --user --scope --quiet -p Delegate=yes \
    bash "$HOME/lab/in-session.sh" ophio run > "$HOME/evidence/companion.log" 2>&1 &
fi
