# SPDX-License-Identifier: Apache-2.0
"""Public packaging rejects fake source pins and unverified Android signatures."""
from __future__ import annotations

import hashlib
import importlib.util
import json
import os
from pathlib import Path
import struct
import subprocess
import sys
import tarfile
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import release_common
import sign_android
from release_common import ReleaseError


PERSONAL = "3" * 64
CHOICE = (
    "pub const RELEASE_CERT_SHA256: &str = if ::core::cfg!(ophio_personal_signing) "
    "{ PERSONAL_CERT_SHA256 } else { PUBLIC_CERT_SHA256 };"
)


def public_line(certificate: str) -> str:
    return f'const PUBLIC_CERT_SHA256: &str = "{certificate}";'


def personal_line(certificate: str = PERSONAL) -> str:
    return f'const PERSONAL_CERT_SHA256: &str = "{certificate}";'


def declaration(certificate: str, personal: str = PERSONAL) -> str:
    return "\n".join((public_line(certificate), personal_line(personal), CHOICE)) + "\n"


def trust(root: Path, certificate: str) -> Path:
    source = root / "crates/companion/src/phone/mod.rs"
    source.parent.mkdir(parents=True, exist_ok=True)
    source.write_text(declaration(certificate), encoding="utf-8")
    policy = root / sign_android.POLICY
    policy.parent.mkdir(parents=True, exist_ok=True)
    policy.write_text(json.dumps({"schema": 1, "certificate_sha256": certificate}), encoding="utf-8")
    return source


class SourcePinTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="ophio-source-pin-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.source = trust(self.root, "1" * 64)
        patch = mock.patch.dict(os.environ)
        patch.start()
        self.addCleanup(patch.stop)
        os.environ.pop("OPHIO_PERSONAL_SIGNING", None)

    def test_literal_pins_and_non_declaration_references(self):
        self.source.write_text(
            declaration("1" * 64) + '\nfn pin() -> &\'static str { RELEASE_CERT_SHA256 }\n',
            encoding="utf-8",
        )
        self.assertEqual(release_common.companion_certificates(self.root), ("1" * 64, PERSONAL))
        self.assertEqual(release_common.release_certificate(self.root), "1" * 64)

    def test_the_build_setting_chooses_the_pin_as_the_companion_does(self):
        for value, expected in ((None, "1" * 64), ("0", "1" * 64), ("1", PERSONAL)):
            with self.subTest(value=value):
                os.environ.pop("OPHIO_PERSONAL_SIGNING", None)
                if value is not None:
                    os.environ["OPHIO_PERSONAL_SIGNING"] = value
                self.assertEqual(release_common.release_certificate(self.root), expected)
        for value in ("", "true", "yes", " 1", "1\n"):
            with self.subTest(value=value), mock.patch.dict(os.environ, {"OPHIO_PERSONAL_SIGNING": value}):
                with self.assertRaisesRegex(ReleaseError, "0 or 1"):
                    release_common.release_certificate(self.root)

    def test_a_public_build_refuses_the_personal_setting(self):
        with mock.patch.dict(os.environ, {"OPHIO_PUBLIC_RELEASE": "1", "OPHIO_PERSONAL_SIGNING": "1"}):
            with self.assertRaisesRegex(ReleaseError, "personal development key"):
                sign_android.public_build_check(self.root)
            with self.assertRaisesRegex(ReleaseError, "personal development key"):
                release_common.release_environment("linux", self.root)
        with mock.patch.dict(os.environ, {"OPHIO_PUBLIC_RELEASE": "1", "OPHIO_PERSONAL_SIGNING": "0"}):
            self.assertTrue(sign_android.public_build_check(self.root))

    def test_fake_declarations_in_comments_and_strings_cannot_override_pin(self):
        fakes = (
            public_line("2" * 64),
            personal_line("2" * 64),
            'pub const RELEASE_CERT_SHA256: &str = "' + "2" * 64 + '";',
            CHOICE.replace("PERSONAL_CERT_SHA256 }", "PUBLIC_CERT_SHA256 }"),
        )
        for fake in fakes:
            prefixes = (
                "// " + fake + "\n",
                "/* " + fake + " */\n",
                "/* outer /* " + fake + " */ still comment */\n",
                'const NOTES: &str = r#"' + fake + '"#;\n',
                'const NOTES: &str = r###"' + fake + '"###;\n',
                'const NOTES: &str = "' + fake.replace('"', '\\"') + '";\n',
                'const NOTES: &[u8] = br#"' + fake + '"#;\n',
                "const SLASH: char = '/';\nconst QUOTE: char = '\"';\n",
            )
            for prefix in prefixes:
                with self.subTest(prefix=prefix):
                    self.source.write_text(prefix + declaration("1" * 64), encoding="utf-8")
                    self.assertEqual(release_common.companion_certificates(self.root), ("1" * 64, PERSONAL))
                    with mock.patch.dict(os.environ, {"OPHIO_PUBLIC_RELEASE": "1"}):
                        self.assertTrue(sign_android.public_build_check(self.root))

    def test_commented_policy_pin_cannot_hide_different_compiled_trust(self):
        self.source.write_text("// " + public_line("1" * 64) + "\n" + declaration("2" * 64), encoding="utf-8")
        with mock.patch.dict(os.environ, {"OPHIO_PUBLIC_RELEASE": "1"}):
            with self.assertRaisesRegex(ReleaseError, "differs from the companion's public pin"):
                sign_android.public_build_check(self.root)

    def test_missing_malformed_and_ambiguous_declarations_fail_closed(self):
        good = declaration("1" * 64)
        literal = '"' + "1" * 64 + '"'
        invalid = (
            "",
            "// " + good,
            "/* " + good + " */",
            'const NOTES: &str = r#"' + good + '"#;',
            good + good,
            good + public_line("2" * 64),
            good + personal_line("2" * 64),
            good + 'const RELEASE_CERT_SHA256: &str = env!("PIN");',
            "macro_rules! unused { () => { " + good + " } }",
            "mod unrelated { " + good + " }",
            "some_macro!(" + good + ");",
            '#[cfg(feature = "other")]\n' + good + '#[cfg(not(feature = "other"))]\n' + good,
            # Each part missing.
            personal_line() + "\n" + CHOICE,
            public_line("1" * 64) + "\n" + CHOICE,
            public_line("1" * 64) + "\n" + personal_line(),
            # The earlier single pin is no longer how the companion chooses.
            public_line("1" * 64) + "\n" + personal_line() + "\n"
            + 'pub const RELEASE_CERT_SHA256: &str = "' + "1" * 64 + '";',
            # The public pin, malformed.
            good.replace(literal, 'concat!(' + literal + ')'),
            good.replace(literal, "r" + literal),
            good.replace("PUBLIC_CERT_SHA256: &str", "PUBLIC_CERT_SHA256: String"),
            good.replace("1" * 64, "A" * 64),
            good.replace("1" * 64, "1" * 63),
            good.replace(literal + ";", literal),
            # The personal pin, malformed or equal to the public one.
            good.replace(PERSONAL, PERSONAL.upper().replace("3", "B")),
            good.replace(PERSONAL, "3" * 65),
            declaration("1" * 64, "1" * 64),
            # The choice, altered.
            good.replace("{ PERSONAL_CERT_SHA256 } else { PUBLIC_CERT_SHA256 }", "{ PUBLIC_CERT_SHA256 } else { PERSONAL_CERT_SHA256 }"),
            good.replace("cfg!(ophio_personal_signing)", "cfg!(not(ophio_personal_signing))"),
            good.replace("cfg!(ophio_personal_signing)", "cfg!(debug_assertions)"),
            good.replace("::core::cfg!(ophio_personal_signing)", "true"),
            good.replace("PUBLIC_CERT_SHA256 };", "PUBLIC_CERT_SHA256 }"),
            good.replace("RELEASE_CERT_SHA256: &str", "RELEASE_CERT_SHA256: String"),
            "/* unterminated comment " + good,
            'const NOTES: &str = "unterminated string;\n' + good,
        )
        for source in invalid:
            with self.subTest(source=source):
                self.assertNotEqual(source, good, "Malformed fixture must actually change the valid source")
                self.source.write_text(source, encoding="utf-8")
                with self.assertRaises(ReleaseError):
                    release_common.release_certificate(self.root)

    def test_attributed_pins_cannot_be_replaced_by_imports(self):
        good = declaration("1" * 64)
        for name in ("PUBLIC_CERT_SHA256", "PERSONAL_CERT_SHA256", "RELEASE_CERT_SHA256"):
            for attribute in ("#[cfg(any())]", "#[cfg_attr(all(), cfg(any()))]"):
                with self.subTest(name=name, attribute=attribute):
                    prefix = "pub const" if name == "RELEASE_CERT_SHA256" else "const"
                    source = good.replace(f"{prefix} {name}", f"{attribute}\n{prefix} {name}")
                    source += f'\nmod alternate {{ pub const OTHER: &str = "{"2" * 64}"; }}\n'
                    source += f'use alternate::OTHER as {name};\n'
                    self.source.write_text(source, encoding="utf-8")
                    with self.assertRaises(ReleaseError):
                        release_common.release_certificate(self.root)

    def test_unqualified_cfg_cannot_hide_macro_shadowing(self):
        source = 'macro_rules! cfg { ($($t:tt)*) => { true } }\n'
        source += declaration("1" * 64).replace("::core::cfg!", "cfg!")
        self.source.write_text(source, encoding="utf-8")
        with self.assertRaises(ReleaseError):
            release_common.release_certificate(self.root)

    def test_executable_must_carry_only_the_selected_ascii_pin(self):
        self.source.write_text(declaration("a" * 64, "b" * 64), encoding="utf-8")
        for mode, chosen, other in (("0", b"a" * 64, b"b" * 64), ("1", b"b" * 64, b"a" * 64)):
            with self.subTest(mode=mode), mock.patch.dict(os.environ, {"OPHIO_PERSONAL_SIGNING": mode}):
                release_common.require_executable_trust(b"binary\0" + chosen + b"\0rest", self.root)
                for data in (other, chosen + other, b"no pin", chosen[:-1], chosen.upper(), bytes.fromhex(chosen.decode())):
                    with self.subTest(data=data), self.assertRaisesRegex(ReleaseError, "rebuild.*OPHIO_PERSONAL_SIGNING"):
                        release_common.require_executable_trust(data, self.root)

    def test_the_real_companion_source_names_both_pins(self):
        public, personal = release_common.companion_certificates()
        self.assertEqual(personal, sign_android.PERSONAL_CERTIFICATE)
        self.assertEqual(public, sign_android.public_certificate(release_common.ROOT / sign_android.POLICY))


