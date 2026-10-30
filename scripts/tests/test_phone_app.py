# SPDX-License-Identifier: Apache-2.0
"""The phone app a computer download may carry, checked to be release-signed."""
from __future__ import annotations

import os
from pathlib import Path
import re
import runpy
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import release_common
from release_common import ReleaseError, apk_signers, phone_app, release_certificate
from tests.apk_fixture import V2, V3, V31, app, certificate_digest, name_release_certificate, prefixed, signed_app, signer


class SignerTests(unittest.TestCase):
    def test_reads_each_scheme_s_signer(self):
        digest = certificate_digest(b"release certificate")
        self.assertEqual(apk_signers(signed_app(b"release certificate")), [digest, digest])

    def test_finds_the_record_behind_a_comment(self):
        bytes_ = app([(V2, prefixed(signer(b"cert", False)))], b"a comment with PK\x05\x06 in it")
        self.assertEqual(apk_signers(bytes_), [certificate_digest(b"cert")])

    def test_tells_what_is_wrong(self):
        moved = bytearray(signed_app(b"cert"))
        # A directory that does not reach the record is not trusted.
        moved[-6] = (moved[-6] - 1) % 256
        cut = prefixed(signer(b"cert", False)[:20])
        for data, problem in (
            (b"not an archive at all, just some text", "not an Android app"),
            (b"", "not an Android app"),
            (bytes(moved), "not an Android app"),
            (app(None), "not signed"),
            (app([(0x42727A6E, b"padding")]), "not signed"),
            (app([(V2, cut)]), "damaged"),
            (app([(V2, prefixed(signer(b"cert", False))), (V3, prefixed(b""))]), "damaged"),
            # An older phone would check only the JAR signature beside these.
            (app([(V3, prefixed(signer(b"cert", True)))]), "not signed"),
            (app([(V31, prefixed(signer(b"cert", True)))]), "not signed"),
        ):
            with self.subTest(problem=problem), self.assertRaisesRegex(ReleaseError, problem):
                apk_signers(data)

    def test_reads_every_scheme_s_signer(self):
        data = app(
            [
                (V2, prefixed(signer(b"release", False))),
                (V3, prefixed(signer(b"release", True))),
                (V31, prefixed(signer(b"other", True))),
            ]
        )
        release, other = certificate_digest(b"release"), certificate_digest(b"other")
        self.assertEqual(apk_signers(data), [release, release, other])

    def test_the_release_certificate_is_the_companion_s(self):
        source = (release_common.ROOT / "crates/companion/src/phone/mod.rs").read_text(encoding="utf-8")
        self.assertIn(f'"{release_certificate()}"', source)
        self.assertRegex(release_certificate(), r"^[0-9a-f]{64}$")


class PhoneAppTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        name_release_certificate(self.root, b"release")
        self.app = self.root / "ophio.apk"
        patch = mock.patch.dict(os.environ)
        patch.start()
        self.addCleanup(patch.stop)
        os.environ.pop("OPHIO_PERSONAL_SIGNING", None)

    def named(self, path: Path | str | None):
        environment = {} if path is None else {"OPHIO_PHONE_APK": str(path)}
        with mock.patch.dict(os.environ, environment):
            if path is None:
                os.environ.pop("OPHIO_PHONE_APK", None)
            return phone_app(self.root)

    def test_is_carried_only_when_named(self):
        self.assertIsNone(self.named(None))
        self.assertIsNone(self.named(""))

    def test_carries_only_an_app_the_release_key_signed(self):
        self.app.write_bytes(signed_app(b"release"))
        self.assertEqual(self.named(self.app), self.app)
        self.app.write_bytes(signed_app(b"a test key"))
        with self.assertRaisesRegex(ReleaseError, re.escape(certificate_digest(b"a test key"))):
            self.named(self.app)
        # Every signer must be the release key, not just one of them.
        self.app.write_bytes(app([(V2, prefixed(signer(b"release", False))), (V3, prefixed(signer(b"other", True)))]))
        with self.assertRaisesRegex(ReleaseError, "not Øphio's release key"):
            self.named(self.app)

    def test_only_a_personal_build_carries_the_development_signed_app(self):
        self.app.write_bytes(signed_app(b"personal development"))
        with self.assertRaisesRegex(ReleaseError, "set OPHIO_PERSONAL_SIGNING=1"):
            self.named(self.app)
        with mock.patch.dict(os.environ, {"OPHIO_PERSONAL_SIGNING": "1"}):
            self.assertEqual(self.named(self.app), self.app)
            self.app.write_bytes(signed_app(b"release"))
            with self.assertRaisesRegex(ReleaseError, "not Øphio's release key"):
                self.named(self.app)

    def test_host_build_checks_the_phone_app_before_running_cargo(self):
        for artifact in ("linux", "windows"):
            for personal, certificate, message in (
                ("0", b"personal development", "OPHIO_PERSONAL_SIGNING=1"),
                ("1", b"release", "not Øphio's release key"),
            ):
                with self.subTest(artifact=artifact, personal=personal):
                    self.app.write_bytes(signed_app(certificate))
                    with mock.patch.dict(os.environ, {
                        "OPHIO_PERSONAL_SIGNING": personal, "OPHIO_PUBLIC_RELEASE": "0",
                        "OPHIO_PHONE_APK": str(self.app),
                    }), mock.patch.object(release_common, "ROOT", self.root), \
                            mock.patch.object(sys, "argv", ["build-host.py", artifact]), \
                            mock.patch.object(subprocess, "run") as run:
                        with self.assertRaisesRegex(SystemExit, message):
                            runpy.run_path(str(release_common.__file__.replace("release_common.py", "build-host.py")))
                        run.assert_not_called()
        self.app.write_bytes(signed_app(b"personal development"))
        with mock.patch.dict(os.environ, {
            "OPHIO_PERSONAL_SIGNING": "1", "OPHIO_PUBLIC_RELEASE": "0",
            "OPHIO_PHONE_APK": str(self.app),
        }), mock.patch.object(release_common, "ROOT", self.root), \
                mock.patch.object(sys, "argv", ["build-host.py", "linux"]), \
                mock.patch.object(subprocess, "run", return_value=subprocess.CompletedProcess([], 0)) as run:
            with self.assertRaises(SystemExit) as stopped:
                runpy.run_path(str(release_common.__file__.replace("release_common.py", "build-host.py")))
            self.assertEqual(stopped.exception.code, 0)
            run.assert_called_once()
            self.assertEqual(run.call_args.args[0][:2], ["cargo", "build"])
            self.assertEqual(run.call_args.kwargs["env"]["OPHIO_PERSONAL_SIGNING"], "1")

    def test_refuses_a_link_or_a_missing_file(self):
        self.app.write_bytes(signed_app(b"release"))
        link = self.root / "linked.apk"
        link.symlink_to(self.app)
        for path in (link, self.root / "missing.apk", self.root):
            with self.subTest(path=path.name), self.assertRaisesRegex(ReleaseError, "not a file"):
                self.named(path)


if __name__ == "__main__":
    unittest.main()
