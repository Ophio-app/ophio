#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Build unsigned ARM64 Android artifacts and sign a separate local test copy."""

from __future__ import annotations

import json
import os
from pathlib import Path
import re
import secrets
import shutil
import subprocess
import sys
import tempfile
import zipfile

from sign_android import public_mode
from release_common import (
    ROOT,
    ReleaseError,
    digest,
    identity,
    manifest,
    readable_output,
    require_licences,
    release_environment,
)


def command(args: list[str], cwd: Path, environment: dict) -> None:
    subprocess.run(args, cwd=cwd, env=environment, check=True)


def verify_assets(path: Path, licences: Path, prefix: str) -> None:
    with zipfile.ZipFile(path) as archive:
        for name in ("LICENSE", "NOTICE", "THIRD-PARTY-NOTICES.txt", "sbom.cdx.json"):
            try:
                content = archive.read(prefix + name)
            except KeyError:
                raise ReleaseError(
                    f"Android licence asset missing: {prefix}{name}"
                ) from None
            if content != (licences / name).read_bytes():
                raise ReleaseError(f"Android embedded licence asset differs: {name}")


def public_destination(*, staging: Path | None = None) -> Path:
    """Refuse old artifacts before building or writing a public candidate."""
    dist = ROOT / "dist"
    destination = dist / "android-public-unsigned"
    if dist.is_symlink() or (dist.exists() and not dist.is_dir()):
        raise ReleaseError("Public distribution directory must not be a symbolic link or file")
    if destination.is_symlink() or (destination.exists() and not destination.is_dir()):
        raise ReleaseError("Public unsigned output directory must not be a symbolic link or file")
    if dist.exists():
        if any(path != destination and path != staging for path in dist.iterdir()):
            raise ReleaseError("Public distribution directory must contain no prior artifacts")
        if destination.exists() and any(destination.iterdir()):
            raise ReleaseError("Public unsigned output directory must be empty")
    return destination


def publish_candidates(paths: list[tuple[Path, str]], licences: Path, verification: str, *, public: bool = False) -> None:
    # Personal packaging retains its existing output and manifest behavior.
    if not public:
        destination = ROOT / "dist/android"
        destination.mkdir(parents=True, exist_ok=True)
        for path, name in paths:
            shutil.copyfile(path, destination / name)
        for name in ("LICENSE", "NOTICE", "THIRD-PARTY-NOTICES.txt", "sbom.cdx.json", "license-report.json"):
            shutil.copyfile(licences / name, destination / name)
        (destination / "verification.txt").write_text(verification, encoding="utf-8")
        print(manifest(), end="")
        return

    destination = public_destination()
    destination.parent.mkdir(parents=True, exist_ok=True)
    existed = destination.exists()
    # No candidate is visible until all copies and the verification note complete.
    with tempfile.TemporaryDirectory(dir=destination.parent, prefix=".android-public-stage-") as temporary:
        staged = Path(temporary) / "candidate"
        staged.mkdir()
        for path, name in paths:
            shutil.copyfile(path, staged / name)
        for name in ("LICENSE", "NOTICE", "THIRD-PARTY-NOTICES.txt", "sbom.cdx.json", "license-report.json"):
            shutil.copyfile(licences / name, staged / name)
        (staged / "verification.txt").write_text(verification, encoding="utf-8")
        # A caller may have changed dist while the copies were in progress.
        public_destination(staging=Path(temporary))
        original = Path(temporary) / "original-empty-directory"
        if existed:
            os.replace(destination, original)
        try:
            os.replace(staged, destination)
        except Exception:
            if existed:
                os.replace(original, destination)
            raise
        try:
            print(manifest(ROOT), end="")
        except Exception:
            # Restore an existing empty directory rather than deleting user data.
            shutil.rmtree(destination)
            (destination.parent / "MANIFEST.txt").unlink(missing_ok=True)
            if existed:
                os.replace(original, destination)
            raise