@unittest.skipUnless(os.environ.get("OPHIO_SIGNING_TEST_SDK"), "Set OPHIO_SIGNING_TEST_SDK for disposable real Android tool tests")
class PublicApkVerificationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        temporary = tempfile.TemporaryDirectory(prefix="ophio-public-apk-")
        cls.addClassCleanup(temporary.cleanup)
        cls.fixture = Path(temporary.name)
        sdk = Path(os.environ["OPHIO_SIGNING_TEST_SDK"])
        cls.tools = sdk / "build-tools/37.0.0"
        manifest = cls.fixture / "AndroidManifest.xml"
        manifest.write_text('<manifest xmlns:android="http://schemas.android.com/apk/res/android" package="org.example.synthetic" android:versionCode="1" android:versionName="1"><uses-sdk android:minSdkVersion="29"/><application android:hasCode="false" android:debuggable="false"/></manifest>', encoding="utf-8")
        unsigned = cls.fixture / "unsigned.apk"
        platform = sdk / "platforms/android-37/android.jar"
        if not platform.is_file():
            platform = sdk / "platforms/android-37.0/android.jar"
        cls.command([str(cls.tools / "aapt2"), "link", "--manifest", str(manifest), "-I", str(platform), "-o", str(unsigned)])
        with zipfile.ZipFile(unsigned, "a", compression=zipfile.ZIP_STORED) as archive:
            archive.writestr("assets/payload.txt", b"original signed payload")
        aligned = cls.fixture / "aligned.apk"
        cls.command([str(cls.tools / "zipalign"), "-P", "16", "4", str(unsigned), str(aligned)])
        password = cls.fixture / "synthetic-password"
        password.write_text("ephemeral-test-only-password\n", encoding="utf-8")
        key = cls.fixture / "synthetic.p12"
        cls.command(["/usr/bin/keytool", "-genkeypair", "-noprompt", "-keystore", str(key), "-storetype", "PKCS12", "-storepass:file", str(password), "-keypass:file", str(password), "-alias", "synthetic", "-keyalg", "RSA", "-keysize", "2048", "-validity", "1", "-dname", "CN=Synthetic Public Packaging,O=Disposable Tests,C=US"])
        certificate = cls.fixture / "synthetic.der"
        cls.command(["/usr/bin/keytool", "-exportcert", "-keystore", str(key), "-storepass:file", str(password), "-alias", "synthetic", "-file", str(certificate)])
        cls.certificate = hashlib.sha256(certificate.read_bytes()).hexdigest()
        signed = cls.fixture / "signed.apk"
        cls.command([str(cls.tools / "apksigner"), "sign", "--ks", str(key), "--ks-key-alias", "synthetic", "--ks-pass", f"file:{password}", "--v1-signing-enabled", "false", "--v2-signing-enabled", "true", "--v3-signing-enabled", "true", "--v4-signing-enabled", "false", "--out", str(signed), str(aligned)])
        cls.valid = signed.read_bytes()
        with zipfile.ZipFile(signed) as archive:
            header = archive.getinfo("assets/payload.txt").header_offset
        name_length, extra_length = struct.unpack_from("<HH", cls.valid, header + 26)
        tampered = bytearray(cls.valid)
        tampered[header + 30 + name_length + extra_length] ^= 1
        cls.tampered = bytes(tampered)
        # The certificate parser alone cannot distinguish this forgery.
        assert release_common.apk_signers(cls.valid) == release_common.apk_signers(cls.tampered)

    @staticmethod
    def command(args):
        result = subprocess.run(args, env=sign_android.tool_environment(), capture_output=True, text=True, encoding="utf-8")
        if result.returncode:
            raise AssertionError(f"Synthetic Android tool test failed: {result.stderr}")
        return result

    def setUp(self):
        patch = mock.patch.dict(os.environ, {"OPHIO_PERSONAL_SIGNING": "0"})
        patch.start()
        self.addCleanup(patch.stop)
        temporary = tempfile.TemporaryDirectory(dir=self.fixture)
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        trust(self.root, self.certificate)
        self.apk = self.root / "phone.apk"
        self.apk.write_bytes(self.valid)
        patch = mock.patch.dict(os.environ, {
            "OPHIO_PUBLIC_RELEASE": "1",
            "OPHIO_PHONE_APK": str(self.apk),
            "OPHIO_ANDROID_BUILD_TOOLS": str(self.tools),
        })
        patch.start()
        self.addCleanup(patch.stop)

    def test_valid_public_apk_is_accepted_without_signing(self):
        original = sign_android.run
        calls = []
        def record(args, **kwargs):
            calls.append(args)
            return original(args, **kwargs)
        with mock.patch.object(sign_android, "run", side_effect=record):
            self.assertEqual(release_common.phone_app_bytes(self.root), self.valid)
        self.assertTrue(any(args[1] == "verify" for args in calls))
        self.assertFalse(any(args[1] == "sign" or "keytool" in args[0] for args in calls))

    def test_tampered_signature_is_rejected_despite_matching_certificate(self):
        self.apk.write_bytes(self.tampered)
        result = subprocess.run([str(self.tools / "apksigner"), "verify", str(self.apk)], env=sign_android.tool_environment(), capture_output=True)
        self.assertNotEqual(result.returncode, 0)
        with self.assertRaises(ReleaseError):
            release_common.phone_app_bytes(self.root)

    def test_explicit_tools_are_required_and_never_fall_back(self):
        for tools in ("", "relative/build-tools", str(self.root / "missing-tools")):
            with self.subTest(tools=tools), mock.patch.dict(os.environ, {"OPHIO_ANDROID_BUILD_TOOLS": tools}):
                with self.assertRaises(ReleaseError):
                    release_common.phone_app_bytes(self.root)
        tools = self.root / "broken-tools"
        tools.mkdir()
        (tools / "apksigner").write_text("not executable", encoding="utf-8")
        with mock.patch.dict(os.environ, {"OPHIO_ANDROID_BUILD_TOOLS": str(tools)}):
            with self.assertRaises(ReleaseError):
                release_common.phone_app_bytes(self.root)

    def test_public_mode_cannot_return_a_mutable_source_path(self):
        with self.assertRaisesRegex(ReleaseError, "verified phone_app_bytes"):
            release_common.phone_app(self.root)

    def test_source_replaced_during_verification_cannot_change_returned_bytes(self):
        original = sign_android.verify
        stages = []
        def replace_source(staged, tools, certificate):
            stages.append(staged)
            self.assertNotEqual(staged, self.apk)
            self.assertEqual(staged.read_bytes(), self.valid)
            self.apk.write_bytes(self.tampered)
            original(staged, tools, certificate)
        with mock.patch.object(sign_android, "verify", side_effect=replace_source):
            self.assertEqual(release_common.phone_app_bytes(self.root), self.valid)
        self.assertEqual(self.apk.read_bytes(), self.tampered)
        self.assertTrue(stages)
        self.assertTrue(all(not path.exists() for path in stages))

    def test_personal_mode_preserves_certificate_only_flow_without_sdk(self):
        # Existing personal fixtures and callers need no new SDK dependency.
        with mock.patch.dict(os.environ, {"OPHIO_PUBLIC_RELEASE": "0", "OPHIO_ANDROID_BUILD_TOOLS": ""}), mock.patch.object(sign_android, "verify") as verify:
            self.assertEqual(release_common.phone_app(self.root), self.apk)
            self.assertEqual(release_common.phone_app_bytes(self.root), self.valid)
            verify.assert_not_called()

    def test_host_archives_embed_verified_bytes_not_replaced_source(self):
        target = self.root / "target"
        for platform, script in (("linux", "package-artifacts.py"), ("windows", "package-windows.py")):
            with self.subTest(platform=platform):
                self.apk.write_bytes(self.valid)
                spec = importlib.util.spec_from_file_location("public_archive_" + platform, Path(release_common.__file__).parent / script)
                module = importlib.util.module_from_spec(spec)
                spec.loader.exec_module(module)
                licences = target / "packaging/licenses" / platform
                licences.mkdir(parents=True)
                for name in ("NOTICE", "THIRD-PARTY-NOTICES.txt", "sbom.cdx.json", "license-report.json"):
                    (licences / name).write_bytes(b"synthetic licence fixture")
                for name in ("LICENSE", f"packaging/{platform}/INSTALL.md", "packaging/linux/install.sh", "packaging/icons/hicolor/scalable/apps/ophio.svg"):
                    path = self.root / name
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_bytes(b"synthetic fixture")
                release = target / release_common.TARGET_TRIPLES[platform] / "release"
                release.mkdir(parents=True)
                if platform == "windows":
                    binary = bytearray(80)
                    binary[:2] = b"MZ"
                    binary[60:64] = (64).to_bytes(4, "little")
                    binary[64:70] = b"PE\0\0\x64\x86"
                    names = ("ophio.exe", "computer-mcp.exe")
                else:
                    binary = bytearray(64)
                    binary[:6] = b"\x7fELF\x02\x01"
                    binary[18:20] = b"\x3e\x00"
                    names = ("ophio", "computer-mcp", "ophio-sandbox-shim")
                for name in names:
                    (release / name).write_bytes(binary + (self.certificate.encode("ascii") if name in {"ophio", "ophio.exe"} else b""))
                checked = []
                def replace_after_check(root):
                    data = release_common.phone_app_bytes(root)
                    checked.append(data)
                    self.apk.write_bytes(self.tampered)
                    return data
                with mock.patch.dict(os.environ, {"CARGO_TARGET_DIR": str(target), "SOURCE_DATE_EPOCH": "1700000000"}), mock.patch.object(module, "identity", return_value=("Ophio", "ophio", "1.0.0")), mock.patch.object(module, "require_licences"), mock.patch.object(module, "manifest", return_value=""), mock.patch.object(module, "phone_app_bytes", side_effect=replace_after_check):
                    if platform == "windows":
                        output = module.assemble(self.root, target, 1700000000)
                        with zipfile.ZipFile(output) as archive:
                            actual = archive.read("ophio-1.0.0-x86_64-windows/ophio.apk")
                    else:
                        with mock.patch.object(module, "ROOT", self.root), mock.patch.object(module, "check_glibc", return_value="2.17"), mock.patch.object(module, "subprocess", SimpleNamespace(run=lambda *args, **kwargs: SimpleNamespace(stdout="ophio 1.0.0\n"))):
                            module.linux()
                        output = next((self.root / "dist/linux").glob("*.tar.gz"))
                        with tarfile.open(output) as archive:
                            actual = archive.extractfile("ophio-1.0.0-x86_64-linux/ophio.apk").read()
                self.assertEqual(checked, [self.valid])
                self.assertEqual(actual, self.valid)
                self.assertNotEqual(actual, self.apk.read_bytes())

    def test_valid_signature_with_wrong_certificate_is_rejected(self):
        trust(self.root, "2" * 64)
        with self.assertRaises(ReleaseError):
            release_common.phone_app_bytes(self.root)


if __name__ == "__main__":
    unittest.main()
