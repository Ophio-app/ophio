#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Assemble archives and verify local artifacts without installing anything."""

from __future__ import annotations

import gzip
import io
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile

from package_linux import check_glibc

from release_common import (
    ROOT,
    ReleaseError,
    atomic_write,
    identity,
    manifest,
    notice,
    phone_app_bytes,
    readable_output,
    require_licences,
    require_executable_trust,
)


# Archive members that run; everything else is read-only documentation or data.
EXECUTABLES = {"ophio", "computer-mcp", "ophio-sandbox-shim", "install.sh"}


def linux() -> None:
    _, slug, version = identity()
    target = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target"))
    epoch = int(os.environ["SOURCE_DATE_EPOCH"])
    name = f"{slug}-{version}-x86_64-linux"
    release = target / "x86_64-unknown-linux-gnu/release"
    licences = target / "packaging/licenses/linux"
    require_licences(licences, "linux")
    companion = release / "ophio"
    if companion.is_symlink() or not companion.is_file():
        raise ReleaseError("Unsafe/missing package input: ophio")
    companion_bytes = companion.read_bytes()
    require_executable_trust(companion_bytes, ROOT)
    files = {
        "ophio": companion_bytes,
        "computer-mcp": release / "computer-mcp",
        "ophio-sandbox-shim": release / "ophio-sandbox-shim",
        "LICENSE": ROOT / "LICENSE",
        "INSTALL.md": ROOT / "packaging/linux/INSTALL.md",
        "install.sh": ROOT / "packaging/linux/install.sh",
    }
    app = phone_app_bytes(ROOT)
    if app is not None:
        files["ophio.apk"] = app
    icons = ROOT / "packaging"
    for icon in sorted((icons / "icons/hicolor").glob("*/apps/ophio.*")):
        files[icon.relative_to(icons).as_posix()] = icon
    if not any(name.startswith("icons/hicolor/scalable/") for name in files):
        raise ReleaseError("Generate packaging/icons with client/src-tauri/icons/build.py")
    for filename in (
        "NOTICE",
        "THIRD-PARTY-NOTICES.txt",
        "sbom.cdx.json",
        "license-report.json",
    ):
        files[filename] = licences / filename
    output = ROOT / "dist/linux"
    output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(
        dir=target / "packaging", prefix="linux-archive-"
    ) as directory:
        scratch = Path(directory)
        archive_path = scratch / (name + ".tar.gz")
        with (
            archive_path.open("wb") as raw,
            gzip.GzipFile(
                filename="", mode="wb", fileobj=raw, mtime=epoch
            ) as compressed,
            tarfile.open(fileobj=compressed, mode="w") as archive,
        ):
            for filename, path in sorted(files.items()):
                if isinstance(path, bytes):
                    # Archive the exact companion and APK snapshots that passed verification.
                    data = path
                else:
                    if path.is_symlink() or not path.is_file():
                        raise ReleaseError(f"Unsafe/missing package input: {filename}")
                    data = path.read_bytes()
                if filename in {"ophio", "computer-mcp", "ophio-sandbox-shim"} and (
                    data[:6] != b"\x7fELF\x02\x01" or data[18:20] != b"\x3e\x00"
                ):
                    raise ReleaseError(f"Expected Linux x86_64 ELF binary: {filename}")
                if filename in {"ophio", "computer-mcp", "ophio-sandbox-shim"}:
                    print(f"{filename}: highest required GLIBC_{check_glibc(release / filename)}")
                entry = tarfile.TarInfo(f"{name}/{filename}")
                entry.size, entry.mtime, entry.mode = (
                    len(data),
                    epoch,
                    0o755 if filename in EXECUTABLES else 0o644,
                )
                archive.addfile(entry, io.BytesIO(data))
        extracted = scratch / "extracted"
        with tarfile.open(archive_path) as archive:
            archive.extractall(extracted, filter="data")
        isolated = scratch / "state"
        environment = {
            k: v
            for k, v in os.environ.items()
            if k not in {"DISPLAY", "WAYLAND_DISPLAY", "DBUS_SESSION_BUS_ADDRESS"}
        }
        for key in (
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_STATE_HOME",
            "XDG_CACHE_HOME",
            "XDG_RUNTIME_DIR",
        ):
            environment[key] = str(isolated / key.lower())
        binary = extracted / name / "ophio"
        for argument in ("--version", "--help"):
            result = subprocess.run(
                [str(binary), argument],
                cwd=extracted,
                env=environment,
                check=True,
                capture_output=True,
                text=True,
                encoding="utf-8",
                timeout=30,
            )
            if argument == "--version" and version not in result.stdout.split():
                raise ReleaseError("Packaged binary version differs from workspace")
            print(f"Extracted ophio {argument}: {result.stdout.splitlines()[0]}")
        destination = output / archive_path.name
        shutil.copyfile(archive_path, destination.with_suffix(".tmp"))
        os.replace(destination.with_suffix(".tmp"), destination)
    print(manifest(), end="")


def stage_android() -> None:
    """Copy tracked inputs; all Tauri/Gradle mutations stay in build output."""
    target = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target"))
    staging = target / "packaging/android/project"
    if staging.exists():
        shutil.rmtree(staging)
    staging.mkdir(parents=True)
    tracked = (
        subprocess.check_output(["git", "ls-files", "-z"], cwd=ROOT)
        .decode()
        .split("\0")
    )
    for name in filter(None, tracked):
        source = ROOT / name
        if source.is_symlink() or not source.is_file():
            raise ReleaseError(f"Unsafe/missing tracked build input: {name}")
        destination = staging / name
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, destination)
        destination.chmod(source.stat().st_mode & 0o777)
    modules = ROOT / "client/node_modules"
    if not modules.is_dir():
        raise ReleaseError(
            "Install the locked client dependencies before Android packaging"
        )
    (staging / "client/node_modules").symlink_to(modules, target_is_directory=True)
    print(f"Staged Android build at {staging}")


def main() -> None:
    command = sys.argv[1] if len(sys.argv) == 2 else ""
    if command == "identity":
        name, _, version = identity()
        if (ROOT / "NOTICE").read_text(encoding="utf-8") != notice():
            raise ReleaseError(
                "NOTICE is stale; regenerate with scripts/package-artifacts.py notice"
            )
        print(f"{name} {version}: local release candidate")
    elif command == "notice":
        atomic_write(ROOT / "NOTICE", notice().encode())
    elif command == "linux":
        linux()
    elif command == "stage-android":
        stage_android()
    elif command == "manifest":
        print(manifest(), end="")
    else:
        raise ReleaseError(
            "usage: package-artifacts.py identity|notice|linux|stage-android|manifest"
        )


if __name__ == "__main__":
    readable_output()
    try:
        main()
    except (
        ReleaseError,
        OSError,
        ValueError,
        subprocess.SubprocessError,
        tarfile.TarError,
    ) as error:
        sys.exit(f"Packaging failed: {error}")
