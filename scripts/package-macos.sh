#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Builds, signs and packs the Mac archive. Runs on an Apple silicon Mac with
# Xcode's command line tools. Signs ad hoc unless OPHIO_MACOS_SIGNING_IDENTITY
# names a Developer ID Application certificate, and notarizes only when
# OPHIO_MACOS_NOTARY_PROFILE is also set (see scripts/sign_macos.py). Neither
# is needed for a personal build.
#
# The phone app is checked with Android's tools, which a Mac usually lacks, so
# leave OPHIO_PHONE_APK unset here and add the app afterwards on a system that
# has them: copy target/packaging/macos there and run
# `scripts/package-artifacts.py macos` with OPHIO_PHONE_APK set.
set -euo pipefail
# shellcheck source=scripts/package-common.sh
source "$(dirname -- "${BASH_SOURCE[0]}")/package-common.sh"
if [[ "$(uname -s)" != Darwin || "$(uname -m)" != arm64 ]]; then
    echo "The Mac archive is built on an Apple silicon Mac." >&2
    exit 1
fi
python3 scripts/build-host.py macos
python3 scripts/sign_macos.py sign
if [[ -n "${OPHIO_MACOS_NOTARY_PROFILE:-}" ]]; then
    python3 scripts/sign_macos.py notarize
fi
python3 scripts/licenses.py --artifact macos --check --output "$CARGO_TARGET_DIR/packaging/licenses"
python3 scripts/package-artifacts.py macos
