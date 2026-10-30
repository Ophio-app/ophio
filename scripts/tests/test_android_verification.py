# SPDX-License-Identifier: Apache-2.0
"""The Android build checks its Gradle distribution and every dependency it resolves."""
from __future__ import annotations

import contextlib
import hashlib
import importlib.util
import io
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest import mock
import xml.etree.ElementTree as ET

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
ROOT = Path(__file__).resolve().parents[2]
ANDROID = "client/src-tauri/gen/android"
WRAPPER = f"{ANDROID}/gradle/wrapper/gradle-wrapper.properties"
WRAPPER_JAR = f"{ANDROID}/gradle/wrapper/gradle-wrapper.jar"
# Gradle's published wrapper checksum for this version, from
# https://gradle.org/release-checksums/. Update both when Gradle changes.
GRADLE_VERSION = "9.6.1"
WRAPPER_JAR_SHA256 = "497c8c2a7e5031f6aa847f88104aa80a93532ec32ee17bdb8d1d2f67a194a9c7"
METADATA = f"{ANDROID}/gradle/verification-metadata.xml"
NS = "{https://schema.gradle.org/dependency-verification}"
# Build files whose literal coordinates the app, buildSrc and plugin builds resolve.
BUILD_FILES = (
    f"{ANDROID}/build.gradle.kts",
    f"{ANDROID}/app/build.gradle.kts",
    f"{ANDROID}/buildSrc/build.gradle.kts",
    "client/src-tauri/plugins/device/android/build.gradle.kts",
)
# Files sync-native.mjs reads, relative to the checkout.
SYNC_INPUTS = (
    "client/src-tauri/tools/sync-native.mjs",
    "client/src-tauri/tauri.conf.json",
    f"{ANDROID}/app/build.gradle.kts",
    f"{ANDROID}/app/src/main/java/app/ophio/MainActivity.kt",
    f"{ANDROID}/app/src/main/res/values/strings.xml",
    f"{ANDROID}/app/src/main/res/values/colors.xml",
    f"{ANDROID}/app/src/main/res/values-night/colors.xml",
    "client/src/design/tokens.css",
    "client/src/brand.ts",
    "crates/companion/src/config.rs",
    "crates/ports/src/lib.rs",
)

spec = importlib.util.spec_from_file_location(
    "package_artifacts", ROOT / "scripts/package-artifacts.py"
)
package_artifacts = importlib.util.module_from_spec(spec)
spec.loader.exec_module(package_artifacts)


def components() -> dict[tuple[str, str, str], list[ET.Element]]:
    root = ET.parse(ROOT / METADATA).getroot()
    return {
        (c.get("group"), c.get("name"), c.get("version")): c.findall(NS + "artifact")
        for c in root.iter(NS + "component")
    }


class AndroidVerificationTests(unittest.TestCase):
    def test_wrapper_pins_the_distribution_checksum(self):
        properties = (ROOT / WRAPPER).read_text(encoding="utf-8")
        self.assertRegex(
            properties,
            r"(?m)^distributionUrl=https\\://services\.gradle\.org/"
            r"distributions/gradle-[0-9.]+-bin\.zip$",
        )
        self.assertRegex(properties, r"(?m)^distributionSha256Sum=[0-9a-f]{64}$")

    def test_wrapper_jar_is_gradles_published_jar(self):
        self.assertIn(
            f"/gradle-{GRADLE_VERSION}-bin.zip\n", (ROOT / WRAPPER).read_text(encoding="utf-8")
        )
        self.assertEqual(
            hashlib.sha256((ROOT / WRAPPER_JAR).read_bytes()).hexdigest(),
            WRAPPER_JAR_SHA256,
        )

    def test_metadata_verifies_every_artifact_by_sha256_without_trust_rules(self):
        root = ET.parse(ROOT / METADATA).getroot()
        configuration = root.find(NS + "configuration")
        self.assertEqual(configuration.findtext(NS + "verify-metadata"), "true")
        self.assertEqual(configuration.findtext(NS + "verify-signatures"), "false")
        self.assertIsNone(configuration.find(NS + "trusted-artifacts"))
        self.assertIsNone(configuration.find(NS + "trusted-keys"))
        found = components()
        self.assertGreater(len(found), 0)
        for coordinates, artifacts in found.items():
            with self.subTest(component=":".join(coordinates)):
                self.assertTrue(artifacts)
                for artifact in artifacts:
                    self.assertRegex(
                        artifact.find(NS + "sha256").get("value"), r"^[0-9a-f]{64}$"
                    )
        # Generated from an empty Gradle home, it names no local path.
        text = (ROOT / METADATA).read_text(encoding="utf-8")
        self.assertNotIn(str(Path.home()), text)
        self.assertNotIn(str(ROOT), text)

    def test_metadata_covers_each_declared_dependency(self):
        # A version bump without regenerated metadata fails verification in the build.
        found = components()
        for name in BUILD_FILES:
            declared = re.findall(
                r'"([\w.-]+):([\w.-]+):([\w.-]+)"', (ROOT / name).read_text(encoding="utf-8")
            )
            self.assertTrue(declared, name)
            for coordinates in declared:
                with self.subTest(build=name, dependency=":".join(coordinates)):
                    self.assertIn(coordinates, found)

    def test_sync_rejects_a_project_without_either_checksum(self):
        for missing in (WRAPPER, METADATA):
            with (
                self.subTest(missing=missing),
                tempfile.TemporaryDirectory() as directory,
            ):
                root = Path(directory)
                for name in (*SYNC_INPUTS, WRAPPER, METADATA):
                    (root / name).parent.mkdir(parents=True, exist_ok=True)
                    shutil.copyfile(ROOT / name, root / name)
                sync = ["node", str(root / SYNC_INPUTS[0])]
                subprocess.run(sync, check=True, capture_output=True)
                if missing == WRAPPER:
                    # Tauri's template properties, as `android init` writes them.
                    text = (root / missing).read_text(encoding="utf-8")
                    (root / missing).write_text(
                        re.sub(r"(?m)^distributionSha256Sum=.*\n", "", text),
                        encoding="utf-8",
                    )
                else:
                    (root / missing).unlink()
                result = subprocess.run(sync, capture_output=True, text=True, encoding="utf-8")
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("Android Gradle checksums missing", result.stderr)

    def test_staged_android_project_keeps_both_checksums(self):
        with (
            tempfile.TemporaryDirectory() as directory,
            contextlib.redirect_stdout(io.StringIO()),
        ):
            root = Path(directory)
            for name in (WRAPPER, METADATA):
                (root / name).parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(ROOT / name, root / name)
            (root / "client/node_modules").mkdir()
            subprocess.run(["git", "init", "-q", str(root)], check=True)
            subprocess.run(
                ["git", "add", "--", WRAPPER, METADATA], cwd=root, check=True
            )
            with (
                mock.patch.object(package_artifacts, "ROOT", root),
                mock.patch.dict(os.environ, {"CARGO_TARGET_DIR": str(root / "target")}),
            ):
                package_artifacts.stage_android()
            staged = root / "target/packaging/android/project"
            for name in (WRAPPER, METADATA):
                with self.subTest(name=name):
                    self.assertEqual(
                        (staged / name).read_bytes(), (ROOT / name).read_bytes()
                    )


if __name__ == "__main__":
    unittest.main()
