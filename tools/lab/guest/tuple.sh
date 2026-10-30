#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
set -euo pipefail
[[ $(cat /etc/companion-lab 2>/dev/null) == disposable-desktop-lab && $EUID != 0 ]]
source "$HOME/lab/session.env"
cat /etc/os-release
uname -srvm
if command -v pacman >/dev/null; then
  pacman -Q | awk '$1 ~ /^(hyprland|aquamarine|kwin|plasma-workspace|xdg-desktop-portal.*|pipewire|wireplumber|mesa|linux)$/ {print}'
else
  dpkg-query -W gnome-shell mutter-common xdg-desktop-portal xdg-desktop-portal-gnome \
    pipewire wireplumber libgl1-mesa-dri
fi
wayland-info
eglinfo -B || printf '%s\n' 'eglinfo reported an unavailable platform; inspect the preceding platform results' >&2 || printf 'Some EGL platforms were unavailable (see output above).\n'
if command -v hyprctl >/dev/null; then hyprctl -j monitors; fi
if command -v kscreen-doctor >/dev/null; then kscreen-doctor -o; fi
