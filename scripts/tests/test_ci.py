# SPDX-License-Identifier: Apache-2.0
"""Exercise stage failures and explicit prerequisite skips without running tools."""
from pathlib import Path
import json
import os
import shutil
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "ci.sh"


class CiTests(unittest.TestCase):
    def run_ci(self, stages, *, cargo_status=0, allow_skip=False, hosted=False,
               audit_tool="path", audit_status=0, yanked=()):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            tools = root / "bin"
            tools.mkdir()
            trace = root / "calls"
            fakes = {
                "cargo": 'printf "%s\\n" "$*" >> "$CI_TEST_TRACE"\n'
                + f'case "$1" in fmt|check) exit {cargo_status} ;; esac\n'
                + 'case "$1" in audit) printf "%s\\n" "$PWD" >> "$CI_TEST_TRACE" ;; esac\n'
                + f'case "$1" in audit) [ {audit_status} = 0 ] || exit {audit_status} ;; esac\n'
                + 'case "$*" in "audit --json") printf "%s" "$CI_TEST_AUDIT_JSON" ;; esac\n',
                "rustc": 'printf "%s\\n" "$CI_TEST_LIBDIR"\n',
                "pkg-config": 'exit 1\n',
                "fixture-linker": 'exit 0\n',
                "npm": 'printf "npm %s\\n%s\\n" "$*" "$PWD" >> "$CI_TEST_TRACE"\n',
            }
            if audit_tool == "path":
                fakes["cargo-audit"] = 'exit 0\n'
            elif audit_tool == "build-tools":
                audit_bin = root / "build-tools/cargo-audit/bin/cargo-audit"
                audit_bin.parent.mkdir(parents=True)
                audit_bin.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
                audit_bin.chmod(0o755)
            for name, body in fakes.items():
                path = tools / name
                path.write_text("#!/bin/sh\n" + body, encoding="utf-8")
                path.chmod(0o755)
            libdir = root / "lib"
            libdir.mkdir()
            (libdir / "libstd-fixture.rlib").touch()
            env = dict(os.environ)
            env.pop("BUILD_TOOLS_HOME", None)
            host_path = env["PATH"]
            if audit_tool is None:
                # Expose only the utilities ci.sh needs, so a host install of
                # cargo-audit cannot hide a missing-tool failure.
                host = root / "host"
                host.mkdir()
                for name in ("bash", "cat", "dirname", "mkdir", "python3", "rm", "tee"):
                    (host / name).symlink_to(shutil.which(name))
                host_path = str(host)
            env.update(
                PATH=str(tools) + os.pathsep + host_path,
                **({"BUILD_TOOLS_HOME": str(root / "build-tools")}
                   if audit_tool == "build-tools" else {}),
                CARGO_TARGET_DIR=str(root / "target"),
                CI_TEST_TRACE=str(trace), CI_TEST_LIBDIR=str(libdir),
                CI_TEST_AUDIT_JSON=json.dumps({"warnings": {"yanked": [
                    {"package": {"name": name, "version": version}} for name, version in yanked
                ]}}),
                CARGO_TARGET_X86_64_PC_WINDOWS_GNULLVM_LINKER="fixture-linker",
                CI_ALLOW_MISSING_TOOLS="1" if allow_skip else "0",
                CI="true" if hosted else "false",
            )
            result = subprocess.run(
                ["bash", str(SCRIPT), *stages], env=env,
                capture_output=True, text=True, timeout=30,
                encoding="utf-8",
            )
            return result, trace.read_text(encoding="utf-8") if trace.exists() else ""

    def test_failure_is_retained_while_other_stages_run(self):
        result, calls = self.run_ci(["fmt", "test"], cargo_status=17)
        self.assertEqual(result.returncode, 1)
        self.assertIn("fmt\tFAIL\t17", result.stdout)
        self.assertIn("test\tPASS\t0", result.stdout)
        self.assertIn("test --locked --workspace", calls)

    def test_first_failure_stops_commands_inside_a_stage(self):
        result, calls = self.run_ci(["windows"], cargo_status=19)
        self.assertEqual(result.returncode, 1)
        self.assertEqual(len(calls.splitlines()), 1)
        self.assertIn("windows\tFAIL\t19", result.stdout)

    def test_missing_prerequisite_requires_explicit_local_opt_in(self):
        for allow, hosted, expected in [(False, False, 1), (True, False, 0), (True, True, 1)]:
            with self.subTest(allow=allow, hosted=hosted):
                result, _ = self.run_ci(["tauri"], allow_skip=allow, hosted=hosted)
                self.assertEqual(result.returncode, expected)
                self.assertIn("MISSING:", result.stdout)
                if expected == 0:
                    self.assertIn("tauri\tSKIP\t77", result.stdout)
                else:
                    self.assertIn("tauri\tFAIL", result.stdout)

    def test_unknown_stage_never_runs_a_command(self):
        result, calls = self.run_ci(["not-a-stage"])
        self.assertEqual(result.returncode, 2)
        self.assertEqual(calls, "")

    def test_tool_exit_77_is_a_failure_not_a_prerequisite_skip(self):
        result, _ = self.run_ci(["fmt"], cargo_status=77, allow_skip=True)
        self.assertEqual(result.returncode, 1)
        self.assertIn("fmt\tFAIL\t77", result.stdout)

    def test_list_includes_the_advisory_stage(self):
        result = subprocess.run(
            ["bash", str(SCRIPT), "--list"], capture_output=True, text=True, timeout=30,
            encoding="utf-8",
        )
        self.assertEqual(result.returncode, 0)
        self.assertIn("advisories", result.stdout.splitlines())

    def test_advisories_audit_each_lock_from_its_own_directory(self):
        result, calls = self.run_ci(["advisories"])
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertIn("advisories\tPASS\t0", result.stdout)
        repo = SCRIPT.parents[1]
        self.assertEqual(calls.splitlines(), [
            "audit --deny warnings --no-yanked", str(repo), "audit --json", str(repo),
            "audit --deny warnings --no-yanked", str(repo / "client/src-tauri"),
            "audit --json", str(repo / "client/src-tauri"),
            "npm audit --omit=dev", str(repo / "client"),
        ])
        for directory in (repo, repo / "client/src-tauri"):
            self.assertTrue((directory / ".cargo/audit.toml").is_file(), directory)

    def test_advisories_accept_only_the_yanked_crates_each_workspace_lists(self):
        result, _ = self.run_ci(["advisories"], yanked=[("zip", "7.2.0")])
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertNotIn("no longer yanked", result.stdout)
        result, calls = self.run_ci(["advisories"], yanked=[("zip", "7.2.0"), ("left-pad", "1.0.0")])
        self.assertEqual(result.returncode, 1)
        self.assertIn("yanked: left-pad 1.0.0", result.stdout + result.stderr)
        self.assertIn("advisories\tFAIL", result.stdout)
        self.assertNotIn("npm audit", calls)

    def test_advisories_point_out_a_yanked_entry_that_no_longer_applies(self):
        result, _ = self.run_ci(["advisories"])
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertIn("note: zip 7.2.0 is no longer yanked or locked", result.stdout)

    def test_advisories_find_cargo_audit_under_build_tools_home(self):
        result, _ = self.run_ci(["advisories"], audit_tool="build-tools")
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertIn("advisories\tPASS\t0", result.stdout)

    def test_advisories_fail_without_cargo_audit(self):
        # Even the explicit local opt-in that lets other stages skip.
        for allow, hosted in [(False, False), (True, False), (True, True)]:
            with self.subTest(allow=allow, hosted=hosted):
                result, calls = self.run_ci(
                    ["advisories"], audit_tool=None, allow_skip=allow, hosted=hosted,
                )
                self.assertEqual(result.returncode, 1)
                self.assertIn("MISSING: Required tool: cargo-audit", result.stdout)
                self.assertIn("advisories\tFAIL", result.stdout)
                self.assertEqual(calls, "")

    def test_advisory_fetch_failure_is_never_a_skip(self):
        # A database fetch failure exits nonzero; even an explicit local skip
        # opt-in must not turn it into a skip, and later audits do not run.
        result, calls = self.run_ci(["advisories"], audit_status=1, allow_skip=True)
        self.assertEqual(result.returncode, 1)
        self.assertIn("advisories\tFAIL\t1", result.stdout)
        self.assertEqual(len(calls.splitlines()), 2)
