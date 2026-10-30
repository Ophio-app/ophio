#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Run from the workspace with its build toolchain already selected.
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.."
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$PWD/target}" CARGO_BUILD_JOBS=8
logs="$CARGO_TARGET_DIR/parallel-evidence/acceptance"
mkdir -p -- "$logs"
: > "$logs/results.tsv"
hogs=()
cleanup() {
    if ((${#hogs[@]})); then
        kill "${hogs[@]}" 2>/dev/null || true
        wait "${hogs[@]}" 2>/dev/null || true
        hogs=()
    fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
run_suite() {
    local label="$1" status=0
    env -u WAYLAND_DISPLAY -u DISPLAY -u DBUS_SESSION_BUS_ADDRESS SHELL=/bin/sh \
        cargo test --workspace > "$logs/$label.log" 2>&1 || status=$?
    python3 - "$label" "$status" "$logs" <<'PY'
import pathlib, re, sys
label, status, directory = sys.argv[1:]
root = pathlib.Path(directory)
counts = re.findall(r'test result: \w+\. (\d+) passed; (\d+) failed; (\d+) ignored;',
                    (root / f'{label}.log').read_text())
sums = [sum(int(row[i]) for row in counts) for i in range(3)]
line = '\t'.join([label, status, *map(str, sums)])
with (root / 'results.tsv').open('a') as output:
    output.write(line + '\n')
print(f'{label}: exit={status}, passed={sums[0]}, failed={sums[1]}, ignored={sums[2]}', flush=True)
PY
    if ((status)); then
        tail -80 "$logs/$label.log"
        return "$status"
    fi
}
for run in $(seq -w 1 "${PARALLEL_NORMAL_RUNS:-25}"); do run_suite "normal-$run"; done
for ((cpu=0; cpu<$(nproc); cpu++)); do
    yes > /dev/null &
    hogs+=("$!")
done
printf 'CPU load: %d yes processes\n' "${#hogs[@]}" | tee "$logs/load.txt"
for run in $(seq -w 1 "${PARALLEL_LOADED_RUNS:-10}"); do run_suite "loaded-$run"; done
