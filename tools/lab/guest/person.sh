#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# A new ordinary desktop user for release-archive qualification, in the marked
# disposable guest only. They get no sudo, lab tools or Øphio state. Commands
# run with the environment their own desktop session gives a terminal.
set -euo pipefail
unset DISPLAY WAYLAND_DISPLAY DBUS_SESSION_BUS_ADDRESS
[[ $(cat /etc/companion-lab 2>/dev/null) == disposable-desktop-lab && $EUID == 0 ]]
[[ $(systemd-detect-virt) == kvm || $(systemd-detect-virt) == qemu ]]
person=newuser
lab_home=$(getent passwd lab | cut -d: -f6)
shared=/usr/local/lib/desktop-lab

case ${1:-} in
  setup)
    target=$(cat /etc/companion-lab-target)
    id "$person" > /dev/null 2>&1 || useradd --create-home --user-group --shell /bin/bash "$person"
    home=$(getent passwd "$person" | cut -d: -f6)
    install -d -m 755 "$shared"
    for name in person-ready.sh input-app.py accessibility.py; do
      install -m 644 "$lab_home/lab/$name" "$shared/$name"
    done
    # The authored harness stands in for an agent CLI installed for all users.
    # Its argument, MCP configuration and stdin logging are removed.
    python3 - "$lab_home/lab/fake_claude.py" /usr/local/bin/claude <<'PY'
import sys
from pathlib import Path
source = Path(sys.argv[1]).read_text()
source = source.replace('Path("mcp.json").write_text(config)', 'pass')
source = source.replace('Path("argv.json").write_text(json.dumps(sys.argv[1:]))', 'pass')
source = source.replace('    with open("stdin.jsonl", "a") as log:\n        log.write(line)\n', '')
# Real Claude Code saves each conversation where --resume finds it, and a
# limited chat carries that file out of its sandbox from one turn to the next.
marker = '        Path("fixture.pid").write_text(str(os.getpid()))\n'
assert source.count(marker) == 1, "fake_claude.py changed; move the saved conversation"
source = source.replace(marker, '''        saved = Path(os.environ.get("CLAUDE_CONFIG_DIR") or Path.home() / ".claude") / "projects" / "".join(c if c.isascii() and c.isalnum() else "-" for c in os.getcwd()) / "native-session.jsonl"
        saved.parent.mkdir(parents=True, exist_ok=True)
        with open(saved, "a") as log:
            log.write(json.dumps(value) + "\\n")
''' + marker)
Path(sys.argv[2]).write_text(source)
PY
    chmod 755 /usr/local/bin/claude
    cat > /etc/xdg/autostart/desktop-lab-person.desktop <<EOF
[Desktop Entry]
Type=Application
Name=Desktop qualification witness
Exec=/usr/bin/bash $shared/person-ready.sh
X-GNOME-Autostart-enabled=true
EOF
    case $target in
      gnome)
        dpkg-query -W -f='${Status}' gdm3 2> /dev/null | grep -q 'install ok installed' ||
          DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends gdm3
        systemctl disable desktop-lab.service 2> /dev/null || true
        cat > /etc/gdm3/custom.conf <<EOF
[daemon]
WaylandEnable=true
AutomaticLoginEnable=true
AutomaticLogin=$person
[security]
[xdmcp]
[chooser]
[debug]
EOF
        install -d /var/lib/AccountsService/users
        printf '[User]\nSession=gnome\nSystemAccount=false\n' > "/var/lib/AccountsService/users/$person"
        systemctl enable gdm3
        systemctl set-default graphical.target
        ;;
      hyprland | plasma)
        if [[ $target == hyprland ]]; then
          session=Hyprland
          environment='AQ_NO_KMS_REQUIREMENT=1 LIBGL_ALWAYS_SOFTWARE=1'
          install -d -o "$person" -g "$person" "$home/.config" "$home/.config/hypr"
          cat > "$home/.config/hypr/hyprland.conf" <<EOF
monitor = ,1280x720@60,auto,1
exec-once = bash $shared/person-ready.sh
misc {
    disable_hyprland_logo = true
    disable_splash_rendering = true
}
animations {
    enabled = false
}
EOF
          chown "$person:" "$home/.config/hypr/hyprland.conf"
        else
          session='/usr/lib/plasma-dbus-run-session-if-needed /usr/bin/startplasma-wayland'
          environment='LIBGL_ALWAYS_SOFTWARE=1 QT_LINUX_ACCESSIBILITY_ALWAYS_ON=1'
        fi
        # The same tty login the lab user had, now a login shell starting the
        # desktop the way a person types it after logging in.
        cat > /etc/systemd/system/desktop-lab.service <<EOF
[Unit]
Description=Disposable desktop qualification session
After=systemd-user-sessions.service
Conflicts=getty@tty1.service
[Service]
User=$person
PAMName=login
TTYPath=/dev/tty1
StandardInput=tty
StandardOutput=journal
StandardError=journal
Environment=$environment
ExecStart=/usr/bin/setpriv --inh-caps=-all --ambient-caps=-all /usr/bin/bash -l -c 'exec $session'
Restart=no
[Install]
WantedBy=multi-user.target
EOF
        systemctl daemon-reload
        systemctl enable desktop-lab.service
        ;;
      *) exit 2 ;;
    esac
    echo "$person set up for $target; reboot into restricted networking to log in."
    ;;
  receive)
    [[ ${2:-} =~ ^[a-z0-9-]+-[0-9]+\.[0-9]+\.[0-9]+-x86_64-linux\.tar\.gz$ ]]
    home=$(getent passwd "$person" | cut -d: -f6)
    install -d -o "$person" -g "$person" -m 755 "$home/Downloads"
    install -o "$person" -g "$person" -m 644 "$lab_home/lab/incoming.tar.gz" "$home/Downloads/$2"
    rm "$lab_home/lab/incoming.tar.gz"
    ;;
  login)
    # A login outside the desktop session, as over SSH: the user's service
    # manager runs, as it does for any login, but no desktop endpoints are set.
    shift
    uid=$(id -u "$person")
    home=$(getent passwd "$person" | cut -d: -f6)
    systemctl start "user@$uid.service"
    exec setpriv --reuid="$person" --regid="$person" --init-groups --inh-caps=-all \
      env -i HOME="$home" USER="$person" LOGNAME="$person" SHELL=/bin/bash LANG=C.UTF-8 \
      XDG_RUNTIME_DIR="/run/user/$uid" DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/$uid/bus" \
      env -C "$home" bash -lc "$(printf '%q ' "$@")"
    ;;
  run)
    shift
    home=$(getent passwd "$person" | cut -d: -f6)
    saved=/run/user/$(id -u "$person")/desktop-lab/session.env
    [[ -f $saved && $(stat -c %U "$saved") == "$person" ]]
    variables=()
    while IFS= read -r -d '' entry; do
      [[ $entry =~ ^(PATH|WAYLAND_DISPLAY|DISPLAY|DBUS_SESSION_BUS_ADDRESS|XDG_[A-Z_]+|DESKTOP_SESSION|HYPRLAND_INSTANCE_SIGNATURE)= ]]
      variables+=("$entry")
    done < "$saved"
    exec setpriv --reuid="$person" --regid="$person" --init-groups --inh-caps=-all \
      env -i HOME="$home" USER="$person" LOGNAME="$person" SHELL=/bin/bash LANG=C.UTF-8 \
      "${variables[@]}" env -C "$home" "$@"
    ;;
  *) echo 'Expected setup, receive, login or run' >&2; exit 2 ;;
esac
