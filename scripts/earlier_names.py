#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Fail when tracked files use the identity the product had before it was named.

The earlier names may appear only in the code that carries an earlier install
across (its directories, login service, Library files and object links), in
that code's tests, and in the instructions for upgrading such an install.
"""

from __future__ import annotations

import fnmatch
from pathlib import Path
import re
import subprocess
import sys

from release_common import ROOT

# Assembled from parts so that this file does not match its own rules.
EARLIER = "compan" + "ion"
# The earlier program's subcommands, as people typed them in examples.
SUBCOMMANDS = (
    "run", "voice", "channels", "pair", "status", "inbox", "devices",
    "projects", "roots", "doctor", "desktop", "retention", "service",
    "rotate-certificate", "uninstall-data", "stop", "stop-task",
)
RULES = {
    "android-namespace": re.compile(rf"\bdev\.{EARLIER}\."),
    "link-scheme": re.compile(rf"\b{EARLIER}(?:-object)?://"),
    "variable": re.compile(rf"{EARLIER.upper()}_"),
    "library-prefix": re.compile(rf"\.{EARLIER}-"),
    "directory": re.compile(rf"\.(?:config|local/(?:share|opt|bin))/{EARLIER}\b"),
    "command": re.compile(
        rf"\b{EARLIER}(?:\.exe)? (?:{'|'.join(map(re.escape, SUBCOMMANDS))})\b"
    ),
}
# The name people saw, anywhere but the generated bindings. A Rust path such as
# `EventSource::Companion` names a protocol value, not the product.
DISPLAY_NAME = re.compile(rf"(?<!::)\b{EARLIER.capitalize()}\b")
GENERATED = "client/src/protocol/generated/*"

# Each exception is compatibility with an earlier install, and nothing else.
ALLOWED = {
    "crates/companion/src/install.rs": "replaces the earlier program's managed service unit",
    "crates/companion/tests/upgrade.rs": "upgrades an earlier install end to end",
    "crates/companion/src/update.rs": "replaces the earlier command's link, and tests that",
    "packaging/linux/INSTALL.md": "explains upgrading an earlier install",
    "docs/upgrading.md": "explains upgrading an earlier install",
    "crates/library/src/lib.rs": "recognises Library files an earlier version left",
    "crates/library/src/tests.rs": "tests recovering those Library files",
    "crates/library/tests/filesystem.rs": "tests hiding and cleaning up those Library files",
    "crates/library/SAFETY.md": "documents those Library files",
    "client/src/screens/library/Folder.tsx": "hides save backups an earlier version left",
    "client/src/screens/library/recovery.test.tsx": "tests hiding those backups",
    "client/src/platform/links.ts": "opens object links made before the rename",
    "client/src-tauri/plugins/device/android/src/main/java/AppLinks.kt": "accepts object links made before the rename",
    "crates/channels/src/context_links.rs": "checks that phone and app agree on the earlier link prefix",
    "docs/app-links.md": "documents object links made before the rename",
    "crates/protocol/src/event.rs": "keeps the event source's wire and stored name, which earlier installs' journals hold",
}


def inspect(path: str, text: str) -> list[dict]:
    """Every line and rule where `path` uses an earlier name."""
    rules = dict(RULES)
    if not fnmatch.fnmatchcase(path, GENERATED):
        rules["display-name"] = DISPLAY_NAME
    return [
        {"path": path, "line": number, "rule": rule}
        for number, line in enumerate(text.splitlines(), 1)
        for rule, pattern in rules.items()
        if pattern.search(line)
    ]


def audit(root: Path) -> list[dict]:
    """Uses of earlier names outside ALLOWED, and exceptions no longer needed."""
    tracked = subprocess.check_output(["git", "ls-files", "-z"], cwd=root)
    hits = []
    unused = set(ALLOWED)
    for name in filter(None, tracked.decode().split("\0")):
        path = root / name
        if path.is_symlink() or not path.is_file():
            continue
        data = path.read_bytes()
        if b"\0" in data:
            continue
        found = inspect(name, data.decode(errors="replace"))
        if name in ALLOWED:
            if found:
                unused.discard(name)
        else:
            hits.extend(found)
    hits.extend({"path": path, "line": 0, "rule": "unneeded-exception"} for path in sorted(unused))
    return hits


def main() -> int:
    hits = audit(ROOT)
    for hit in hits:
        print(f"{hit['path']}:{hit['line']}: {hit['rule']}")
    print(f"Earlier names {'FAIL' if hits else 'PASS'}: {len(hits)} findings")
    return int(bool(hits))


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, subprocess.CalledProcessError) as error:
        sys.exit(f"Earlier-name check failed: {error}")
