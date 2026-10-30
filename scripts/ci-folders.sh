#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Print the folders a CI job builds in, as lines for $GITHUB_ENV.
#
#   bash scripts/ci-folders.sh github-hosted|self-hosted >> "$GITHUB_ENV"
#
# GitHub's runners start empty, so they build inside the checkout and
# actions/cache restores it. The self-hosted runner keeps its home between jobs
# (docs/operations/self-hosted-runner.md) while checkout empties the workspace,
# so it builds in its home and restores nothing. Its build folder is emptied
# once it grows past CI_TARGET_CAP_GIB, so old dependency builds don't pile up.
set -euo pipefail

case "${1:?usage: ci-folders.sh github-hosted|self-hosted}" in
    self-hosted)
        tools="$HOME/ci-cache/tools"
        target="$HOME/ci-cache/target"
        cap="${CI_TARGET_CAP_GIB:-100}"
        if [[ -d "$target" ]]; then
            size=$(du -s --block-size=1G -- "$target" | cut -f1)
            if ((size > cap)); then
                printf 'Emptying the %s GiB build folder, over its %s GiB cap.\n' "$size" "$cap" >&2
                rm -rf -- "$target"
            fi
        fi
        ;;
    github-hosted)
        tools="$GITHUB_WORKSPACE/.ci-tools"
        target="$GITHUB_WORKSPACE/target"
        ;;
    *)
        printf 'Unknown runner environment: %s\n' "$1" >&2
        exit 2
        ;;
esac
mingw="$tools/llvm-mingw/bin"
cat <<EOF
CARGO_TARGET_DIR=$target
CARGO_HOME=$tools/cargo
RUSTUP_HOME=$tools/rustup
BUILD_TOOLS_HOME=$tools
CARGO_TARGET_X86_64_PC_WINDOWS_GNULLVM_LINKER=$mingw/x86_64-w64-mingw32-clang
CC_x86_64_pc_windows_gnullvm=$mingw/x86_64-w64-mingw32-clang
CXX_x86_64_pc_windows_gnullvm=$mingw/x86_64-w64-mingw32-clang++
AR_x86_64_pc_windows_gnullvm=$mingw/llvm-ar
EOF
