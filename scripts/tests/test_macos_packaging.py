# SPDX-License-Identifier: Apache-2.0
from __future__ import annotations

import contextlib
import functools
import hashlib
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
import sign_macos
from tests.apk_fixture import certificate_digest, name_release_certificate

spec = importlib.util.spec_from_file_location(
    "package_artifacts", Path(__file__).resolve().parents[1] / "package-artifacts.py"
)
package_artifacts = importlib.util.module_from_spec(spec)
spec.loader.exec_module(package_artifacts)
POLICY = tomllib.loads((licenses.ROOT / "packaging/license-policy.toml").read_text(encoding="utf-8"))


def macho(signed: bool = True, cpu: bytes = b"\x0c\x00\x00\x01") -> bytes:
    """The smallest arm64 Mach-O header with one load command."""
    command = (0x1D if signed else 0x19).to_bytes(4, "little") + (16).to_bytes(4, "little") + bytes(8)
    header = b"\xcf\xfa\xed\xfe" + cpu + bytes(4) + (2).to_bytes(4, "little")
    header += (1).to_bytes(4, "little") + len(command).to_bytes(4, "little") + bytes(8)
    return header + command


class MacArchiveTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        root = self.root = Path(self.directory.name)
        files = {
            "Cargo.toml": '[workspace.package]\nversion = "1.2.3"\n',
            "client/src/brand.ts": 'export const PRODUCT_NAME = "Fïxture";\nexport const PROGRAM_NAME = "fixture";\n',
            "client/package.json": '{"version":"1.2.3"}',
            "client/src-tauri/tauri.conf.json": '{"version":"1.2.3","productName":"Fïxture"}',
            "client/src-tauri/Cargo.toml": '[package]\nversion = "1.2.3"\n',
            "LICENSE": "Apache licence fixture\n",
            "packaging/macos/INSTALL.md": (release_common.ROOT / "packaging/macos/INSTALL.md").read_text(encoding="utf-8"),
            "packaging/macos/install.sh": (release_common.ROOT / "packaging/macos/install.sh").read_text(encoding="utf-8"),
            "packaging/icons/hicolor/16x16/apps/ophio.png": "Linux only\n",
        }
        for name, text in files.items():
            path = root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")
        name_release_certificate(root, b"release")
        self.public = certificate_digest(b"release").encode("ascii")
        self.personal = certificate_digest(b"personal development").encode("ascii")
        policy = root / "packaging/android/public-signing.json"
        policy.parent.mkdir(parents=True, exist_ok=True)
        policy.write_text(json.dumps({"schema": 1, "certificate_sha256": self.public.decode("ascii")}), encoding="utf-8")
        self.target = root / "target"
        self.signed = self.target / "packaging/macos"
        (self.signed / "bin").mkdir(parents=True)
        self.place({"ophio": macho() + self.public, "computer-mcp": macho()})
        evidence = self.target / "packaging/licenses/macos"
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertTrue(licenses.emit(root, "macos", evidence, [], {}, [], POLICY))
        patches = [
            mock.patch.object(package_artifacts, "ROOT", root),
            mock.patch.object(package_artifacts, "identity", functools.partial(release_common.identity, root)),
            mock.patch.object(
                package_artifacts, "require_licences", functools.partial(release_common.require_licences, root=root)
            ),
            mock.patch.object(package_artifacts, "manifest", functools.partial(release_common.manifest, root)),
            mock.patch.object(package_artifacts.sys, "platform", "linux"),
            mock.patch.dict(os.environ, {
                "OPHIO_PERSONAL_SIGNING": "0", "OPHIO_PUBLIC_RELEASE": "1",
                "CARGO_TARGET_DIR": str(self.target), "SOURCE_DATE_EPOCH": "1700000000",
            }),
            contextlib.redirect_stdout(io.StringIO()),
        ]
        for patch in patches:
            patch.__enter__()
            self.addCleanup(patch.__exit__, None, None, None)
        os.environ.pop("OPHIO_PHONE_APK", None)

    def place(self, programs: dict[str, bytes], record: dict | None = None) -> None:
        record = record or {"schema": 1, "signature": "ad-hoc", "programs": {}, "notarization": None}
        for name, data in programs.items():
            (self.signed / "bin" / name).write_bytes(data)
            record["programs"][name] = {"sha256": hashlib.sha256(data).hexdigest()}
        (self.signed / "signing.json").write_text(json.dumps(record), encoding="utf-8")

    def archive(self) -> Path:
        return self.root / "dist/macos/fixture-1.2.3-aarch64-macos.tar.gz"

    def test_archive_holds_the_signed_programs_and_is_reproducible(self):
        with mock.patch.object(package_artifacts.subprocess, "run") as run:
            package_artifacts.macos()
            first = self.archive().read_bytes()
            package_artifacts.macos()
        # Off a Mac the programs are never started.
        run.assert_not_called()
        self.assertEqual(first, self.archive().read_bytes())
        with tarfile.open(self.archive()) as packed:
            names = {member.name.split("/", 1)[1] for member in packed}
            self.assertEqual(names, {
                "ophio", "computer-mcp", "LICENSE", "INSTALL.md", "install.sh", "NOTICE",
                "THIRD-PARTY-NOTICES.txt", "sbom.cdx.json", "license-report.json",
            })
            for member in packed.getmembers():
                executable = Path(member.name).name in {"ophio", "computer-mcp", "install.sh"}
                self.assertEqual(member.mode, 0o755 if executable else 0o644)
                self.assertEqual(member.mtime, 1700000000)
            self.assertEqual(
                packed.extractfile("fixture-1.2.3-aarch64-macos/ophio").read(), macho() + self.public
            )

    def test_refuses_programs_that_are_unsigned_changed_or_for_another_processor(self):
        cases = {
            "not signed": {"ophio": macho(signed=False) + self.public},
            "Apple silicon": {"ophio": macho(cpu=b"\x07\x00\x00\x01") + self.public},
            "signing setting": {"ophio": macho() + self.personal},
        }
        for message, programs in cases.items():
            with self.subTest(message):
                self.place({"computer-mcp": macho(), **programs})
                with self.assertRaisesRegex(release_common.ReleaseError, message):
                    package_artifacts.macos()
                self.assertFalse(self.archive().exists())
        self.place({"ophio": macho() + self.public, "computer-mcp": macho()})
        (self.signed / "bin/computer-mcp").write_bytes(macho() + b"changed")
        with self.assertRaisesRegex(release_common.ReleaseError, "differs from the signing record"):
            package_artifacts.macos()

    def test_starts_the_packed_program_only_on_an_apple_silicon_mac(self):
        def version(args, **kwargs):
            self.assertEqual(Path(args[0]).name, "ophio")
            self.assertEqual(args[1:], ["--version"])
            return subprocess.CompletedProcess(args, 0, stdout="ophio 1.2.3 (personal)\n")

        with (
            mock.patch.object(package_artifacts.sys, "platform", "darwin"),
            mock.patch.object(package_artifacts.os, "uname", return_value=mock.Mock(machine="arm64")),
            mock.patch.object(package_artifacts.subprocess, "run", side_effect=version) as run,
        ):
            package_artifacts.macos()
        run.assert_called_once()


