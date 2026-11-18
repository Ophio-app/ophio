#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Explicit, optional Azure Artifact Signing of an already packaged Windows ZIP.

Run on Windows with Python 3, Microsoft's SDK SignTool (10.0.2261.755 or newer),
.NET 8, the Visual C++ runtime, and the matching-architecture Artifact Signing
client plugin (Microsoft.ArtifactSigning.Client / Azure.CodeSigning.Dlib.dll).
No tools are downloaded, no login is started, and ordinary packaging never calls
this script. The selected Azure credential must already be ready to use.

Both scripts/package-windows.sh and the CI package-windows stage produce the
input ZIP in dist/windows. The original ZIP and build outputs are never changed.
Signing incurs Azure usage: run only after the account owner approves it.

Official invocation, prerequisites, metadata, and timestamp service:
https://learn.microsoft.com/azure/artifact-signing/how-to-signing-integrations
Official verification switches and exit codes:
https://learn.microsoft.com/windows/win32/seccrypto/signtool
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import stat
import subprocess
import sys
import tempfile
from urllib.parse import urlsplit
import zipfile


TIMESTAMP_URL = "http://timestamp.acs.microsoft.com"
CREDENTIALS = {
    "azure-cli": "AzureCliCredential",
    "environment": "EnvironmentCredential",
    "workload-identity": "WorkloadIdentityCredential",
    "managed-identity": "ManagedIdentityCredential",
}
# Keep credential selection noninteractive and deterministic. These names are
# supported by the client's documented ExcludeCredentials metadata field.
ALL_CREDENTIALS = (
    "EnvironmentCredential", "WorkloadIdentityCredential", "ManagedIdentityCredential",
    "SharedTokenCacheCredential", "VisualStudioCredential", "VisualStudioCodeCredential",
    "AzureCliCredential", "AzurePowerShellCredential", "AzureDeveloperCliCredential",
    "InteractiveBrowserCredential",
)
MAX_MEMBER_BYTES = 512 * 1024 * 1024
MAX_ARCHIVE_BYTES = 2 * 1024 * 1024 * 1024


def checksums(contents: dict[str, bytes]) -> bytes:
    return "".join(
        f"{hashlib.sha256(data).hexdigest()}  {name}\n"
        for name, data in sorted(contents.items()) if name != "SHA256SUMS"
    ).encode("utf-8")


def reserved_on_windows(component: str) -> bool:
    # Match the shared Windows library rule: devices are recognized before the
    # first dot/colon, ignoring spaces there and including superscript digits.
    stem = re.split(r"[.:]", component, maxsplit=1)[0].rstrip(" ").upper()
    return stem in {"CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"} or bool(
        re.fullmatch(r"(?:COM|LPT)[1-9¹²³]", stem)
    )


def read_package(path: Path) -> tuple[str, dict[str, bytes], dict[str, zipfile.ZipInfo]]:
    """Read, never extract, archive paths; reject ambiguous Windows filenames."""
    contents, entries = {}, {}
    seen, directories = set(), set()
    package = None
    total = 0
    with zipfile.ZipFile(path) as archive:
        members = archive.infolist()
        if len(members) > 10000:
            raise ValueError("Too many archive members")
        for member in members:
            name = member.filename
            parts = name.split("/")
            if (
                len(parts) < 2 or "\\" in name or "\x00" in name
                or any(
                    not part or part in {".", ".."} or part.endswith((".", " "))
                    or re.search(r'[<>:"|?*\x00-\x1f]', part)
                    or reserved_on_windows(part)
                    for part in parts
                )
                or stat.S_IFMT(member.external_attr >> 16) not in {0, stat.S_IFREG}
                or member.flag_bits & 1
            ):
                raise ValueError(f"Unsafe archive member: {name!r}")
            key = name.casefold()
            parents = {"/".join(parts[:index]).casefold() for index in range(1, len(parts))}
            if key in seen:
                raise ValueError("Duplicate or case-colliding archive member")
            if key in directories or parents & seen:
                raise ValueError("Archive file/directory collision")
            seen.add(key)
            directories.update(parents)
            if package is None:
                package = parts[0]
            if package != parts[0]:
                raise ValueError("Expected a single package directory")
            total += member.file_size
            if member.file_size > MAX_MEMBER_BYTES or total > MAX_ARCHIVE_BYTES:
                raise ValueError("Archive exceeds signing size limits")
            relative = "/".join(parts[1:])
            contents[relative] = archive.read(member)
            entries[relative] = member
    if not {"ophio.exe", "computer-mcp.exe", "SHA256SUMS"}.issubset(contents):
        raise ValueError("Expected the packaged Windows companion ZIP with SHA256SUMS")
    if contents["SHA256SUMS"] != checksums(contents):
        raise ValueError("Package SHA256SUMS does not match its contents")
    for name, data in contents.items():
        if is_binary(name):
            offset = int.from_bytes(data[60:64], "little")
            if (
                len(data) < 64 or data[:2] != b"MZ" or offset < 64
                or data[offset:offset + 4] != b"PE\0\0"
            ):
                raise ValueError(f"Expected a PE binary: {name}")
    return package, contents, entries


