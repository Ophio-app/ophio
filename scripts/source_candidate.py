#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Audit the committed public export, never working-tree/private file contents."""

from __future__ import annotations

import argparse
import fnmatch
import hashlib
import json
from pathlib import Path, PurePosixPath
import re
import subprocess
import sys
import tarfile
import tempfile
import tomllib

from release_common import ROOT, ReleaseError

# A fixed legacy-name fingerprint is not the name itself. Check every byte
# window so substrings, mixed case and names embedded in paths remain detected.
# The private Git-only term list supplies further names for trusted audits.
LEGACY_NAME_LENGTH = 10
LEGACY_NAME_SHA256 = "8f32baa49f0aab4e383e8df94b827415706382ab36a80de0bb2c08d9746d8cce"


def legacy_name_offsets(data: bytes):
    lowered = data.lower()
    for offset in range(len(lowered) - LEGACY_NAME_LENGTH + 1):
        if (hashlib.sha256(lowered[offset:offset + LEGACY_NAME_LENGTH]).hexdigest()
                == LEGACY_NAME_SHA256):
            yield offset


def has_legacy_name(data: bytes) -> bool:
    return next(legacy_name_offsets(data), None) is not None

HOME_PREFIX = ("/" + "home/").encode()
MAC_HOME_PREFIX = ("/" + "Users/").encode()
# Linux, macOS and Windows home directories, with the user captured. A Windows
# separator may be escaped, as it is inside string literals.
HOME_USER = re.compile(
    rb"(?:"
    + re.escape(HOME_PREFIX)
    + rb"|"
    + re.escape(MAC_HOME_PREFIX)
    + rb"|\b[A-Za-z]:(?:\\{1,2}|/)Users(?:\\{1,2}|/))([^/\\\s\"'`$<>]*)"
)
PRIVATE_KEY = re.compile(rb"-----BEGIN (?:[A-Z0-9]+ )*PRIVATE KEY-----")
PRIVATE_REFERENCE = re.compile(rb"(?<![A-Za-z0-9_-])(?:planning|records)/", re.I)
FORBIDDEN_DIRS = {
    ".git",
    ".claude",
    "planning",
    "records",
    "graphify-out",
    ".build-policy",
    ".lanes",
    "review-notes",
    ".reviews",
}
FORBIDDEN_FILES = {"claude.md", "private-readme.md"}
# A file named after a work item such as U32 or m2 says how it was made, not
# what it covers. These technical names only look like work items.
WORK_ITEM = re.compile(r"[A-Za-z]{1,2}[0-9]{1,2}")
TECHNICAL_NAMES = {"x11", "x64", "x86"}
# Notes on how the product was built rather than what it does: its parallel
# work streams and their owners, reviewing sessions, and hand-offs to whoever
# integrates the work.
BUILD_NOTE = re.compile(
    r"\b(?:(?:this|that|another|other|owning|sibling|concurrent|parallel)[-\s]"
    r"(?:build[-\s])?lanes?|build[-\s]lanes?|lane[-\s](?:local|owned)"
    r"|(?:opus|sonnet|haiku|fable)[-\s]owned|review\s(?:pass(?:es)?|sessions?|rounds?)"
    r"|checkpoint\sreview|(?:another|other)\sworkers?|the\sintegrator\s(?:must|should)"
    r"|supplied\sbuild|build\sowner)\b",
    re.IGNORECASE,
)
SPDX = b"SPDX-License-Identifier: Apache-2.0"
# This ceiling belongs to the trusted audit tool, not the candidate's policy.
# A candidate may request fewer example exemptions, but cannot add real users.
HOME_EXAMPLE_USERS = frozenset({b"sam", b"you"})


