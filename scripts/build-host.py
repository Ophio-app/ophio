#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Build only the shipped host executables with private path remapping."""

import os
from pathlib import Path
import subprocess
import sys

from release_common import ROOT, TARGET_TRIPLES, ReleaseError, phone_app_bytes, release_environment
import windows_runtime

if len(sys.argv) != 2 or sys.argv[1] not in {"linux", "windows"}:
    sys.exit("usage: build-host.py linux|windows")

artifact = sys.argv[1]
cargo = ["--locked", "--offline", "--release", "--target", TARGET_TRIPLES[artifact]]
try:
    environment = release_environment(artifact)
    # The phone app is checked again when it is packed; checking it first means
    # a companion that would refuse it is never built.
    phone_app_bytes(ROOT)
except ReleaseError as error:
    sys.exit(f"Release check failed: {error}")

if artifact == "linux":
    result = subprocess.run(
        ["cargo", "build", *cargo, "-p", "companion", "-p", "computer-mcp", "-p", "sandbox"],
        cwd=ROOT,
        env=environment,
        check=False,
    )
    sys.exit(result.returncode)

# Each Windows executable is linked with a map, so the runtime objects it
# statically includes can be checked against the reviewed list.
maps = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target")) / "packaging/link-maps"
maps.mkdir(parents=True, exist_ok=True)
for package, binary in (("companion", "ophio"), ("computer-mcp", "computer-mcp")):
    result = subprocess.run(
        ["cargo", "rustc", *cargo, "-p", package, "--bin", binary, "--"]
        + [f"-Clink-arg=-Wl,--Map={maps / (binary + '.map')}"],
        cwd=ROOT,
        env=environment,
        check=False,
    )
    if result.returncode:
        sys.exit(result.returncode)
try:
    names = windows_runtime.check(
        [maps / "ophio.map", maps / "computer-mcp.map"]
    )
except (OSError, KeyError, ReleaseError) as error:
    sys.exit(f"Windows runtime check failed: {error}")
print(f"Windows runtime: {len(names)} linked MinGW-w64 objects, all reviewed")
