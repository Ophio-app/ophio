#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Local and hosted checks. Uses the caller's toolchain; never installs tools,
# changes user configuration, signs artifacts, publishes, or opens a desktop.
# Usage: bash scripts/ci.sh [STAGE ...] | --list
# CI_ALLOW_MISSING_TOOLS=1 permits explicit local prerequisite skips. Hosted CI
# always rejects skips. Logs and a stage summary stay under CARGO_TARGET_DIR/ci.
set -euo pipefail
repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_dir"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$repo_dir/target}"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-6}"
unset WAYLAND_DISPLAY DISPLAY DBUS_SESSION_BUS_ADDRESS
export SHELL=/bin/sh
mkdir -p "$CARGO_TARGET_DIR/ci"
CARGO_TARGET_DIR=$(cd -- "$CARGO_TARGET_DIR" && pwd)
export CARGO_TARGET_DIR
stages=(fmt clippy test parallel windows client intake-e2e tauri python source licenses advisories package-linux package-windows)

missing() {
    printf 'MISSING: %s\n' "$*"
    if [[ "${CI_ALLOW_MISSING_TOOLS:-0}" == 1 && "${CI:-false}" != true && -n "${skip_reason:-}" ]]; then
        printf 'SKIP: missing prerequisite; this is not a passing stage.\n'
        printf '%s\n' "$*" > "$skip_reason"
        exit 77
    fi
    return 1
}
require() { command -v "$1" > /dev/null || missing "Required tool: $1"; }
windows_tools() {
    local libdir linker
    libdir=$(rustc --print target-libdir --target x86_64-pc-windows-gnullvm)
    compgen -G "$libdir/libstd-*.rlib" > /dev/null || missing 'Rust target x86_64-pc-windows-gnullvm'
    linker="${CARGO_TARGET_X86_64_PC_WINDOWS_GNULLVM_LINKER:-x86_64-w64-mingw32-clang}"
    command -v "$linker" > /dev/null || missing 'LLVM/MinGW linker for x86_64-pc-windows-gnullvm'
}
fmt() { cargo fmt --all --check; }
clippy() { cargo clippy --locked --workspace --all-targets -- -D warnings; }
test_stage() { cargo test --locked --workspace; }
parallel() { bash crates/test-support/check-parallel.sh; }
windows() {
    windows_tools
    cargo check --locked -p companion -p computer-mcp --target x86_64-pc-windows-gnullvm
    cargo check --locked --target x86_64-pc-windows-gnullvm --tests -p harness-claude
    cargo clippy --locked -p harness-claude -p services -p companion -p library --all-targets --target x86_64-pc-windows-gnullvm -- -D warnings
}
client() {
    require node
    require npm
    [[ -d client/node_modules ]] || missing 'Locked client dependencies: run npm ci in client/'
    (
        cd client
        node scripts/protocol-barrel.mjs
        npx --no-install tsc --noEmit -p .
        npm test
        npm run build
        npm run smoke
    )
    # Protocol tests export bindings before this stage. Fail on stale tracked
    # bindings, and also catch new exported files absent from the commit.
    git diff --exit-code -- client/src/protocol/generated
    [[ -z "$(git ls-files --others --exclude-standard -- client/src/protocol/generated)" ]] || {
        printf 'Untracked generated protocol bindings; regenerate and commit them.\n'
        return 1
    }
}
intake_e2e() {
    require node
    require npm
    [[ -d client/node_modules ]] || missing 'Locked client dependencies: run npm ci in client/'
    node client/scripts/intake-e2e.mjs
}
tauri() {
    require pkg-config
    pkg-config --exists webkit2gtk-4.1 gtk+-3.0 || missing 'Tauri Linux development libraries (WebKitGTK 4.1 and GTK 3)'
    # The app embeds the built screens. The client stage builds them first
    # when both run; a plan with only Rust changes builds them here.
    [[ -d client/dist ]] || (cd client && npm run build)
    (
        cd client/src-tauri
        cargo fmt --check
        cargo clippy --locked --all-targets -- -D warnings
        cargo test --locked
    )
}
python() { python3 -m unittest discover -s scripts/tests -t .; }
source_stage() {
    bash scripts/source-candidate.sh
    python3 scripts/earlier_names.py
}
licenses() {
    python3 scripts/licenses.py --artifact linux --check
    python3 scripts/licenses.py --artifact windows --check
}
advisories() {
    # Needs the network: a failed advisory database or registry fetch fails the
    # stage. A missing checker fails it too; it is never skipped, even where
    # CI_ALLOW_MISSING_TOOLS=1 lets other stages skip. cargo-audit reads
    # ./.cargo/audit.toml, so each workspace is audited from its own directory
    # with its own recorded, justified ignores.
    local skip_reason=
    if [[ -n "${BUILD_TOOLS_HOME:-}" ]]; then PATH="$PATH:$BUILD_TOOLS_HOME/cargo-audit/bin"; fi
    require cargo-audit
    require npm
    audit_cargo
    (cd client/src-tauri && audit_cargo)
    (cd client && npm audit --omit=dev)
}
audit_cargo() {
    # cargo-audit can only turn its yanked check off as a whole, so yanked
    # crates are held to the justified list in ./.cargo/audit-yanked.txt.
    cargo audit --deny warnings --no-yanked
    cargo audit --json | python3 -c '
import json, sys
allowed = {tuple(l.split()[:2]) for l in open(".cargo/audit-yanked.txt") if l.strip() and not l.startswith("#")}
yanked = {(w["package"]["name"], w["package"]["version"]) for w in json.load(sys.stdin)["warnings"].get("yanked", [])}
for name, version in sorted(allowed - yanked):
    print(f"note: {name} {version} is no longer yanked or locked; drop it from .cargo/audit-yanked.txt")
for name, version in sorted(yanked - allowed):
    print(f"yanked: {name} {version}", file=sys.stderr)
sys.exit(1 if yanked - allowed else 0)'
}
package_linux() {
    # Validate the real unsigned host archive paths. No install or publish step.
    # Building a Linux release on newer libc needs an Ubuntu 24.04 sysroot;
    # never weaken the artifact's GLIBC baseline just to pass a local check.
    python3 scripts/package-artifacts.py identity
    require readelf
    local libc
    libc=$(getconf GNU_LIBC_VERSION)
    if ! python3 - "$libc" <<'PY'
import sys
version = tuple(map(int, sys.argv[1].split()[-1].split('.')))
sys.exit(version > (2, 39))
PY
    then
        missing "Linux packaging needs glibc <= 2.39 or an equivalent sysroot; found $libc"
    fi
    bash scripts/package-linux.sh
    printf 'Unsigned host packages assembled locally; nothing installed or uploaded.\n'
    printf 'Android signing/package checks require a separate device release environment.\n'
}

