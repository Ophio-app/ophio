#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Run the repository's own Linux packaging, offline, on the Ubuntu 24.04 baseline.
# The arguments are the commit's timestamp and hash; the copied source has no
# Git history, so the hash is how the executables learn which commit they are.
# A third argument of "public" makes the public archive: it must carry the
# signed phone app, which the packaging checks against the release key.
set -euo pipefail
unset DISPLAY WAYLAND_DISPLAY DBUS_SESSION_BUS_ADDRESS
unset OPHIO_PERSONAL_SIGNING OPHIO_PUBLIC_RELEASE OPHIO_PHONE_APK OPHIO_ANDROID_BUILD_TOOLS
# Shared packaging can source BUILD_ENV_FILE after this script's exports.
# The guest must use its explicit inputs, not an inherited build profile.
unset OPHIO_SOURCE_COMMIT BUILD_ENV_FILE
[[ $(cat /etc/companion-lab 2>/dev/null) == disposable-desktop-lab && $EUID != 0 ]]
[[ $(systemd-detect-virt) == kvm || $(systemd-detect-virt) == qemu ]]
. /etc/os-release
[[ $ID == ubuntu && $VERSION_ID == 24.04 && ${1:-} =~ ^[0-9]+$ ]]
[[ ${2:-} =~ ^[0-9a-f]{40}([0-9a-f]{24})?$ ]]
[[ $# == 2 || ($# == 3 && $3 == public) ]]
export CARGO_HOME="$HOME/lab/cargo" RUSTUP_HOME="$HOME/lab/rustup"
export PATH="$CARGO_HOME/bin:/usr/bin:/bin" SOURCE_DATE_EPOCH=$1 OPHIO_SOURCE_COMMIT=$2
if [[ ${3:-} == public ]]; then
  export OPHIO_PUBLIC_RELEASE=1 OPHIO_PHONE_APK="$HOME/lab/package-phone.apk"
  export OPHIO_ANDROID_BUILD_TOOLS="$HOME/lab/android-build-tools"
  [[ -f $OPHIO_PHONE_APK && -x $OPHIO_ANDROID_BUILD_TOOLS/apksigner && -x $OPHIO_ANDROID_BUILD_TOOLS/zipalign ]]
  [[ -f $OPHIO_ANDROID_BUILD_TOOLS/lib/apksigner.jar && -f $OPHIO_ANDROID_BUILD_TOOLS/lib64/libc++.so ]]
  java -version 2>&1 | sed -n '1p'
fi
work=$HOME/lab/package
rm -rf "$work" "$CARGO_HOME/vendor"
mkdir -p "$work"
tar -xf "$HOME/lab/package-source.tar" -C "$work"
# Vendored crates live in Cargo's home, whose path the release build remaps
# like the registry's, so no guest path reaches the executables.
tar -xf "$HOME/lab/package-vendor.tar" -C "$CARGO_HOME"
cat > "$CARGO_HOME/config.toml" <<CONFIG
[source.crates-io]
replace-with = "vendored-sources"
[source.vendored-sources]
directory = "$CARGO_HOME/vendor"
CONFIG
cd "$work"
rustc --version
ldd --version | sed -n '1p'
bash scripts/package-linux.sh
cd dist/linux
sha256sum -- *-x86_64-linux.tar.gz
