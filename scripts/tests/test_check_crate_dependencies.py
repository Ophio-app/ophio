# SPDX-License-Identifier: Apache-2.0
"""Mutation and metadata tests for the reviewed crate dependency permissions."""
from pathlib import Path
import os
import shutil
import subprocess
import tempfile
import unittest

from scripts.check_crate_dependencies import check, workspace_edges


class CrateDependencyTests(unittest.TestCase):
    def test_current_edges_pass(self):
        self.assertEqual(check({"a": {"b"}, "b": set()}, {"a": ["b"], "b": []}), ([], []))

    def test_added_edge_fails_with_removal_and_review_instructions(self):
        errors, _ = check({"a": {"b"}, "b": set()}, {"a": [], "b": []})
        self.assertEqual(len(errors), 1)
        self.assertIn("a depends on b", errors[0])
        self.assertIn("Remove this dependency", errors[0])
        self.assertIn("crate-dependencies.toml", errors[0])

    def test_explicitly_approving_added_edge_recovers(self):
        actual = {"a": {"b"}, "b": set()}
        self.assertTrue(check(actual, {"a": [], "b": []})[0])
        self.assertFalse(check(actual, {"a": ["b"], "b": []})[0])

    def test_new_crate_without_edges_requires_review(self):
        errors, _ = check({"new": set()}, {})
        self.assertIn("use [] if none", errors[0])

    def test_removed_edges_pass_with_reminder_to_remove_permission(self):
        errors, notes = check({"a": set()}, {"a": ["b"], "b": []})
        self.assertFalse(errors)
        self.assertEqual(len(notes), 2)
        self.assertTrue(any("remove this permission" in note for note in notes))

    def test_real_metadata_rejects_unreviewed_edge_and_phone_lock_change(self):
        if not shutil.which("cargo"):
            self.skipTest("Cargo is required for the real metadata mutation test")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name in ("a", "b"):
                crate = root / name
                (crate / "src").mkdir(parents=True)
                (crate / "src/lib.rs").write_text("", encoding="utf-8")
                (crate / "Cargo.toml").write_text(
                    f'[package]\nname = "{name}"\nversion = "0.1.0"\nedition = "2024"\n', encoding="utf-8",
                )
            (root / "Cargo.toml").write_text('[workspace]\nmembers = ["a", "b"]\nresolver = "3"\n', encoding="utf-8")
            policy = root / "scripts/policies/crate-dependencies.toml"
            policy.parent.mkdir(parents=True)
            policy.write_text('[allowed]\na = []\nb = []\n', encoding="utf-8")
            # Isolate the fixture from a parent build's custom target and git
            # hook environment. No registry packages or compilation are needed.
            env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
            subprocess.run(["cargo", "generate-lockfile", "--offline"], cwd=root, env=env,
                           check=True, capture_output=True)
            script = Path(__file__).resolve().parents[1] / "check_crate_dependencies.py"
            def run_check():
                return subprocess.run(["python3", str(script), "--root", str(root)], env=env,
                                      capture_output=True, text=True, encoding="utf-8", check=False)
            self.assertEqual(run_check().returncode, 0)
            with (root / "a/Cargo.toml").open("a", encoding="utf-8") as manifest:
                manifest.write('\n[dependencies]\nb = { path = "../b" }\n')
            # This is the exact lock resolution used by the quick stage; a new
            # local dependency must fail even without anything to download.
            locked = subprocess.run(
                ["cargo", "metadata", "--locked", "--offline", "--format-version", "1",
                 "--manifest-path", str(root / "a/Cargo.toml")], cwd=root, env=env,
                capture_output=True, text=True, encoding="utf-8", check=False,
            )
            self.assertNotEqual(locked.returncode, 0)
            self.assertIn("lock", locked.stderr.lower())
            failure = run_check()
            self.assertNotEqual(failure.returncode, 0)
            self.assertIn("a depends on b", failure.stderr)
            self.assertIn("add it to scripts/policies/crate-dependencies.toml", failure.stderr)
            policy.write_text('[allowed]\na = ["b"]\nb = []\n', encoding="utf-8")
            self.assertEqual(run_check().returncode, 0)

    def test_declared_paths_include_optional_target_test_and_renamed_edges(self):
        root = Path("/tmp/dependency-test").resolve()
        def package(name, dependencies=()):
            return {"id": name, "name": name, "manifest_path": str(root / name / "Cargo.toml"),
                    "dependencies": list(dependencies)}
        def edge(name, **extra):
            return {"name": name, "path": str(root / name), **extra}
        metadata = {"workspace_members": ["a", "b", "c", "d"], "packages": [
            package("a", [edge("b", optional=True, rename="alias"),
                          edge("c", target="cfg(windows)", kind="dev"),
                          edge("d", kind="build"),
                          {"name": "registry", "path": None},
                          {"name": "b", "path": str(root / "external-b")}]),
            package("b"), package("c"), package("d"), package("registry"),
        ]}
        self.assertEqual(workspace_edges(metadata), {"a": {"b", "c", "d"}, "b": set(), "c": set(), "d": set()})


if __name__ == "__main__":
    unittest.main()
