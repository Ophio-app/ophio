#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
set -euo pipefail
unset DISPLAY WAYLAND_DISPLAY DBUS_SESSION_BUS_ADDRESS
[[ $(cat /etc/companion-lab 2>/dev/null) == disposable-desktop-lab && $EUID != 0 ]]
umask 077
mkdir -p "$HOME/.config/ophio" "$HOME/project" "$HOME/evidence"
python3 - <<'PY'
from pathlib import Path
home = Path.home()
for name in ('claude', 'codex'):
    source = (home / 'lab' / ('fake_' + name + '.py')).read_text()
    source = source.replace('Path("mcp.json").write_text(config)', 'pass')
    source = source.replace('Path("argv.json").write_text(json.dumps(sys.argv[1:]))', 'pass')
    source = source.replace('    with open("stdin.jsonl", "a") as log:\n        log.write(line)\n', '')
    (home / 'bin' / name).write_text(source)
PY
printf '#!/bin/sh\nexit 1\n' > "$HOME/bin/tailscale"
chmod 700 "$HOME/bin/"{ophio,computer-mcp,claude,codex,tailscale}
if [[ ! -e $HOME/.config/ophio/config.toml ]]; then
  cat > "$HOME/.config/ophio/config.toml" <<'EOF'
port = 47112
listen = ["0.0.0.0"]
allow_lan = true
desktop_enabled = false
EOF
fi
"$HOME/bin/ophio" --version

