#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
set -euo pipefail
unset DISPLAY WAYLAND_DISPLAY DBUS_SESSION_BUS_ADDRESS
[[ $(cat /etc/companion-lab 2>/dev/null) == disposable-desktop-lab && $EUID != 0 ]]
export XDG_RUNTIME_DIR="/run/user/$(id -u)" XDG_SESSION_TYPE=wayland
export DBUS_SESSION_BUS_ADDRESS="unix:path=$XDG_RUNTIME_DIR/bus"
export PATH="$HOME/bin:/usr/bin:/bin" LANG=C.UTF-8
export LIBGL_ALWAYS_SOFTWARE=1
export QT_LINUX_ACCESSIBILITY_ALWAYS_ON=1
case ${1:-} in
  hyprland)
    export XDG_CURRENT_DESKTOP=Hyprland AQ_NO_KMS_REQUIREMENT=1
    mkdir -p "$HOME/.config/hypr"
    cat > "$HOME/.config/hypr/hyprland.conf" <<'EOF'
monitor = ,1280x720@60,auto,1
exec-once = bash $HOME/lab/ready.sh
misc {
    disable_hyprland_logo = true
    disable_splash_rendering = true
}
animations {
    enabled = false
}
EOF
    exec Hyprland --config "$HOME/.config/hypr/hyprland.conf"
    ;;
  gnome)
    export XDG_CURRENT_DESKTOP=GNOME
    gsettings set org.gnome.desktop.interface toolkit-accessibility true
    /usr/bin/gnome-shell --wayland --headless --no-x11 --virtual-monitor 1280x720 &
    compositor_pid=$!
    for _ in $(seq 1 120); do
      [[ -S "$XDG_RUNTIME_DIR/wayland-0" ]] && break
      sleep 0.5
    done
    export WAYLAND_DISPLAY=wayland-0
    bash "$HOME/lab/ready.sh"
    wait "$compositor_pid"
    ;;
  plasma)
    export XDG_CURRENT_DESKTOP=KDE QT_QPA_PLATFORM=wayland
    /usr/bin/kwin_wayland --drm &
    compositor_pid=$!
    for _ in $(seq 1 120); do
      [[ -S "$XDG_RUNTIME_DIR/wayland-0" ]] && break
      sleep 0.5
    done
    export WAYLAND_DISPLAY=wayland-0
    plasmashell &
    bash "$HOME/lab/ready.sh"
    wait "$compositor_pid"
    ;;
  *) exit 2 ;;
esac
