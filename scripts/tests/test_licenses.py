# SPDX-License-Identifier: Apache-2.0
from __future__ import annotations

import contextlib
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import sys
import tempfile
import tomllib
import unittest
from unittest import mock
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import licenses
import source_candidate
from release_common import ReleaseError, release_environment, require_licences
from package_android import verify_assets
from tests.apk_fixture import certificate_digest, name_release_certificate, signed_app

windows_spec = importlib.util.spec_from_file_location(
    "package_windows", Path(__file__).resolve().parents[1] / "package-windows.py"
)
package_windows = importlib.util.module_from_spec(windows_spec)
windows_spec.loader.exec_module(package_windows)

FIXTURES = Path(__file__).with_name("fixtures")
POLICY = tomllib.loads(
    (licenses.ROOT / "packaging/license-policy.toml").read_text(encoding="utf-8")
)


class LicenseTests(unittest.TestCase):
    def test_normal_build_and_target_filter_excludes_dev(self):
        metadata = json.loads((FIXTURES / "metadata.json").read_text(encoding="utf-8"))
        selected = {("host", "1.0.0"), ("normal", "2.0.0"), ("builder", "3.0.0")}
        packages, edges = licenses.cargo_closure(metadata, ["host"], selected)
        self.assertEqual({p["name"] for p in packages}, {"host", "normal", "builder"})
        self.assertEqual(edges["host"], {"normal", "builder"})

    def test_missing_cargo_root_fails(self):
        metadata = json.loads((FIXTURES / "metadata.json").read_text(encoding="utf-8"))
        with self.assertRaises(licenses.LicenseError):
            licenses.cargo_closure(metadata, ["absent"])

    def test_spdx_boolean_policy(self):
        for expression in (
            "MIT",
            "MIT/Apache-2.0",
            "(MIT OR Apache-2.0) AND IJG",
            "GPL-3.0-only OR MIT",
        ):
            with self.subTest(expression=expression):
                self.assertEqual(licenses.policy_result(expression, POLICY)[0], 0)
        for expression in (
            "GPL-3.0-only",
            "MIT AND AGPL-3.0-only",
            "SSPL-1.0",
            "Unknown",
            "",
            "MIT OR",
            "(MIT",
            "MIT WITH",
            "MIT;GPL",
        ):
            with self.subTest(expression=expression):
                self.assertEqual(licenses.policy_result(expression, POLICY)[0], 2)
        self.assertEqual(licenses.policy_result("MIT AND MPL-2.0", POLICY)[0], 1)
        self.assertEqual(
            licenses.policy_result("LGPL-2.1-or-later OR MIT", POLICY), (0, "MIT")
        )

    def test_all_nested_notice_and_copyright_texts_preserved(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "native").mkdir()
            (root / "LICENSE").write_text("full licence\n", encoding="utf-8")
            (root / "native/NOTICE").write_text("upstream notice\n", encoding="utf-8")
            (root / "native/COPYRIGHT").write_text("upstream copyright\n", encoding="utf-8")
            (root / "node_modules").mkdir()
            (root / "node_modules/LICENSE").write_text("different package", encoding="utf-8")
            self.assertEqual(
                licenses.legal_texts(root),
                {
                    "LICENSE": "full licence\n",
                    "native/COPYRIGHT": "upstream copyright\n",
                    "native/NOTICE": "upstream notice\n",
                },
            )

    def test_licence_symlink_is_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "LICENSE").symlink_to(root / "outside")
            with self.assertRaises(licenses.LicenseError):
                licenses.legal_texts(root)

    def test_production_npm_graph_and_nested_versions(self):
        packages = {
            "": {"dependencies": {"first": "1"}, "devDependencies": {"test-only": "1"}},
            "node_modules/first": {
                "dependencies": {"shared": "2"},
                "peerDependencies": {"peer": "1"},
            },
            "node_modules/first/node_modules/shared": {},
            "node_modules/shared": {},
            "node_modules/test-only": {},
            "node_modules/peer": {},
        }
        selected, graph = licenses.npm_closure({"packages": packages})
        self.assertEqual(
            selected,
            {
                "",
                "node_modules/first",
                "node_modules/first/node_modules/shared",
                "node_modules/peer",
            },
        )
        self.assertNotIn("node_modules/test-only", graph)

    def test_unresolved_required_npm_dependency_fails(self):
        with self.assertRaises(licenses.LicenseError):
            licenses.npm_closure({"packages": {"": {"dependencies": {"missing": "1"}}}})

    def test_report_is_deterministic_and_preserves_lock_hash_and_text(self):
        lock = tomllib.loads((FIXTURES / "Cargo.lock").read_text(encoding="utf-8"))["package"][0]
        comp = licenses.Component(
            "pkg:cargo/normal@2.0.0",
            "normal",
            "2.0.0",
            "MIT",
            {"LICENSE": "full text\n", "NOTICE": "copyright holder\n"},
            hashes=[{"alg": "SHA-256", "content": lock["checksum"]}],
        )
        with (
            tempfile.TemporaryDirectory() as directory,
            contextlib.redirect_stdout(io.StringIO()),
        ):
            output = Path(directory)
            self.assertTrue(
                licenses.emit(licenses.ROOT, "linux", output, [comp], {}, [], POLICY)
            )
            first = (output / "sbom.cdx.json").read_bytes()
            self.assertTrue(
                licenses.emit(licenses.ROOT, "linux", output, [comp], {}, [], POLICY)
            )
            self.assertEqual(first, (output / "sbom.cdx.json").read_bytes())
            bom = json.loads(first)
            self.assertEqual(bom["specVersion"], "1.5")
            self.assertEqual(
                bom["components"][0]["hashes"][0]["content"], lock["checksum"]
            )
            notices = (output / "THIRD-PARTY-NOTICES.txt").read_text(encoding="utf-8")
            self.assertIn("full text\n", notices)
            self.assertIn("copyright holder\n", notices)
            require_licences(output, "linux")

    def test_missing_notice_and_incomplete_inventory_fail(self):
        component = licenses.Component("pkg:cargo/empty@1", "empty", "1", "MIT")
        with (
            tempfile.TemporaryDirectory() as directory,
            contextlib.redirect_stdout(io.StringIO()),
        ):
            output = Path(directory)
            self.assertFalse(
                licenses.emit(
                    licenses.ROOT,
                    "android",
                    output,
                    [component],
                    {},
                    ["Gradle offline cache unavailable"],
                    POLICY,
                )
            )
            report = json.loads((output / "license-report.json").read_text(encoding="utf-8"))
            self.assertEqual(len(report["failures"]), 2)
            with self.assertRaises(ReleaseError):
                require_licences(output, "android")

    def test_external_models_are_not_shipped_and_have_pinned_hashes(self):
        values = licenses.speech_components(licenses.ROOT)
        self.assertEqual(len(values), 9)
        for value in values:
            self.assertFalse(value.shipped)
            self.assertEqual(value.bom()["scope"], "optional")
            self.assertEqual(len(value.hashes[0]["content"]), 64)
            self.assertIn("/resolve/", value.url)


