#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Check the MinGW-w64 runtime objects linked into the Windows executables.

The runtime's notice covers far more than a program links, including maths
whose licence it leaves unresolved. Only objects reviewed in
packaging/windows/runtime-objects.toml may ship, and the MinGW component's
declared licence must cover every one of them.
"""

from __future__ import annotations

from pathlib import Path, PurePosixPath
import re
import sys
import tomllib

from release_common import ROOT, ReleaseError

COMPONENT = "MinGW-w64 runtime"
# An input row of an lld map: "address size align path:(section)".
INPUT = re.compile(r"[0-9a-f]+\s+[0-9a-f]+\s+\d+\s+(.+):\([^()\s]*\)")
MEMBER = re.compile(r"lib64_lib\w+_a-.+\.o")
STARTUP = re.compile(r"crt\w*\.o")


def linked(map_text: str) -> set[str]:
    """Runtime objects a linker map names as inputs."""
    names = set()
    for line in map_text.splitlines():
        row = INPUT.fullmatch(line.strip())
        if not row:
            continue
        path = row[1].replace("\\", "/")
        name = PurePosixPath(path).name
        if MEMBER.fullmatch(name) or (
            STARTUP.fullmatch(name) and "-w64-mingw32/lib/" in path
        ):
            names.add(name)
    return names


def declared(root: Path) -> set[str]:
    components = tomllib.loads(
        (root / "packaging/components.toml").read_text(encoding="utf-8")
    )
    entry = next(
        (c for c in components["component"] if c["name"] == COMPONENT), None
    )
    if entry is None:
        raise ReleaseError(f"No {COMPONENT} component to check against")
    terms = [term.strip() for term in entry["license"].split(" AND ")]
    if any(not term or re.search(r"[()\s]", term) for term in terms):
        raise ReleaseError(f"{COMPONENT} licence must be a plain AND of licences")
    return set(terms)


def check(maps: list[Path], root: Path = ROOT) -> set[str]:
    table = tomllib.loads(
        (root / "packaging/windows/runtime-objects.toml").read_text(encoding="utf-8")
    )
    objects = table["objects"]
    for name, row in objects.items():
        if not row.get("source") or not row.get("licenses"):
            raise ReleaseError(f"Runtime object {name} needs a source and licences")
        if unknown := set(row["licenses"]) - table["licenses"].keys():
            raise ReleaseError(f"Runtime object {name} names undefined licences {sorted(unknown)}")
    names: set[str] = set()
    for path in maps:
        found = linked(path.read_text(encoding="utf-8", errors="replace"))
        # Every executable starts from the runtime's startup object; its
        # absence means the map was not read, not that nothing was linked.
        if "crt2.o" not in found:
            raise ReleaseError(f"No MinGW-w64 startup object in linker map {path.name}")
        names |= found
    if unreviewed := sorted(names - objects.keys()):
        raise ReleaseError(
            "Unreviewed MinGW-w64 runtime objects linked: "
            + ", ".join(unreviewed)
            + ". Review their sources, then list them in packaging/windows/runtime-objects.toml."
        )
    needed = {licence for name in names for licence in objects[name]["licenses"]}
    if missing := sorted(needed - declared(root)):
        raise ReleaseError(f"{COMPONENT} licence does not declare {', '.join(missing)}")
    return names


def main() -> int:
    if len(sys.argv) < 2:
        sys.exit("usage: windows_runtime.py LINKER_MAP...")
    names = check([Path(arg) for arg in sys.argv[1:]])
    print(f"Windows runtime: {len(names)} linked MinGW-w64 objects, all reviewed")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, KeyError, ReleaseError) as error:
        sys.exit(f"Windows runtime check failed: {error}")
