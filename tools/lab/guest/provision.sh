#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
set -euo pipefail
unset DISPLAY WAYLAND_DISPLAY DBUS_SESSION_BUS_ADDRESS
[[ $(cat /etc/companion-lab 2>/dev/null) == disposable-desktop-lab && $EUID == 0 ]]
[[ $(systemd-detect-virt) == kvm || $(systemd-detect-virt) == qemu ]]
case ${1:-} in
  hyprland)
    pacman -Syu --noconfirm --needed hyprland hyprlock xdg-desktop-portal \
      xdg-desktop-portal-hyprland pipewire wireplumber mesa mesa-utils \
      python python-gobject python-atspi python-dbus gtk3 foot grim wayland-utils \
      ttf-dejavu dbus xorg-xwayland
    ;;
  plasma)
    pacman -Syu --noconfirm --needed plasma-desktop plasma-workspace kwin \
      xdg-desktop-portal xdg-desktop-portal-kde pipewire wireplumber mesa \
      mesa-utils python python-gobject python-atspi python-dbus gtk3 konsole spectacle \
      wayland-utils ttf-dejavu dbus xorg-xwayland kscreen
    ;;
  gnome)
    export DEBIAN_FRONTEND=noninteractive
    apt-get update
    apt-get install -y --no-install-recommends gnome-shell gnome-session \
      gnome-settings-daemon gnome-control-center gnome-terminal gnome-screenshot gjs \
      xdg-desktop-portal xdg-desktop-portal-gnome pipewire wireplumber \
      libgl1-mesa-dri mesa-utils python3-gi python3-pyatspi gir1.2-gtk-3.0 \
      dbus-user-session xwayland wayland-utils fonts-dejavu-core \
      gnome-keyring libpipewire-0.3-0 libxkbcommon0
    ;;
  builder)
    # Release archives only: build tools, no desktop or session service.
    runuser -u lab -- bash "$(getent passwd lab | cut -d: -f6)/lab/toolchain.sh"
    printf '%s\n' "$1" > /etc/companion-lab-target
    exit 0
    ;;
  *) echo 'Expected hyprland, gnome, plasma or builder' >&2; exit 2 ;;
esac
loginctl enable-linger lab
# The guest's RTC is supplied by QEMU. Waiting for internet time synchronization
# blocks sshdgenkeys/cloud-final on Arch when qualification networking is restricted.
systemctl disable --now systemd-time-wait-sync.service || true
systemctl mask systemd-time-wait-sync.service
if [[ $1 == gnome ]]; then
  systemctl enable ssh
else
  systemctl enable sshd
fi
usermod -aG video,render lab
lab_home=$(getent passwd lab | cut -d: -f6)
install -d -o lab -g lab "$lab_home"/{bin,evidence,project,.config}
# A guest-local boot service establishes a PAM/logind session on the guest VT.
# It has no network access beyond the VM's explicitly selected QEMU mode.
cat > /etc/systemd/system/desktop-lab.service <<EOF
[Unit]
Description=Disposable desktop qualification session
After=systemd-user-sessions.service
Conflicts=getty@tty1.service
[Service]
User=lab
PAMName=login
TTYPath=/dev/tty1
StandardInput=tty
StandardOutput=append:$lab_home/evidence/session.log
StandardError=append:$lab_home/evidence/session.log
Environment=HOME=$lab_home
# A PAM VT login may add CAP_WAKE_ALARM. Drop inherited capabilities so the
# portal can inspect this ordinary user's processes using the normal rules.
ExecStart=/usr/bin/setpriv --inh-caps=-all --ambient-caps=-all /usr/bin/bash $lab_home/lab/session.sh $1
Restart=no
[Install]
WantedBy=multi-user.target
EOF
systemctl daemon-reload
systemctl enable desktop-lab.service
printf '%s\n' "$1" > /etc/companion-lab-target
