#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# See docs/operations/self-hosted-runner.md.
# Builds the Ubuntu 24.04 root filesystem the Ophio runner's jobs see.
# Run as: unshare --map-auto --map-root-user --mount sh provision-rootfs.sh ROOTFS TARBALL
set -eu
root=$1 tarball=$2
if [ ! -e "$root/etc/os-release" ]; then
  mkdir -p "$root"
  tar -xzf "$tarball" -C "$root" --numeric-owner
fi
cat /etc/resolv.conf > "$root/etc/resolv.conf"
mount --rbind /dev "$root/dev"
mount --rbind /proc "$root/proc"
mount --rbind /sys "$root/sys"
chroot "$root" /bin/sh -eu <<'IN'
export DEBIAN_FRONTEND=noninteractive LC_ALL=C
echo 'APT::Sandbox::User "root";' > /etc/apt/apt.conf.d/99sandbox
apt-get update
# Ophio's own container list (.github/workflows/checks.yml), the runner's
# .NET dependencies, and Playwright's Ubuntu 24.04 Chromium dependencies.
apt-get install -y --no-install-recommends \
  ca-certificates curl git build-essential pkg-config python3 xz-utils \
  clang libclang-dev libssl-dev libxkbcommon-dev libwayland-dev \
  libpipewire-0.3-dev libspa-0.2-dev libwebkit2gtk-4.1-dev \
  libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev patchelf \
  libicu74 libkrb5-3 zlib1g liblttng-ust1t64 iproute2 sudo- \
  xvfb fonts-noto-color-emoji fonts-unifont libfontconfig1 libfreetype6 xfonts-cyrillic xfonts-scalable fonts-liberation fonts-ipafont-gothic fonts-wqy-zenhei fonts-tlwg-loma-otf fonts-freefont-ttf libasound2t64 libatk-bridge2.0-0t64 libatk1.0-0t64 libatspi2.0-0t64 libcairo2 libcups2t64 libdbus-1-3 libdrm2 libgbm1 libglib2.0-0t64 libnspr4 libnss3 libpango-1.0-0 libx11-6 libxcb1 libxcomposite1 libxdamage1 libxext6 libxfixes3 libxkbcommon0 libxrandr2
apt-get clean
rm -rf /var/lib/apt/lists/*
# The runner runs as uid 1000 inside the sandbox. /ci-shared is where the
# runners share their record of passed trees.
grep -q '^runner:' /etc/passwd || echo 'runner:x:1000:1000:Actions runner:/runner/home:/bin/bash' >> /etc/passwd
grep -q '^runner:' /etc/group || echo 'runner:x:1000:' >> /etc/group
mkdir -p /runner /ci-shared
# Tests look for a browser at /usr/bin/chromium. Each job writes the locked
# Playwright Chromium's path to the runner's home; this hands over to it.
cat > /usr/bin/chromium <<'SH'
#!/bin/sh
target=$(cat "$(getent passwd runner | cut -d: -f6)/.ophio-ci-chromium" 2>/dev/null) || {
  echo 'chromium: no Playwright Chromium recorded for this job' >&2; exit 127; }
exec "$target" --no-sandbox "$@"
SH
chmod 755 /usr/bin/chromium
IN
