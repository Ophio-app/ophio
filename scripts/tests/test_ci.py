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
               audit_tool="path", audit_status=0, yanked=(), missing_tool=None,
               rust_std=True, clippy_status=0, trace_env=False, windows_archiver=False,
               allowed_yanked=None):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            tools = root / "bin"
            tools.mkdir()
            trace = root / "calls"
            fakes = {
                "cargo": 'printf "%s\\n" "$*" >> "$CI_TEST_TRACE"\n'
                + ('printf "env|%s|%s|%s\\n" "$CC_aarch64_apple_darwin" '
                   '"$AR_aarch64_apple_darwin" "$CARGO_TARGET_DIR" >> "$CI_TEST_TRACE"\n'
                   if trace_env else '')
                + f'case "$1" in fmt|check|metadata) exit {cargo_status} ;; clippy) exit {clippy_status} ;; esac\n'
                + 'case "$1" in audit) printf "%s\\n" "$PWD" >> "$CI_TEST_TRACE" ;; esac\n'
                + f'case "$1" in audit) [ {audit_status} = 0 ] || exit {audit_status} ;; esac\n'
                + 'case "$*" in "audit --json") printf "%s" "$CI_TEST_AUDIT_JSON" ;; esac\n',
                "rustc": 'printf "%s\\n" "$CI_TEST_LIBDIR"\n',
                "pkg-config": 'exit 1\n',
                "fixture-linker": 'exit 0\n',
                "fixture-archiver": 'exit 0\n',
                "clang": 'exit 0\n',
                "llvm-ar": 'exit 0\n',
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
                if name == missing_tool:
                    continue
                path = tools / name
                path.write_text("#!/bin/sh\n" + body, encoding="utf-8")
                path.chmod(0o755)
            libdir = root / "lib"
            libdir.mkdir()
            if rust_std:
                (libdir / "libstd-fixture.rlib").touch()
            env = dict(os.environ)
            env.pop("BUILD_TOOLS_HOME", None)
            env.pop("CC_aarch64_apple_darwin", None)
            env.pop("AR_aarch64_apple_darwin", None)
            env.pop("AR_x86_64_pc_windows_gnullvm", None)
            if windows_archiver:
                env["AR_x86_64_pc_windows_gnullvm"] = str(tools / "fixture-archiver")
            host_path = env["PATH"]
            if audit_tool is None or missing_tool:
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
            script = SCRIPT
            if allowed_yanked is not None:
                # Exercise the allowlist policy independently of whichever
                # dependencies the real workspaces currently need to allow.
                repo = root / "fixture"
                script = repo / "scripts/ci.sh"
                script.parent.mkdir(parents=True)
                shutil.copyfile(SCRIPT, script)
                for workspace in (repo, repo / "client/src-tauri"):
                    config = workspace / ".cargo"
                    config.mkdir(parents=True)
                    (config / "audit-yanked.txt").write_text(
                        "".join(f"{name} {version}\n" for name, version in allowed_yanked),
                        encoding="utf-8",
                    )
            result = subprocess.run(
                ["bash", str(script), *stages], env=env,
                capture_output=True, text=True, timeout=30,
                encoding="utf-8",
            )
            return result, trace.read_text(encoding="utf-8") if trace.exists() else ""

    def test_failure_is_retained_while_other_stages_run(self):
        result, calls = self.run_ci(["fmt", "test"], cargo_status=17)
        self.assertEqual(result.returncode, 1)
        self.assertIn("fmt\tFAIL\t17", result.stdout)
        self.assertIn("test\tPASS\t0", result.stdout)
        self.assertIn("test --locked --workspace --no-fail-fast", calls)

    def test_quick_checks_the_phone_lock_before_any_build(self):
        result, calls = self.run_ci(["quick"], cargo_status=17)
        self.assertEqual(result.returncode, 1)
        self.assertIn("quick\tFAIL", result.stdout)
        self.assertEqual(calls.splitlines(), [
            "metadata --locked --offline --format-version 1 --manifest-path client/src-tauri/Cargo.toml",
        ])
        self.assertIn("regenerate and commit client/src-tauri/Cargo.lock", result.stdout)
        self.assertIn("cargo fetch --locked", result.stdout)

    def test_first_failure_stops_commands_inside_a_stage(self):
        result, calls = self.run_ci(["windows"], cargo_status=19)
        self.assertEqual(result.returncode, 1)
        self.assertEqual(len(calls.splitlines()), 1)
        self.assertIn("windows\tFAIL\t19", result.stdout)

    def test_macos_commands_are_strict_and_environment_is_scoped(self):
        result, calls = self.run_ci(["clippy", "macos", "clippy"], trace_env=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        lines = calls.splitlines()
        flags = "--locked --workspace --exclude platform-linux --all-targets --target aarch64-apple-darwin"
        self.assertEqual(lines[::2], [
            "clippy --locked --workspace --all-targets -- -D warnings",
            f"check {flags}", f"clippy {flags} -- -D warnings",
            "clippy --locked --workspace --all-targets -- -D warnings",
        ])
        before, check, clippy, after = [line.split("|") for line in lines[1::2]]
        self.assertEqual(before[1:3], ["", ""])
        self.assertEqual(after, before)
        self.assertEqual(check, clippy)
        self.assertEqual(check[1:3], [str(SCRIPT.parent / "macos-check-cc.sh"), "llvm-ar"])
        self.assertEqual(check[3], before[3] + "/macos-check-only")

    def test_macos_reuses_ci_windows_llvm_archiver(self):
        result, calls = self.run_ci(
            ["macos"], trace_env=True, windows_archiver=True, missing_tool="llvm-ar",
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        environments = [line.split("|") for line in calls.splitlines()[1::2]]
        self.assertEqual(len(environments), 2)
        self.assertEqual(environments[0], environments[1])
        self.assertTrue(environments[0][2].endswith("/bin/fixture-archiver"))

    def test_macos_failures_are_not_hidden(self):
        for check_status, clippy_status, expected_calls in [(19, 0, 1), (0, 23, 2)]:
            with self.subTest(check=check_status, clippy=clippy_status):
                result, calls = self.run_ci(
                    ["macos"], cargo_status=check_status, clippy_status=clippy_status,
                )
                self.assertEqual(result.returncode, 1)
                self.assertEqual(len(calls.splitlines()), expected_calls)
                self.assertIn(f"macos\tFAIL\t{check_status or clippy_status}", result.stdout)

    def test_macos_requires_target_and_both_native_tools(self):
        for missing_tool, rust_std, message in [
            (None, False, "Rust target aarch64-apple-darwin"),
            ("clang", True, "Required tool: clang"),
            ("llvm-ar", True, "Required tool: llvm-ar"),
        ]:
            for allow, hosted, expected in [(False, False, 1), (True, False, 0), (True, True, 1)]:
                with self.subTest(tool=missing_tool, std=rust_std, allow=allow, hosted=hosted):
                    result, calls = self.run_ci(
                        ["macos"], missing_tool=missing_tool, rust_std=rust_std,
                        allow_skip=allow, hosted=hosted,
                    )
                    self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
                    self.assertIn(f"MISSING: {message}", result.stdout)
                    label = "SKIP\t77" if expected == 0 else "FAIL"
                    self.assertIn(f"macos\t{label}", result.stdout)
                    self.assertEqual(calls, "")

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
        allowed = [("fixture-crate", "1.0.0")]
        result, _ = self.run_ci(["advisories"], yanked=allowed, allowed_yanked=allowed)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertNotIn("no longer yanked", result.stdout)
        result, calls = self.run_ci(
            ["advisories"], yanked=[*allowed, ("left-pad", "1.0.0")], allowed_yanked=allowed,
        )
        self.assertEqual(result.returncode, 1)
        self.assertIn("yanked: left-pad 1.0.0", result.stdout + result.stderr)
        self.assertIn("advisories\tFAIL", result.stdout)
        self.assertNotIn("npm audit", calls)

    def test_advisories_point_out_a_yanked_entry_that_no_longer_applies(self):
        result, _ = self.run_ci(["advisories"], allowed_yanked=[("fixture-crate", "1.0.0")])
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertIn("note: fixture-crate 1.0.0 is no longer yanked or locked", result.stdout)

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
