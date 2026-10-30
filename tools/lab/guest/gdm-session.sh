#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Configure only the marked, disposable Ubuntu guest for a normal GDM session.
set -euo pipefail
unset DISPLAY WAYLAND_DISPLAY DBUS_SESSION_BUS_ADDRESS
[[ $(cat /etc/companion-lab 2>/dev/null) == disposable-desktop-lab && $EUID == 0 ]]
[[ $(systemd-detect-virt) == kvm || $(systemd-detect-virt) == qemu ]]
. /etc/os-release
[[ $ID == ubuntu && $VERSION_ID == 24.04 ]]
if [[ ${1:-} == --install ]]; then
  export DEBIAN_FRONTEND=noninteractive
  apt-get update
  apt-get install -y --no-install-recommends gdm3
fi
dpkg-query -W -f='${Status}' gdm3 | grep -q 'install ok installed'
lab_home=$(getent passwd lab | cut -d: -f6)
systemctl disable --now desktop-lab.service
cat > /etc/gdm3/custom.conf <<'EOF'
[daemon]
WaylandEnable=true
AutomaticLoginEnable=true
AutomaticLogin=lab
[security]
[xdmcp]
[chooser]
[debug]
EOF
install -d /var/lib/AccountsService/users
cat > /var/lib/AccountsService/users/lab <<'EOF'
[User]
Session=gnome
XSession=gnome
SystemAccount=false
EOF
install -d -o lab -g lab "$lab_home/.config/autostart"
cat > "$lab_home/.config/autostart/desktop-lab.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Desktop qualification witness
Exec=/usr/bin/bash $lab_home/lab/ready.sh
OnlyShowIn=GNOME;
X-GNOME-Autostart-enabled=true
EOF
chown lab:lab "$lab_home/.config/autostart/desktop-lab.desktop"
systemctl enable gdm3
systemctl set-default graphical.target
echo 'GDM configured; reboot into restricted networking before qualification.'
