#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Deterministic local qualification archive; no account or network access."""

import hashlib
import os
from pathlib import Path
import sys
import tempfile
import time
import zipfile

from release_common import ReleaseError, identity, manifest, phone_app_bytes, require_executable_trust, require_licences


def assemble(repo: Path, target: Path, epoch: int) -> Path:
    require_licences(target / "packaging/licenses/windows", "windows", repo)
    _, slug, version = identity(repo)
    package = f"{slug}-{version}-x86_64-windows"
    release = target / "x86_64-pc-windows-gnullvm" / "release"
    files = {
        "ophio.exe": release / "ophio.exe",
        "computer-mcp.exe": release / "computer-mcp.exe",
        "LICENSE": repo / "LICENSE",
        "NOTICE": target / "packaging/licenses/windows/NOTICE",
        "THIRD-PARTY-NOTICES.txt": target
        / "packaging/licenses/windows/THIRD-PARTY-NOTICES.txt",
        "sbom.cdx.json": target / "packaging/licenses/windows/sbom.cdx.json",
        "license-report.json": target
        / "packaging/licenses/windows/license-report.json",
        "INSTALL.md": repo / "packaging/windows/INSTALL.md",
    }
    app = phone_app_bytes(repo)
    if app is not None:
        files["ophio.apk"] = app
    # ZIP's timestamp format spans 1980..2107 with two-second resolution.
    if not 315532800 <= epoch <= 4354819198:
        raise ValueError("SOURCE_DATE_EPOCH must be within the ZIP timestamp range")
    stamp = time.gmtime(epoch - epoch % 2)[:6]
    contents = {}
    for name, path in files.items():
        if isinstance(path, bytes):
            # The APK snapshot is exactly what passed verification.
            data = path
        else:
            if path.is_symlink() or not path.is_file():
                raise ValueError(f"missing or unsafe package input: {name}")
            data = path.read_bytes()
        if name.endswith(".exe"):
            # Reject arbitrary renamed files and the wrong target architecture.
            offset = int.from_bytes(data[60:64], "little")
            if (
                len(data) < 64
                or data[:2] != b"MZ"
                or data[offset : offset + 6] != b"PE\0\0\x64\x86"
            ):
                raise ValueError(f"expected a Windows x64 PE executable: {name}")
        if name == "ophio.exe":
            require_executable_trust(data, repo)
        contents[name] = data
    contents["SHA256SUMS"] = "".join(
        f"{hashlib.sha256(data).hexdigest()}  {name}\n"
        for name, data in sorted(contents.items())
    ).encode("ascii")
    output = repo / "dist/windows"
    output.mkdir(parents=True, exist_ok=True)
    destination = output / f"{package}.zip"
    # Stable file order, timestamps, permissions and compression settings. The
    # resulting bytes are reproducible for identical inputs/Python/zlib versions.
    with tempfile.NamedTemporaryFile(dir=output, suffix=".zip", delete=False) as temp:
        temporary = Path(temp.name)
    try:
        with zipfile.ZipFile(
            temporary, "w", compression=zipfile.ZIP_DEFLATED
        ) as archive:
            for name, data in sorted(contents.items()):
                info = zipfile.ZipInfo(f"{package}/{name}", date_time=stamp)
                info.create_system = 3
                info.external_attr = 0o100644 << 16
                archive.writestr(
                    info, data, compress_type=zipfile.ZIP_DEFLATED, compresslevel=9
                )
        os.replace(temporary, destination)
    finally:
        temporary.unlink(missing_ok=True)
    for name, data in sorted(contents.items()):
        print(f"{name}: {len(data)} bytes")
    print(f"Archive: {destination} ({destination.stat().st_size} bytes)")
    print(f"SHA-256: {hashlib.sha256(destination.read_bytes()).hexdigest()}")
    print(manifest(repo), end="")
    return destination


if __name__ == "__main__":
    if len(sys.argv) != 4:
        sys.exit(
            "usage: package-windows.py REPOSITORY TARGET_DIRECTORY SOURCE_DATE_EPOCH"
        )
    try:
        assemble(Path(sys.argv[1]), Path(sys.argv[2]), int(sys.argv[3]))
    except (ReleaseError, OSError, ValueError, KeyError, zipfile.BadZipFile) as error:
        sys.exit(f"Packaging failed: {error}")