def private_terms(root: Path, *, required: bool = False) -> re.Pattern[str] | None:
    """Load private terms from Git metadata, never from committed source.

    Contributors may run a local audit without the list. Official audits and
    exports must request it and fail closed when it is absent or empty. A UTF-8
    byte-order mark at the start is ignored, before parsing comments and terms.
    """
    common = subprocess.check_output(
        ["git", "rev-parse", "--git-common-dir"], cwd=root, encoding="utf-8"
    ).strip()
    try:
        lines = (root / common / "info/private-terms").read_text(encoding="utf-8-sig")
    except FileNotFoundError:
        if required:
            raise ReleaseError("Required private-term list is missing") from None
        return None
    terms = [
        term.strip()
        for term in lines.splitlines()
        if term.strip() and not term.lstrip().startswith("#")
    ]
    if not terms:
        if required:
            raise ReleaseError("Required private-term list is empty")
        return None
    alternatives = "|".join(map(re.escape, sorted(terms, key=len, reverse=True)))
    return re.compile(rf"(?<![^\W_])(?:{alternatives})(?![^\W_])", re.IGNORECASE)


def redact(path: str, private: re.Pattern[str] | None) -> str:
    """Mask both Git-only terms and the fixed legacy marker in reported paths."""
    raw = (private.sub("[private]", path) if private else path).encode("utf-8")
    offsets = list(legacy_name_offsets(raw))
    if not offsets:
        return raw.decode("utf-8")
    masked = bytearray()
    cursor = 0
    for offset in offsets:
        if offset < cursor:
            continue
        masked.extend(raw[cursor:offset])
        masked.extend(b"[private]")
        cursor = offset + LEGACY_NAME_LENGTH
    masked.extend(raw[cursor:])
    return masked.decode("utf-8")


def work_item_name(part: str) -> bool:
    """Whether a path part is named after a work item, ignoring its extension.

    A version qualifier after another word, as in `mipmap-anydpi-v26`, is not.
    """
    words = re.split(r"[-_]+", part.split(".")[0])
    return any(
        WORK_ITEM.fullmatch(word)
        and word.lower() not in TECHNICAL_NAMES
        and not (index and re.fullmatch(r"v[0-9]+", word, re.I))
        for index, word in enumerate(words)
    )


def inspect(
    path: str,
    data: bytes,
    exemptions: list[dict],
    home_examples: frozenset[bytes] = frozenset(),
    private: re.Pattern[str] | None = None,
) -> list[dict]:
    """Return every location/rule, without echoing potentially private content.

    A home directory is a finding unless its user is a requested example that
    this audit tool permits. Candidate policy cannot expand that fixed set.
    A private term is reported by rule and location only, never by the term it
    matched, and a path that contains one is reported with the term masked.
    """
    home_examples = home_examples & HOME_EXAMPLE_USERS
    hits = []
    shown = redact(path, private)

    def hit(rule: str, line: int = 0) -> None:
        hits.append({"path": shown, "line": line, "rule": rule})

    parts = PurePosixPath(path).parts
    lower = path.lower()
    if any(p.lower() in FORBIDDEN_DIRS for p in parts):
        hit("private-directory")
    if any(p.lower() in FORBIDDEN_FILES for p in parts):
        hit("private-document")
    if any(
        re.fullmatch(
            r"(?:review|review[-_]?notes?|.*[-_]review|review[-_].*)\.(?:md|txt|json)",
            p,
            re.I,
        )
        for p in parts
    ):
        hit("review-notes")
    if has_legacy_name(lower.encode()):
        hit("internal-codename-filename")
    if private and private.search(path):
        hit("private-term")
    if any(work_item_name(p) for p in parts):
        hit("work-item-filename")
    if any(p.lower() == ".env" or p.lower().startswith(".env.") for p in parts):
        hit("environment-file")
    if PurePosixPath(lower).suffix in {".keystore", ".jks", ".p12", ".pfx", ".key"}:
        hit("key-material-filename")
    for number, line in enumerate(data.splitlines(), 1):
        if has_legacy_name(line):
            hit("internal-codename-content", number)
        if any(m[1] not in home_examples for m in HOME_USER.finditer(line)):
            hit("absolute-home-path", number)
        if PRIVATE_KEY.search(line):
            hit("private-key-block", number)
        if PRIVATE_REFERENCE.search(line):
            hit("private-tree-reference", number)
        text = line.decode("utf-8", "replace")
        if private and private.search(text):
            hit("private-term", number)
        if BUILD_NOTE.search(text):
            hit("build-process-note", number)
    owned = (
        (path.startswith("crates/") and path.endswith(".rs"))
        or (
            path.startswith("client/src/")
            and PurePosixPath(path).suffix in {".ts", ".tsx", ".css"}
        )
        or (
            path.startswith("scripts/")
            and PurePosixPath(path).suffix in {".py", ".sh", ".mjs", ".js"}
        )
    )
    if owned and SPDX not in b"\n".join(data.splitlines()[:12]):
        exempt = any(
            fnmatch.fnmatchcase(path, row["path"])
            and row["marker"].encode() in data[:1024]
            for row in exemptions
        )
        if not exempt:
            hit("missing-apache-spdx-header")
    return hits


