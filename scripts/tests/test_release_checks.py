# SPDX-License-Identifier: Apache-2.0
"""Release packaging and source checks on synthetic public inputs, with no credentials."""

from __future__ import annotations

import ast
import contextlib
import hashlib
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from release_common import ROOT, ReleaseError
import source_candidate
import windows_runtime


class RuntimeMapReview(unittest.TestCase):
    def check(self, text: str) -> set[str]:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "app.map"
            path.write_text(text, encoding="utf-8")
            return windows_runtime.check([path])

    def test_lld_paths_spaces_parentheses_and_non_text_inputs(self):
        startup = r"C:\Build tools (current)\x86_64-w64-mingw32\lib\crt2.o"
        text = f"00001000 00000010 16 {startup}:(.text)\n"
        names = {
            "lib64_libmingw32_a-cinitexe.o": ".CRT$XIA",
            "lib64_libmingwex_a-gdtoa.o": ".pdata",
            "lib64_libmingwex_a-misc.o": ".debug_info",
            "lib64_libmingwex_a-dmisc.o": ".text$comdat",
        }
        text += "".join(
            f"00002000 00000001 4 C:/Build tools (current)/{name}:({section})\n"
            for name, section in names.items()
        )
        self.assertEqual(self.check(text), {"crt2.o", *names})

    def test_unreviewed_runtime_in_debug_or_unwind_sections_is_rejected(self):
        for section in (".debug_info", ".pdata", ".text$comdat"):
            with self.subTest(section=section):
                text = (
                    "00001000 00000010 16 /tools/x86_64-w64-mingw32/lib/crt2.o:(.text)\n"
                    f"00002000 00000001 4 lib64_libmingwex_a-unreviewed.o:({section})\n"
                )
                with self.assertRaisesRegex(ReleaseError, "Unreviewed"):
                    self.check(text)

    def test_import_library_members_are_not_classified_as_runtime_code(self):
        text = (
            "00001000 00000010 16 /tools/x86_64-w64-mingw32/lib/crt2.o:(.text)\n"
            "00002000 00000010 4 libkernel32.a:(.idata$5)\n"
            "00002010 00000008 4 api-ms-win-crt-stdio-l1-1-0.dll:(.idata$6)\n"
            "00002020 00000008 4 libucrt.a(api-ms-win-crt-runtime-l1-1-0.o):(.idata$5)\n"
        )
        self.assertEqual(self.check(text), {"crt2.o"})

    def test_gnu_ld_map_is_refused_instead_of_approving_an_empty_inventory(self):
        text = (
            "Linker script and memory map\n"
            " .text 0x0000000140001000 0x400 /tools/x86_64-w64-mingw32/lib/crt2.o\n"
            " .text 0x0000000140001400 0x100 /tools/libmingwex.a(lib64_libmingwex_a-unreviewed.o)\n"
        )
        with self.assertRaisesRegex(ReleaseError, "startup object"):
            self.check(text)


def commit_all(root: Path) -> None:
    subprocess.run(["git", "add", "."], cwd=root, check=True)
    subprocess.run(
        [
            "git", "-c", "user.name=Review fixture",
            "-c", "user.email=fixture@example.invalid",
            "-c", "commit.gpgsign=false",
            "commit", "-qm", "Synthetic audit input",
        ],
        cwd=root,
        check=True,
    )