def is_binary(name: str) -> bool:
    return PurePosixPath(name).suffix.lower() in {".exe", ".dll"}


def invoke(command: list[str]) -> None:
    # SignTool code 2 is a warning, NOT success. In particular /tw makes a
    # missing timestamp a warning, and timestamp-server failures also warn.
    subprocess.run(command, check=True, timeout=300, stdin=subprocess.DEVNULL)


def verify_contents(contents: dict[str, bytes], signtool: Path, scratch: Path) -> int:
    count = 0
    for name, data in sorted(contents.items()):
        if not is_binary(name):
            continue
        binary = scratch / f"verify-{count}{PurePosixPath(name).suffix.lower()}"
        binary.write_bytes(data)
        print(f"Verifying embedded signature and trusted timestamp: {name}", flush=True)
        # /pa verifies Authenticode trust and revocation; /all checks every
        # signature; /tw rejects missing timestamps via warning exit code 2.
        # Do not use /a: that could accept a catalog rather than an embedded signature.
        invoke([str(signtool), "verify", "/pa", "/all", "/tw", "/v", str(binary)])
        count += 1
    return count


def signing_metadata(endpoint: str, account: str, profile: str, credential: str) -> dict:
    parsed = urlsplit(endpoint)
    if (
        parsed.scheme != "https" or parsed.username or parsed.password
        or not re.fullmatch(r"[a-z0-9]+\.codesigning\.azure\.net", parsed.netloc)
        or parsed.path not in {"", "/"} or parsed.query or parsed.fragment
    ):
        raise ValueError("Use the account's regional HTTPS codesigning.azure.net endpoint")
    if not account.strip() or not profile.strip():
        raise ValueError("Account and certificate profile names are required")
    return {
        "Endpoint": endpoint,
        "CodeSigningAccountName": account,
        "CertificateProfileName": profile,
        "ExcludeCredentials": [
            name for name in ALL_CREDENTIALS if name != CREDENTIALS[credential]
        ],
    }


def publish_no_replace(source: Path, destination: Path) -> None:
    if os.name == "nt":
        # Windows rename atomically refuses an existing destination, including
        # on FAT/exFAT. Unlike POSIX rename, it never silently replaces a file.
        os.rename(source, destination)
    else:
        # Preserve no-overwrite semantics in offline orchestration tests run on
        # POSIX. The actual signing command is Windows-only.
        os.link(source, destination)


def preflight_output(output: Path) -> None:
    """Check destination publication BEFORE any potentially paid signing call."""
    try:
        output.parent.mkdir(parents=True, exist_ok=True)
        # FILE_ADD_FILE and FILE_ADD_SUBDIRECTORY are distinct Windows rights.
        # Probe sibling files, just like the final archive, not a child directory.
        descriptor, filename = tempfile.mkstemp(
            dir=output.parent, prefix=".ophio-publication-check-", suffix=".zip",
        )
        probe = Path(filename)
        destination = probe.with_name(f"{probe.stem}-published.zip")
        published = False
        try:
            try:
                os.write(descriptor, b"publication check")
            finally:
                os.close(descriptor)
            publish_no_replace(probe, destination)
            published = True
        finally:
            try:
                probe.unlink(missing_ok=True)
            finally:
                # Never remove a different process's file if publication lost a race.
                if published:
                    destination.unlink(missing_ok=True)
    except OSError as error:
        raise ValueError("Output directory cannot safely publish an archive; choose another destination") from error


