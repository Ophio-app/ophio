#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Build on the oldest target distribution to use its libc ABI.
set -euo pipefail
unset DISPLAY WAYLAND_DISPLAY DBUS_SESSION_BUS_ADDRESS
[[ $(cat /etc/companion-lab 2>/dev/null) == disposable-desktop-lab && $EUID != 0 ]]
[[ $(systemd-detect-virt) == kvm || $(systemd-detect-virt) == qemu ]]
bash "$HOME/lab/toolchain.sh"
export CARGO_HOME="$HOME/lab/cargo" RUSTUP_HOME="$HOME/lab/rustup"
export CARGO_TARGET_DIR="$HOME/lab/build-target" CARGO_BUILD_JOBS=8
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0
cd "$HOME/lab"
export PATH="$CARGO_HOME/bin:/usr/bin:/bin"
mkdir -p source
tar -xf source.tar -C source
cd source
rustc --version
cargo build --locked -p companion -p computer-mcp
install -m 700 "$CARGO_TARGET_DIR/debug/ophio" "$HOME/bin/ophio"
install -m 700 "$CARGO_TARGET_DIR/debug/computer-mcp" "$HOME/bin/computer-mcp"
"$HOME/bin/ophio" --version
