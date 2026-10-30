# SPDX-License-Identifier: Apache-2.0
"""Committed-source audit checks using synthetic repositories, without private terms."""

from __future__ import annotations

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
import source_candidate
from release_common import ReleaseError


def commit_all(root: Path) -> str:
    subprocess.run(["git", "add", "."], cwd=root, check=True)
    subprocess.run(
        ["git", "-c", "user.name=Audit fixture",
         "-c", "user.email=fixture@example.invalid", "-c", "commit.gpgsign=false",
         "commit", "-qm", "Synthetic audit input"],
        cwd=root, check=True,
    )
    return subprocess.check_output(
        ["git", "rev-parse", "HEAD"], cwd=root, text=True, encoding="utf-8"
    ).strip()


class SourceAuditReview(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory(prefix="source-audit-test-")
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.root = self.base / "repo"
        subprocess.run(["git", "init", "-q", str(self.root)], check=True)

    def add_file(self, name: str, data: bytes) -> None:
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)

    def home_policy(self, users: list[str]) -> None:
        self.add_file(
            "packaging/source-policy.toml",
            ("generated = []\n[home_examples]\n"
             f"users = {json.dumps(users)}\nreason = \"Synthetic examples\"\n").encode(),
        )

    def test_fixed_fingerprint_catches_mixed_case_substrings_in_paths_and_bytes(self) -> None:
        sentinel = b"Kestrelwood"
        with (
            mock.patch.object(source_candidate, "LEGACY_NAME_LENGTH", len(sentinel)),
            mock.patch.object(source_candidate, "LEGACY_NAME_SHA256", hashlib.sha256(sentinel.lower()).hexdigest()),
        ):
            hits = source_candidate.inspect(
                "notes/prefixKESTRELWOODsuffix.txt",
                b"binary\\x00" + sentinel.upper() + b"suffix\n",
                [],
            )
        self.assertEqual(
            {item["rule"] for item in hits},
            {"internal-codename-filename", "internal-codename-content"},
        )
        self.assertNotIn(sentinel.decode().lower(), json.dumps(hits).lower())

    def test_trusted_audit_fails_closed_without_private_list(self) -> None:
        self.add_file("README.md", b"Public source\n")
        commit_all(self.root)
        with self.assertRaisesRegex(ReleaseError, "Required private-term list"):
            source_candidate.audit(self.root, require_private_terms=True)
        self.assertEqual(source_candidate.audit(self.root)[1], [])
        private_file = self.root / ".git/info/private-terms"
        private_file.write_text("# no terms\n", encoding="utf-8")
        with self.assertRaisesRegex(ReleaseError, "Required private-term list"):
            source_candidate.audit(self.root, require_private_terms=True)
        private_file.write_text("SyntheticUnavailableName\n", encoding="utf-8")
        self.assertEqual(source_candidate.audit(self.root, require_private_terms=True)[1], [])

    def test_bom_comment_only_lists_are_empty_and_ordinary_lists_still_work(self) -> None:
        private_file = self.root / ".git/info/private-terms"
        for contents in ("﻿", "﻿# comment only\n", "﻿# one\n# two\n"):
            with self.subTest(contents=contents):
                private_file.write_text(contents, encoding="utf-8")
                self.assertIsNone(source_candidate.private_terms(self.root))
                with self.assertRaisesRegex(ReleaseError, "Required private-term list is empty"):
                    source_candidate.private_terms(self.root, required=True)
        term = "Kestrel" + "wood"
        for contents in (f"{term}\n", f"﻿{term}\n", f"﻿# comment\n{term}\n"):
            with self.subTest(contents=contents):
                private_file.write_text(contents, encoding="utf-8")
                pattern = source_candidate.private_terms(self.root, required=True)
                self.assertIsNotNone(pattern)
                self.assertIsNotNone(pattern.search(term.upper()))
                self.assertIsNone(pattern.search("OtherPublicName"))

    def test_candidate_policy_cannot_add_a_home_user_without_private_terms(self) -> None:
        username = "sensitive" + "user"
        self.home_policy(["sam", "you", username])
        self.add_file("docs/setup.txt", source_candidate.HOME_PREFIX + username.encode() + b"/work\n")
        revision = commit_all(self.root)
        self.assertIsNone(source_candidate.private_terms(self.root))
        actual_revision, hits = source_candidate.audit(self.root)
        self.assertEqual(actual_revision, revision)
        self.assertIn(
            {"path": "docs/setup.txt", "line": 1, "rule": "absolute-home-path"}, hits
        )
        self.assertNotIn(username, json.dumps(hits))

    def test_established_synthetic_home_examples_still_pass(self) -> None:
        self.home_policy(["sam", "you"])
        self.add_file(
            "docs/setup.txt",
            source_candidate.HOME_PREFIX + b"sam/work\n"
            + source_candidate.MAC_HOME_PREFIX + b"you/work\n"
            + b"C:\\Users\\sam\\work\n",
        )
        revision = commit_all(self.root)
        self.assertEqual(source_candidate.audit(self.root), (revision, []))

    def test_export_ignore_cannot_hide_committed_key_file(self) -> None:
        self.add_file("docs/secret.key", b"Synthetic opaque material\n")
        self.add_file(".gitattributes", b"docs/secret.key export-ignore\n")
        commit_all(self.root)
        with self.assertRaisesRegex(ReleaseError, "Archive differs from the commit"):
            source_candidate.audit(self.root)

    def test_local_export_attributes_cannot_hide_committed_bytes(self) -> None:
        self.add_file("docs/secret.key", b"Synthetic opaque material\n")
        self.add_file("README.md", b"Public source\n")
        commit_all(self.root)
        (self.root / ".git/info/attributes").write_text(
            "docs/secret.key export-ignore\n", encoding="utf-8"
        )
        with self.assertRaisesRegex(ReleaseError, "Archive differs from the commit"):
            source_candidate.audit(self.root)

    def test_export_subst_cannot_rewrite_committed_bytes(self) -> None:
        self.add_file("README.md", b"$Format:%H$\n")
        self.add_file(".gitattributes", b"README.md export-subst\n")
        commit_all(self.root)
        with self.assertRaisesRegex(ReleaseError, "Archive differs from the commit"):
            source_candidate.audit(self.root)

    def test_default_cli_and_json_report_keep_findings_redacted(self) -> None:
        username = "sensitive" + "user"
        self.home_policy([username])
        self.add_file("docs/setup.txt", source_candidate.HOME_PREFIX + username.encode() + b"/work\n")
        revision = commit_all(self.root)
        report = self.base / "audit.json"
        output = io.StringIO()
        with (
            mock.patch.object(source_candidate, "ROOT", self.root),
            mock.patch.object(sys, "argv", ["source_candidate.py", "--json", str(report)]),
            contextlib.redirect_stdout(output),
        ):
            self.assertEqual(source_candidate.main(), 1)
        result = json.loads(report.read_text(encoding="utf-8"))
        self.assertEqual(result["revision"], revision)
        self.assertEqual(result["findings"], [
            {"path": "docs/setup.txt", "line": 1, "rule": "absolute-home-path"}
        ])
        self.assertIn("FAIL: 1 findings", output.getvalue())
        self.assertNotIn(username, output.getvalue())
        self.assertNotIn(username, report.read_text(encoding="utf-8"))

    def test_integrity_error_does_not_echo_private_filename(self) -> None:
        term = "Kestrel" + "wood"
        name = f"docs/{term}.key"
        self.add_file(name, b"Synthetic opaque material\n")
        self.add_file(".gitattributes", b"*.key export-ignore\n")
        commit_all(self.root)
        (self.root / ".git/info/private-terms").write_text(term, encoding="utf-8")
        with self.assertRaises(ReleaseError) as failure:
            source_candidate.audit(self.root)
        self.assertNotIn(term.lower(), str(failure.exception).lower())


if __name__ == "__main__":
    unittest.main()
