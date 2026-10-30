#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Choose the scripts/ci.sh stages a commit needs, and record trees that passed.

A commit whose tree already passed needs none: a merge to main that a pull
request's run already checked runs nothing again. Otherwise the stages follow
from what changed since the nearest earlier commit on the first-parent line
whose tree passed. A change only to the phone app's screens skips the Rust
stages, and a change only to Rust code that the app doesn't read skips the
app's. The repeated test runs and the release packages, which take most of a
full run, run only for the files they check. With no such commit within
reach, or a change this doesn't recognise, every stage runs.

Passed trees are empty files named by tree hash in a folder the CI host keeps
between jobs (docs/operations/self-hosted-runner.md). Without that folder,
as on GitHub's runners, every stage always runs.

Usage:
  ci_plan.py --marks DIR [--full]   print stages=, tree= and base= lines
  ci_plan.py --marks DIR --record   record HEAD's tree as passed
"""

from __future__ import annotations

import argparse
from pathlib import Path
import re
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]
# Its own job, which runs on every push whatever changed.
SEPARATE = {"advisories"}
# Quick, and about the whole tree.
ALWAYS = {"source", "licenses", "python"}
# The phone and the computer against a real intake: both sides can break it.
RUST = {"fmt", "clippy", "test", "windows", "tauri", "intake-e2e"}
CLIENT = {"client", "tauri", "intake-e2e"}
# The whole test suite run again and again, some runs with every core busy,
# to catch a test that only fails now and then. That takes about a quarter of
# an hour, so a change runs it only when it changes the shared test helpers.
REPEATS = {"parallel"}
REPEAT_DIRS = ("crates/test-support/",)
# Release builds that check how the packages are put together, from these
# inputs. Every update build packages too.
PACKAGES = {"package-linux", "package-windows"}
PACKAGING = re.compile(r"packaging/|scripts/package[-_]")
RUST_DIRS = ("crates/", "fuzz/", ".cargo/")
RUST_FILES = {"Cargo.toml", "Cargo.lock"}
# Locked client packages reach Rust tests too (they start the app's dev server).
CLIENT_LOCKS = {"client/package.json", "client/package-lock.json"}
# Rust tests write the protocol bindings the client stage then checks.
BINDINGS = "client/src/protocol/generated"
# How far back to look for a tree that passed.
DEPTH = 50
# Marks older than this are removed when one is recorded.
KEEP_DAYS = 90
RELATIVE = r"(\.\./)+"
PATH_CHARS = r"[A-Za-z0-9_./@-]*"


def git(*args: str) -> str:
    return subprocess.run(
        ["git", *args], cwd=ROOT, check=True, capture_output=True, text=True,
        encoding="utf-8",
    ).stdout.strip()


def all_stages() -> list[str]:
    listed = subprocess.run(
        ["bash", str(ROOT / "scripts/ci.sh"), "--list"],
        check=True, capture_output=True, text=True, encoding="utf-8",
    ).stdout.split()
    return [s for s in listed if s not in SEPARATE]


def references(pattern: str, *trees: str, exclude: tuple[str, ...] = ()) -> set[str]:
    """Paths one part of the repository reads from another, by relative path."""
    found = subprocess.run(
        ["git", "grep", "-ohE", pattern, "--", *trees, *(f":!{e}" for e in exclude)],
        cwd=ROOT, capture_output=True, text=True, encoding="utf-8",
    ).stdout.split()
    return {re.sub(f"^{RELATIVE}", "", p).rstrip("/") for p in found}


def coupled() -> set[str]:
    """Paths whose change needs both the Rust and the client stages."""
    client_read_by_rust = references(f"{RELATIVE}client(/{PATH_CHARS})?", "crates", "fuzz")
    # Cargo manifests only build the app's Rust half, which RUST covers.
    rust_read_by_client = references(
        f"{RELATIVE}crates/{PATH_CHARS}", "client",
        exclude=("*Cargo.toml", BINDINGS),
    )
    # Crates that export bindings; the client stage checks they are current.
    exporters = subprocess.run(
        ["git", "grep", "-l", "^ts-rs", "--", "crates/*/Cargo.toml"],
        cwd=ROOT, capture_output=True, text=True, encoding="utf-8",
    ).stdout.split()
    exporting = {str(Path(manifest).parent) for manifest in exporters}
    return client_read_by_rust | rust_read_by_client | exporting | {BINDINGS}


def within(path: str, prefix: str) -> bool:
    return path == prefix or path.startswith(prefix + "/")


def needs(path: str, couplings: set[str], every: set[str]) -> set[str]:
    """The stages beyond ALWAYS that a change to `path` needs."""
    if path.startswith("docs/") or ("/" not in path and path.endswith(".md")):
        return set()
    if path in CLIENT_LOCKS:
        return every
    if PACKAGING.match(path):
        return PACKAGES
    extra = set()
    if path.startswith(REPEAT_DIRS):
        extra |= REPEATS
    # Manifests choose the dependencies, whose licences the packages carry.
    if path == "Cargo.toml" or path.endswith(("/Cargo.toml", "Cargo.lock")):
        extra |= PACKAGES
    if any(within(path, c) for c in couplings):
        return extra | RUST | CLIENT
    if path.startswith("client/"):
        return extra | CLIENT
    if path.startswith(RUST_DIRS) or path in RUST_FILES:
        return extra | RUST
    return every


def stages_for(paths: list[str], couplings: set[str], order: list[str]) -> list[str]:
    every = set(order)
    if not paths:
        return order
    wanted = set(ALWAYS)
    for path in paths:
        wanted |= needs(path, couplings, every)
    return [s for s in order if s in wanted]


def tree_of(commit: str) -> str:
    return git("rev-parse", f"{commit}^{{tree}}")


def passed_base(marks: Path) -> str | None:
    """The nearest earlier first-parent commit whose tree passed."""
    try:
        line = git("rev-list", "--first-parent", f"--max-count={DEPTH}", "HEAD^")
    except subprocess.CalledProcessError:
        return None
    for commit in line.split():
        if (marks / tree_of(commit)).is_file():
            return commit
    return None


def plan(marks: Path | None, full: bool) -> tuple[list[str], str, str, str]:
    """Stages to run, HEAD's tree, the base compared with and why."""
    order = all_stages()
    tree = tree_of("HEAD")
    if full:
        return order, tree, "", "every stage was asked for"
    if marks is None:
        return order, tree, "", "this host keeps no record of passed trees"
    if not marks.is_dir():
        return order, tree, "", "no tree has passed on this host yet"
    if (marks / tree).is_file():
        return [], tree, "", "this exact tree already passed"
    base = passed_base(marks)
    if base is None:
        return order, tree, "", f"no passed tree in the last {DEPTH} first-parent commits"
    changed = git("diff", "--name-only", "--no-renames", base, "HEAD").splitlines()
    stages = stages_for(changed, coupled(), order)
    return stages, tree, base, f"{len(changed)} files changed since {base[:12]}, which passed"


def record(marks: Path) -> str:
    marks.mkdir(parents=True, exist_ok=True)
    tree = tree_of("HEAD")
    (marks / tree).touch()
    cutoff = time.time() - KEEP_DAYS * 86400
    for old in marks.iterdir():
        if old.is_file() and old.stat().st_mtime < cutoff:
            old.unlink(missing_ok=True)
    return tree


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--marks", type=Path, help="folder of passed trees")
    parser.add_argument("--full", action="store_true", help="run every stage")
    parser.add_argument("--record", action="store_true", help="record HEAD's tree as passed")
    args = parser.parse_args()
    if args.record:
        if args.marks is None or not args.marks.parent.is_dir():
            print("No record of passed trees is kept here.", file=sys.stderr)
            return 0
        print(f"Recorded tree {record(args.marks)} as passed.", file=sys.stderr)
        return 0
    stages, tree, base, why = plan(args.marks, args.full)
    print(f"stages={' '.join(stages)}")
    print(f"tree={tree}")
    print(f"base={base}")
    shown = " ".join(stages) or "none"
    print(f"Stages: {shown} ({why}).", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
