#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Sourced by packaging entry points. Does not change any persistent configuration.
set -euo pipefail
repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_dir"
if [[ -n "${BUILD_ENV_FILE:-}" ]]; then
    # shellcheck source=/dev/null
    source "$BUILD_ENV_FILE"
fi
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$repo_dir/target}"
export CARGO_BUILD_JOBS=8
export CARGO_NET_OFFLINE=true
export npm_config_offline=true
export SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct)}"
mkdir -p "$CARGO_TARGET_DIR/packaging"
CARGO_TARGET_DIR=$(cd -- "$CARGO_TARGET_DIR" && pwd)
export CARGO_TARGET_DIR
# Keep all display, input and session-bus access out of build/test subprocesses.
unset DISPLAY WAYLAND_DISPLAY DBUS_SESSION_BUS_ADDRESS
python3 "$repo_dir/scripts/package-artifacts.py" identity
