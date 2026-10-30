#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# Runs Vitest with a temporary folder of its own and removes it afterwards.
# Vitest keeps a few megabytes of transformed modules under the temporary
# directory and leaves them behind when it is killed or its shutdown outlasts
# teardownTimeout, which fills a RAM-backed /tmp over many runs. A run killed
# outright cannot clean up, so each run also clears folders whose run is gone.
# Arguments go to `vitest run`, e.g. `npm test -- src/screens/talk`.
set -u
cd "$(dirname "$0")/.." || exit 1
base=${TMPDIR:-/tmp}
for old in "$base"/companion-vitest.*; do
    [ -f "$old/pid" ] || continue
    kill -0 "$(cat "$old/pid")" 2>/dev/null || rm -rf "$old"
done
dir=$(mktemp -d "$base/companion-vitest.XXXXXX") || exit 1
echo $$ >"$dir/pid"
trap 'rm -rf "$dir"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
status=0
TMPDIR=$dir node_modules/.bin/vitest run "$@" || status=$?
exit "$status"