class SourceCandidateTests(unittest.TestCase):
    def test_every_sensitive_line_is_reported_without_contents(self):
        forbidden = b"Kestrelwood"
        with (
            mock.patch.object(source_candidate, "LEGACY_NAME_LENGTH", len(forbidden)),
            mock.patch.object(source_candidate, "LEGACY_NAME_SHA256", hashlib.sha256(forbidden.lower()).hexdigest()),
        ):
            data = (
                b"public\n"
                + b"before" + forbidden + b"after\n"
                + forbidden.upper()
                + b"\n"
                + source_candidate.HOME_PREFIX
                + b"user/file\n"
            )
            hits = source_candidate.inspect("test.txt", data, [])
        self.assertEqual([h["line"] for h in hits], [2, 3, 4])
        self.assertNotIn(forbidden.decode(), json.dumps(hits))

    def test_only_listed_example_homes_are_allowed(self):
        examples = frozenset({b"sam"})
        home = source_candidate.HOME_PREFIX
        allowed = b'path: "' + home + b'sam/Projects/app", cwd: "' + home + b'sam"\n'
        self.assertFalse(source_candidate.inspect("x.ts", allowed, [], examples))
        for line in (
            home + b"samuel/app\n",
            home + b"sam/app " + home + b"builder/app\n",
            home + b"\n",
        ):
            with self.subTest(line=line):
                self.assertTrue(source_candidate.inspect("x.ts", line, [], examples))

    def test_home_directories_on_every_platform(self):
        examples = frozenset({b"sam"})
        mac = source_candidate.MAC_HOME_PREFIX
        for line in (
            mac + b"builder/app\n",
            b'root: "C:\\' + b'Users\\builder\\app"\n',
            b'root: "C:\\\\' + b'Users\\\\builder\\\\app"\n',
            b"D:/" + b"Users/builder\n",
        ):
            with self.subTest(line=line):
                hits = source_candidate.inspect("x.ts", line, [], examples)
                self.assertEqual([h["rule"] for h in hits], ["absolute-home-path"])
        for line in (
            mac + b"sam/app\n",
            b'root: "C:\\\\' + b'Users\\\\sam\\\\Inbox\\\\"\n',
        ):
            with self.subTest(line=line):
                self.assertFalse(source_candidate.inspect("x.ts", line, [], examples))

    def test_files_named_after_work_items(self):
        for path in (
            "client/scripts/plans/U32.json",
            "client/scripts/plans/U42-ui.json",
            "crates/example/tests/m2_wire.rs",
            "scripts/tests/test_m3_release.py",
            "client/src/screens/talk/u21.test.tsx",
            "docs/Y6/notes.md",
        ):
            with self.subTest(path=path):
                rules = {h["rule"] for h in source_candidate.inspect(path, b"x", [])}
                self.assertIn("work-item-filename", rules)
        for path in (
            "crates/platform-linux/src/x11.rs",
            "res/mipmap-anydpi-v26/ic_launcher.xml",
            "client/src/a11y.ts",
            "clips/demo.mp4",
            "crates/protocol/tests/wire.rs",
        ):
            with self.subTest(path=path):
                rules = {h["rule"] for h in source_candidate.inspect(path, b"x", [])}
                self.assertNotIn("work-item-filename", rules)

    def test_notes_on_how_the_product_was_built(self):
        lane = b"lane"
        for line in (
            b"needs its owning " + lane + b"'s follow-up",
            b"Other build " + lane + b"s were active",
            b"with " + lane + b"-local userdata",
            b"No Opus" + b"-owned contract changed",
            b"Three review " + b"passes were made",
            b"an emulator owned by another " + b"worker",
            b"The integrator " + b"must regenerate the lockfile",
        ):
            with self.subTest(line=line):
                hits = source_candidate.inspect("notes.md", line, [])
                self.assertEqual([h["rule"] for h in hits], ["build-process-note"])
        for line in (
            b"Open Review before the computer starts the task",
            b"The agent owns this task until it stops",
            b"Channel workers restart together",
            b"Keep each request in its own lane on screen",
            b"Build a release after a review of its content",
        ):
            with self.subTest(line=line):
                self.assertFalse(source_candidate.inspect("notes.md", line, []))

    def test_private_key_and_names(self):
        data = b"-----BEGIN " + b"RSA PRIVATE KEY-----\nfixture\n"
        self.assertIn(
            "private-key-block",
            {h["rule"] for h in source_candidate.inspect("fixture.txt", data, [])},
        )
        for path in (
            "/".join(("planning", "design.md")),
            "/".join(("records", "note.txt")),
            "CLAUDE.md",
            ".env.local",
            "test.keystore",
            "notes/review-notes.md",
            ".lanes/x.rs",
        ):
            with self.subTest(path=path):
                self.assertTrue(source_candidate.inspect(path, b"fixture", []))

    def test_generated_exception_requires_path_and_marker(self):
        exemptions = [
            {
                "path": "client/src/protocol/generated/*.ts",
                "marker": "generated by ts-rs",
                "reason": "generated",
            }
        ]
        path = "client/src/protocol/generated/Foo.ts"
        self.assertFalse(
            source_candidate.inspect(path, b"// generated by ts-rs\n", exemptions)
        )
        self.assertTrue(source_candidate.inspect(path, b"manual\n", exemptions))
        self.assertTrue(
            source_candidate.inspect(
                "client/src/manual.ts", b"// generated by ts-rs", exemptions
            )
        )

    def test_apache_header_must_be_at_top(self):
        self.assertFalse(
            source_candidate.inspect(
                "crates/example/src/lib.rs", b"// " + source_candidate.SPDX + b"\n", []
            )
        )
        self.assertTrue(
            source_candidate.inspect(
                "crates/example/src/lib.rs", b"\n" * 20 + source_candidate.SPDX, []
            )
        )

    def test_private_references_are_distinct_from_public_url_words(self):
        reference = b"see " + b"/".join((b"records", b"inventory.md"))
        hits = source_candidate.inspect("source.txt", reference, [])
        self.assertEqual([hit["rule"] for hit in hits], ["private-tree-reference"])
        public_url = b"https://example.org/" + b"/".join((b"imagerecords", b"123.jpg"))
        self.assertFalse(source_candidate.inspect("source.txt", public_url, []))