def resolve_revision(root: Path, revision: str) -> str:
    """Resolve once to a commit; callers must use the returned object ID."""
    return subprocess.check_output(
        ["git", "--no-replace-objects", "rev-parse", "--verify", "--end-of-options", f"{revision}^{{commit}}"],
        cwd=root,
        encoding="utf-8",
    ).strip()


def write_archive(root: Path, revision: str, destination: Path) -> dict[str, tuple[str, str]]:
    # Git archives silently omit submodule contents. Refuse an incomplete export.
    entries = subprocess.check_output(
        ["git", "--no-replace-objects", "ls-tree", "-r", "-z", revision], cwd=root
    )
    files = {}
    for entry in entries.split(b"\0"):
        if not entry:
            continue
        metadata, path = entry.split(b"\t", 1)
        mode, kind, object_id = metadata.decode("ascii").split()
        if kind != "blob":
            raise ReleaseError("Source archives cannot include submodules")
        files[path.decode("utf-8")] = (mode, object_id)
    with destination.open("xb") as output:
        subprocess.run(
            ["git", "--no-replace-objects", "archive", "--format=tar", revision],
            cwd=root,
            stdout=output,
            check=True,
        )
    return files


def safe_archive_path(path: str) -> bool:
    """Require a canonical relative name, also safe to copy to Windows."""
    return bool(path) and not any(
        char in path for char in ("\\", ":", "\0")
    ) and all(
        part not in {"", ".", ".."}
        and not part.endswith((".", " "))
        and not re.fullmatch(r"(?:con|prn|aux|nul|com[1-9]|lpt[1-9])(?:\..*)?", part, re.I)
        and not any(ord(char) < 32 or ord(char) == 127 for char in part)
        for part in path.split("/")
    )


def is_workflow(path: str) -> bool:
    return path.casefold() == ".github/workflows" or path.casefold().startswith(
        ".github/workflows/"
    )


def verify_archive_commit(
    archive: tarfile.TarFile,
    committed: dict[str, tuple[str, str]],
    *,
    omit_workflows: bool = False,
) -> dict[str, tuple[str, str]]:
    """Verify complete, unchanged commit bytes after the archive audit passes.

    Both the standalone audit and exporter use this check. Git attributes may
    omit or rewrite archive contents, so scanning the archive alone is not
    evidence that the committed source passed. Errors never echo source paths.
    """
    expected = {
        path: entry for path, entry in committed.items()
        if not (omit_workflows and is_workflow(path))
    }
    members = [
        member for member in archive.getmembers()
        if not (omit_workflows and is_workflow(member.name)) and not member.isdir()
    ]
    mismatch = "Archive differs from the commit; check export attributes"
    if len(members) != len(expected) or {m.name for m in members} != set(expected):
        raise ReleaseError(mismatch)
    for member in members:
        mode, blob = expected[member.name]
        if not member.isfile() or mode not in {"100644", "100755"}:
            raise ReleaseError(mismatch)
        stream = archive.extractfile(member)
        if stream is None:
            raise ReleaseError("Unreadable source archive member")
        data = stream.read()
        algorithm = "sha1" if len(blob) == 40 else "sha256"
        digest = hashlib.new(algorithm)
        digest.update(f"blob {len(data)}\0".encode("ascii"))
        digest.update(data)
        if digest.hexdigest() != blob:
            raise ReleaseError(mismatch)
    return expected


