#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Writes crates/services/src/html_references.rs, the HTML Standard's named
character references, from Python's copy of them (html.entities.html5).
Run `cargo fmt -p services` after."""

from html.entities import html5
from pathlib import Path

OUT = Path(__file__).resolve().parent.parent / "crates/services/src/html_references.rs"


def literal(text: str) -> str:
    out = []
    for c in text:
        if c in '"\\':
            out.append("\\" + c)
        elif c == "\t":
            out.append("\\t")
        elif c == "\n":
            out.append("\\n")
        elif " " <= c <= "~":
            out.append(c)
        else:
            out.append(f"\\u{{{ord(c):X}}}")
    return '"' + "".join(out) + '"'


def main() -> None:
    names = sorted(html5, key=lambda n: n.encode())
    rows = "\n".join(f"    ({literal(n)}, {literal(html5[n])})," for n in names)
    OUT.write_text(
        "// SPDX-License-Identifier: Apache-2.0 AND CC-BY-4.0\n"
        "//! The HTML Standard's named character references, as a browser reads\n"
        "//! them: <https://html.spec.whatwg.org/multipage/named-characters.html>.\n"
        "//! The table is the WHATWG's (Apple, Google, Mozilla, Microsoft), under\n"
        "//! CC BY 4.0. Written by `scripts/html_references.py`; not edited by hand.\n"
        "\n"
        "/// The longest name, with its `;`.\n"
        f"pub(crate) const LONGEST: usize = {max(map(len, names))};\n"
        "\n"
        "/// Each name, without its `&`, with or without its `;` as the standard\n"
        "/// lists it, and what it stands for. Sorted, to be searched.\n"
        "pub(crate) const NAMED: &[(&str, &str)] = &[\n"
        f"{rows}\n"
        "];\n",
        encoding="utf-8",
    )


if __name__ == "__main__":
    main()
