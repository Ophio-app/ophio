#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Offline release build in the existing, disposable Ubuntu 24.04 guest.
set -euo pipefail
unset DISPLAY WAYLAND_DISPLAY DBUS_SESSION_BUS_ADDRESS
[[ $(cat /etc/companion-lab 2>/dev/null) == disposable-desktop-lab && $EUID != 0 ]]
[[ $(systemd-detect-virt) == kvm || $(systemd-detect-virt) == qemu ]]
. /etc/os-release
[[ $ID == ubuntu && $VERSION_ID == 24.04 ]]
export CARGO_HOME="$HOME/lab/cargo" RUSTUP_HOME="$HOME/lab/rustup"
export PATH="$CARGO_HOME/bin:/usr/bin:/bin"
export CARGO_TARGET_DIR="$HOME/lab/release-target" CARGO_BUILD_JOBS=8 CARGO_INCREMENTAL=0
mkdir -p "$HOME/lab/release-source" "$HOME/lab/link-libraries"
# The guest already has the runtime library. This local linker name needs no package installation.
ln -sf /usr/lib/x86_64-linux-gnu/libxkbcommon.so.0 "$HOME/lab/link-libraries/libxkbcommon.so"
export LIBRARY_PATH="$HOME/lab/link-libraries"
cd "$HOME/lab/release-source"
python3 - <<'COPY'
from pathlib import Path
import shutil
import tarfile
import tempfile
with tempfile.TemporaryDirectory(dir=Path.home() / "lab") as scratch:
    with tarfile.open("../release-source.tar") as archive:
        archive.extractall(scratch, filter="data")
    for source in Path(scratch).rglob("*"):
        if source.is_file():
            destination = Path(source.relative_to(scratch))
            if not destination.exists() or destination.read_bytes() != source.read_bytes():
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(source, destination)
COPY
mkdir -p .cargo
cat > .cargo/config.toml <<'CONFIG'
[source.crates-io]
replace-with = "vendored-sources"
[source.vendored-sources]
directory = "../vendor"
CONFIG
export RUSTFLAGS="--remap-path-prefix=$HOME=/build -C link-arg=-Wl,--build-id=none"
rustc --version
ldd --version | sed -n '1p'
cargo build --offline --locked --release --target x86_64-unknown-linux-gnu -p companion -p computer-mcp
for binary in ophio computer-mcp; do
  if [[ $binary == ophio ]]; then "$CARGO_TARGET_DIR/x86_64-unknown-linux-gnu/release/$binary" --version; fi
  readelf --version-info "$CARGO_TARGET_DIR/x86_64-unknown-linux-gnu/release/$binary" | sed -n 's/.*Name: \(GLIBC_[0-9.]*\).*/\1/p' | sort -Vu | tail -1
done
