#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Count source-level lint exceptions without building or adding dependencies.

Rust counts each lint in allow attributes, including inner attributes and nested
cfg_attr. JavaScript/TypeScript counts each rule in ESLint disable comments; '*'
means all rules. Each occurrence counts, regardless of its scope or condition.
This is a lexical counter, not a Rust or JavaScript syntax validator. The normal
compiler and ESLint checks remain responsible for validating source syntax.

The baseline is protected policy: never regenerate it automatically in CI.
Use --show-counts to inspect current counts, then deliberately edit only the
accepted limits in scripts/policies/lint-suppressions.json and explain why in
the change being reviewed. Lower limits when exceptions are removed.
"""

import argparse
from collections import Counter
import json
from pathlib import Path
import re
import subprocess
import sys

BASELINE = "scripts/policies/lint-suppressions.json"
CLIENT_EXTENSIONS = {".js", ".jsx", ".ts", ".tsx", ".mjs", ".cjs", ".mts", ".cts"}
IDENTIFIER = re.compile(r"[A-Za-z_][A-Za-z_0-9]*")
RAW_STRING = re.compile(r'(?:br|cr|r)(#*)"')
CHAR = re.compile(r"(?:b)?'(?:\\(?:u\{[0-9a-fA-F_]+\}|x[0-9a-fA-F]{2}|.)|[^'\\\n])'")


def lex(source, rust=False):
    """Return code tokens and real comments, skipping quoted text and literals."""
    tokens, comments = [], []
    i = 0
    # Template interpolation is code; template text itself is not.
    templates = []
    template_text = False
    while i < len(source):
        if template_text:
            if source[i] == "\\":
                i += 2
            elif source[i] == "`":
                template_text = False
                i += 1
            elif source.startswith("${", i):
                templates.append(0)
                template_text = False
                i += 2
            else:
                i += 1
            continue
        if source[i].isspace():
            i += 1
            continue
        if source.startswith("//", i):
            end = source.find("\n", i)
            end = len(source) if end < 0 else end
            comments.append(source[i + 2:end])
            i = end
            continue
        if source.startswith("/*", i):
            start, depth = i + 2, 1
            i += 2
            while i < len(source) and depth:
                if rust and source.startswith("/*", i):
                    depth += 1
                    i += 2
                elif source.startswith("*/", i):
                    depth -= 1
                    i += 2
                else:
                    i += 1
            comments.append(source[start:i - 2] if not depth else source[start:])
            continue
        raw = RAW_STRING.match(source, i) if rust else None
        if raw:
            end_marker = '"' + raw[1]
            end = source.find(end_marker, raw.end())
            i = len(source) if end < 0 else end + len(end_marker)
            tokens.append("<literal>")
            continue
        char = CHAR.match(source, i) if rust else None
        if char:
            i = char.end()
            tokens.append("<literal>")
            continue
        if source[i] == '"' or (not rust and source[i] == "'"):
            quote = source[i]
            i += 1
            while i < len(source):
                if source[i] == "\\":
                    i += 2
                elif source[i] == quote:
                    i += 1
                    break
                else:
                    i += 1
            tokens.append("<literal>")
            continue
        if not rust and source[i] == "`":
            template_text = True
            i += 1
            tokens.append("<literal>")
            continue
        # A slash at an expression start introduces a regular-expression literal.
        if (not rust and source[i] == "/" and
                (not tokens or tokens[-1] in {
                    "=", "(", "[", "{", ",", ":", ";", "!", "?", "return", "=>",
                })):
            i += 1
            in_class = False
            while i < len(source) and source[i] != "\n":
                c = source[i]
                if c == "\\":
                    i += 2
                    continue
                if c == "[":
                    in_class = True
                elif c == "]":
                    in_class = False
                elif c == "/" and not in_class:
                    i += 1
                    break
                i += 1
            tokens.append("<literal>")
            continue
        if not rust and templates:
            if source[i] == "{":
                templates[-1] += 1
            elif source[i] == "}":
                if templates[-1] == 0:
                    templates.pop()
                    template_text = True
                    i += 1
                    continue
                templates[-1] -= 1
        word = IDENTIFIER.match(source, i)
        if word:
            tokens.append(word[0])
            i = word.end()
        elif source.startswith("::", i) or source.startswith("=>", i):
            tokens.append(source[i:i + 2])
            i += 2
        else:
            tokens.append(source[i])
            i += 1
    return tokens, comments


def comma_parts(tokens):
    """Split a token list only at commas outside nested delimiters."""
    start, depth = 0, 0
    for i, token in enumerate(tokens):
        if token in {"(", "[", "{"}:
            depth += 1
        elif token in {")", "]", "}"}:
            depth -= 1
        elif token == "," and depth == 0:
            yield tokens[start:i]
            start = i + 1
    yield tokens[start:]


def rust_counts(source):
    tokens, _ = lex(source, rust=True)
    counts = Counter()

    def attribute(meta):
        if len(meta) < 3 or meta[1] != "(" or meta[-1] != ")":
            return
        parts = list(comma_parts(meta[2:-1]))
        if meta[0] == "allow":
            for part in parts:
                name = "".join(part)
                if re.fullmatch(r"[A-Za-z_]\w*(?:::[A-Za-z_]\w*)*", name):
                    counts["rust:" + name] += 1
        elif meta[0] == "cfg_attr":
            for part in parts[1:]:
                attribute(part)

    i = 0
    while i < len(tokens):
        if tokens[i] != "#":
            i += 1
            continue
        start = i + 1
        if start < len(tokens) and tokens[start] == "!":
            start += 1
        if start >= len(tokens) or tokens[start] != "[":
            i += 1
            continue
        end, depth = start + 1, 1
        while end < len(tokens) and depth:
            if tokens[end] == "[":
                depth += 1
            elif tokens[end] == "]":
                depth -= 1
            end += 1
        if not depth:
            attribute(tokens[start + 1:end - 1])
        i = end
    return counts


def eslint_counts(source):
    _, comments = lex(source)
    counts = Counter()
    for comment in comments:
        comment = re.sub(r"(?m)^\s*\* ?", "", comment).strip()
        match = re.match(r"eslint-disable(?:-next-line|-line)?(?=\s|$)(.*)",
                         comment, flags=re.DOTALL)
        if not match:
            continue
        rules = match[1].split("--", 1)[0].strip()
        for rule in re.split(r"[\s,]+", rules) if rules else ["*"]:
            if rule:
                counts["eslint:" + rule] += 1
    return counts


def collect(root):
    # Include new, unignored files so a newly added file cannot escape the check.
    result = subprocess.run(
        ["git", "-C", str(root), "ls-files", "--cached", "--others",
         "--exclude-standard", "-z"], check=True, capture_output=True)
    counts, locations = Counter(), {}
    for name in sorted(set(result.stdout.decode().split("\0")) - {""}):
        path = root / name
        if not path.is_file():
            continue  # A tracked deletion is a decrease, not a read error.
        if path.suffix == ".rs":
            found = rust_counts(path.read_text(encoding="utf-8"))
        elif name.startswith("client/") and path.suffix in CLIENT_EXTENSIONS:
            found = eslint_counts(path.read_text(encoding="utf-8"))
        else:
            continue
        counts.update(found)
        for lint in found:
            locations.setdefault(lint, []).append(f"{name} ({found[lint]})")
    return counts, locations


def load_baseline(path):
    data = json.loads(path.read_text(encoding="utf-8"))
    if (not isinstance(data, dict) or data.get("version") != 1 or
            not isinstance(data.get("counts"), dict)):
        raise ValueError("The suppression baseline needs version 1 and a counts object.")
    for lint, count in data["counts"].items():
        if (not lint.startswith(("rust:", "eslint:")) or
                type(count) is not int or count < 0):
            raise ValueError(f"Invalid suppression limit for {lint!r}.")
    return data["counts"]


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--baseline", type=Path, default=Path(BASELINE))
    parser.add_argument("--show-counts", action="store_true",
                        help="Print current counts for a deliberate baseline edit; do not write it.")
    args = parser.parse_args(argv)
    try:
        counts, locations = collect(args.root)
        if args.show_counts:
            print(json.dumps(dict(sorted(counts.items())), indent=2))
            return 0
        baseline = args.baseline if args.baseline.is_absolute() else args.root / args.baseline
        limits = load_baseline(baseline)
        increases = [lint for lint in sorted(counts) if counts[lint] > limits.get(lint, 0)]
        for lint in increases:
            print(f"FAIL: {lint} has {counts[lint]} suppressions; baseline allows {limits.get(lint, 0)}.")
            print("  Found in: " + ", ".join(locations[lint]))
        for lint in sorted(limits):
            if counts[lint] < limits[lint]:
                print(f"Reminder: lower {lint} in {args.baseline} from {limits[lint]} to {counts[lint]}.")
        if increases:
            print(f"Remove the new exceptions, or deliberately edit the protected baseline {args.baseline}.")
            print("Inspect counts with python3 scripts/check_suppressions.py --show-counts; "
                  "change only accepted limits and explain each increase in the review.")
            return 1
        print(f"Suppression counts pass ({sum(counts.values())} exceptions across {len(counts)} lint names).")
        return 0
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f"Cannot check suppression counts: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
