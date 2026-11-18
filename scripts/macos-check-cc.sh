#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# CHECK/CLIPPY ONLY. Use as CC_aarch64_apple_darwin, paired with llvm-ar.
# Cargo check/clippy type-check target Rust without linking its native objects;
# host build scripts and procedural macros still use their real compilers.
# Target build scripts need placeholder objects, so compile requests produce
# genuine empty arm64 Mach-O objects without Apple headers or an SDK. This verifies
# neither native Rust linking nor FFI correctness. Forbidden for builds/tests;
# this wrapper does NOT establish that a real Mac build is safe or correct.
set -euo pipefail

refuse() {
    printf 'macos-check-cc: CHECK/CLIPPY ONLY: %s\n' "$*" >&2
    exit 2
}

# Only standalone informational invocations are version probes. In particular,
# `clang -v input.c -o program` is a link, not a version probe.
if (( $# == 1 )); then
    case "$1" in
        --version|-v|-V|-dumpversion|-dumpmachine)
            exec clang "$@"
            ;;
    esac
fi

original=("$@")
compile=false
preprocess=false
output=''
input=false
end_options=false
while (( $# )); do
    if "$end_options"; then
        input=true
        shift
        continue
    fi
    case "$1" in
        --) end_options=true ;;
        -c) compile=true ;;
        -E) preprocess=true ;;
        -o)
            (( $# >= 2 )) || refuse 'missing value after -o'
            [[ -n "$2" && "$2" != -* ]] || refuse 'invalid output path after -o'
            [[ -z "$output" ]] || refuse 'multiple output paths'
            output=$2
            shift
            ;;
        -o?*)
            [[ -z "$output" ]] || refuse 'multiple output paths'
            output=${1#-o}
            ;;
        -I|-isystem|-iquote|-idirafter|-include|-imacros|-x|-target|--target|-arch|-isysroot|--sysroot|-D|-U|-MF|-MT|-MQ|-Xclang|-Xpreprocessor)
            (( $# >= 2 )) || refuse "missing value after $1"
            [[ -n "$2" && "$2" != -* ]] || refuse "invalid value after $1"
            shift
            ;;
        @*) refuse 'response files are unsupported; pass arguments explicitly' ;;
        -S|-fsyntax-only|--version|-V|-dumpversion|-dumpmachine)
            refuse 'unsupported or mixed compiler action'
            ;;
        -) input=true ;;
        -*) ;; # Compile flags are intentionally discarded for placeholder objects.
        *) input=true ;;
    esac
    shift
done

if "$preprocess"; then
    ! "$compile" || refuse 'mixed preprocessing and compilation actions'
    "$input" || refuse 'preprocessing requires an input'
    exec clang "${original[@]}"
fi

"$compile" || refuse 'linking is forbidden (expected -c, -E, or a standalone version probe)'
[[ -n "$output" ]] || refuse 'compilation requires an explicit -o output path'
"$input" || refuse 'compilation requires an input'
exec clang --target=arm64-apple-macosx -x c -c /dev/null -o "$output"
