#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Sign the Mac programs for an archive, and optionally notarize them.

Runs on a Mac after `build-host.py macos`. Never builds, publishes or changes
any system setting.

    sign_macos.py sign       copy the built programs and sign them
    sign_macos.py notarize   send the signed programs to Apple's notary service

Without settings, `sign` signs ad hoc: macOS runs the programs, but nothing
identifies who made them, and the signature differs with every build. With
`OPHIO_MACOS_SIGNING_IDENTITY` set to a Developer ID Application certificate's
name or SHA-1, it signs with that certificate and a secure timestamp instead.
`OPHIO_MACOS_KEYCHAIN` names a keychain file holding it, unlocked beforehand.

`notarize` needs a Developer ID signature and `OPHIO_MACOS_NOTARY_PROFILE`, the
name of credentials saved once with `xcrun notarytool store-credentials`
(in `OPHIO_MACOS_KEYCHAIN` when that is set). Apple cannot attach ("staple")
a notarization ticket to a bare program, so macOS looks the ticket up online
the first time a downloaded copy runs.

Both commands write `packaging/macos/signing.json` under the target folder,
which `package-artifacts.py macos` checks against the programs it archives.
"""

from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile

from release_common import ROOT, TARGET_TRIPLES, ReleaseError, atomic_write, readable_output

PROGRAMS = ("ophio", "computer-mcp")
# Each program's signing identifier: what a lasting signature's requirement
# names, so permissions given at the Mac can follow it across updates.
IDENTIFIER = "app.ophio.{}"
DEVELOPER_ID = "Developer ID Application: "
APPLE_CHAIN = ("Developer ID Certification Authority", "Apple Root CA")


def target() -> Path:
    return Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target"))


def folder() -> Path:
    return target() / "packaging/macos"


def tool(args: list[str], *, check: bool = True) -> subprocess.CompletedProcess:
    # Apple's own tools by absolute path, with a plain environment.
    environment = {"PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "LANG": "en_US.UTF-8", "HOME": str(Path.home())}
    result = subprocess.run(args, capture_output=True, text=True, encoding="utf-8", env=environment, check=False)
    if check and result.returncode:
        raise ReleaseError(f"{Path(args[0]).name} failed: {(result.stderr or result.stdout).strip()[-800:]}")
    return result


def sha256(path: Path) -> str:
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def public() -> bool:
    mode = os.environ.get("OPHIO_PUBLIC_RELEASE", "0")
    if mode not in {"0", "1"}:
        raise ReleaseError("OPHIO_PUBLIC_RELEASE must be 0 or 1")
    return mode == "1"


def details(path: Path) -> dict:
    """What codesign says of a signed program: its authorities, team,
    identifier, code directory hash and designated requirement."""
    tool(["/usr/bin/codesign", "--verify", "--strict", "--verbose=2", str(path)])
    shown = tool(["/usr/bin/codesign", "--display", "--verbose=4", str(path)]).stderr
    requirement = tool(["/usr/bin/codesign", "--display", "--requirements", "-", str(path)])
    found = re.search(r"^(?:# )?designated => (.+)$", requirement.stdout + requirement.stderr, re.M)
    fields = dict(
        line.split("=", 1) for line in shown.splitlines() if "=" in line and not line.startswith("Authority=")
    )
    return {
        "sha256": sha256(path),
        "identifier": fields.get("Identifier"),
        "cdhash": fields.get("CDHash"),
        "team": fields.get("TeamIdentifier"),
        "authorities": re.findall(r"^Authority=(.+)$", shown, re.M),
        "runtime": "runtime" in fields.get("CodeDirectory v", "") or "(runtime)" in shown,
        "timestamped": "Timestamp=" in shown,
        "requirement": found[1] if found else None,
    }


def sign() -> dict:
    identity = os.environ.get("OPHIO_MACOS_SIGNING_IDENTITY", "").strip()
    keychain = os.environ.get("OPHIO_MACOS_KEYCHAIN", "").strip()
    if keychain and not Path(keychain).is_file():
        raise ReleaseError("OPHIO_MACOS_KEYCHAIN does not name a keychain file")
    built = target() / TARGET_TRIPLES["macos"] / "release"
    out = folder() / "bin"
    if out.exists():
        shutil.rmtree(out)
    out.mkdir(parents=True)
    record = {"schema": 1, "signature": "developer-id" if identity else "ad-hoc", "programs": {}, "notarization": None}
    for name in PROGRAMS:
        source = built / name
        if source.is_symlink() or not source.is_file():
            raise ReleaseError(f"Build the Mac programs first: {name} is missing")
        shutil.copyfile(source, out / name)
        (out / name).chmod(0o755)
        # The hardened runtime is on in every build, so an ad hoc build behaves
        # as a notarized one will; a secure timestamp needs a real certificate.
        command = [
            "/usr/bin/codesign", "--force", "--options", "runtime",
            "--identifier", IDENTIFIER.format(name),
            "--sign", identity or "-",
            "--timestamp" if identity else "--timestamp=none",
        ]
        if keychain:
            command += ["--keychain", keychain]
        tool([*command, str(out / name)])
        signed = details(out / name)
        if signed["identifier"] != IDENTIFIER.format(name) or not signed["runtime"]:
            raise ReleaseError(f"{name} was not signed as expected")
        if identity:
            chain = signed["authorities"]
            apple = len(chain) == 3 and chain[0].startswith(DEVELOPER_ID) and tuple(chain[1:]) == APPLE_CHAIN
            if public() and not apple:
                # A self-made certificate means nothing on anyone else's Mac.
                raise ReleaseError("A public build is signed ad hoc or with a Developer ID Application certificate")
            if apple and not (signed["team"] and signed["timestamped"]):
                raise ReleaseError(f"{name} lacks a team or secure timestamp")
            # A personal build may use a lasting certificate of its own, which
            # keeps the Mac's permissions across rebuilds on that one Mac.
            record["signature"] = "developer-id" if apple else "certificate"
            record["team"] = signed["team"]
            record["authority"] = chain[0] if chain else None
        elif signed["authorities"] or signed["team"] not in (None, "not set"):
            raise ReleaseError(f"{name} was expected to be signed ad hoc")
        record["programs"][name] = signed
    atomic_write(folder() / "signing.json", (json.dumps(record, indent=2) + "\n").encode())
    return record


def notarize() -> dict:
    profile = os.environ.get("OPHIO_MACOS_NOTARY_PROFILE", "").strip()
    keychain = os.environ.get("OPHIO_MACOS_KEYCHAIN", "").strip()
    if not profile:
        raise ReleaseError("Set OPHIO_MACOS_NOTARY_PROFILE to saved notarytool credentials")
    record = json.loads((folder() / "signing.json").read_text(encoding="utf-8"))
    if record.get("signature") != "developer-id" or not record.get("authority", "").startswith(DEVELOPER_ID):
        raise ReleaseError("Only programs signed with a Developer ID can be notarized")
    for name in PROGRAMS:
        if sha256(folder() / "bin" / name) != record["programs"][name]["sha256"]:
            raise ReleaseError(f"{name} changed after it was signed; sign again")
    credentials = ["--keychain-profile", profile] + (["--keychain", keychain] if keychain else [])
    with tempfile.TemporaryDirectory(prefix="ophio-notarize-") as scratch:
        upload = Path(scratch) / "ophio-macos.zip"
        tool(["/usr/bin/ditto", "-c", "-k", "--keepParent", str(folder() / "bin"), str(upload)])
        sent = tool(["/usr/bin/xcrun", "notarytool", "submit", str(upload), *credentials,
                     "--wait", "--timeout", "2h", "--output-format", "json"], check=False)
    try:
        answer = json.loads(sent.stdout)
    except json.JSONDecodeError as error:
        raise ReleaseError(f"notarytool gave no answer: {sent.stderr.strip()[-800:]}") from error
    submission = answer.get("id")
    log = tool(["/usr/bin/xcrun", "notarytool", "log", str(submission), *credentials], check=False).stdout if submission else ""
    atomic_write(folder() / "notary-log.json", log.encode())
    if answer.get("status") != "Accepted":
        raise ReleaseError(f"Apple did not accept the programs ({answer.get('status')}); see {folder() / 'notary-log.json'}")
    issues = json.loads(log).get("issues") if log else None
    record["notarization"] = {"id": submission, "status": "Accepted", "issues": issues or []}
    atomic_write(folder() / "signing.json", (json.dumps(record, indent=2) + "\n").encode())
    return record


def main() -> None:
    if sys.platform != "darwin":
        raise ReleaseError("Mac programs are signed on a Mac")
    command = sys.argv[1] if len(sys.argv) == 2 else ""
    if command == "sign":
        record = sign()
    elif command == "notarize":
        record = notarize()
    else:
        raise ReleaseError("usage: sign_macos.py sign|notarize")
    print(json.dumps(record, indent=2))


if __name__ == "__main__":
    readable_output()
    try:
        main()
    except (ReleaseError, OSError, ValueError, KeyError) as error:
        sys.exit(f"Mac signing failed: {error}")
