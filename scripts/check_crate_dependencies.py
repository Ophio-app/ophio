#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Reject workspace dependency edges that have not been explicitly reviewed."""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import subprocess
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[1]
POLICY = Path("scripts/policies/crate-dependencies.toml")


def workspace_edges(metadata: dict) -> dict[str, set[str]]:
    """Read declared edges, including tests, optional features and every platform.

    A resolved graph only includes the active platform/features. Cargo's package
    declarations include all of them, and identify renamed dependencies by their
    real package name. Paths keep a registry package with the same name out.
    """
    members = set(metadata["workspace_members"])
    packages = [p for p in metadata["packages"] if p["id"] in members]
    paths = {str(Path(p["manifest_path"]).parent.resolve()): p["name"] for p in packages}
    result = {}
    for package in packages:
        result[package["name"]] = {
            paths[str(Path(dependency["path"]).resolve())]
            for dependency in package["dependencies"]
            if dependency.get("path") and str(Path(dependency["path"]).resolve()) in paths
        }
    return result


def check(actual: dict[str, set[str]], allowed: dict[str, list[str]]) -> tuple[list[str], list[str]]:
    errors, notes = [], []
    for name, dependencies in sorted(actual.items()):
        if name not in allowed:
            errors.append(f"New workspace crate {name!r}: add its reviewed dependencies to {POLICY} (use [] if none).")
        for dependency in sorted(dependencies - set(allowed.get(name, []))):
            errors.append(
                f"New workspace dependency: {name} depends on {dependency}. "
                f"Remove this dependency, or explain why it is needed and add it to {POLICY} for review."
            )
    for name, dependencies in sorted(allowed.items()):
        if name not in actual:
            notes.append(f"{name} is no longer a workspace crate; remove its entry from {POLICY}.")
        else:
            for dependency in sorted(set(dependencies) - actual[name]):
                notes.append(f"{name} no longer depends on {dependency}; remove this permission from {POLICY}.")
    return errors, notes


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT)
    args = parser.parse_args()
    root = args.root.resolve()
    try:
        policy = tomllib.loads((root / POLICY).read_text(encoding="utf-8"))["allowed"]
        if not isinstance(policy, dict) or any(
            not isinstance(value, list) or any(not isinstance(name, str) for name in value)
            for value in policy.values()
        ):
            raise ValueError("Each allowed crate must have a list of dependency names.")
        metadata = subprocess.run(
            ["cargo", "metadata", "--locked", "--offline", "--no-deps", "--format-version", "1"],
            cwd=root, check=True, capture_output=True, text=True, encoding="utf-8",
        )
        errors, notes = check(workspace_edges(json.loads(metadata.stdout)), policy)
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError) as error:
        print(f"Cannot check workspace dependencies: {error}", file=sys.stderr)
        if isinstance(error, subprocess.CalledProcessError):
            print(error.stderr, file=sys.stderr)
        return 1
    for note in notes:
        print(f"Reminder: {note}")
    for error in errors:
        print(error, file=sys.stderr)
    if errors:
        return 1
    print("Workspace dependencies match the reviewed permissions.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