def check_identity(path: Path, tools: Path, environment: dict, version: str) -> str:
    badging = subprocess.check_output(
        [str(tools / "aapt2"), "dump", "badging", str(path)],
        env=environment, text=True, encoding="utf-8",
    )
    config = json.loads((ROOT / "client/src-tauri/tauri.conf.json").read_text(encoding="utf-8"))
    if not re.search(
        r"package: name='" + re.escape(config["identifier"])
        + r"'.*versionName='" + re.escape(version) + "'", badging,
    ):
        raise ReleaseError("APK application id/version differs from Tauri/workspace")
    if "native-code: 'arm64-v8a'" not in badging:
        raise ReleaseError("APK ABI is not the requested ARM64 target")
    return badging


def main() -> None:
    if public_mode():
        public_destination()
    _, slug, version = identity()
    target = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target"))
    work = target / "packaging/android"
    work.mkdir(parents=True, exist_ok=True)
    # The staged copy is built in place of the checkout, often in a target
    # folder outside it; both are remapped so no builder path is compiled in.
    environment = release_environment("android", staged=work / "project")
    # Release-owned destinations compiled into Welcome; no distribution default.
    environment["VITE_OPHIO_DOWNLOAD_URL"] = environment.get("OPHIO_DOWNLOAD_URL", "")
    environment["VITE_OPHIO_CHECKSUM_URL"] = environment.get("OPHIO_CHECKSUM_URL", "")
    for key in list(environment):
        if key.startswith("TAURI_ANDROID_KEYSTORE_") or key in {
            "DISPLAY",
            "WAYLAND_DISPLAY",
            "DBUS_SESSION_BUS_ADDRESS",
        }:
            del environment[key]
    environment.update(
        {
            "CARGO_NET_OFFLINE": "true",
            "CI": "true",
            "RELEASE_GRADLE_INIT": str(ROOT / "packaging/android/release.init.gradle"),
            "RELEASE_GRADLE_REPORT": str(work / "gradle-runtime.json"),
            "RELEASE_ANDROID_ASSETS": str(work / "assets"),
        }
    )
    (work / "assets/licenses").mkdir(parents=True, exist_ok=True)
    command(
        [sys.executable, str(ROOT / "scripts/package-artifacts.py"), "stage-android"],
        ROOT,
        environment,
    )
    project = work / "project"
    client = project / "client"
    android = client / "src-tauri/gen/android"
    wrapper = android / "gradlew"
    text = wrapper.read_text(encoding="utf-8")
    first, rest = text.split("\n", 1)
    wrapper.write_text(
        first
        + '\nset -- --offline --no-daemon --no-configuration-cache --max-workers=8 --init-script "$RELEASE_GRADLE_INIT" "$@"\n'
        + rest,
        encoding="utf-8",
    )
    wrapper.chmod(0o755)
    # Tauri generates native plugin settings during this first, local-only pass.
    # A second pass embeds the complete inventory after Gradle has resolved it.
    tauri = [
        "node",
        str(ROOT / "client/node_modules/@tauri-apps/cli/tauri.js"),
        "android",
        "build",
        "--ci",
        "--target",
        "aarch64",
        "--apk",
        "--aab",
    ]
    command(tauri, client, environment)
    if digest(project / "client/src-tauri/Cargo.lock") != digest(
        ROOT / "client/src-tauri/Cargo.lock"
    ):
        raise ReleaseError("Tauri changed the locked native dependency graph")
    command([str(wrapper), ":app:writeReleaseLicenseReport"], android, environment)
    output = target / "packaging/licenses"
    command(
        [
            sys.executable,
            str(ROOT / "scripts/licenses.py"),
            "--root",
            str(project),
            "--artifact",
            "android",
            "--check",
            "--gradle-report",
            str(work / "gradle-runtime.json"),
            "--web-bundle",
            str(client / "dist"),
            "--output",
            str(output),
        ],
        ROOT,
        environment,
    )
    licences = output / "android"
    require_licences(licences, "android")
    shutil.copyfile(ROOT / "LICENSE", licences / "LICENSE")
    for directory in (work / "assets/licenses", client / "dist/licenses"):
        directory.mkdir(parents=True, exist_ok=True)
        for name in ("LICENSE", "NOTICE", "THIRD-PARTY-NOTICES.txt", "sbom.cdx.json"):
            shutil.copyfile(licences / name, directory / name)
    overlay = work / "embed-licences.json"
    overlay.write_text(
        json.dumps({"build": {"beforeBuildCommand": ""}}), encoding="utf-8"
    )
    command([*tauri, "--config", str(overlay)], client, environment)
    apks = sorted(
        (android / "app/build/outputs/apk/universal/release").glob("*-unsigned.apk")
    )
    aabs = sorted((android / "app/build/outputs/bundle/universalRelease").glob("*.aab"))
    if len(apks) != 1 or len(aabs) != 1:
        raise ReleaseError("Expected one unsigned universal ARM64 APK and one AAB")
    tools = Path(environment["ANDROID_HOME"]) / "build-tools/37.0.0"
    for name in ("aapt2", "zipalign", "apksigner"):
        if not (tools / name).is_file():
            raise ReleaseError(f"Android build-tools 37.0.0 missing {name}")
    unsigned = work / f"{slug}-{version}-arm64-unsigned.apk"
    command(
        [str(tools / "zipalign"), "-f", "-P", "16", "4", str(apks[0]), str(unsigned)],
        ROOT,
        environment,
    )
    check = subprocess.run(
        [str(tools / "apksigner"), "verify", str(unsigned)],
        env=environment,
        capture_output=True,
        text=True,
        encoding="utf-8",
        check=False,
    )
    if check.returncode == 0 or "DOES NOT VERIFY" not in check.stderr + check.stdout:
        raise ReleaseError(
            "Unsigned APK check did not establish absence of a valid signature"
        )
    verify_assets(unsigned, licences, "assets/licenses/")
    verify_assets(aabs[0], licences, "base/assets/licenses/")
    with zipfile.ZipFile(aabs[0]) as archive:
        if any(
            re.fullmatch(r"META-INF/[^/]+\.(?:SF|RSA|DSA|EC)", name, re.I)
            for name in archive.namelist()
        ):
            raise ReleaseError("Expected an unsigned AAB, found signing entries")
    if public_mode():
        badging = check_identity(unsigned, tools, environment, version)
        publish_candidates(
            [(unsigned, unsigned.name), (aabs[0], f"{slug}-{version}-arm64-unsigned.aab")],
            licences,
            "UNSIGNED ONLY. Owner-operated offline signing is still required.\n" + badging,
            public=True,
        )
        return
    # All key material is temporary and only authenticates this local test copy.
    with tempfile.TemporaryDirectory(dir=work, prefix="test-signing-") as directory:
        signing = Path(directory)
        password = signing / "password.txt"
        password.write_text(secrets.token_urlsafe(32) + "\n", encoding="utf-8")
        password.chmod(0o600)
        keystore = signing / "test.keystore"
        command(
            [
                str(Path(environment["JAVA_HOME"]) / "bin/keytool"),
                "-genkeypair",
                "-noprompt",
                "-keystore",
                str(keystore),
                "-storetype",
                "PKCS12",
                "-storepass:file",
                str(password),
                "-keypass:file",
                str(password),
                "-alias",
                "local-test",
                "-keyalg",
                "RSA",
                "-keysize",
                "3072",
                "-validity",
                "30",
                "-dname",
                "CN=Android Debug,O=Local Testing,C=US",
            ],
            ROOT,
            environment,
        )
        signed = work / f"{slug}-{version}-arm64-test-signed.apk"
        command(
            [
                str(tools / "apksigner"),
                "sign",
                "--ks",
                str(keystore),
                "--ks-key-alias",
                "local-test",
                "--ks-pass",
                f"file:{password}",
                "--out",
                str(signed),
                str(unsigned),
            ],
            ROOT,
            environment,
        )
    certificates = subprocess.check_output(
        [str(tools / "apksigner"), "verify", "--verbose", "--print-certs", str(signed)],
        env=environment,
        text=True,
        encoding="utf-8",
    )
    badging = check_identity(signed, tools, environment, version)
    verify_assets(signed, licences, "assets/licenses/")
    print(certificates)
    print(badging)
    # Publish local files only after all validation succeeds. No signing material
    # enters dist; a test key is intentionally different on every invocation.
    publish_candidates(
        [(unsigned, unsigned.name), (signed, signed.name),
         (aabs[0], f"{slug}-{version}-arm64-unsigned.aab")],
        licences,
        "Unsigned APK: signature absent (expected).\n" + certificates + "\n" + badging,
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
        zipfile.BadZipFile,
    ) as error:
        sys.exit(f"Android packaging failed: {error}")