def audit_archive(
    archive: tarfile.TarFile,
    private: re.Pattern[str] | None = None,
    *,
    omit_workflows: bool = False,
) -> list[dict]:
    """Audit the very archive used by the exporter, never extract untrusted links."""
    hits = []
    members = []
    names: set[str] = set()
    for member in archive.getmembers():
        path = member.name
        rule = None
        if not safe_archive_path(path):
            rule = "unsafe-archive-path"
        elif path.casefold() in names:
            rule = "duplicate-archive-path"
        names.add(path.casefold())
        if rule:
            hits.append({"path": redact(path, private), "line": 0, "rule": rule})
        elif not (omit_workflows and is_workflow(path)):
            members.append(member)

    exemptions = []
    home_examples: frozenset[bytes] = frozenset()
    policy_member = next(
        (m for m in members if m.name == "packaging/source-policy.toml"), None
    )
    if policy_member:
        if not policy_member.isfile() or policy_member.size > 64 * 1024 * 1024:
            raise ReleaseError("Invalid source policy")
        stream = archive.extractfile(policy_member)
        if stream is None:
            raise ReleaseError("Invalid source policy")
        policy = tomllib.loads(stream.read().decode())
        exemptions = policy["generated"]
        examples = policy.get("home_examples", {})
        if examples.get("users") and not examples.get("reason", "").strip():
            raise ReleaseError("Example home directories need a reason")
        home_examples = frozenset(user.encode() for user in examples.get("users", []))
        for row in exemptions:
            if not row.get("reason", "").strip() or not row.get("marker", "").strip():
                raise ReleaseError(
                    "Every generated-source exception needs a reason and marker"
                )
    for member in members:
        path = member.name
        if member.isdir():
            hits.extend(inspect(path, b"", exemptions, home_examples, private))
            continue
        if not member.isfile():
            hits.append(
                {"path": redact(path, private), "line": 0, "rule": "non-regular-archive-member"}
            )
            continue
        if member.size > 64 * 1024 * 1024:
            hits.append(
                {"path": redact(path, private), "line": 0, "rule": "unexpected-large-source-file"}
            )
            continue
        stream = archive.extractfile(member)
        if stream is None:
            raise ReleaseError("Unreadable source archive member")
        hits.extend(inspect(path, stream.read(), exemptions, home_examples, private))
    return sorted(hits, key=lambda h: (h["path"], h["line"], h["rule"]))


def audit(root: Path, revision: str = "HEAD", *, require_private_terms: bool = False) -> tuple[str, list[dict]]:
    revision = resolve_revision(root, revision)
    private = private_terms(root, required=require_private_terms)
    with tempfile.TemporaryDirectory(prefix="source-candidate-") as directory:
        archive_path = Path(directory) / "source.tar"
        committed = write_archive(root, revision, archive_path)
        with tarfile.open(archive_path) as archive:
            hits = audit_archive(archive, private)
            if not hits:
                verify_archive_commit(archive, committed)
    return revision, hits


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--json", type=Path, help="also save the full machine-readable report"
    )
    parser.add_argument("--revision", default="HEAD", help="commit to audit (default: HEAD)")
    parser.add_argument("--require-private-terms", action="store_true", help="fail closed unless the private Git-only term list is present")
    args = parser.parse_args()
    revision, hits = audit(ROOT, args.revision, require_private_terms=args.require_private_terms)
    print(f"Source candidate commit {revision}")
    for item in hits:
        print(f"{item['path']}:{item['line']}: {item['rule']}")
    print(f"{'FAIL' if hits else 'PASS'}: {len(hits)} findings")
    if args.json:
        args.json.parent.mkdir(parents=True, exist_ok=True)
        args.json.write_text(
            json.dumps({"revision": revision, "findings": hits}, indent=2) + "\n",
            encoding="utf-8",
        )
    return int(bool(hits))


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (
        OSError,
        ValueError,
        ReleaseError,
        subprocess.CalledProcessError,
        tarfile.TarError,
    ) as error:
        sys.exit(f"Source candidate audit failed: {error}")
