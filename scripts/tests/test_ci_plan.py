# SPDX-License-Identifier: Apache-2.0
"""Check which CI stages a change needs, and that passed trees are not run again."""
from pathlib import Path
import os
import shutil
import subprocess
import tempfile
import time
import unittest

SCRIPTS = Path(__file__).resolve().parents[1]
ORDER = ["fmt", "clippy", "test", "parallel", "windows", "client", "intake-e2e", "tauri", "python",
         "source", "licenses", "package-linux", "package-windows"]
ALWAYS = ["python", "source", "licenses"]
CLIENT = ["client", "tauri", "intake-e2e"]
RUST = ["fmt", "clippy", "test", "windows", "tauri", "intake-e2e"]
REPEATS = ["parallel"]
PACKAGES = ["package-linux", "package-windows"]


def ordered(*groups):
    wanted = {stage for group in groups for stage in group}
    return [stage for stage in ORDER if stage in wanted]


class CiPlanTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.root)
        (self.root / "scripts").mkdir()
        for name in ("ci.sh", "ci_plan.py"):
            shutil.copy(SCRIPTS / name, self.root / "scripts" / name)
        self.marks = self.root.parent / f"{self.root.name}-marks"
        self.marks.mkdir()
        self.addCleanup(shutil.rmtree, self.marks)
        # Run from a git hook, GIT_DIR and the like would point every git
        # command here at the repository being pushed.
        inherited = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
        self.env = dict(inherited, GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1")
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.name", "CI plan test")
        self.git("config", "user.email", "ci-plan@example.invalid")
        self.write("crates/protocol/src/lib.rs", "pub struct Frame;\n")
        self.write("crates/protocol/Cargo.toml", "[dependencies]\nts-rs.workspace = true\n")
        self.write("crates/services/src/lib.rs", "pub fn serve() {}\n")
        self.write("crates/companion/src/brand.rs",
                   'const BRAND: &str = include_str!("../../../client/src/brand.ts");\n')
        self.write("client/src/brand.ts", "export const brand = 'Øphio';\n")
        self.write("client/src/screens/Home.tsx", "export function Home() {}\n")
        self.write("client/src/fixtures.test.ts",
                   "import frames from '../../crates/ports/test-fixtures/frames.json';\n")
        self.write("crates/ports/test-fixtures/frames.json", "[]\n")
        self.write("client/src-tauri/Cargo.toml",
                   '[dependencies]\nservices = { path = "../../crates/services" }\n')
        self.write("client/package-lock.json", "{}\n")
        self.write("docs/guide.md", "# Guide\n")
        self.write("README.md", "# Øphio\n")
        self.commit("start")

    def git(self, *args):
        return subprocess.run(["git", *args], cwd=self.root, env=self.env, check=True,
                              capture_output=True, text=True,
                              encoding="utf-8").stdout.strip()

    def write(self, path, text):
        target = self.root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text, encoding="utf-8")

    def commit(self, message):
        self.git("add", "-A")
        self.git("commit", "-q", "-m", message)

    def change(self, *paths):
        for path in paths:
            self.write(path, f"// changed {time.monotonic_ns()}\n")
        self.commit("change " + " ".join(paths))

    def mark_head(self):
        (self.marks / self.git("rev-parse", "HEAD^{tree}")).touch()

    def run_plan(self, *args):
        return subprocess.run(
            ["python3", str(self.root / "scripts/ci_plan.py"), *args],
            cwd=self.root, env=self.env, check=True, capture_output=True, text=True,
            encoding="utf-8",
        )

    def plan(self, *extra, marks=True):
        args = ["--marks", str(self.marks)] if marks else []
        done = self.run_plan(*args, *extra)
        fields = dict(line.split("=", 1) for line in done.stdout.splitlines())
        return fields["stages"].split(), fields["base"], done.stderr

    def after_passed(self, *paths):
        self.mark_head()
        base = self.git("rev-parse", "HEAD")
        self.change(*paths)
        stages, found, _ = self.plan()
        self.assertEqual(found, base)
        return stages

    def test_screen_change_skips_rust_stages(self):
        self.assertEqual(self.after_passed("client/src/screens/Home.tsx"),
                         ordered(CLIENT, ALWAYS))

    def test_rust_change_skips_client_stage(self):
        self.assertEqual(self.after_passed("crates/services/src/lib.rs"),
                         ordered(RUST, ALWAYS))

    def test_app_crate_change_still_checks_the_tauri_app(self):
        self.assertIn("tauri", self.after_passed("crates/services/src/lib.rs"))

    def test_code_change_skips_the_repeat_runs_and_the_packages(self):
        stages = self.after_passed("crates/services/src/lib.rs", "client/src/brand.ts")
        self.assertEqual(stages, ordered(RUST, CLIENT, ALWAYS))
        for skipped in REPEATS + PACKAGES:
            self.assertNotIn(skipped, stages)

    def test_test_helper_change_runs_the_repeat_runs(self):
        self.assertEqual(self.after_passed("crates/test-support/check-parallel.sh"),
                         ordered(RUST, REPEATS, ALWAYS))

    def test_manifest_change_builds_the_packages(self):
        self.assertEqual(self.after_passed("crates/services/Cargo.toml"),
                         ordered(RUST, PACKAGES, ALWAYS))
        self.assertEqual(self.after_passed("Cargo.lock"), ordered(RUST, PACKAGES, ALWAYS))
        self.assertEqual(self.after_passed("client/src-tauri/Cargo.toml"),
                         ordered(CLIENT, PACKAGES, ALWAYS))

    def test_packaging_change_builds_only_the_packages(self):
        self.assertEqual(self.after_passed("packaging/linux/install.sh",
                                           "scripts/package-linux.sh",
                                           "scripts/package_linux.py"),
                         ordered(PACKAGES, ALWAYS))

    def test_client_file_rust_reads_runs_both(self):
        self.assertEqual(self.after_passed("client/src/brand.ts"), ordered(RUST, CLIENT, ALWAYS))

    def test_rust_fixture_client_reads_runs_both(self):
        self.assertEqual(self.after_passed("crates/ports/test-fixtures/frames.json"),
                         ordered(RUST, CLIENT, ALWAYS))

    def test_protocol_change_runs_both(self):
        self.assertEqual(self.after_passed("crates/protocol/src/lib.rs"),
                         ordered(RUST, CLIENT, ALWAYS))

    def test_generated_bindings_change_runs_both(self):
        self.assertEqual(self.after_passed("client/src/protocol/generated/Frame.ts"),
                         ordered(RUST, CLIENT, ALWAYS))

    def test_locked_client_packages_run_everything(self):
        self.assertEqual(self.after_passed("client/package-lock.json"), ORDER)

    def test_docs_change_runs_only_the_quick_stages(self):
        self.assertEqual(self.after_passed("docs/guide.md", "README.md"), ALWAYS)

    def test_unrecognised_change_runs_everything(self):
        self.assertEqual(self.after_passed("scripts/release.sh"), ORDER)
        self.assertEqual(self.after_passed(".github/workflows/checks.yml"), ORDER)

    def test_changes_add_up_since_the_last_passed_tree(self):
        self.mark_head()
        self.change("client/src/screens/Home.tsx")
        self.change("crates/services/src/lib.rs")
        stages, base, _ = self.plan()
        self.assertEqual(base, self.git("rev-parse", "HEAD~2"))
        self.assertEqual(stages, ordered(RUST, CLIENT, ALWAYS))

    def test_tree_that_passed_runs_nothing(self):
        self.change("crates/services/src/lib.rs")
        self.mark_head()
        stages, _, why = self.plan()
        self.assertEqual(stages, [])
        self.assertIn("already passed", why)

    def test_merge_of_a_passed_branch_runs_nothing(self):
        self.git("checkout", "-q", "-b", "feature")
        self.change("crates/services/src/lib.rs")
        self.mark_head()
        self.git("checkout", "-q", "main")
        self.git("merge", "-q", "--squash", "feature")
        self.git("commit", "-q", "-m", "squash")
        self.assertEqual(self.plan()[0], [])

    def test_no_passed_tree_runs_everything(self):
        self.change("client/src/screens/Home.tsx")
        stages, base, why = self.plan()
        self.assertEqual((stages, base), (ORDER, ""))
        self.assertIn("no passed tree", why)

    def test_no_record_folder_runs_everything(self):
        self.mark_head()
        self.change("docs/guide.md")
        stages, _, why = self.plan(marks=False)
        self.assertEqual(stages, ORDER)
        self.assertIn("keeps no record", why)
        shutil.rmtree(self.marks)
        stages, _, why = self.plan()
        self.assertEqual(stages, ORDER)
        self.assertIn("passed on this host yet", why)
        self.marks.mkdir()

    def test_full_runs_everything_even_for_a_passed_tree(self):
        self.mark_head()
        self.assertEqual(self.plan("--full")[0], ORDER)

    def test_record_marks_head_and_forgets_old_trees(self):
        old = self.marks / ("0" * 40)
        old.touch()
        stale = time.time() - 91 * 86400
        os.utime(old, (stale, stale))
        recent = self.marks / ("1" * 40)
        recent.touch()
        self.run_plan("--marks", str(self.marks), "--record")
        self.assertTrue((self.marks / self.git("rev-parse", "HEAD^{tree}")).is_file())
        self.assertFalse(old.exists())
        self.assertTrue(recent.exists())

    def test_record_without_a_kept_folder_does_nothing(self):
        missing = self.root / "absent" / "passed"
        self.run_plan("--marks", str(missing), "--record")
        self.assertFalse(missing.exists())


if __name__ == "__main__":
    unittest.main()
