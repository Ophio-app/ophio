#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
set -euo pipefail
# shellcheck source=scripts/package-common.sh
source "$(dirname -- "${BASH_SOURCE[0]}")/package-common.sh"
python3 scripts/build-host.py linux
python3 scripts/licenses.py --artifact linux --check --output "$CARGO_TARGET_DIR/packaging/licenses"
python3 scripts/package-artifacts.py linux
