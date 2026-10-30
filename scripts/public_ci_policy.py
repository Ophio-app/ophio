#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Reject GitHub workflow infrastructure in a plain public-beta source export.

This checks files only. GitHub Actions must separately be disabled, with no
private runner access, before importing the export into its new repository.
Run this checker from a trusted checkout, not from a contributor's revision.
"""
from pathlib import Path
import argparse
import stat
import sys


def check_public_tree(root: Path) -> None:
    """Raise ValueError or OSError rather than treating an unreadable tree as safe."""
    if not stat.S_ISDIR(root.lstat().st_mode):
        raise ValueError("The export root must be a real directory, not a symlink.")
    github = root / ".github"
    try:
        mode = github.lstat().st_mode
    except FileNotFoundError:
        return
    if not stat.S_ISDIR(mode):
        raise ValueError("The export's .github path must be a real directory, not a symlink.")
    try:
        (github / "workflows").lstat()
    except FileNotFoundError:
        return
    # Refuse every entry, including empty directories, non-YAML files and broken
    # symlinks. The initial beta has no allowlist of executable workflows.
    raise ValueError("Public beta exports must omit .github/workflows entirely.")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tree", type=Path, help="Plain exported source directory to inspect")
    args = parser.parse_args()
    try:
        check_public_tree(args.tree)
    except (OSError, ValueError) as error:
        print(f"Public CI policy failed: {error}", file=sys.stderr)
        return 1
    print("Public CI tree policy passed: .github/workflows is absent.")
    print("GitHub settings and runner access were not checked.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
