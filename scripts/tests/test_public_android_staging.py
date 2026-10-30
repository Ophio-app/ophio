# SPDX-License-Identifier: Apache-2.0
"""Public Android staging uses synthetic artifacts, never Gradle or real keys."""
from __future__ import annotations

import os
from pathlib import Path
import shutil
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import package_android
from release_common import ReleaseError


class PublicAndroidStagingTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name) / "source"
        self.root.mkdir()
        self.licences = self.root / "licences"
        self.licences.mkdir()
        for name in ("LICENSE", "NOTICE", "THIRD-PARTY-NOTICES.txt", "sbom.cdx.json", "license-report.json"):
            (self.licences / name).write_text("synthetic " + name, encoding="utf-8")
        self.apk = self.root / "unsigned.apk"
        self.aab = self.root / "unsigned.aab"
        self.apk.write_bytes(b"synthetic unsigned APK")
        self.aab.write_bytes(b"synthetic unsigned AAB")
        self.paths = [(self.apk, "unsigned.apk"), (self.aab, "unsigned.aab")]
        self.dist = self.root / "dist"
        self.output = self.dist / "android-public-unsigned"
        patch = mock.patch.object(package_android, "ROOT", self.root)
        patch.start()
        self.addCleanup(patch.stop)

    def publish(self):
        package_android.publish_candidates(self.paths, self.licences, "UNSIGNED ONLY\n", public=True)

    def test_stale_personal_test_signed_apk_fails_before_public_build(self):
        personal = self.dist / "android"
        personal.mkdir(parents=True)
        (personal / "old-test-signed.apk").write_bytes(b"not public")
        with self.assertRaisesRegex(ReleaseError, "prior artifacts"):
            self.publish()
        with mock.patch.dict(os.environ, {"OPHIO_PUBLIC_RELEASE": "1"}), mock.patch.object(package_android, "identity") as identity:
            with self.assertRaisesRegex(ReleaseError, "prior artifacts"):
                package_android.main()
            identity.assert_not_called()
        self.assertFalse(self.output.exists())
        self.assertFalse((self.dist / "MANIFEST.txt").exists())
        self.assertEqual((personal / "old-test-signed.apk").read_bytes(), b"not public")

    def test_manifest_contains_only_new_unsigned_candidate(self):
        self.publish()
        manifest = (self.dist / "MANIFEST.txt").read_text(encoding="utf-8")
        self.assertIn("android-public-unsigned/unsigned.apk\t", manifest)
        self.assertIn("android-public-unsigned/unsigned.aab\t", manifest)
        self.assertNotIn("test-signed", manifest)
        self.assertEqual(self.apk.read_bytes(), (self.output / "unsigned.apk").read_bytes())
        self.assertEqual(sorted(path.name for path in self.dist.iterdir()), ["MANIFEST.txt", "android-public-unsigned"])

    def test_symlinked_dist_and_output_are_rejected_without_external_writes(self):
        outside = self.root / "outside"
        outside.mkdir()
        self.dist.symlink_to(outside, target_is_directory=True)
        with self.assertRaisesRegex(ReleaseError, "symbolic link"):
            self.publish()
        with mock.patch.dict(os.environ, {"OPHIO_PUBLIC_RELEASE": "1"}), mock.patch.object(package_android, "identity") as identity:
            with self.assertRaisesRegex(ReleaseError, "symbolic link"):
                package_android.main()
            identity.assert_not_called()
        self.assertEqual(list(outside.iterdir()), [])
        self.dist.unlink()
        self.dist.mkdir()
        self.output.symlink_to(outside, target_is_directory=True)
        with self.assertRaisesRegex(ReleaseError, "symbolic link"):
            self.publish()
        self.assertEqual(list(outside.iterdir()), [])
        self.assertTrue(self.output.is_symlink())
        with mock.patch.dict(os.environ, {"OPHIO_PUBLIC_RELEASE": "1"}), mock.patch.object(package_android, "identity") as identity:
            with self.assertRaisesRegex(ReleaseError, "symbolic link"):
                package_android.main()
            identity.assert_not_called()

    def test_second_copy_failure_leaves_no_candidate_and_retry_succeeds(self):
        self.output.mkdir(parents=True)
        original = shutil.copyfile
        calls = 0

        def fail_second_copy(source, destination):
            nonlocal calls
            calls += 1
            if calls == 2:
                raise OSError("synthetic second-copy failure")
            return original(source, destination)

        with mock.patch.object(package_android.shutil, "copyfile", side_effect=fail_second_copy):
            with self.assertRaisesRegex(OSError, "second-copy failure"):
                self.publish()
        self.assertEqual(list(self.output.iterdir()), [])
        self.assertFalse((self.dist / "MANIFEST.txt").exists())
        self.assertEqual(list(self.dist.iterdir()), [self.output])
        self.publish()
        self.assertTrue((self.output / "unsigned.aab").is_file())

    def test_manifest_failure_removes_only_own_candidate_and_allows_retry(self):
        self.output.mkdir(parents=True)
        original_inode = self.output.stat().st_ino
        real_manifest = package_android.manifest

        def fail_after_manifest(root):
            real_manifest(root)
            raise OSError("synthetic manifest failure")

        with mock.patch.object(package_android, "manifest", side_effect=fail_after_manifest):
            with self.assertRaisesRegex(OSError, "manifest failure"):
                self.publish()
        self.assertTrue(self.output.is_dir())
        self.assertEqual(self.output.stat().st_ino, original_inode)
        self.assertEqual(list(self.output.iterdir()), [])
        self.assertEqual(list(self.dist.iterdir()), [self.output])
        self.publish()
        self.assertTrue((self.dist / "MANIFEST.txt").is_file())


if __name__ == "__main__":
    unittest.main()
