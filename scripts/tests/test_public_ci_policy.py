# SPDX-License-Identifier: Apache-2.0
"""Public-export policy and accidental-copy guards; not a workflow sandbox."""
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

from scripts.public_ci_policy import check_public_tree

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/public_ci_policy.py"


class PublicCiPolicyTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name) / "export"
        self.root.mkdir()

    def test_plain_source_tree_passes_without_git(self):
        (self.root / "Cargo.toml").write_text("[workspace]\n", encoding="utf-8")
        check_public_tree(self.root)

    def test_issue_templates_are_not_workflows(self):
        templates = self.root / ".github/ISSUE_TEMPLATE"
        templates.mkdir(parents=True)
        (templates / "bug.yml").write_text("name: Bug\n", encoding="utf-8")
        check_public_tree(self.root)

    def test_any_workflow_directory_fails_even_when_empty(self):
        workflows = self.root / ".github/workflows"
        workflows.mkdir(parents=True)
        with self.assertRaisesRegex(ValueError, "omit .github/workflows"):
            check_public_tree(self.root)
        for name in ("checks.yml", "checks.yaml", "README.txt", ".hidden"):
            with self.subTest(name=name):
                (workflows / name).write_text("untrusted content\n", encoding="utf-8")
                with self.assertRaises(ValueError):
                    check_public_tree(self.root)
                (workflows / name).unlink()

    def test_workflows_file_is_not_an_escape(self):
        (self.root / ".github").mkdir()
        (self.root / ".github/workflows").touch()
        with self.assertRaises(ValueError):
            check_public_tree(self.root)

    def test_workflows_symlink_is_rejected_even_when_broken(self):
        (self.root / ".github").mkdir()
        (self.root / ".github/workflows").symlink_to(self.root / "absent")
        with self.assertRaises(ValueError):
            check_public_tree(self.root)

    def test_github_symlink_is_rejected_even_without_workflows(self):
        outside = self.root.parent / "outside"
        outside.mkdir()
        link = self.root / ".github"
        link.symlink_to(outside, target_is_directory=True)
        with self.assertRaises(ValueError):
            check_public_tree(self.root)
        outside.rmdir()
        with self.assertRaises(ValueError):
            check_public_tree(self.root)

    def test_github_file_is_rejected(self):
        (self.root / ".github").touch()
        with self.assertRaises(ValueError):
            check_public_tree(self.root)

    def test_missing_root_is_not_a_pass(self):
        with self.assertRaises(FileNotFoundError):
            check_public_tree(self.root / "missing")

    def test_file_and_symlink_roots_are_rejected(self):
        file = self.root / "file"
        file.touch()
        link = self.root.parent / "link"
        link.symlink_to(self.root, target_is_directory=True)
        for root in (file, link):
            with self.subTest(root=root), self.assertRaises(ValueError):
                check_public_tree(root)

    def test_permission_error_is_not_treated_as_absence(self):
        original = Path.lstat
        for blocked in (self.root, self.root / ".github", self.root / ".github/workflows"):
            (self.root / ".github").mkdir(exist_ok=True)
            def deny(path, *args, **kwargs):
                if path == blocked:
                    raise PermissionError("fixture: unreadable export")
                return original(path, *args, **kwargs)
            with self.subTest(blocked=blocked), patch.object(Path, "lstat", deny):
                with self.assertRaises(PermissionError):
                    check_public_tree(self.root)

    def run_checker(self, path):
        return subprocess.run(
            [sys.executable, str(SCRIPT), str(path)],
            capture_output=True, text=True, timeout=10, encoding="utf-8",
        )

    def test_cli_states_its_limits_and_returns_failure_for_workflows(self):
        result = self.run_checker(self.root)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("runner access were not checked", result.stdout)
        (self.root / ".github/workflows").mkdir(parents=True)
        result = self.run_checker(self.root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("must omit", result.stderr)
        self.assertNotIn("passed", result.stdout)

    def test_cli_missing_root_fails_without_traceback(self):
        result = self.run_checker(self.root / "missing")
        self.assertEqual(result.returncode, 1)
        self.assertNotIn("Traceback", result.stderr)


class PrivateWorkflowGuardTests(unittest.TestCase):
    def test_every_private_job_preserves_the_accidental_copy_guard(self):
        workflow = ROOT / ".github/workflows/checks.yml"
        if not workflow.exists():
            self.skipTest("Private workflows are deliberately absent from public exports.")
        # Deliberately test the checked-in text, not arbitrary contributor YAML.
        # This regression check is no security boundary: a PR can edit it too.
        text = workflow.read_text(encoding="utf-8")
        jobs = dict(re.findall(r"^  ([a-z_]+):\n(.*?)(?=^  [a-z_]+:|\Z)",
                               text.split("\njobs:\n", 1)[1], re.M | re.S))
        guard = ("github.event.repository.private == true && "
                 "github.repository_id == '1391009005' && "
                 "(github.event_name != 'pull_request' || "
                 "github.event.pull_request.head.repo.full_name == github.repository)")
        suffixes = {
            "plan": "",
            "linux": " && needs.plan.outputs.stages != ''",
            "advisories": "",
            "windows": " && vars.CI_WINDOWS_RUNS_ON != 'off' && "
                       "!startsWith(github.head_ref || github.ref_name, 'report/')",
        }
        self.assertEqual(set(jobs), set(suffixes))
        for name, suffix in suffixes.items():
            with self.subTest(job=name):
                self.assertEqual(re.findall(r"^    if: (.*)$", jobs[name], re.M),
                                 [guard + suffix])
        self.assertIn("permissions:\n  contents: read\n", text)
        self.assertEqual(text.count("- uses: actions/checkout@v4"), 4)
        self.assertEqual(text.count("persist-credentials: false"), 4)
        self.assertNotIn("pull_request_target:", text)
        self.assertNotIn("workflow_run:", text)
        self.assertNotIn("secrets.", text)


if __name__ == "__main__":
    unittest.main()
