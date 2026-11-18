# SPDX-License-Identifier: Apache-2.0
from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


WRAPPER = Path(__file__).resolve().parents[1] / "macos-check-cc.sh"


class MacosCheckCcTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="mac check cc ")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.log = self.root / "clang arguments.json"
        tools = self.root / "fake tools"
        tools.mkdir()
        clang = tools / "clang"
        clang.write_text(
            f"#!{sys.executable}\n"
            "import json, os, pathlib, sys\n"
            "pathlib.Path(os.environ['CLANG_LOG']).write_text(json.dumps(sys.argv[1:]), encoding='utf-8')\n"
            "print('fake clang probe result')\n"
            "sys.exit(int(os.environ.get('CLANG_STATUS', '0')))\n",
            encoding="utf-8",
        )
        clang.chmod(0o755)
        self.environment = {
            **os.environ,
            "PATH": str(tools) + os.pathsep + os.environ.get("PATH", ""),
            "CLANG_LOG": str(self.log),
            "CLANG_STATUS": "0",
        }

    def invoke(self, *arguments):
        return subprocess.run(
            [str(WRAPPER), *arguments],
            cwd=self.root,
            env=self.environment,
            text=True,
            encoding="utf-8",
            capture_output=True,
            check=False,
        )

    def arguments(self):
        return json.loads(self.log.read_text(encoding="utf-8"))

    def assert_refused(self, *arguments):
        self.log.unlink(missing_ok=True)
        result = self.invoke(*arguments)
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("CHECK/CLIPPY ONLY", result.stderr)
        self.assertFalse(self.log.exists(), "refused requests must not invoke clang")

    def test_compile_replaces_source_and_flags_with_fixed_empty_object_request(self):
        result = self.invoke(
            "-O2", "-I", "/missing Apple headers", "--target=x86_64-linux-gnu",
            "-c", "unavailable source.c", "-o", "placeholder.o",
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            self.arguments(),
            ["--target=arm64-apple-macosx", "-x", "c", "-c", "/dev/null", "-o", "placeholder.o"],
        )

    def test_output_paths_preserve_whitespace_and_shell_characters(self):
        output = str(self.root / "objects with spaces" / "a;$(touch unwanted).o")
        result = self.invoke("-c", "source with spaces.c", "-o", output)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.arguments()[-2:], ["-o", output])
        self.assertFalse((self.root / "unwanted").exists())

    def test_attached_output_is_supported(self):
        result = self.invoke("-c", "source.c", "-oobject with spaces.o")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.arguments()[-2:], ["-o", "object with spaces.o"])

    def test_preprocessor_macro_probes_pass_through_unchanged(self):
        for arguments in (
            ["-E", "-dM", "-x", "c", "-"],
            ["-E", "family probe.c"],
            ["-I", "headers with spaces", "-E", "probe.c", "-o", "macro output.txt"],
        ):
            with self.subTest(arguments=arguments):
                result = self.invoke(*arguments)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(self.arguments(), arguments)
                self.assertEqual(result.stdout, "fake clang probe result\n")

    def test_standalone_version_probes_pass_through_unchanged(self):
        for flag in ("--version", "-v", "-V", "-dumpversion", "-dumpmachine"):
            with self.subTest(flag=flag):
                result = self.invoke(flag)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(self.arguments(), [flag])

    def test_linking_is_refused_even_with_verbose_flag(self):
        for arguments in (
            ["input.c", "-o", "binary"],
            ["-v", "input.c", "-o", "binary"],
            ["input.o", "-shared", "-o", "library.dylib"],
            ["--version", "input.c", "-o", "binary"],
        ):
            with self.subTest(arguments=arguments):
                self.assert_refused(*arguments)

    def test_malformed_and_ambiguous_requests_are_refused(self):
        for arguments in (
            [], ["-c", "input.c"], ["-c", "input.c", "-o"],
            ["-c", "input.c", "-o", ""],
            ["-c", "input.c", "-o", "-E"],
            ["-c", "-o", "output.o"],
            ["-c", "input.c", "-o", "a.o", "-o", "b.o"],
            ["-c", "input.c", "-o", "a.o", "-ob.o"],
            ["-c", "-E", "input.c", "-o", "output.o"],
            ["-c", "-S", "input.c", "-o", "output.o"],
            ["-c", "-fsyntax-only", "input.c", "-o", "output.o"],
            ["-E"], ["-E", "input.c", "-o"],
            ["-c", "input.c", "-o", "output.o", "-I"],
            ["-c", "@hidden-options", "-o", "output.o"],
        ):
            with self.subTest(arguments=arguments):
                self.assert_refused(*arguments)

    def test_option_values_are_not_mistaken_for_actions_or_inputs(self):
        self.assert_refused("-I", "-c", "input.c", "-o", "output.o")
        self.assert_refused("-c", "-include", "header.h", "-o", "output.o")
        self.assert_refused("--", "-c", "input.c", "-o", "output.o")

    def test_clang_failure_status_is_preserved(self):
        self.environment["CLANG_STATUS"] = "7"
        for arguments in (["--version"], ["-E", "probe.c"], ["-c", "input.c", "-o", "output.o"]):
            with self.subTest(arguments=arguments):
                self.assertEqual(self.invoke(*arguments).returncode, 7)


if __name__ == "__main__":
    unittest.main()
