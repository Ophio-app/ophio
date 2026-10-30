#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
set -euo pipefail

# Source the build-scoped toolchain first, or set BUILD_ENV_FILE.
# shellcheck source=scripts/package-common.sh
source "$(dirname -- "${BASH_SOURCE[0]}")/package-common.sh"
: "${CARGO_TARGET_X86_64_PC_WINDOWS_GNULLVM_LINKER:?Configure the LLVM/MinGW linker in the build environment}"

python3 scripts/build-host.py windows
python3 scripts/licenses.py --artifact windows --check --output "$CARGO_TARGET_DIR/packaging/licenses"
python3 "$repo_dir/scripts/package-windows.py" \
    "$repo_dir" "$CARGO_TARGET_DIR" "$SOURCE_DATE_EPOCH"
