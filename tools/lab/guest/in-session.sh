#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
set -euo pipefail
unset DISPLAY WAYLAND_DISPLAY DBUS_SESSION_BUS_ADDRESS
[[ $(cat /etc/companion-lab 2>/dev/null) == disposable-desktop-lab && $EUID != 0 ]]
source "$HOME/lab/session.env"
export PATH="$HOME/bin:/usr/bin:/bin" SHELL=/bin/sh LANG=C.UTF-8
for name in claude codex tailscale; do
  [[ $(command -v "$name") == "$HOME/bin/$name" ]] || exit 1
done
exec "$@"

