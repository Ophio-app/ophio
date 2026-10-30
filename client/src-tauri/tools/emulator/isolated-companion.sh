#!/bin/bash
# SPDX-License-Identifier: Apache-2.0
# Runs only this checkout's companion, with authored harnesses and isolated state.
set -euo pipefail
lane_dir=$(cd "$(dirname "$0")/../../../.." && pwd)
fixture_dir="$lane_dir/target/phone-e2e"
evidence_dir=${OPHIO_EVIDENCE:-$lane_dir/target/e2e-evidence}
if [[ ${1:-} == setup ]]; then
  umask 077
  mkdir -p "$fixture_dir"/{bin,home/Downloads,config/ophio,data,state,cache,runtime,project,approvals,stream,notes}
  cp "$lane_dir/crates/harness-claude/tests/fake_claude.py" "$fixture_dir/bin/claude"
  cp "$lane_dir/crates/harness-codex/tests/fake_codex.py" "$fixture_dir/bin/codex"
  # Fixtures must not retain native bridge tokens or prompt logs.
  python3 - "$fixture_dir/bin/claude" "$fixture_dir/bin/codex" <<'PY'
import sys
from pathlib import Path
for file in sys.argv[1:]:
    p = Path(file)
    s = p.read_text().replace('Path("mcp.json").write_text(config)', 'pass')
    s = s.replace('Path("argv.json").write_text(json.dumps(sys.argv[1:]))', 'pass')
    s = s.replace('    with open("stdin.jsonl", "a") as log:\n        log.write(line)\n', '')
    p.write_text(s)
PY
  printf '#!/bin/sh\nexit 1\n' > "$fixture_dir/bin/tailscale"
  chmod 700 "$fixture_dir/bin/"*
  printf 'port = %s\nlisten = ["127.0.0.1"]\nallow_lan = false\ndesktop_enabled = false\n' "${OPHIO_PORT:-47112}" > "$fixture_dir/config/ophio/config.toml"
  printf '# Phone fixture\n\nOriginal text.\n' > "$fixture_dir/notes/guide.md"
  exit
fi
for executable in claude codex tailscale; do
  [[ -x "$fixture_dir/bin/$executable" ]] || { echo "Run fixture setup before starting the isolated environment." >&2; exit 1; }
done
if [[ ${1:-} == start ]]; then
  # Detach the fixture from a short-lived tool/shell session. Keep its exact PID
  # for connection-loss injection and cleanup; never search for another server.
  exec python3 - "$0" "$lane_dir" "$fixture_dir" "$evidence_dir" <<'PY'
import os, subprocess, sys
from pathlib import Path
wrapper, lane, fixture, evidence = map(Path, sys.argv[1:])
os.umask(0o077)
evidence.mkdir(parents=True, exist_ok=True)
with (evidence / "companion.log").open("ab") as log:
    child = subprocess.Popen([str(wrapper.resolve()), str(lane / "target/debug/ophio"), "run"],
        stdin=subprocess.DEVNULL, stdout=log, stderr=log, start_new_session=True)
(fixture / "companion.pid").write_text(str(child.pid))
PY
fi
if [[ $# == 0 ]]; then set -- "$lane_dir/target/debug/ophio"; fi
exec env -i \
  HOME="$fixture_dir/home" XDG_CONFIG_HOME="$fixture_dir/config" \
  XDG_DATA_HOME="$fixture_dir/data" XDG_STATE_HOME="$fixture_dir/state" \
  XDG_CACHE_HOME="$fixture_dir/cache" XDG_RUNTIME_DIR="$fixture_dir/runtime" \
  PATH="$fixture_dir/bin:/usr/bin:/bin" SHELL=/bin/sh LANG=C.UTF-8 \
  "$@"
