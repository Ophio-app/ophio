# SPDX-License-Identifier: Apache-2.0
"""Compile the companion's trust choice independently of application adapters."""
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import release_common


@unittest.skipUnless(shutil.which("cargo"), "Cargo is required for compiler trust-choice tests")
class CompiledTrustTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="ophio-compiled-trust-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        # Compile the actual build script and trust declarations in a tiny,
        # dependency-free fixture, not the application or its native adapters.
        shutil.copyfile(release_common.ROOT / "crates/companion/build.rs", self.root / "build.rs")
        source = (release_common.ROOT / "crates/companion/src/phone/mod.rs").read_text(encoding="utf-8")
        pins = source[source.index("const PUBLIC_CERT_SHA256"):source.index("const TAILSCALE")]
        (self.root / "main.rs").write_text(pins + '\nfn main() { println!("{}", RELEASE_CERT_SHA256); }\n', encoding="utf-8")
        (self.root / "Cargo.toml").write_text(
            '[package]\nname = "trust-fixture"\nversion = "0.0.0"\nedition = "2024"\n'
            '[[bin]]\nname = "trust-fixture"\npath = "main.rs"\n', encoding="utf-8",
        )
        self.environment = {
            key: value for key, value in os.environ.items()
            if not key.startswith(("OPHIO_", "CARGO_"))
            and key not in {"RUSTFLAGS", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER"}
        }
        if "CARGO_HOME" in os.environ:
            self.environment["CARGO_HOME"] = os.environ["CARGO_HOME"]
        self.environment.update({
            "CARGO_TARGET_DIR": str(self.root / "target"),
            "OPHIO_SOURCE_COMMIT": "0" * 40,
        })

    def cargo(self, settings, *args):
        return subprocess.run(
            ["cargo", *args, "--offline", "--quiet"], cwd=self.root,
            env=self.environment | settings, capture_output=True, text=True,
            encoding="utf-8", timeout=60,
        )

    def test_rebuilds_when_the_selected_certificate_changes(self):
        public, personal = release_common.companion_certificates()
        for settings, expected in (
            ({}, public),
            ({"OPHIO_PERSONAL_SIGNING": "1"}, personal),
            ({"OPHIO_PERSONAL_SIGNING": "0", "OPHIO_PUBLIC_RELEASE": "1"}, public),
        ):
            with self.subTest(settings=settings):
                result = self.cargo(settings, "run")
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout.strip(), expected)

    def test_direct_cargo_refuses_conflicting_or_malformed_settings(self):
        for settings, message in (
            ({"OPHIO_PERSONAL_SIGNING": "1", "OPHIO_PUBLIC_RELEASE": "1"}, "cannot trust the personal"),
            ({"OPHIO_PERSONAL_SIGNING": "yes"}, "OPHIO_PERSONAL_SIGNING must be 0 or 1"),
            ({"OPHIO_PUBLIC_RELEASE": "yes"}, "OPHIO_PUBLIC_RELEASE must be 0 or 1"),
        ):
            with self.subTest(settings=settings):
                result = self.cargo(settings, "check")
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(message, result.stderr)

    def test_python_and_rust_refuse_every_malformed_personal_setting(self):
        values = ["", "true", "yes", " 1", "1\n", "01", "１"]
        if os.name == "posix":
            values.append(os.fsdecode(b"\xff"))
        for value in values:
            with self.subTest(value=repr(value)):
                result = self.cargo({"OPHIO_PERSONAL_SIGNING": value}, "check")
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("OPHIO_PERSONAL_SIGNING must be 0 or 1", result.stderr)
                with mock.patch.dict(os.environ, {"OPHIO_PERSONAL_SIGNING": value}):
                    with self.assertRaisesRegex(release_common.ReleaseError, "must be 0 or 1"):
                        release_common.release_certificate()

    def test_local_cfg_macro_cannot_change_the_compiled_pin(self):
        source = self.root / "main.rs"
        source.write_text(
            '#[allow(unused_macros)]\nmacro_rules! cfg { ($($t:tt)*) => { true } }\n'
            + source.read_text(encoding="utf-8"), encoding="utf-8",
        )
        result = self.cargo({}, "run")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), release_common.companion_certificates()[0])

    def test_compiler_flags_cannot_override_public_trust(self):
        for settings in (
            {"RUSTFLAGS": "--cfg ophio_personal_signing"},
            {"CARGO_ENCODED_RUSTFLAGS": "--cfg\x1fophio_personal_signing"},
        ):
            with self.subTest(settings=settings):
                result = self.cargo(settings | {"OPHIO_PUBLIC_RELEASE": "1"}, "check")
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("instead of injecting", result.stderr)
        # Extra rustc arguments bypass the build script's CARGO_CFG variables,
        # but must still fail the assertion in the compiled trust declarations.
        result = subprocess.run(
            ["cargo", "rustc", "--offline", "--quiet", "--", "--cfg", "ophio_personal_signing"],
            cwd=self.root, env=self.environment, capture_output=True, text=True,
            encoding="utf-8", timeout=60,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("instead of injecting", result.stderr)