class MacSigningTests(unittest.TestCase):
    """The checks around codesign, with codesign itself stood in for."""

    def signing(self, authorities: list[str], team: str, *, public: str, identity: str):
        shown = (
            "Identifier={identifier}\nCodeDirectory v=20500 size=1 flags=0x10000(runtime) hashes=1+1 location=embedded\n"
            "CDHash=abc\n" + "".join(f"Authority={name}\n" for name in authorities)
            + f"TeamIdentifier={team}\n" + ("Timestamp=1 Jan 2026\n" if authorities else "")
        )

        def tool(args, check=True):
            if "--display" in args and "--requirements" not in args:
                identifier = "app.ophio." + Path(args[-1]).name
                return subprocess.CompletedProcess(args, 0, stdout="", stderr=shown.format(identifier=identifier))
            if "--requirements" in args:
                return subprocess.CompletedProcess(args, 0, stdout="designated => identifier x\n", stderr="")
            return subprocess.CompletedProcess(args, 0, stdout="", stderr="")

        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory)
            built = target / "aarch64-apple-darwin/release"
            built.mkdir(parents=True)
            for name in sign_macos.PROGRAMS:
                (built / name).write_bytes(macho())
            with (
                mock.patch.object(sign_macos, "tool", side_effect=tool) as called,
                mock.patch.dict(os.environ, {
                    "CARGO_TARGET_DIR": str(target), "OPHIO_PUBLIC_RELEASE": public,
                    "OPHIO_MACOS_SIGNING_IDENTITY": identity, "OPHIO_MACOS_KEYCHAIN": "",
                }),
            ):
                record = sign_macos.sign()
                written = json.loads((target / "packaging/macos/signing.json").read_text(encoding="utf-8"))
            self.assertEqual(record, written)
            commands = [args for (args,), _ in called.call_args_list if "--sign" in args]
            return record, commands

    def test_ad_hoc_signing_turns_on_the_hardened_runtime(self):
        record, commands = self.signing([], "not set", public="1", identity="")
        self.assertEqual(record["signature"], "ad-hoc")
        self.assertEqual(set(record["programs"]), {"ophio", "computer-mcp"})
        for command in commands:
            self.assertIn("runtime", command)
            self.assertIn("--timestamp=none", command)
            self.assertEqual(command[command.index("--sign") + 1], "-")

    def test_a_public_build_takes_only_an_apple_developer_id(self):
        apple = ["Developer ID Application: Fixture (TEAM123456)", *sign_macos.APPLE_CHAIN]
        record, commands = self.signing(apple, "TEAM123456", public="1", identity="Developer ID Application: Fixture")
        self.assertEqual((record["signature"], record["team"]), ("developer-id", "TEAM123456"))
        self.assertTrue(all("--timestamp" in command for command in commands))
        with self.assertRaisesRegex(release_common.ReleaseError, "Developer ID Application"):
            self.signing(["Fixture Dev"], "not set", public="1", identity="Fixture Dev")
        # A personal build may use a lasting certificate of its own.
        record, _ = self.signing(["Fixture Dev"], "not set", public="0", identity="Fixture Dev")
        self.assertEqual((record["signature"], record["authority"]), ("certificate", "Fixture Dev"))


if __name__ == "__main__":
    unittest.main()
