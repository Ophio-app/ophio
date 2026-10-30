# SPDX-License-Identifier: Apache-2.0
"""Source export checks using disposable repositories and deliberately hostile archives."""

from __future__ import annotations

import hashlib
import io
import json
from pathlib import Path
import stat
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import export_source
from release_common import ReleaseError
import source_candidate


def git(root: Path, *args: str) -> str:
    return subprocess.check_output(
        ["git", *args], cwd=root, text=True, encoding="utf-8"
    ).strip()


def commit_all(root: Path) -> str:
    subprocess.run(["git", "add", "."], cwd=root, check=True)
    subprocess.run(
        [
            "git", "-c", "user.name=Export fixture",
            "-c", "user.email=fixture@example.invalid",
            "-c", "commit.gpgsign=false", "commit", "-qm", "Synthetic source",
        ],
        cwd=root,
        check=True,
    )
    private_file = root / ".git/info/private-terms"
    if not private_file.exists():
        private_file.write_text("SyntheticUnavailableName\n", encoding="utf-8")
    return git(root, "rev-parse", "HEAD")


class ExportReview(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory(prefix="source-export-test-")
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.root = self.base / "repo"
        subprocess.run(["git", "init", "-q", str(self.root)], check=True)
        self.destination = self.base / "export"
        self.receipt = self.base / "receipt.json"

    def add_file(self, name: str, data: bytes, mode: int = 0o644) -> Path:
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        path.chmod(mode)
        return path

    def good_commit(self) -> str:
        self.add_file("README.md", b"Public source\n")
        return commit_all(self.root)

    def assert_no_output(self) -> None:
        self.assertFalse(self.destination.exists())
        self.assertFalse(self.receipt.exists())

    def test_export_requires_nonempty_private_list_even_for_clean_source(self) -> None:
        revision = self.good_commit()
        private_file = self.root / ".git/info/private-terms"
        for contents in (None, "", "# comments only\n", "﻿", "﻿# comments only\n"):
            with self.subTest(contents=contents):
                private_file.unlink(missing_ok=True)
                if contents is not None:
                    private_file.write_text(contents, encoding="utf-8")
                with self.assertRaisesRegex(ReleaseError, "Required private-term list"):
                    export_source.export(self.root, revision, self.destination, self.receipt)
                self.assert_no_output()
        self.assertEqual(source_candidate.audit(self.root), (revision, []))

    def test_bom_first_private_term_refuses_official_export_without_leaking_it(self) -> None:
        term = "Kestrel" + "wood"
        self.add_file(f"docs/prefix-{term.upper()}-suffix.txt", b"Public fixture\n")
        revision = commit_all(self.root)
        private_file = self.root / ".git/info/private-terms"
        private_file.write_text(f"﻿{term}\n", encoding="utf-8")
        with self.assertRaisesRegex(ReleaseError, "Source audit refused") as failure:
            export_source.export(self.root, revision, self.destination, self.receipt)
        self.assertNotIn(term.lower(), str(failure.exception).lower())
        self.assert_no_output()
        _, hits = source_candidate.audit(self.root, require_private_terms=True)
        self.assertIn("private-term", {hit["rule"] for hit in hits})
        self.assertNotIn(term.lower(), json.dumps(hits).lower())

    def test_revision_must_be_the_full_commit_object_id(self) -> None:
        revision = self.good_commit()
        git(self.root, "tag", "lightweight", revision)
        subprocess.run(
            ["git", "-c", "user.name=Export fixture",
             "-c", "user.email=fixture@example.invalid", "-c", "tag.gpgsign=false",
             "tag", "-am", "Annotated fixture", "annotated", revision],
            cwd=self.root, check=True,
        )
        tag_object = git(self.root, "rev-parse", "annotated^{tag}")
        for candidate in ("HEAD", "lightweight", "annotated", revision[:12], tag_object):
            with self.subTest(candidate=candidate):
                with self.assertRaises(ReleaseError):
                    export_source.export(self.root, candidate, self.destination, self.receipt)
                self.assert_no_output()
        self.assertEqual(
            export_source.export(self.root, revision, self.destination, self.receipt)["revision"],
            revision,
        )

    def test_old_commit_ignores_later_head_dirty_files_and_untracked_files(self) -> None:
        old = self.good_commit()
        self.add_file("new-in-head.txt", b"Not in old commit\n")
        commit_all(self.root)
        (self.root / "README.md").write_bytes(b"Dirty working tree\n")
        self.add_file("untracked.txt", b"Not committed\n")
        report = export_source.export(self.root, old, self.destination, self.receipt)
        self.assertEqual(report["revision"], old)
        self.assertTrue(report["private_terms_checked"])
        self.assertEqual((self.destination / "README.md").read_bytes(), b"Public source\n")
        self.assertEqual([item["path"] for item in report["files"]], ["README.md"])
        self.assertFalse((self.destination / "new-in-head.txt").exists())
        self.assertFalse((self.destination / "untracked.txt").exists())

    def test_git_replace_objects_cannot_change_pinned_source(self) -> None:
        original = self.good_commit()
        (self.root / "README.md").write_bytes(b"Replacement contents\n")
        replacement = commit_all(self.root)
        git(self.root, "replace", original, replacement)
        report = export_source.export(self.root, original, self.destination, self.receipt)
        self.assertEqual(report["revision"], original)
        self.assertEqual((self.destination / "README.md").read_bytes(), b"Public source\n")

    def test_exact_bytes_modes_and_deterministic_receipt_inventory(self) -> None:
        binary = b"\x00\xffsource\r\n"
        runnable = b"#!/bin/sh\nprintf ready\\n\n"
        self.add_file("tools/binary.dat", binary)
        self.add_file("tools/run.sh", runnable, 0o755)
        revision = commit_all(self.root)
        first = export_source.export(self.root, revision, self.destination, self.receipt)
        second_dir = self.base / "export-again"
        second_receipt = self.base / "receipt-again.json"
        second = export_source.export(self.root, revision, second_dir, second_receipt)
        self.assertEqual(first, second)
        self.assertEqual(json.loads(self.receipt.read_text(encoding="utf-8")), first)
        self.assertEqual(self.receipt.read_bytes(), second_receipt.read_bytes())
        self.assertEqual(first["schema"], 1)
        self.assertEqual(first["audit_findings"], [])
        self.assertEqual(first["directories"], ["tools"])
        self.assertEqual([entry["path"] for entry in first["files"]], ["tools/binary.dat", "tools/run.sh"])
        self.assertEqual(stat.S_IMODE(self.destination.stat().st_mode), 0o700)
        self.assertEqual(stat.S_IMODE(self.receipt.stat().st_mode), 0o600)
        for name, data, mode in (("tools/binary.dat", binary, 0o644), ("tools/run.sh", runnable, 0o755)):
            with self.subTest(name=name):
                target = self.destination / name
                item = next(row for row in first["files"] if row["path"] == name)
                self.assertEqual(target.read_bytes(), data)
                self.assertEqual(stat.S_IMODE(target.stat().st_mode), mode)
                self.assertEqual(item["mode"], f"{mode:04o}")
                self.assertEqual(item["size"], len(data))
                self.assertEqual(item["sha256"], hashlib.sha256(data).hexdigest())
                self.assertEqual(item["git_blob"], git(self.root, "rev-parse", f"{revision}:{name}"))
        inventory = {key: first[key] for key in ("directories", "files")}
        self.assertEqual(
            first["inventory_sha256"],
            hashlib.sha256(export_source.json_bytes(inventory)).hexdigest(),
        )

    def test_export_has_no_repository_history_origin_or_workflows(self) -> None:
        self.add_file("README.md", b"Source\n")
        self.add_file(".github/workflows/check.yml", b"name: internal\n")
        self.add_file(".github/workflows/nested/other.yml", b"name: other\n")
        self.add_file(".github/ISSUE_TEMPLATE/question.md", b"Public template\n")
        revision = commit_all(self.root)
        git(self.root, "remote", "add", "origin", "https://example.invalid/private.git")
        report = export_source.export(self.root, revision, self.destination, self.receipt)
        self.assertEqual(
            [item["path"] for item in report["files"]],
            [".github/ISSUE_TEMPLATE/question.md", "README.md"],
        )
        self.assertEqual(
            report["omitted_workflows"],
            [".github/workflows/check.yml", ".github/workflows/nested/other.yml"],
        )
        self.assertTrue((self.destination / ".github/ISSUE_TEMPLATE/question.md").is_file())
        self.assertFalse((self.destination / ".github/workflows").exists())
        self.assertFalse((self.destination / ".git").exists())
        self.assertFalse((self.destination / "origin").exists())
        self.assertEqual(
            {path.relative_to(self.destination).as_posix() for path in self.destination.rglob("*")},
            {"README.md", ".github", ".github/ISSUE_TEMPLATE", ".github/ISSUE_TEMPLATE/question.md"},
        )
        self.assertNotIn("private.git", self.receipt.read_text(encoding="utf-8"))

    def test_private_terms_are_recorded_and_matches_refuse_without_echoing(self) -> None:
        term = "Kestrel" + "wood"
        self.add_file("README.md", b"No private terms here\n")
        safe_revision = commit_all(self.root)
        info = self.root / ".git" / "info"
        info.mkdir(exist_ok=True)
        (info / "private-terms").write_text(f"# fixture\n{term}\n", encoding="utf-8")
        report = export_source.export(self.root, safe_revision, self.destination, self.receipt)
        self.assertTrue(report["private_terms_checked"])
        self.assertNotIn(term.lower(), self.receipt.read_text(encoding="utf-8").lower())
        self.destination.rename(self.base / "safe-export")
        self.receipt.rename(self.base / "safe-receipt.json")
        self.add_file(f"notes/{term.lower()}.txt", term.upper().encode("utf-8"))
        matching_revision = commit_all(self.root)
        with self.assertRaises(ReleaseError) as failure:
            export_source.export(self.root, matching_revision, self.destination, self.receipt)
        self.assertNotIn(term.lower(), str(failure.exception).lower())
        self.assert_no_output()

    def test_candidate_policy_cannot_exempt_a_private_home_without_private_terms(self) -> None:
        username = "sensitive" + "user"
        self.add_file(
            "packaging/source-policy.toml",
            ("generated = []\n[home_examples]\n"
             f'users = ["sam", "you", "{username}"]\n'
             'reason = "Claimed example"\n').encode(),
        )
        self.add_file("README.md", source_candidate.HOME_PREFIX + username.encode() + b"/work\n")
        revision = commit_all(self.root)
        (self.root / ".git/info/private-terms").unlink()
        self.assertIsNone(source_candidate.private_terms(self.root))
        _, hits = source_candidate.audit(self.root)
        self.assertIn("absolute-home-path", {hit["rule"] for hit in hits})
        with self.assertRaisesRegex(ReleaseError, "Required private-term list"):
            export_source.export(self.root, revision, self.destination, self.receipt)
        self.assert_no_output()

    def test_forbidden_private_directory_and_key_sentinels_leave_no_output(self) -> None:
        forbidden = "plan" + "ning"
        cases = (
            (f"{forbidden}/notes.txt", b"Private notes\n"),
            ("rec" + "ords/notes.txt", b"Private notes\n"),
            (".claude/internal.txt", b"Private notes\n"),
            ("documents/secret.key", b"Opaque key material\n"),
            ("documents/text.txt", b"-----BEGIN PRIVATE " + b"KEY-----\n"),
            ("documents/link.txt", ("rec" + "ords/").encode("ascii") + b"memo\n"),
        )
        for name, content in cases:
            with self.subTest(name=name), tempfile.TemporaryDirectory(prefix="export-hazard-") as directory:
                root = Path(directory) / "repo"
                subprocess.run(["git", "init", "-q", str(root)], check=True)
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(content)
                revision = commit_all(root)
                destination = Path(directory) / "output"
                receipt = Path(directory) / "receipt.json"
                with self.assertRaises(ReleaseError):
                    export_source.export(root, revision, destination, receipt)
                self.assertFalse(destination.exists())
                self.assertFalse(receipt.exists())

    def test_a_top_level_name_off_the_public_list_is_refused(self) -> None:
        self.add_file("README.md", b"Public source\n")
        self.add_file("drafts/launch.md", b"Not meant to ship\n")
        revision = commit_all(self.root)
        with self.assertRaisesRegex(ReleaseError, "Not on the public top-level list: drafts"):
            export_source.export(self.root, revision, self.destination, self.receipt)
        self.assert_no_output()

    def test_committed_symlinks_inside_and_outside_are_refused(self) -> None:
        for target in ("README.md", "../outside"):
            with self.subTest(target=target), tempfile.TemporaryDirectory(prefix="export-link-") as directory:
                root = Path(directory) / "repo"
                subprocess.run(["git", "init", "-q", str(root)], check=True)
                (root / "README.md").write_text("Public\n", encoding="utf-8")
                (root / "link").symlink_to(target)
                revision = commit_all(root)
                destination = Path(directory) / "output"
                receipt = Path(directory) / "receipt.json"
                with self.assertRaises(ReleaseError):
                    export_source.export(root, revision, destination, receipt)
                self.assertFalse(destination.exists())
                self.assertFalse(receipt.exists())

    def test_nonempty_and_symlink_destinations_preserve_existing_data(self) -> None:
        revision = self.good_commit()
        self.destination.mkdir()
        sentinel = self.destination / "keep.txt"
        sentinel.write_text("Do not touch\n", encoding="utf-8")
        with self.assertRaises(ReleaseError):
            export_source.export(self.root, revision, self.destination, self.receipt)
        self.assertEqual(sentinel.read_text(encoding="utf-8"), "Do not touch\n")
        self.assertFalse(self.receipt.exists())
        alias = self.base / "alias"
        alias.symlink_to(self.destination, target_is_directory=True)
        with self.assertRaises(ReleaseError):
            export_source.export(self.root, revision, alias, self.receipt)
        with self.assertRaises(ReleaseError):
            export_source.export(self.root, revision, alias / "nested", self.receipt)
        self.assertEqual(sentinel.read_text(encoding="utf-8"), "Do not touch\n")
        self.assertFalse((self.destination / "nested").exists())
        self.assertFalse(self.receipt.exists())

    def test_destination_inside_normal_or_bare_git_repository_is_refused(self) -> None:
        revision = self.good_commit()
        bare = self.base / "bare.git"
        subprocess.run(["git", "init", "--bare", "-q", str(bare)], check=True)
        for destination in (self.root / "nested-output", bare / "nested-output"):
            with self.subTest(destination=destination):
                with self.assertRaises(ReleaseError):
                    export_source.export(self.root, revision, destination, self.receipt)
                self.assertFalse(destination.exists())
                self.assertFalse(self.receipt.exists())

    def test_rename_failure_removes_receipt_without_installing_export(self) -> None:
        revision = self.good_commit()
        with mock.patch.object(Path, "rename", side_effect=OSError("Synthetic rename failure")):
            with self.assertRaisesRegex(OSError, "Synthetic rename failure"):
                export_source.export(self.root, revision, self.destination, self.receipt)
        self.assert_no_output()

    def test_destination_populated_after_preflight_is_not_overwritten(self) -> None:
        revision = self.good_commit()
        original_write_archive = export_source.write_archive

        def write_then_add_sentinel(root: Path, commit: str, archive: Path) -> dict:
            entries = original_write_archive(root, commit, archive)
            self.destination.mkdir()
            (self.destination / "keep.txt").write_text("Keep this\n", encoding="utf-8")
            return entries

        with mock.patch.object(export_source, "write_archive", side_effect=write_then_add_sentinel):
            with self.assertRaises(ReleaseError):
                export_source.export(self.root, revision, self.destination, self.receipt)
        self.assertEqual(
            (self.destination / "keep.txt").read_text(encoding="utf-8"), "Keep this\n"
        )
        self.assertEqual([path.name for path in self.destination.iterdir()], ["keep.txt"])
        self.assertFalse(self.receipt.exists())

    def test_existing_empty_destination_is_supported(self) -> None:
        revision = self.good_commit()
        self.destination.mkdir()
        report = export_source.export(self.root, revision, self.destination, self.receipt)
        self.assertEqual((self.destination / "README.md").read_bytes(), b"Public source\n")
        self.assertEqual([row["path"] for row in report["files"]], ["README.md"])

    def test_receipt_must_be_new_outside_export_and_not_a_symlink(self) -> None:
        revision = self.good_commit()
        for receipt in (self.destination, self.destination / "receipt.json"):
            with self.subTest(receipt=receipt):
                with self.assertRaises(ReleaseError):
                    export_source.export(self.root, revision, self.destination, receipt)
                self.assert_no_output()
        self.receipt.write_text("Keep this\n", encoding="utf-8")
        with self.assertRaises(ReleaseError):
            export_source.export(self.root, revision, self.destination, self.receipt)
        self.assertEqual(self.receipt.read_text(encoding="utf-8"), "Keep this\n")
        self.receipt.unlink()
        self.receipt.symlink_to(self.base / "missing-target")
        with self.assertRaises(ReleaseError):
            export_source.export(self.root, revision, self.destination, self.receipt)
        self.assertTrue(self.receipt.is_symlink())
        self.assertFalse(self.destination.exists())
        receipt_parent = self.base / "receipt-parent"
        receipt_parent.mkdir()
        alias = self.base / "receipt-parent-alias"
        alias.symlink_to(receipt_parent, target_is_directory=True)
        with self.assertRaises(ReleaseError):
            export_source.export(self.root, revision, self.destination, alias / "receipt.json")
        self.assertFalse((receipt_parent / "receipt.json").exists())
        self.assertFalse(self.destination.exists())

    def test_git_export_attributes_cannot_silently_change_source(self) -> None:
        for attribute in ("export-ignore", "export-subst"):
            with self.subTest(attribute=attribute), tempfile.TemporaryDirectory(prefix="export-attr-") as directory:
                root = Path(directory) / "repo"
                subprocess.run(["git", "init", "-q", str(root)], check=True)
                (root / ".gitattributes").write_text(
                    f"README.md {attribute}\n", encoding="utf-8"
                )
                (root / "README.md").write_text("$Format:%H$\n", encoding="utf-8")
                revision = commit_all(root)
                destination = Path(directory) / "output"
                receipt = Path(directory) / "receipt.json"
                with self.assertRaises(ReleaseError):
                    export_source.export(root, revision, destination, receipt)
                self.assertFalse(destination.exists())
                self.assertFalse(receipt.exists())

    def test_only_exporter_may_omit_workflows_from_integrity_check(self) -> None:
        self.add_file("README.md", b"Public source\n")
        self.add_file(".github/workflows/check.yml", b"name: example\n")
        self.add_file(".gitattributes", b".github/workflows/* export-ignore\n")
        revision = commit_all(self.root)
        with self.assertRaisesRegex(ReleaseError, "Archive differs from the commit"):
            source_candidate.audit(self.root)
        report = export_source.export(self.root, revision, self.destination, self.receipt)
        self.assertEqual(report["omitted_workflows"], [".github/workflows/check.yml"])
        self.assertFalse((self.destination / ".github/workflows").exists())
        self.assertEqual(
            [item["path"] for item in report["files"]], [".gitattributes", "README.md"]
        )

    def test_submodule_gitlink_cannot_make_incomplete_export(self) -> None:
        self.add_file("README.md", b"Source\n")
        initial = commit_all(self.root)
        subprocess.run(
            ["git", "update-index", "--add", "--cacheinfo", f"160000,{initial},dependency"],
            cwd=self.root, check=True,
        )
        subprocess.run(
            ["git", "-c", "user.name=Export fixture",
             "-c", "user.email=fixture@example.invalid", "-c", "commit.gpgsign=false",
             "commit", "-qm", "Gitlink fixture"],
            cwd=self.root, check=True,
        )
        revision = git(self.root, "rev-parse", "HEAD")
        with self.assertRaisesRegex(ReleaseError, "submodules"):
            export_source.export(self.root, revision, self.destination, self.receipt)
        self.assert_no_output()


class ArchiveAuditReview(unittest.TestCase):
    @staticmethod
    def audit(members: list[tuple[str, bytes | None, bytes | None]]) -> list[dict]:
        """Build tar members without extracting them; tuple holds name, data, type."""
        buffer = io.BytesIO()
        with tarfile.open(fileobj=buffer, mode="w") as output:
            for name, data, member_type in members:
                info = tarfile.TarInfo(name)
                info.type = member_type or tarfile.REGTYPE
                if info.type == tarfile.REGTYPE:
                    payload = data if data is not None else b"ok\n"
                    info.size = len(payload)
                    output.addfile(info, io.BytesIO(payload))
                else:
                    if info.type in {tarfile.SYMTYPE, tarfile.LNKTYPE}:
                        info.linkname = "outside"
                    output.addfile(info)
        buffer.seek(0)
        with tarfile.open(fileobj=buffer, mode="r:") as archive:
            return source_candidate.audit_archive(archive, private=None, omit_workflows=False)

    def test_traversal_absolute_backslash_duplicate_and_git_names_are_rejected(self) -> None:
        cases = (
            ([ ("safe/../escape", None, None) ], "unsafe-archive-path"),
            ([ ("/absolute.txt", None, None) ], "unsafe-archive-path"),
            ([ (r"safe\escape.txt", None, None) ], "unsafe-archive-path"),
            ([ ("C:/drive.txt", None, None) ], "unsafe-archive-path"),
            ([ ("nested/file:stream", None, None) ], "unsafe-archive-path"),
            ([ ("nested/CON.txt", None, None) ], "unsafe-archive-path"),
            ([ ("nested/aux", None, None) ], "unsafe-archive-path"),
            ([ ("file.txt", None, None), ("FILE.txt", None, None) ], "duplicate-archive-path"),
            ([ (".git/config", None, None) ], "private-directory"),
            ([ ("nested/.GIT/config", None, None) ], "private-directory"),
        )
        for members, rule in cases:
            with self.subTest(rule=rule, members=members):
                hits = self.audit(members)
                self.assertIn(rule, {hit["rule"] for hit in hits})

    def test_hard_links_symlinks_and_special_tar_members_are_refused(self) -> None:
        for member_type in (tarfile.LNKTYPE, tarfile.SYMTYPE, tarfile.FIFOTYPE):
            with self.subTest(member_type=member_type):
                hits = self.audit([("link", None, member_type)])
                self.assertIn("non-regular-archive-member", {hit["rule"] for hit in hits})


if __name__ == "__main__":
    unittest.main()
