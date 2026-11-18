#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Assemble archives and verify local artifacts without installing anything."""

from __future__ import annotations

import gzip
import hashlib
import io
import json
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


def write_tarball(path: Path, name: str, files: dict, epoch: int, check) -> None:
    """A gzip tarball of `files` under the folder `name`, with stable order,
    times and permissions. `check` sees each member's exact bytes first."""
    with (
        path.open("wb") as raw,
        gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=epoch) as compressed,
        tarfile.open(fileobj=compressed, mode="w") as archive,
    ):
        for filename, source in sorted(files.items()):
            if isinstance(source, bytes):
                # Archive the exact companion and APK snapshots that passed verification.
                data = source
            else:
                if source.is_symlink() or not source.is_file():
                    raise ReleaseError(f"Unsafe/missing package input: {filename}")
                data = source.read_bytes()
            check(filename, data)
            entry = tarfile.TarInfo(f"{name}/{filename}")
            entry.size, entry.mtime, entry.mode = (
                len(data),
                epoch,
                0o755 if filename in EXECUTABLES else 0o644,
            )
            archive.addfile(entry, io.BytesIO(data))


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

        def check(filename: str, data: bytes) -> None:
            if filename in {"ophio", "computer-mcp", "ophio-sandbox-shim"}:
                if data[:6] != b"\x7fELF\x02\x01" or data[18:20] != b"\x3e\x00":
                    raise ReleaseError(f"Expected Linux x86_64 ELF binary: {filename}")
                print(f"{filename}: highest required GLIBC_{check_glibc(release / filename)}")

        write_tarball(archive_path, name, files, epoch, check)
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


# Mach-O 64-bit, little-endian, for ARM64.
MACHO_ARM64 = b"\xcf\xfa\xed\xfe\x0c\x00\x00\x01"
LC_CODE_SIGNATURE = 0x1D


def require_macho(filename: str, data: bytes) -> None:
    """An Apple silicon program that carries a code signature: macOS refuses to
    run any unsigned ARM64 program."""
    if len(data) < 32 or data[:8] != MACHO_ARM64:
        raise ReleaseError(f"Expected an Apple silicon (arm64) Mach-O program: {filename}")
    count = int.from_bytes(data[16:20], "little")
    at = 32
    for _ in range(count):
        command, size = int.from_bytes(data[at : at + 4], "little"), int.from_bytes(data[at + 4 : at + 8], "little")
        if command == LC_CODE_SIGNATURE:
            return
        if size < 8:
            break
        at += size
    raise ReleaseError(f"{filename} is not signed; run scripts/sign_macos.py sign on a Mac")


def macos() -> None:
    """The Mac archive, from programs `sign_macos.py sign` signed (and perhaps
    notarized). This runs on any system, so a phone app that only Linux tools
    can verify may be added after the Mac build."""
    _, slug, version = identity()
    target = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target"))
    epoch = int(os.environ["SOURCE_DATE_EPOCH"])
    name = f"{slug}-{version}-aarch64-macos"
    signed = target / "packaging/macos"
    licences = target / "packaging/licenses/macos"
    require_licences(licences, "macos")
    record = json.loads((signed / "signing.json").read_text(encoding="utf-8"))
    programs = {}
    for program in ("ophio", "computer-mcp"):
        path = signed / "bin" / program
        if path.is_symlink() or not path.is_file():
            raise ReleaseError(f"Unsafe/missing package input: {program}")
        data = path.read_bytes()
        if hashlib.sha256(data).hexdigest() != record["programs"][program]["sha256"]:
            raise ReleaseError(f"{program} differs from the signing record; sign it again")
        require_macho(program, data)
        programs[program] = data
    require_executable_trust(programs["ophio"], ROOT)
    if os.environ.get("OPHIO_PUBLIC_RELEASE") == "1" and record.get("signature") not in {"ad-hoc", "developer-id"}:
        raise ReleaseError("Unknown Mac signature kind")
    files = {
        **programs,
        "LICENSE": ROOT / "LICENSE",
        "INSTALL.md": ROOT / "packaging/macos/INSTALL.md",
        "install.sh": ROOT / "packaging/macos/install.sh",
    }
    app = phone_app_bytes(ROOT)
    if app is not None:
        files["ophio.apk"] = app
    for filename in ("NOTICE", "THIRD-PARTY-NOTICES.txt", "sbom.cdx.json", "license-report.json"):
        files[filename] = licences / filename
    output = ROOT / "dist/macos"
    output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=target / "packaging", prefix="macos-archive-") as directory:
        scratch = Path(directory)
        archive_path = scratch / (name + ".tar.gz")
        write_tarball(archive_path, name, files, epoch, lambda *_: None)
        with tarfile.open(archive_path) as archive:
            extracted = scratch / "extracted"
            archive.extractall(extracted, filter="data")
        for program, data in programs.items():
            if (extracted / name / program).read_bytes() != data:
                raise ReleaseError(f"Archived {program} differs from the signed program")
        if sys.platform == "darwin" and os.uname().machine == "arm64":
            result = subprocess.run(
                [str(extracted / name / "ophio"), "--version"],
                cwd=extracted, check=True, capture_output=True, text=True, encoding="utf-8", timeout=30,
            )
            if version not in result.stdout.split():
                raise ReleaseError("Packaged binary version differs from workspace")
            print(f"Extracted ophio --version: {result.stdout.splitlines()[0]}")
        else:
            print("Not on an Apple silicon Mac: the archived programs were not started")
        destination = output / archive_path.name
        shutil.copyfile(archive_path, destination.with_suffix(".tmp"))
        os.replace(destination.with_suffix(".tmp"), destination)
    print(f"Signature: {record['signature']}"
          + (f", notarized ({record['notarization']['id']})" if record.get("notarization") else ", not notarized"))
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
    elif command == "macos":
        macos()
    elif command == "stage-android":
        stage_android()
    elif command == "manifest":
        print(manifest(), end="")
    else:
        raise ReleaseError(
            "usage: package-artifacts.py identity|notice|linux|macos|stage-android|manifest"
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
