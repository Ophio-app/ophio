#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Owner-operated, offline fresh-key APK signing. Never builds or publishes."""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import zipfile

from release_common import ROOT, ReleaseError, apk_signers, digest

# This certificate has authenticated personal builds. It must never become the
# protected public identity merely by copying its fingerprint into the policy.
PERSONAL_CERTIFICATE = "a4b1d0fb6bb576729111ef5ddd6d333dfa76275710606fbfdb574ced0d9741bf"
POLICY = Path("packaging/android/public-signing.json")


def sha256(value: object) -> str:
    if not isinstance(value, str) or not re.fullmatch(r"[0-9a-f]{64}", value):
        raise ReleaseError("Expected a lowercase SHA-256 fingerprint (64 hex characters)")
    return value


def public_certificate(policy: Path) -> str:
    data = json.loads(policy.read_text(encoding="utf-8"))
    if not isinstance(data, dict) or data.get("schema") != 1:
        raise ReleaseError("Unsupported public signing policy")
    if data.get("certificate_sha256") is None:
        raise ReleaseError("Public signing is not configured; complete the owner signing ceremony")
    certificate = sha256(data["certificate_sha256"])
    if certificate == PERSONAL_CERTIFICATE:
        raise ReleaseError("The personal signing certificate is forbidden for public releases")
    return certificate


def public_mode() -> bool:
    mode = os.environ.get("OPHIO_PUBLIC_RELEASE", "0")
    if mode not in {"0", "1"}:
        raise ReleaseError("OPHIO_PUBLIC_RELEASE must be 0 or 1")
    return mode == "1"


def public_build_check(root: Path) -> bool:
    if not public_mode():
        return False
    from release_common import personal_signing, release_certificate

    certificate = public_certificate(root / POLICY)
    if personal_signing():
        raise ReleaseError("A public build cannot trust the personal development key; unset OPHIO_PERSONAL_SIGNING")
    if release_certificate(root) != certificate:
        raise ReleaseError("Public certificate differs from the companion's public pin")
    return True


def tool_environment() -> dict[str, str]:
    # Do not inherit JAVA_TOOL_OPTIONS, CLASSPATH, loader hooks or build config.
    # This is defence against accidental inheritance, not a hostile/root user.
    return {"PATH": "/usr/bin:/bin", "LANG": "C.UTF-8", "LC_ALL": "C.UTF-8"}


def run(args: list[str], *, capture: bool = False) -> str:
    result = subprocess.run(
        args, check=True, env=tool_environment(), text=True, encoding="utf-8",
        stdout=subprocess.PIPE if capture else None,
        stderr=subprocess.PIPE if capture else None,
    )
    return result.stdout if capture else ""


def verify(apk: Path, tools: Path, certificate: str) -> None:
    report = run([str(tools / "apksigner"), "verify", "--verbose", "--print-certs", str(apk)], capture=True)
    if not re.search(r"Verified using v3 scheme .*: true", report):
        raise ReleaseError("APK has no verified v3 signature")
    signers = apk_signers(apk.read_bytes())
    if not signers or any(signer != certificate for signer in signers):
        raise ReleaseError("APK signer differs from the approved public certificate; rotation is not supported here")
    run([str(tools / "zipalign"), "-c", "-P", "16", "4", str(apk)], capture=True)


def sign(args: argparse.Namespace) -> None:
    certificate = public_certificate(args.policy)
    expected = sha256(args.input_sha256)
    tools = args.build_tools.resolve(strict=True)
    for tool in ("apksigner", "zipalign"):
        if not (tools / tool).is_file():
            raise ReleaseError(f"Android build-tools missing {tool}")
    if args.input.is_symlink() or not args.input.is_file():
        raise ReleaseError("Unsigned APK must be a regular file, not a symbolic link")
    if args.output.exists() or args.output.is_symlink():
        raise ReleaseError("Output already exists; refusing to replace it")
    if not sys.stdin.isatty():
        raise ReleaseError("Signing requires an owner-operated terminal for password prompts")
    # The owner must copy these tools and this script into trusted, non-agent-
    # writable storage first. No Python check can establish host isolation.
    with tempfile.TemporaryDirectory(prefix="ophio-sign-", dir=args.output.parent) as directory:
        work = Path(directory)
        unsigned = work / "input.apk"
        unsigned.write_bytes(args.input.read_bytes())
        if digest(unsigned) != expected:
            raise ReleaseError("Unsigned APK differs from the owner-approved SHA-256")
        # A broken signature is not an unsigned input. Be conservative about
        # signing-block markers, and reject JAR signature entries as well.
        with zipfile.ZipFile(unsigned) as archive:
            jar_signed = any(re.fullmatch(r"META-INF/[^/]+\.(?:SF|RSA|DSA|EC)", name, re.I) for name in archive.namelist())
        if jar_signed or b"APK Sig Block 42" in unsigned.read_bytes():
            raise ReleaseError("Input must be unsigned; signing markers are present")
        check = subprocess.run(
            [str(tools / "apksigner"), "verify", str(unsigned)],
            env=tool_environment(), capture_output=True, text=True, encoding="utf-8",
        )
        if check.returncode == 0 or "DOES NOT VERIFY" not in check.stdout + check.stderr:
            raise ReleaseError("Input was not established to be unsigned")
        aligned, signed = work / "aligned.apk", work / "signed.apk"
        run([str(tools / "zipalign"), "-P", "16", "4", str(unsigned), str(aligned)])
        # No password flags or environment variable fallback. apksigner prompts.
        run([
            str(tools / "apksigner"), "sign", "--ks", str(args.keystore.resolve()),
            "--ks-key-alias", args.alias, "--v1-signing-enabled", "false",
            "--v2-signing-enabled", "true", "--v3-signing-enabled", "true",
            "--v4-signing-enabled", "false", "--debuggable-apk-permitted", "false",
            "--out", str(signed), str(aligned),
        ])
        verify(signed, tools, certificate)
        # Atomic and no-clobber, on the same filesystem. Failure leaves no final
        # artifact, and a concurrently created destination is never overwritten.
        os.link(signed, args.output)
    print(f"Signed APK SHA-256: {digest(args.output)}")
    print(f"Public certificate SHA-256: {certificate}")
    print("Signed locally only. Moving installs to this key and approving the release are separate steps.")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", type=Path, required=True, help="reviewed unsigned APK")
    parser.add_argument("--input-sha256", required=True, help="owner-approved unsigned APK digest")
    parser.add_argument("--output", type=Path, required=True, help="new signed APK; must not exist")
    parser.add_argument("--build-tools", type=Path, required=True, help="trusted Android build-tools directory")
    parser.add_argument("--keystore", type=Path, required=True, help="owner's protected keystore; no default")
    parser.add_argument("--alias", required=True)
    parser.add_argument("--policy", type=Path, default=ROOT / POLICY, help="owner-reviewed public certificate policy")
    args = parser.parse_args()
    try:
        sign(args)
    except (ReleaseError, OSError, ValueError, subprocess.SubprocessError, zipfile.BadZipFile) as error:
        # Captured tool diagnostics may include local paths; do not dump them.
        if isinstance(error, subprocess.SubprocessError):
            sys.exit("Signing failed: Android tool rejected the artifact or signing operation")
        sys.exit(f"Signing failed: {error}")


if __name__ == "__main__":
    main()
