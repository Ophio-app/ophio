# SPDX-License-Identifier: Apache-2.0
"""Parser and real-command mutation tests for the suppression baseline."""
from collections import Counter
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

from scripts.check_suppressions import eslint_counts, load_baseline, rust_counts

SCRIPT = Path(__file__).resolve().parents[1] / "check_suppressions.py"


class ParserTests(unittest.TestCase):
    def test_rust_lists_inner_attributes_and_multiline(self):
        self.assertEqual(rust_counts('''
            #![allow(dead_code, clippy::too_many_arguments)]
            #[allow(
                unused_variables,
                clippy /* explanation */ :: type_complexity,
                reason = "A reason containing commas, and allow(fake)",
            )]
        '''), Counter({"rust:dead_code": 1, "rust:clippy::too_many_arguments": 1,
                       "rust:unused_variables": 1, "rust:clippy::type_complexity": 1}))

    def test_rust_conditional_and_nested_attributes(self):
        self.assertEqual(rust_counts('''
            #[cfg_attr(any(unix, target_os = "windows"),
                allow(dead_code, unused),
                cfg_attr(test, allow(dead_code)), derive(Clone))]
            #![cfg_attr(test, allow(clippy::unwrap_used))]
            #[other(allow(not_a_lint_attribute))]
            #[warn(unused)] #[expect(dead_code)]
        '''), Counter({"rust:dead_code": 2, "rust:unused": 1,
                       "rust:clippy::unwrap_used": 1}))

    def test_rust_comments_strings_chars_and_lifetimes(self):
        self.assertEqual(rust_counts('''
            // #[allow(fake)]
            /* nested /* #[allow(fake)] */ #[allow(fake)] */
            let text = "escaped \\\" #[allow(fake)]";
            let bytes = b"#[allow(fake)]";
            let raw = br###"quotes " #[allow(fake)] "###;
            let c = '\\''; let byte = b'"';
            fn real<'a>(value: &'a str) { }
            #[allow(real)]
        '''), Counter({"rust:real": 1}))

    def test_rust_attributes_in_macros_still_count(self):
        self.assertEqual(rust_counts('''
            macro_rules! m { () => { #[allow(dead_code)] fn x() {} }; }
            #[allow(dead_code)] fn y() {}
        '''), Counter({"rust:dead_code": 2}))

    def test_eslint_lists_multiline_explanations_and_all_rules(self):
        self.assertEqual(eslint_counts('''
            /* eslint-disable @typescript-eslint/no-explicit-any,
             * no-console -- This is intentional. */
            // eslint-disable-next-line no-console, no-alert -- explanation
            run(); // eslint-disable-line no-console
            /* eslint-disable -- A blanket exception. */
            // eslint-disable-next-line
            /* eslint-enable no-console */
            // Talking about eslint-disable is not a directive.
        '''), Counter({"eslint:@typescript-eslint/no-explicit-any": 1,
                       "eslint:no-console": 3, "eslint:no-alert": 1, "eslint:*": 2}))

    def test_eslint_ignores_quoted_text_and_regular_expressions(self):
        self.assertEqual(eslint_counts(r'''
            const a = "// eslint-disable fake";
            const b = '/* eslint-disable fake */';
            const c = `// eslint-disable fake ${"/* eslint-disable fake */"}`;
            const pattern = /[/*] eslint-disable fake [*/]/;
            // eslint-disable-next-line real
        '''), Counter({"eslint:real": 1}))

    def test_eslint_comments_inside_template_expressions_count(self):
        self.assertEqual(eslint_counts('''
            const text = `not a // eslint-disable fake ${
                /* eslint-disable real */
                { nested: `text ${ /* eslint-disable nested */ value }` }
            }`;
        '''), Counter({"eslint:real": 1, "eslint:nested": 1}))

    def test_directive_names_must_match(self):
        self.assertFalse(eslint_counts('''
            // eslint-disabled fake
            // eslint-disable-next-lines fake
            /* example: eslint-disable fake */
        '''))


class CommandTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        subprocess.run(["git", "init", "-q", str(self.root)], check=True)
        (self.root / "client").mkdir()
        self.rust = self.root / "lib.rs"
        self.rust.write_text("#[allow(dead_code)] fn example() {}\n", encoding="utf-8")
        self.client = self.root / "client" / "app.ts"
        self.client.write_text("// eslint-disable-next-line no-console\n", encoding="utf-8")
        self.baseline = self.root / "scripts/policies/lint-suppressions.json"
        self.baseline.parent.mkdir(parents=True)
        self.write_baseline({"rust:dead_code": 1, "eslint:no-console": 1})
        subprocess.run(["git", "-C", str(self.root), "add", "."], check=True)

    def write_baseline(self, counts):
        self.baseline.write_text(json.dumps({"version": 1, "counts": counts}), encoding="utf-8")

    def run_check(self, *args):
        return subprocess.run([sys.executable, str(SCRIPT), "--root", str(self.root), *args],
                              capture_output=True, text=True, encoding="utf-8")

    def test_baseline_passes(self):
        result = self.run_check()
        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        self.assertIn("2 exceptions across 2 lint names", result.stdout)

    def test_rust_mutation_fails_then_deliberate_baseline_edit_recovers(self):
        self.rust.write_text("#![cfg_attr(test, allow(dead_code, unused))]\n"
                             "#[allow(dead_code)] fn example() {}", encoding="utf-8")
        result = self.run_check()
        self.assertEqual(result.returncode, 1, result.stderr + result.stdout)
        self.assertIn("rust:dead_code has 2 suppressions; baseline allows 1", result.stdout)
        self.assertIn("rust:unused has 1 suppressions; baseline allows 0", result.stdout)
        self.assertIn("lib.rs", result.stdout)
        self.assertIn("protected baseline scripts/policies/lint-suppressions.json", result.stdout)
        self.assertIn("--show-counts", result.stdout)
        self.write_baseline({"rust:dead_code": 2, "rust:unused": 1, "eslint:no-console": 1})
        self.assertEqual(self.run_check().returncode, 0)

    def test_eslint_mutation_fails(self):
        with self.client.open("a", encoding="utf-8") as output:
            output.write("/* eslint-disable no-console, no-alert */\n")
        result = self.run_check()
        self.assertEqual(result.returncode, 1)
        self.assertIn("eslint:no-console has 2 suppressions; baseline allows 1", result.stdout)
        self.assertIn("eslint:no-alert has 1 suppressions; baseline allows 0", result.stdout)
        self.assertIn("client/app.ts", result.stdout)

    def test_decreases_and_deleted_files_pass_with_reminder(self):
        self.rust.unlink()
        self.client.write_text("// Nothing is suppressed.\n", encoding="utf-8")
        result = self.run_check()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("lower rust:dead_code", result.stdout)
        self.assertIn("lower eslint:no-console", result.stdout)
        self.assertIn("from 1 to 0", result.stdout)

    def test_new_untracked_source_is_counted_but_ignored_files_are_not(self):
        (self.root / "new.rs").write_text("#[allow(unused)] fn f() {}", encoding="utf-8")
        (self.root / ".gitignore").write_text("ignored/\n", encoding="utf-8")
        (self.root / "ignored").mkdir()
        (self.root / "ignored/generated.rs").write_text("#[allow(fake)]", encoding="utf-8")
        result = self.run_check()
        self.assertEqual(result.returncode, 1)
        self.assertIn("rust:unused", result.stdout)
        self.assertNotIn("rust:fake", result.stdout)

    def test_only_client_javascript_counts(self):
        (self.root / "outside.js").write_text("/* eslint-disable fake */", encoding="utf-8")
        self.assertEqual(self.run_check().returncode, 0)

    def test_show_counts_does_not_update_baseline(self):
        original = self.baseline.read_bytes()
        self.rust.unlink()
        result = self.run_check("--show-counts")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout), {"eslint:no-console": 1})
        self.assertEqual(self.baseline.read_bytes(), original)

    def test_malformed_or_missing_baseline_fails_closed(self):
        for data in ([], {"version": 2, "counts": {}},
                     {"version": 1, "counts": {"rust:unused": -1}},
                     {"version": 1, "counts": {"rust:unused": True}},
                     {"version": 1, "counts": {"typo": 1}}):
            with self.subTest(data=data):
                self.baseline.write_text(json.dumps(data), encoding="utf-8")
                self.assertEqual(self.run_check().returncode, 2)
                with self.assertRaises(ValueError):
                    load_baseline(self.baseline)
        self.baseline.unlink()
        self.assertEqual(self.run_check().returncode, 2)


if __name__ == "__main__":
    unittest.main()
