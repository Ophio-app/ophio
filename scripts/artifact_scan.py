#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Scan built release files for private terms, home folders and private keys.

The source audit checks what is committed; this checks what is shipped, after
compilers, bundlers and packagers have added their own paths and strings. It
reads the phone app, the Linux download and anything packed inside them (zip,
APK, JAR and AAR members, tar and gzip), and reports each finding by file and
rule only, never by the text it matched.
"""

from __future__ import annotations

import argparse
from collections.abc import Iterator
import gzip
import io
from pathlib import Path
import re
import sys
import tarfile
import zipfile

from release_common import ROOT, ReleaseError
from source_candidate import HOME_EXAMPLE_USERS, HOME_USER, PRIVATE_KEY, private_terms, redact

# Nested archives are opened up to this depth; deeper ones are a finding.
DEPTH = 4
ZIPS = {".zip", ".apk", ".jar", ".aar", ".aab"}
# A compiled source root under temporary staging, not ordinary runtime files in
# /tmp or Android's /data/local/tmp. Rust may concatenate literals directly
# before /tmp and directly after the source directory, without separators.
# Cap each component and the number of components to keep malformed binaries
# from making an unbounded search, while allowing deeply nested staging roots.
TEMP_BUILD_ROOT = re.compile(
    rb"(?<!/data/local)(?:/tmp/|/var/tmp/)"
    rb"(?:[^\x00-\x1f/\\\"'`<>]{1,255}/){1,128}"
    rb"(?:client/src-tauri|packaging/android/project|crates/[A-Za-z0-9_-]+)"
)


def bytes_pattern(private: re.Pattern[str]) -> re.Pattern[bytes]:
    """The same terms, matched in raw bytes: binaries are not valid UTF-8."""
    return re.compile(private.pattern.encode(), re.IGNORECASE)


def content_rules(data: bytes, private: re.Pattern[str]) -> set[str]:
    rules = set()
    if bytes_pattern(private).search(data):
        rules.add("private-term")
    # A home prefix with no user after it is a string in code that hides them.
    if any(m[1] and m[1] not in HOME_EXAMPLE_USERS for m in HOME_USER.finditer(data)):
        rules.add("absolute-home-path")
    if PRIVATE_KEY.search(data):
        rules.add("private-key-block")
    if TEMP_BUILD_ROOT.search(data):
        rules.add("absolute-build-path")
    return rules


def shown_path(name: str, private: re.Pattern[str]) -> str:
    # Home paths need masking even when their user isn't a private term.
    shown = HOME_USER.sub(b"[home]", redact(name, private).encode()).decode()
    return "".join(c if c.isprintable() else repr(c)[1:-1] for c in shown)


def scan_bytes(name: str, data: bytes, private: re.Pattern[str], depth: int = 0,
               *, member_name: str | None = None) -> list[dict]:
    shown = shown_path(name, private)
    hits: list[dict] = []
    # Only this level's own name: its parents were checked on the way in.
    own_name = name if member_name is None else member_name
    if private.search(own_name):
        hits.append({"path": shown, "rule": "private-term-in-name"})
    if "absolute-home-path" in content_rules(own_name.encode(), private):
        hits.append({"path": shown, "rule": "absolute-home-path"})
    # The envelope matters too: ZIP comments, extra fields and APK signing
    # blocks are outside extracted files. UTF-16 strings occur in Android
    # resources and native binaries, at either byte alignment.
    rules = content_rules(data, private)
    if b"\0" in data:
        for encoding in ("utf-16le", "utf-16be"):
            for offset in (0, 1):
                text = data[offset:].decode(encoding, errors="ignore")
                rules.update(content_rules(text.encode(), private))
    hits.extend({"path": shown, "rule": rule} for rule in sorted(rules))
    try:
        inner = members(name, data)
        if inner is not None:
            if depth >= DEPTH:
                return hits + [{"path": shown, "rule": "archive-too-deep"}]
            for member, content in inner:
                hits.extend(scan_bytes(f"{name}!{member}", content, private, depth + 1,
                                       member_name=member))
    except ReleaseError:
        raise
    except Exception:
        # Archive exceptions may include an unredacted member name. Do not
        # echo them, including in exception chains or unexpected format errors.
        raise ReleaseError(f"Cannot read archive: {shown}") from None
    return hits


def members(name: str, data: bytes) -> Iterator[tuple[str, bytes]] | None:
    """Archive files and metadata, or None when data is not an archive.

    Recognition uses contents too: nested archives need not have extensions.
    Yield lazily so the depth limit is checked before any members are read.
    """
    lower = name.lower().rsplit("!", 1)[-1]
    if Path(lower).suffix in ZIPS or zipfile.is_zipfile(io.BytesIO(data)):
        def zip_members() -> Iterator[tuple[str, bytes]]:
            with zipfile.ZipFile(io.BytesIO(data)) as archive:
                for info in archive.infolist():
                    if info.is_dir():
                        yield info.filename + "!<metadata>", b""
                    else:
                        yield info.filename, archive.read(info)
        return zip_members()
    if lower.endswith((".tar.gz", ".tgz", ".tar")) or tarfile.is_tarfile(io.BytesIO(data)):
        def tar_members() -> Iterator[tuple[str, bytes]]:
            with tarfile.open(fileobj=io.BytesIO(data)) as archive:
                for info in archive:
                    # Directories, links, owner/group and PAX headers can carry
                    # private paths too, even when every regular file is clean.
                    metadata = "\n".join([info.linkname, info.uname, info.gname,
                                          *info.pax_headers.keys(), *info.pax_headers.values()])
                    yield info.name + "!<metadata>", metadata.encode()
                    if info.isfile():
                        content = archive.extractfile(info)
                        assert content is not None
                        yield info.name, content.read()
                yield "<metadata>", repr(archive.pax_headers).encode()
        return tar_members()
    if lower.endswith(".gz") or data[:2] == b"\x1f\x8b":
        def gzip_members() -> Iterator[tuple[str, bytes]]:
            # Keep the original case; the name still needs a private-term check.
            child = name.rsplit("!", 1)[-1].rsplit("/", 1)[-1]
            yield child[:-3] if lower.endswith(".gz") else "<gzip>", gzip.decompress(data)
        return gzip_members()
    return None


def scan(paths: list[Path], root: Path = ROOT) -> list[dict]:
    private = private_terms(root, required=True)
    assert private is not None
    hits = []
    for path in paths:
        try:
            data = path.read_bytes()
        except OSError:
            raise ReleaseError(f"Cannot read file: {shown_path(path.name, private)}") from None
        hits.extend(scan_bytes(path.name, data, private))
    return hits


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("files", type=Path, nargs="+", help="built release files to scan")
    args = parser.parse_args()
    hits = scan(args.files)
    for hit in hits:
        print(f"{hit['rule']}: {hit['path']}")
    if hits:
        print(f"{len(hits)} findings in {len(args.files)} files.")
        return 1
    print(f"No private terms, home folders or private keys in {len(args.files)} files.")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except ReleaseError as error:
        sys.exit(f"Artifact scan failed: {error}")
    except Exception:
        sys.exit("Artifact scan failed: could not read the inputs.")
