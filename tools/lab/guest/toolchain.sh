#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Build prerequisites on the oldest target distribution: native development
# packages and the pinned Rust toolchain. Needs the provisioning network.
set -euo pipefail
unset DISPLAY WAYLAND_DISPLAY DBUS_SESSION_BUS_ADDRESS
[[ $(cat /etc/companion-lab 2>/dev/null) == disposable-desktop-lab && $EUID != 0 ]]
[[ $(systemd-detect-virt) == kvm || $(systemd-detect-virt) == qemu ]]
sudo apt-get update
sudo env DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
  build-essential pkg-config libpipewire-0.3-dev libclang-dev libxkbcommon-dev \
  ca-certificates curl python3 openjdk-21-jre-headless
export CARGO_HOME="$HOME/lab/cargo" RUSTUP_HOME="$HOME/lab/rustup"
cd "$HOME/lab"
url=https://static.rust-lang.org/rustup/dist/x86_64-unknown-linux-gnu/rustup-init
curl --fail --location --proto '=https' --proto-redir '=https' --tlsv1.2 "$url" -o rustup-init
curl --fail --location --proto '=https' --proto-redir '=https' --tlsv1.2 "$url.sha256" -o rustup-init.sha256
sha256sum --check rustup-init.sha256
chmod 700 rustup-init
./rustup-init -y --no-modify-path --profile minimal --default-toolchain 1.98.0
