# SPDX-License-Identifier: Apache-2.0
from __future__ import annotations

import contextlib
import functools
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import licenses
import release_common
from tests.apk_fixture import certificate_digest, name_release_certificate, signed_app

spec = importlib.util.spec_from_file_location(
    "package_artifacts", Path(__file__).resolve().parents[1] / "package-artifacts.py"
)
package_artifacts = importlib.util.module_from_spec(spec)
spec.loader.exec_module(package_artifacts)
POLICY = tomllib.loads(
    (licenses.ROOT / "packaging/license-policy.toml").read_text(encoding="utf-8")
)


class LinuxArchiveTests(unittest.TestCase):
    def test_archive_has_one_install_path_and_remains_reproducible(self):
        with (
            tempfile.TemporaryDirectory() as directory,
            contextlib.redirect_stdout(io.StringIO()),
        ):
            root = Path(directory)
            files = {
                "Cargo.toml": '[workspace.package]\nversion = "1.2.3"\n',
                "client/src/brand.ts": 'export const PRODUCT_NAME = "Fïxture";\nexport const PROGRAM_NAME = "fixture";\n',
                "client/package.json": '{"version":"1.2.3"}',
                "client/src-tauri/tauri.conf.json": '{"version":"1.2.3","productName":"Fïxture"}',
                "client/src-tauri/Cargo.toml": '[package]\nversion = "1.2.3"\n',
                "LICENSE": "Apache licence fixture\n",
                "packaging/linux/INSTALL.md": (
                    release_common.ROOT / "packaging/linux/INSTALL.md"
                ).read_text(encoding="utf-8"),
                "packaging/linux/install.sh": (
                    release_common.ROOT / "packaging/linux/install.sh"
                ).read_text(encoding="utf-8"),
                "packaging/icons/hicolor/16x16/apps/ophio.png": "16 pixel icon\n",
                "packaging/icons/hicolor/scalable/apps/ophio.svg": "<svg/>\n",
                "packaging/icons/ophio.ico": "Windows only\n",
            }
            for name, text in files.items():
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(text, encoding="utf-8")
            name_release_certificate(root, b"release")
            public = certificate_digest(b"release").encode("ascii")
            personal = certificate_digest(b"personal development").encode("ascii")
            policy = root / "packaging/android/public-signing.json"
            policy.parent.mkdir(parents=True, exist_ok=True)
            policy.write_text(json.dumps({"schema": 1, "certificate_sha256": public.decode("ascii")}), encoding="utf-8")
            target = root / "target"
            release = target / "x86_64-unknown-linux-gnu/release"
            release.mkdir(parents=True)
            executable = bytearray(64)
            executable[:6] = b"\x7fELF\x02\x01"
            executable[18:20] = b"\x3e\x00"
            for name in ("ophio", "computer-mcp", "ophio-sandbox-shim"):
                (release / name).write_bytes(executable + (public if name == "ophio" else b""))
            evidence = target / "packaging/licenses/linux"
            self.assertTrue(licenses.emit(root, "linux", evidence, [], {}, [], POLICY))

            def smoke_test(args, **kwargs):
                if args[0] == "readelf":
                    self.assertIn(Path(args[-1]).name, ("ophio", "computer-mcp", "ophio-sandbox-shim"))
                    return subprocess.CompletedProcess(args, 0, stdout="Version needs section\nName: GLIBC_2.39\n")
                self.assertEqual(Path(args[0]).name, "ophio")
                self.assertIn(args[1:], [["--version"], ["--help"]])
                for key in ("DISPLAY", "WAYLAND_DISPLAY", "DBUS_SESSION_BUS_ADDRESS"):
                    self.assertNotIn(key, kwargs["env"])
                return subprocess.CompletedProcess(args, 0, stdout="ophio 1.2.3\n")

            with (
                mock.patch.object(package_artifacts, "ROOT", root),
                mock.patch.object(
                    package_artifacts, "identity",
                    functools.partial(release_common.identity, root),
                ),
                mock.patch.object(
                    package_artifacts, "require_licences",
                    functools.partial(release_common.require_licences, root=root),
                ),
                mock.patch.object(
                    package_artifacts, "manifest",
                    functools.partial(release_common.manifest, root),
                ),
                mock.patch.object(
                    package_artifacts.subprocess, "run", side_effect=smoke_test
                ) as run,
                mock.patch.dict(
                    os.environ,
                    {
                        "OPHIO_PERSONAL_SIGNING": "0",
                        "OPHIO_PUBLIC_RELEASE": "0",
                        "CARGO_TARGET_DIR": str(target),
                        "SOURCE_DATE_EPOCH": "1700000000",
                        "DISPLAY": ":synthetic",
                        "WAYLAND_DISPLAY": "synthetic",
                        "DBUS_SESSION_BUS_ADDRESS": "synthetic",
                    },
                ),
            ):
                # A stale companion is refused before dist is even created,
                # including public preparation and ordinary local packaging.
                for mode, public_mode, wrong_pin in (("0", "1", personal), ("0", "0", personal), ("1", "0", public)):
                    with self.subTest(mode=mode, public_mode=public_mode), mock.patch.dict(os.environ, {
                        "OPHIO_PERSONAL_SIGNING": mode, "OPHIO_PUBLIC_RELEASE": public_mode,
                    }):
                        (release / "ophio").write_bytes(executable + wrong_pin)
                        with self.assertRaisesRegex(release_common.ReleaseError, "rebuild.*OPHIO_PERSONAL_SIGNING"):
                            package_artifacts.linux()
                        self.assertFalse((root / "dist").exists())
                        run.assert_not_called()
                (release / "ophio").write_bytes(executable + public)
                package_artifacts.linux()
                archive = next((root / "dist/linux").glob("*.tar.gz"))
                first = archive.read_bytes()
                check = package_artifacts.require_executable_trust
                def replace_after_check(data, checked_root):
                    check(data, checked_root)
                    (release / "ophio").write_bytes(executable + personal)
                with mock.patch.object(package_artifacts, "require_executable_trust", side_effect=replace_after_check) as checked:
                    package_artifacts.linux()
                checked.assert_called_once()
                self.assertEqual((release / "ophio").read_bytes(), executable + personal)
                self.assertEqual(first, archive.read_bytes())
                (release / "ophio").write_bytes(executable + public)
                self.assertEqual(run.call_count, 10)
                with tarfile.open(archive) as packed:
                    expected = {
                        "ophio",
                        "computer-mcp",
                        "ophio-sandbox-shim",
                        "LICENSE",
                        "INSTALL.md",
                        "install.sh",
                        "NOTICE",
                        "THIRD-PARTY-NOTICES.txt",
                        "sbom.cdx.json",
                        "license-report.json",
                        "icons/hicolor/16x16/apps/ophio.png",
                        "icons/hicolor/scalable/apps/ophio.svg",
                    }
                    self.assertEqual({m.name.split("/", 1)[1] for m in packed}, expected)
                    for member in packed.getmembers():
                        mode = (
                            0o755 if Path(member.name).name in {"ophio", "computer-mcp", "ophio-sandbox-shim", "install.sh"}
                            else 0o644
                        )
                        self.assertEqual(member.mode, mode)
                        self.assertEqual(member.mtime, 1700000000)
                    guide = packed.extractfile(
                        "fixture-1.2.3-x86_64-linux/INSTALL.md"
                    ).read().decode()
                    for command in ("service install", "service status", "service uninstall"):
                        self.assertIn(command, guide)
                # A release-signed phone app is installed where setup looks.
                name_release_certificate(root, b"release")
                (root / "ophio.apk").write_bytes(signed_app(b"release"))
                with mock.patch.dict(os.environ, {"OPHIO_PHONE_APK": str(root / "ophio.apk"), "OPHIO_PERSONAL_SIGNING": "0"}):
                    package_artifacts.linux()
                with tarfile.open(archive) as packed:
                    member = packed.getmember("fixture-1.2.3-x86_64-linux/ophio.apk")
                    self.assertEqual(member.mode, 0o644)
                    self.assertEqual(packed.extractfile(member).read(), signed_app(b"release"))
                report = json.loads((evidence / "license-report.json").read_text(encoding="utf-8"))
                report["failures"] = ["synthetic failed audit"]
                (evidence / "license-report.json").write_text(json.dumps(report), encoding="utf-8")
                with self.assertRaises(release_common.ReleaseError):
                    package_artifacts.linux()


if __name__ == "__main__":
    unittest.main()
