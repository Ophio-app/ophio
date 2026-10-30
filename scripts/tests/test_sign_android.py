# SPDX-License-Identifier: Apache-2.0
"""Protected signing preparation; real-tool checks use disposable synthetic keys only."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import package_android
import release_common
import sign_android
from release_common import ReleaseError


class PolicyTests(unittest.TestCase):
    def setUp(self):
        patch = mock.patch.dict(os.environ, {"OPHIO_PERSONAL_SIGNING": "0"})
        patch.start()
        self.addCleanup(patch.stop)
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.policy = self.root / sign_android.POLICY
        self.policy.parent.mkdir(parents=True)

    def policy_for(self, certificate):
        self.policy.write_text(json.dumps({"schema": 1, "certificate_sha256": certificate}), encoding="utf-8")

    def test_unconfigured_policy_fails_before_signing(self):
        self.policy_for(None)
        with mock.patch.object(sign_android, "run") as run:
            with self.assertRaisesRegex(ReleaseError, "not configured"):
                sign_android.sign(argparse.Namespace(policy=self.policy))
            run.assert_not_called()

    def test_public_policy_rejects_personal_or_malformed_certificate(self):
        for certificate in (sign_android.PERSONAL_CERTIFICATE, "", "ab", "A" * 64, 123):
            self.policy_for(certificate)
            with self.subTest(certificate=certificate), self.assertRaises(ReleaseError):
                sign_android.public_certificate(self.policy)
        self.policy_for("1" * 64)
        self.assertEqual(sign_android.public_certificate(self.policy), "1" * 64)

    def test_personal_builds_need_no_public_policy(self):
        with mock.patch.dict(os.environ, {"OPHIO_PUBLIC_RELEASE": "0"}):
            self.assertFalse(sign_android.public_build_check(self.root))
            self.assertIn("CARGO_ENCODED_RUSTFLAGS", release_common.release_environment("android", self.root))

    def test_public_build_requires_policy_and_matching_companion_pin(self):
        with mock.patch.dict(os.environ, {"OPHIO_PUBLIC_RELEASE": "1"}):
            with self.assertRaises(FileNotFoundError):
                sign_android.public_build_check(self.root)
            self.policy_for("1" * 64)
            with mock.patch.object(release_common, "release_certificate", return_value="2" * 64):
                with self.assertRaisesRegex(ReleaseError, "differs from the companion"):
                    sign_android.public_build_check(self.root)
            with mock.patch.object(release_common, "release_certificate", return_value="1" * 64):
                self.assertTrue(sign_android.public_build_check(self.root))

    def test_public_packaging_cannot_fall_back_when_no_phone_is_supplied(self):
        self.policy_for(None)
        with mock.patch.dict(os.environ, {"OPHIO_PUBLIC_RELEASE": "1", "OPHIO_PHONE_APK": ""}):
            for call in (lambda: release_common.phone_app(self.root), lambda: release_common.release_environment("android", self.root)):
                with self.assertRaisesRegex(ReleaseError, "not configured"):
                    call()

    def test_mistyped_public_mode_does_not_silently_select_personal(self):
        with mock.patch.dict(os.environ, {"OPHIO_PUBLIC_RELEASE": "true"}):
            with self.assertRaisesRegex(ReleaseError, "0 or 1"):
                sign_android.public_mode()

    def test_signer_does_not_inherit_java_or_build_injection(self):
        with mock.patch.dict(os.environ, {"JAVA_TOOL_OPTIONS": "bad", "LD_PRELOAD": "bad", "BUILD_ENV_FILE": "bad"}):
            self.assertEqual(set(sign_android.tool_environment()), {"PATH", "LANG", "LC_ALL"})

    def test_public_candidates_never_overwrite_or_share_personal_directory(self):
        licences = self.root / "licences"
        licences.mkdir()
        for name in ("LICENSE", "NOTICE", "THIRD-PARTY-NOTICES.txt", "sbom.cdx.json", "license-report.json"):
            (licences / name).write_text("synthetic", encoding="utf-8")
        unsigned = self.root / "unsigned.apk"
        unsigned.write_bytes(b"synthetic")
        with mock.patch.object(package_android, "ROOT", self.root), mock.patch.object(package_android, "manifest", return_value=""):
            package_android.publish_candidates([(unsigned, "unsigned.apk")], licences, "unsigned", public=True)
            self.assertFalse((self.root / "dist/android").exists())
            self.assertEqual((self.root / "dist/android-public-unsigned/unsigned.apk").read_bytes(), b"synthetic")
            with self.assertRaisesRegex(ReleaseError, "must be empty"):
                package_android.publish_candidates([(unsigned, "unsigned.apk")], licences, "unsigned", public=True)


@unittest.skipUnless(os.environ.get("OPHIO_SIGNING_TEST_SDK"), "Set OPHIO_SIGNING_TEST_SDK for disposable real Android tool tests")
class RealSigningTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temporary = tempfile.TemporaryDirectory(prefix="ophio-synthetic-signing-")
        cls.addClassCleanup(cls.temporary.cleanup)
        cls.root = Path(cls.temporary.name)
        sdk = Path(os.environ["OPHIO_SIGNING_TEST_SDK"])
        cls.tools = sdk / "build-tools/37.0.0"
        cls.password = cls.root / "synthetic-password"
        cls.password.write_text("ephemeral-test-only-password\n" * 4, encoding="utf-8")
        cls.manifest = cls.root / "AndroidManifest.xml"
        cls.manifest.write_text('<manifest xmlns:android="http://schemas.android.com/apk/res/android" package="org.example.synthetic" android:versionCode="1" android:versionName="1"><uses-sdk android:minSdkVersion="29"/><application android:hasCode="false" android:debuggable="false"/></manifest>', encoding="utf-8")
        cls.unsigned = cls.root / "unsigned.apk"
        platform = sdk / "platforms/android-37/android.jar"
        if not platform.is_file():
            platform = sdk / "platforms/android-37.0/android.jar"
        cls.command([str(cls.tools / "aapt2"), "link", "--manifest", str(cls.manifest), "-I", str(platform), "-o", str(cls.unsigned)])
        cls.certificates = {}
        for name in ("old", "new"):
            key = cls.root / f"{name}.p12"
            cls.command(["/usr/bin/keytool", "-genkeypair", "-noprompt", "-keystore", str(key), "-storetype", "PKCS12", "-storepass:file", str(cls.password), "-keypass:file", str(cls.password), "-alias", name, "-keyalg", "RSA", "-keysize", "2048", "-validity", "1", "-dname", f"CN=Synthetic {name},O=Disposable Tests,C=US"])
            cert = cls.root / f"{name}.der"
            cls.command(["/usr/bin/keytool", "-exportcert", "-keystore", str(key), "-storepass:file", str(cls.password), "-alias", name, "-file", str(cert)])
            cls.certificates[name] = hashlib.sha256(cert.read_bytes()).hexdigest()

    @staticmethod
    def command(args):
        result = subprocess.run(args, env=sign_android.tool_environment(), capture_output=True, text=True, encoding="utf-8")
        if result.returncode:
            raise AssertionError(f"Synthetic Android tool test failed: {result.stderr}")
        return result

    def setUp(self):
        temporary = tempfile.TemporaryDirectory(dir=self.root)
        self.addCleanup(temporary.cleanup)
        self.work = Path(temporary.name)
        self.policy = self.work / "policy.json"
        self.policy.write_text(json.dumps({"schema": 1, "certificate_sha256": self.certificates["new"]}), encoding="utf-8")
        self.args = argparse.Namespace(policy=self.policy, input=self.unsigned, input_sha256=release_common.digest(self.unsigned), output=self.work / "signed.apk", build_tools=self.tools, keystore=self.root / "new.p12", alias="new")

    def sign(self):
        original = sign_android.run
        def synthetic_password(args, **kwargs):
            if len(args) > 1 and args[1] == "sign":
                args = [*args[:-1], "--ks-pass", f"file:{self.password}", args[-1]]
            return original(args, **kwargs)
        with mock.patch.object(sys.stdin, "isatty", return_value=True), mock.patch.object(sign_android, "run", side_effect=synthetic_password):
            sign_android.sign(self.args)

    def test_real_fresh_key_signs_and_verifies_without_modifying_input(self):
        before = self.unsigned.read_bytes()
        self.sign()
        sign_android.verify(self.args.output, self.tools, self.certificates["new"])
        self.assertEqual(self.unsigned.read_bytes(), before)
        self.assertFalse(list(self.work.glob("ophio-sign-*")))
        with self.assertRaisesRegex(ReleaseError, "already exists"):
            self.sign()

    def test_wrong_input_hash_or_certificate_leaves_no_output(self):
        self.args.input_sha256 = "0" * 64
        with self.assertRaisesRegex(ReleaseError, "owner-approved"):
            self.sign()
        self.assertFalse(self.args.output.exists())
        self.args.input_sha256 = release_common.digest(self.unsigned)
        self.policy.write_text(json.dumps({"schema": 1, "certificate_sha256": self.certificates["old"]}), encoding="utf-8")
        with self.assertRaisesRegex(ReleaseError, "differs"):
            self.sign()
        self.assertFalse(self.args.output.exists())

    def test_signed_input_and_unattended_use_are_refused(self):
        with mock.patch.object(sys.stdin, "isatty", return_value=False):
            with self.assertRaisesRegex(ReleaseError, "owner-operated terminal"):
                sign_android.sign(self.args)
        self.sign()
        self.args.input = self.args.output
        self.args.input_sha256 = release_common.digest(self.args.input)
        self.args.output = self.work / "resigned.apk"
        with self.assertRaisesRegex(ReleaseError, "unsigned"):
            self.sign()
        self.assertFalse(self.args.output.exists())

    def test_v3_rotation_proves_companion_and_fresh_key_helper_need_followup(self):
        lineage = self.work / "lineage"
        signer = lambda name: ["--ks", str(self.root / f"{name}.p12"), "--ks-key-alias", name, "--ks-pass", f"file:{self.password}"]
        self.command([str(self.tools / "apksigner"), "rotate", "--out", str(lineage), "--old-signer", *signer("old"), "--new-signer", *signer("new")])
        aligned = self.work / "aligned.apk"
        self.command([str(self.tools / "zipalign"), "-P", "16", "4", str(self.unsigned), str(aligned)])
        rotated = self.work / "rotated.apk"
        self.command([str(self.tools / "apksigner"), "sign", *signer("old"), "--next-signer", *signer("new"), "--lineage", str(lineage), "--rotation-min-sdk-version", "28", "--v2-signing-enabled", "true", "--v3-signing-enabled", "true", "--v4-signing-enabled", "false", "--out", str(rotated), str(aligned)])
        for api in (29, 32, 33):
            self.command([str(self.tools / "apksigner"), "verify", "--min-sdk-version", str(api), "--max-sdk-version", str(api), str(rotated)])
        signers = release_common.apk_signers(rotated.read_bytes())
        self.assertEqual(set(signers), set(self.certificates.values()))
        # Both the existing single-pin trust check and the fresh-key-only helper
        # must reject this genuinely valid mixed-certificate rotation artifact.
        with mock.patch.dict(os.environ, {"OPHIO_PHONE_APK": str(rotated), "OPHIO_PUBLIC_RELEASE": "0"}), mock.patch.object(release_common, "release_certificate", return_value=self.certificates["old"]):
            with self.assertRaisesRegex(ReleaseError, "not .*release key"):
                release_common.phone_app()
        with self.assertRaisesRegex(ReleaseError, "rotation is not supported"):
            sign_android.verify(rotated, self.tools, self.certificates["new"])


if __name__ == "__main__":
    unittest.main()