def sign_package(
    source: Path, output: Path, signtool: Path, dlib: Path, metadata: dict,
) -> int:
    source, output = source.resolve(), output.resolve()
    if source == output or output.exists():
        raise ValueError("Choose a separate output ZIP that does not already exist")
    package, contents, entries = read_package(source)
    preflight_output(output)
    # Temporary unsigned/signed binaries stay in the operating system's temp
    # folder. Only a completely verified archive is copied to the output folder.
    with tempfile.TemporaryDirectory(prefix="ophio-authenticode-") as directory:
        scratch = Path(directory)
        metadata_path = scratch / "metadata.json"
        metadata_path.write_text(json.dumps(metadata), encoding="utf-8")
        for index, (name, data) in enumerate(sorted(contents.items())):
            if not is_binary(name):
                continue
            binary = scratch / f"sign-{index}{PurePosixPath(name).suffix.lower()}"
            binary.write_bytes(data)
            print(f"Signing with Azure Artifact Signing: {name}", flush=True)
            invoke([
                str(signtool), "sign", "/v", "/fd", "SHA256", "/tr", TIMESTAMP_URL,
                "/td", "SHA256", "/dlib", str(dlib), "/dmdf", str(metadata_path),
                str(binary),
            ])
            contents[name] = binary.read_bytes()
        contents["SHA256SUMS"] = checksums(contents)
        signed = scratch / "signed.zip"
        with zipfile.ZipFile(signed, "w", compression=zipfile.ZIP_DEFLATED) as archive:
            for name, data in sorted(contents.items()):
                original = entries[name]
                info = zipfile.ZipInfo(f"{package}/{name}", original.date_time)
                info.create_system = 3
                info.external_attr = 0o100644 << 16
                archive.writestr(info, data, compress_type=zipfile.ZIP_DEFLATED, compresslevel=9)
        # Verify bytes read back from the FINAL ZIP, not just intermediate files.
        _, final_contents, _ = read_package(signed)
        count = verify_contents(final_contents, signtool, scratch)
        # Publish the completed file atomically without replacing an output
        # created by another process during signing.
        descriptor, filename = tempfile.mkstemp(dir=output.parent, suffix=".zip")
        temporary = Path(filename)
        try:
            with os.fdopen(descriptor, "wb") as temp, signed.open("rb") as stream:
                shutil.copyfileobj(stream, temp)
            publish_no_replace(temporary, output)
        finally:
            # Close handles before deleting, including when copying fails on Windows.
            temporary.unlink(missing_ok=True)
    return count


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "Sign (explicit Azure usage):\n"
            "  py -3 scripts/sign-windows.py unsigned.zip --output signed.zip "
            "--signtool PATH --dlib PATH --endpoint https://eus.codesigning.azure.net "
            "--account ACCOUNT --profile PROFILE\n\n"
            "Verify only (no Azure signing request):\n"
            "  py -3 scripts/sign-windows.py signed.zip --verify-only --signtool PATH\n\n"
            "The account needs completed identity validation, a Public Trust profile, "
            "and the Artifact Signing Certificate Profile Signer role for the selected "
            "identity. Default credential: an ALREADY authenticated Azure CLI. "
            "No interactive login or credential fallback is enabled. Verification "
            "uses Windows trust stores and may access revocation/timestamp chains. "
            "It does not certify SmartScreen reputation or a clean Windows install."
        ),
    )
    result.add_argument("archive", type=Path)
    result.add_argument("--signtool", type=Path, required=True, help="Explicit SDK SignTool.exe path")
    result.add_argument("--verify-only", action="store_true")
    result.add_argument("--output", type=Path)
    result.add_argument("--dlib", type=Path, help="Matching-architecture Azure.CodeSigning.Dlib.dll")
    result.add_argument("--endpoint", help="Account's regional Artifact Signing endpoint")
    result.add_argument("--account", help="Artifact Signing account name")
    result.add_argument("--profile", help="Public Trust certificate profile name")
    result.add_argument("--credential", choices=CREDENTIALS, default="azure-cli")
    return result


def main(argv: list[str] | None = None) -> None:
    cli = parser()
    args = cli.parse_args(argv)
    if sys.platform != "win32":
        cli.error("Run this command on Windows; unsigned packaging remains available on other systems")
    signtool = args.signtool.resolve()
    if not signtool.is_file():
        cli.error("The specified SignTool does not exist")
    if args.verify_only:
        if any((args.output, args.dlib, args.endpoint, args.account, args.profile)):
            cli.error("--verify-only cannot be combined with signing options")
        _, contents, _ = read_package(args.archive)
        with tempfile.TemporaryDirectory(prefix="ophio-authenticode-verify-") as directory:
            count = verify_contents(contents, signtool, Path(directory))
        print(f"Verified {count} PE binaries, their embedded signatures and timestamps.")
    else:
        if not all((args.output, args.dlib, args.endpoint, args.account, args.profile)):
            cli.error("Signing needs --output, --dlib, --endpoint, --account, and --profile")
        dlib = args.dlib.resolve()
        if not dlib.is_file():
            cli.error("The specified Artifact Signing plugin does not exist")
        metadata = signing_metadata(args.endpoint, args.account, args.profile, args.credential)
        count = sign_package(args.archive, args.output, signtool, dlib, metadata)
        print(f"Signed and verified {count} PE binaries. Output: {args.output}")
        print(f"SHA-256: {hashlib.sha256(args.output.read_bytes()).hexdigest()}")


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, zipfile.BadZipFile, subprocess.SubprocessError) as error:
        sys.exit(f"Windows signing/verification failed: {error}")
