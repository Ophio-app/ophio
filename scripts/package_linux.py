# SPDX-License-Identifier: Apache-2.0
"""Reject ELF executables that exceed the declared Linux libc baseline."""
from pathlib import Path
import re
import subprocess

from release_common import ReleaseError

GLIBC_BASELINE = (2, 39)


def required_glibc(readelf: str) -> tuple[int, ...]:
    # Only version requirements are relevant; definitions are not dependencies.
    needs = readelf.partition("Version needs section")[2]
    if not needs:
        raise ReleaseError("ELF has no inspectable version needs section")
    versions = [tuple(map(int, v.split("."))) for v in re.findall(r"Name: GLIBC_([0-9]+(?:\.[0-9]+)+)\b", needs)]
    if not versions:
        raise ReleaseError("ELF does not declare an inspectable glibc requirement")
    return max(versions)


def check_glibc(binary: Path) -> str:
    result = subprocess.run(["readelf", "--version-info", "--wide", str(binary)],
                            capture_output=True, text=True, encoding="utf-8",
                            check=True, timeout=30,
                            env={"PATH": "/usr/bin:/bin", "LC_ALL": "C"})
    version = required_glibc(result.stdout)
    label = ".".join(map(str, version))
    if version > GLIBC_BASELINE:
        baseline = ".".join(map(str, GLIBC_BASELINE))
        raise ReleaseError(f"{binary.name} requires GLIBC_{label}; Linux baseline is GLIBC_{baseline}. Build on Ubuntu 24.04 or an equivalent older sysroot.")
    return label
