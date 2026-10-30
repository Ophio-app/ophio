#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
set -euo pipefail
# Source the build-scoped SDK/NDK/JDK environment first, or set BUILD_ENV_FILE.
# shellcheck source=scripts/package-common.sh
source "$(dirname -- "${BASH_SOURCE[0]}")/package-common.sh"
: "${ANDROID_HOME:?Set ANDROID_HOME in the build environment}"
: "${NDK_HOME:?Set NDK_HOME in the build environment}"
: "${JAVA_HOME:?Set JAVA_HOME in the build environment}"
exec python3 scripts/package_android.py