package_windows() {
    windows_tools
    bash scripts/package-windows.sh
    printf 'Unsigned Windows package assembled locally; nothing installed or uploaded.\n'
}

if [[ "${1:-}" == --list ]]; then
    printf '%s\n' "${stages[@]}"
    exit 0
fi
if (($#)); then
    for selected in "$@"; do
        found=0
        for stage in "${stages[@]}"; do
            if [[ "$selected" == "$stage" ]]; then found=1; fi
        done
        if ((found == 0)); then printf 'Unknown stage: %s\n' "$selected" >&2; exit 2; fi
    done
    stages=("$@")
fi
require cargo
require rustc
require python3
summary="$CARGO_TARGET_DIR/ci/results.tsv"
: > "$summary"
failed=0
for stage in "${stages[@]}"; do
    printf '\nCI stage: %s\n' "$stage"
    function_name="$stage"
    case "$stage" in test) function_name=test_stage ;; source) function_name=source_stage ;; intake-e2e) function_name=intake_e2e ;; package-linux) function_name=package_linux ;; package-windows) function_name=package_windows ;; esac
    skip_reason="$CARGO_TARGET_DIR/ci/$stage.skip"
    rm -f -- "$skip_reason"
    # Do not call the function as an if-condition: doing so suppresses errexit
    # inside every command of a stage and can conceal an earlier failed gate.
    set +e
    (set -e; "$function_name") 2>&1 | tee "$CARGO_TARGET_DIR/ci/$stage.log"
    codes=("${PIPESTATUS[@]}")
    set -e
    status=${codes[0]}
    if ((codes[1] != 0)); then status=${codes[1]}; fi
    case "$status" in
        0) label=PASS ;;
        77)
            if [[ -s "$skip_reason" && "${CI_ALLOW_MISSING_TOOLS:-0}" == 1 && "${CI:-false}" != true ]]; then
                label=SKIP
            else
                label=FAIL; failed=1
            fi
            ;;
        *) label=FAIL; failed=1 ;;
    esac
    printf '%s\t%s\t%s\n' "$stage" "$label" "$status" | tee -a "$summary"
done
printf '\nCI results: %s\n' "$summary"
cat "$summary"
exit "$failed"
