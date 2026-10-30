#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Stage an audited, history-free source directory locally. Never publish it."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tarfile
import tempfile

from release_common import ROOT, ReleaseError
from source_candidate import (
    audit_archive,
    is_workflow,
    private_terms,
    redact,
    resolve_revision,
    verify_archive_commit,
    write_archive,
)

# Every top-level name a public export may contain. Anything new is refused
# until it is added here on purpose, so a stray folder never ships by default.
PUBLIC_TOP_LEVEL = frozenset({
    ".cargo", ".github", ".gitattributes", ".gitignore",
    "BUILDING.md", "CHANGELOG.md", "CODE_OF_CONDUCT.md", "CONTRIBUTING.md",
    "Cargo.lock", "Cargo.toml", "LICENSE", "NOTICE", "README.md", "SECURITY.md", "SUPPORT.md",
    "client", "crates", "docs", "fuzz", "packaging", "scripts", "tools",
})


def output_path(path: Path) -> Path:
    """Do not resolve away a symlink or traversal in an operator's output path."""
    path = path.absolute()
    if ".." in path.parts:
        raise ReleaseError("Output paths must not contain parent traversal")
    for part in [*reversed(path.parents), path]:
        if part.is_symlink():
            raise ReleaseError("Output paths must not contain symlinks")
    if not path.parent.is_dir():
        raise ReleaseError("Output parent directories must already exist")
    return path


def check_destination(destination: Path) -> None:
    if destination.exists() and (
        not destination.is_dir() or any(destination.iterdir())
    ):
        raise ReleaseError("The destination must be new or an empty directory")
    probe = subprocess.run(
        ["git", "rev-parse", "--absolute-git-dir"],
        cwd=destination.parent,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        check=False,
    )
    if probe.returncode == 0:
        raise ReleaseError("The destination must be outside existing Git repositories")


def json_bytes(value: object) -> bytes:
    return (json.dumps(value, indent=2, sort_keys=True, ensure_ascii=True) + "\n").encode("utf-8")


def file_sha256(path: Path) -> str:
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def export(root: Path, revision: str, destination: Path, receipt: Path) -> dict:
    """Use one immutable archive for the audit, file copy and content receipt.

    Output parents must be trusted local directories, not concurrently modified
    by another process. No existing file is overwritten, and a failed audit never
    creates the destination or receipt. The receipt is private, outside the tree.
    """
    if not re.fullmatch(r"(?:[0-9a-f]{40}|[0-9a-f]{64})", revision):
        raise ReleaseError("Supply a full lowercase commit object ID, not a branch or tag")
    if resolve_revision(root, revision) != revision:
        raise ReleaseError("The revision must identify a commit, not a tag object")
    destination = output_path(destination)
    receipt = output_path(receipt)
    check_destination(destination)
    if receipt == destination or destination in receipt.parents:
        raise ReleaseError("The private receipt must be outside the exported tree")
    if receipt.exists():
        raise ReleaseError("The receipt must be a new file")
    private = private_terms(root, required=True)
    with tempfile.TemporaryDirectory(prefix="source-export-") as scratch:
        archive_path = Path(scratch) / "source.tar"
        committed = write_archive(root, revision, archive_path)
        with tarfile.open(archive_path) as archive:
            hits = audit_archive(archive, private, omit_workflows=True)
            if hits:
                raise ReleaseError(
                    f"Source audit refused the export: {len(hits)} findings. "
                    "Run source_candidate.py for the redacted audit report."
                )
            expected = verify_archive_commit(archive, committed, omit_workflows=True)
            members = [m for m in archive.getmembers() if not is_workflow(m.name)]
            if not expected:
                raise ReleaseError("Refusing an empty source export")
            unlisted = sorted({m.name.split("/", 1)[0] for m in members} - PUBLIC_TOP_LEVEL)
            if unlisted:
                raise ReleaseError(
                    "Not on the public top-level list: "
                    + ", ".join(redact(name, private) for name in unlisted)
                )
            # The staging directory is private and contains only audited regular
            # files. Never use extractall(), follow archive links, or copy a checkout.
            with tempfile.TemporaryDirectory(
                prefix=".source-export-", dir=destination.parent
            ) as staging:
                stage = Path(staging) / "source"
                stage.mkdir(mode=0o700)
                files = []
                for member in members:
                    target = stage / member.name
                    if member.isdir():
                        target.mkdir(parents=True, exist_ok=True)
                        continue
                    stream = archive.extractfile(member)
                    if stream is None:
                        raise ReleaseError("Unreadable source archive member")
                    data = stream.read()
                    mode, blob = expected[member.name]
                    target.parent.mkdir(parents=True, exist_ok=True)
                    with target.open("xb") as output:
                        output.write(data)
                    permissions = 0o755 if mode == "100755" else 0o644
                    target.chmod(permissions)
                    files.append({
                        "path": member.name,
                        "mode": f"{permissions:04o}",
                        "size": len(data),
                        "sha256": hashlib.sha256(data).hexdigest(),
                        "git_blob": blob,
                    })
                directories = sorted(
                    path.relative_to(stage).as_posix()
                    for path in stage.rglob("*") if path.is_dir()
                )
                for directory in directories:
                    (stage / directory).chmod(0o755)
                inventory = {"directories": directories, "files": sorted(files, key=lambda f: f["path"])}
                report = {
                    "schema": 1,
                    "revision": revision,
                    "archive_sha256": file_sha256(archive_path),
                    "audit_tool_sha256": file_sha256(Path(__file__).with_name("source_candidate.py")),
                    "export_tool_sha256": file_sha256(Path(__file__)),
                    "inventory_sha256": hashlib.sha256(json_bytes(inventory)).hexdigest(),
                    "private_terms_checked": private is not None,
                    "audit_findings": [],
                    "omitted_workflows": sorted(path for path in committed if is_workflow(path)),
                    "root_mode": "0700",
                    **inventory,
                }
                # Recheck immediately before installing. rename refuses a nonempty
                # directory; no merge, cleanup of existing contents or force option.
                output_path(destination)
                check_destination(destination)
                output_path(receipt)
                descriptor = os.open(receipt, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
                try:
                    with os.fdopen(descriptor, "wb") as output:
                        output.write(json_bytes(report))
                    stage.rename(destination)
                except BaseException:
                    receipt.unlink()
                    raise
    return report


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--revision", required=True, help="full immutable commit object ID")
    parser.add_argument("--destination", type=Path, required=True, help="new or empty local directory")
    parser.add_argument("--receipt", type=Path, required=True, help="new private JSON file outside the export")
    args = parser.parse_args()
    report = export(ROOT, args.revision, args.destination, args.receipt)
    print(f"Prepared {len(report['files'])} files from commit {report['revision']}.")
    print(f"Inventory SHA-256: {report['inventory_sha256']}")
    print("No Git history, workflows, remote, or publication was created.")
    print("Pattern checks are not proof that the source contains no secrets. Keep the receipt private.")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, ValueError, ReleaseError, subprocess.CalledProcessError, tarfile.TarError) as error:
        sys.exit(f"Source export failed: {error}")