class PublicSourceReview(unittest.TestCase):
    def test_private_terms_come_from_the_git_directory_and_are_never_printed(self):
        term = "Kestrel" + "wood"
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            subprocess.run(["git", "init", "-q", str(root)], check=True)
            notes = root / "notes" / f"{term.lower()}-setup.md"
            notes.parent.mkdir()
            notes.write_text(
                f"Ask {term.upper()}.\n{term}\u2019s laptop\n{term}s differ\n",
                encoding="utf-8",
            )
            commit_all(root)
            _, hits = source_candidate.audit(root)
            self.assertNotIn("private-term", {hit["rule"] for hit in hits})

            (root / ".git/info").mkdir(exist_ok=True)
            (root / ".git/info/private-terms").write_text(
                f"# One per line\n{term}\n", encoding="utf-8"
            )
            output = io.StringIO()
            with (
                mock.patch.object(source_candidate, "ROOT", root),
                mock.patch.object(sys, "argv", ["source_candidate.py"]),
                contextlib.redirect_stdout(output),
            ):
                self.assertEqual(source_candidate.main(), 1)
            _, hits = source_candidate.audit(root)
        self.assertEqual(
            [(hit["path"], hit["line"]) for hit in hits if hit["rule"] == "private-term"],
            [("notes/[private]-setup.md", n) for n in (0, 1, 2)],
        )
        self.assertNotIn(term.lower(), output.getvalue().lower())
        self.assertNotIn(term.lower(), json.dumps(hits).lower())

    def test_new_committed_file_still_catches_all_three_private_sentinels(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            policy = root / "packaging/source-policy.toml"
            policy.parent.mkdir()
            policy.write_bytes((ROOT / "packaging/source-policy.toml").read_bytes())
            subprocess.run(["git", "init", "-q", str(root)], check=True)
            path = root / "new-document.txt"
            sentinel = b"Kestrelwood"
            path.write_bytes(
                source_candidate.HOME_PREFIX
                + b"builder/workspace\n"
                + sentinel
                + b"\n"
                + b"plan"
                + b"ning/design.md\n"
            )
            commit_all(root)
            with (
                mock.patch.object(source_candidate, "LEGACY_NAME_LENGTH", len(sentinel)),
                mock.patch.object(source_candidate, "LEGACY_NAME_SHA256", hashlib.sha256(sentinel.lower()).hexdigest()),
            ):
                _, hits = source_candidate.audit(root)
            self.assertEqual(
                {hit["rule"] for hit in hits if hit["path"] == path.name},
                {"absolute-home-path", "internal-codename-content", "private-tree-reference"},
            )


# These open() functions take an archive or return a raw file descriptor.
BINARY_OPEN_MODULES = {"gzip", "tarfile", "zipfile", "os"}


class NonUtf8Locale(unittest.TestCase):
    """The display name is not ASCII, so packaging must not depend on the locale."""

    def run_python(self, *args: str) -> subprocess.CompletedProcess[bytes]:
        # Python otherwise coerces the C locale to UTF-8, hiding the failure.
        environment = {
            "PATH": "/usr/bin:/bin",
            "LC_ALL": "C",
            "PYTHONUTF8": "0",
            "PYTHONCOERCECLOCALE": "0",
        }
        return subprocess.run(
            [sys.executable, *args], cwd=ROOT, env=environment, capture_output=True
        )

    def test_release_identity_is_checked_and_reported(self):
        result = self.run_python("scripts/package-artifacts.py", "identity")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(b"phio ", result.stdout)
        self.assertIn(b": local release candidate", result.stdout)

    def test_licence_evidence_with_the_generated_notice_is_accepted(self):
        with tempfile.TemporaryDirectory() as directory:
            script = """
import json, sys
from pathlib import Path
sys.path.insert(0, "scripts")
from release_common import atomic_write, identity, notice, require_licences
out = Path(sys.argv[1])
atomic_write(out / "NOTICE", notice().encode())
report = {"artifact": "linux", "failures": []}
(out / "license-report.json").write_text(json.dumps(report))
properties = [
    {"name": "distribution:inventory-complete", "value": "true"},
    {"name": "distribution:artifact", "value": "linux"},
]
bom = {"metadata": {"properties": properties, "component": {"version": identity()[2]}}}
(out / "sbom.cdx.json").write_text(json.dumps(bom))
require_licences(out, "linux")
"""
            result = self.run_python("-c", script, directory)
            self.assertEqual(result.returncode, 0, result.stderr)

    def test_installer_output_is_read_without_a_utf8_locale(self):
        # The installer names the app, so its tests decode non-ASCII output.
        result = self.run_python("-m", "unittest", "scripts.tests.test_linux_install")
        self.assertEqual(result.returncode, 0, result.stderr.decode(errors="replace"))
        self.assertIn(b"OK", result.stderr)

    def test_release_and_lab_text_names_its_encoding(self):
        # Release logs, notices and evidence carry the display name, so any text
        # read or write left to the locale fails on a machine that is not UTF-8.
        # The tests read the same files and output, so they are held to it too.
        unnamed = []
        for path in sorted(
            [*ROOT.glob("scripts/**/*.py"), *ROOT.glob("tools/lab/**/*.py")]
        ):
            tree = ast.parse(path.read_text(encoding="utf-8"))
            for node in ast.walk(tree):
                if not isinstance(node, ast.Call):
                    continue
                keywords = {k.arg: k.value for k in node.keywords}
                name = getattr(node.func, "attr", getattr(node.func, "id", ""))
                text_mode = (
                    name in {"read_text", "write_text"}
                    or (
                        name == "open"
                        and getattr(getattr(node.func, "value", None), "id", "")
                        not in BINARY_OPEN_MODULES
                        and not any(
                            isinstance(a, ast.Constant) and "b" in str(a.value)
                            for a in [*node.args, keywords.get("mode")]
                        )
                    )
                    or (
                        isinstance(keywords.get("text"), ast.Constant)
                        and keywords["text"].value is True
                    )
                )
                if text_mode and "encoding" not in keywords:
                    unnamed.append(f"{path.relative_to(ROOT)}:{node.lineno}")
        self.assertEqual(unnamed, [])


if __name__ == "__main__":
    unittest.main()
