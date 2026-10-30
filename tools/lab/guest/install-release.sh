#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Install only the assembled archive into a disposable guest user's bin directory.
set -euo pipefail
unset DISPLAY WAYLAND_DISPLAY DBUS_SESSION_BUS_ADDRESS
[[ $(cat /etc/companion-lab 2>/dev/null) == disposable-desktop-lab && $EUID != 0 ]]
[[ $(systemd-detect-virt) == kvm || $(systemd-detect-virt) == qemu ]]
archive=$HOME/lab/release.tar.gz
[[ -f $archive ]]
mkdir -p "$HOME/lab/release-install" "$HOME/bin"
tar -xzf "$archive" -C "$HOME/lab/release-install"
# The lab has exactly one companion, launched by the guest bootstrap.
pkill -x ophio || [[ $? == 1 ]]
for attempt in $(seq 1 50); do
  pgrep -x ophio > /dev/null || break
  sleep 0.1
done
if pgrep -x ophio > /dev/null; then exit 1; fi
for binary in ophio computer-mcp; do
  install -m 700 "$HOME/lab/release-install/ophio-0.1.0-x86_64-linux/$binary" "$HOME/bin/$binary"
done
if ! grep -q 'export XDG_DATA_HOME=' "$HOME/lab/session.env"; then
  printf '\nexport XDG_DATA_HOME="$HOME/lab/release-data"\n' >> "$HOME/lab/session.env"
fi
"$HOME/bin/ophio" --version
"$HOME/bin/ophio" --help > /dev/null
ldd "$HOME/bin/ophio"
# Delegated as the installed user service is; see ready.sh.
nohup systemd-run --user --scope --quiet -p Delegate=yes \
  bash "$HOME/lab/in-session.sh" ophio run > "$HOME/evidence/release-run.log" 2>&1 < /dev/null &

# The local CLI must find the running transport, not contend for its state lock.
for attempt in $(seq 1 300); do
  if bash "$HOME/lab/in-session.sh" ophio status > /dev/null 2>&1; then
    exit 0
  fi
  sleep 0.1
done
printf '%s\n' 'The installed companion did not become ready' >&2
exit 1