class AndroidAssetTests(unittest.TestCase):
    def test_assets_must_exist_and_match_exactly(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            assets = root / "licences"
            assets.mkdir()
            for name in (
                "LICENSE",
                "NOTICE",
                "THIRD-PARTY-NOTICES.txt",
                "sbom.cdx.json",
            ):
                (assets / name).write_text("fixture " + name, encoding="utf-8")
            apk = root / "fixture.apk"
            with zipfile.ZipFile(apk, "w") as archive:
                for path in assets.iterdir():
                    archive.write(path, "assets/licenses/" + path.name)
            verify_assets(apk, assets, "assets/licenses/")
            with self.assertRaisesRegex(ReleaseError, "missing"):
                verify_assets(apk, assets, "wrong-prefix/")
            (assets / "NOTICE").write_text("changed notice", encoding="utf-8")
            with self.assertRaisesRegex(ReleaseError, "differs"):
                verify_assets(apk, assets, "assets/licenses/")


class ReleaseEnvironmentTests(unittest.TestCase):
    def test_target_flags_and_paths_with_spaces_survive_remapping(self):
        with mock.patch.dict(
            os.environ,
            {
                "CARGO_TARGET_X86_64_PC_WINDOWS_GNULLVM_RUSTFLAGS": "-C target-feature=+crt-static",
                "CARGO_HOME": "/builder/cache with spaces",
            },
            clear=True,
        ):
            result = release_environment(
                "windows", Path("/builder/project with spaces")
            )
        flags = result["CARGO_ENCODED_RUSTFLAGS"].split("\x1f")
        self.assertIn("target-feature=+crt-static", flags)
        self.assertIn("--remap-path-prefix=/builder/project with spaces=/src", flags)
        self.assertIn("--remap-path-prefix=/builder/cache with spaces=/cargo", flags)

    def test_android_keeps_platform_links_and_explicit_encoded_flags(self):
        with mock.patch.dict(
            os.environ, {"CARGO_ENCODED_RUSTFLAGS": "-C\x1fopt-level=2"}, clear=True
        ):
            result = release_environment("android")
        flags = result["CARGO_ENCODED_RUSTFLAGS"].split("\x1f")
        self.assertEqual(flags[:2], ["-C", "opt-level=2"])
        self.assertIn("-Clink-arg=-landroid", flags)


class WindowsArchiveTests(unittest.TestCase):
    def test_zip_inventory_is_complete_reproducible_and_refuses_failed_audit(self):
        with (
            tempfile.TemporaryDirectory() as directory,
            contextlib.redirect_stdout(io.StringIO()),
            mock.patch.dict(os.environ, {"OPHIO_PERSONAL_SIGNING": "0", "OPHIO_PUBLIC_RELEASE": "0"}),
        ):
            root = Path(directory)
            files = {
                "Cargo.toml": '[workspace.package]\nversion = "1.2.3"\n',
                "client/src/brand.ts": 'export const PRODUCT_NAME = "Fïxture";\nexport const PROGRAM_NAME = "fixture";\n',
                "client/package.json": '{"version":"1.2.3"}',
                "client/src-tauri/tauri.conf.json": '{"version":"1.2.3","productName":"Fïxture"}',
                "client/src-tauri/Cargo.toml": '[package]\nversion = "1.2.3"\n',
                "LICENSE": "Apache licence fixture\n",
                "packaging/windows/INSTALL.md": "Installation fixture\n",
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
            release = target / "x86_64-pc-windows-gnullvm/release"
            release.mkdir(parents=True)
            executable = bytearray(134)
            executable[:2] = b"MZ"
            executable[60:64] = (128).to_bytes(4, "little")
            executable[128:] = b"PE\0\0\x64\x86"
            for name in ("ophio.exe", "computer-mcp.exe"):
                (release / name).write_bytes(executable + (public if name == "ophio.exe" else b""))
            output = target / "packaging/licenses/windows"
            self.assertTrue(licenses.emit(root, "windows", output, [], {}, [], POLICY))
            for mode, public_mode, wrong_pin in (("0", "1", personal), ("0", "0", personal), ("1", "0", public)):
                with self.subTest(mode=mode, public_mode=public_mode), mock.patch.dict(os.environ, {
                    "OPHIO_PERSONAL_SIGNING": mode, "OPHIO_PUBLIC_RELEASE": public_mode,
                }):
                    (release / "ophio.exe").write_bytes(executable + wrong_pin)
                    with self.assertRaisesRegex(ReleaseError, "rebuild.*OPHIO_PERSONAL_SIGNING"):
                        package_windows.assemble(root, target, 1700000000)
                    self.assertFalse((root / "dist").exists())
            (release / "ophio.exe").write_bytes(executable + public)
            archive = package_windows.assemble(root, target, 1700000000)
            first = archive.read_bytes()
            check = package_windows.require_executable_trust
            def replace_after_check(data, checked_root):
                check(data, checked_root)
                (release / "ophio.exe").write_bytes(executable + personal)
            with mock.patch.object(package_windows, "require_executable_trust", side_effect=replace_after_check) as checked:
                package_windows.assemble(root, target, 1700000000)
            checked.assert_called_once()
            self.assertEqual((release / "ophio.exe").read_bytes(), executable + personal)
            self.assertEqual(first, archive.read_bytes())
            (release / "ophio.exe").write_bytes(executable + public)
            with zipfile.ZipFile(archive) as zipped:
                expected = {
                    "ophio.exe",
                    "computer-mcp.exe",
                    "LICENSE",
                    "NOTICE",
                    "THIRD-PARTY-NOTICES.txt",
                    "sbom.cdx.json",
                    "license-report.json",
                    "INSTALL.md",
                    "SHA256SUMS",
                }
                self.assertEqual(
                    {Path(name).name for name in zipped.namelist()}, expected
                )
                self.assertIsNone(zipped.testzip())
            # A release-signed phone app goes beside ophio.exe, where setup looks.
            name_release_certificate(root, b"release")
            (root / "ophio.apk").write_bytes(signed_app(b"release"))
            with mock.patch.dict(os.environ, {"OPHIO_PHONE_APK": str(root / "ophio.apk"), "OPHIO_PERSONAL_SIGNING": "0"}):
                archive = package_windows.assemble(root, target, 1700000000)
            with zipfile.ZipFile(archive) as zipped:
                self.assertEqual(zipped.read("fixture-1.2.3-x86_64-windows/ophio.apk"), signed_app(b"release"))
                self.assertIn(b"  ophio.apk\n", zipped.read("fixture-1.2.3-x86_64-windows/SHA256SUMS"))
            report = json.loads((output / "license-report.json").read_text(encoding="utf-8"))
            report["failures"] = ["unknown runtime licence"]
            (output / "license-report.json").write_text(json.dumps(report), encoding="utf-8")
            with self.assertRaises(ReleaseError):
                package_windows.assemble(root, target, 1700000000)


if __name__ == "__main__":
    unittest.main()
