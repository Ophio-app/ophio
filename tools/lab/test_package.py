# SPDX-License-Identifier: Apache-2.0
"""Exercise the real guest preflight in Bash without a VM or a build."""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


PACKAGE = Path(__file__).parent / "guest/package.sh"
TOOLS = ("apksigner", "zipalign", "lib/apksigner.jar", "lib64/libc++.so")
COMMIT = "0123456789abcdef" * 2 + "01234567"
VARIABLES = (
    "OPHIO_PUBLIC_RELEASE", "OPHIO_PHONE_APK", "OPHIO_ANDROID_BUILD_TOOLS",
    "OPHIO_PERSONAL_SIGNING",
)
# Only the machine identity is simulated. Stop at the first source extraction,
# before running any build command, and capture the exported environment there.
PRELUDE = r'''
cat() {
  if [[ $1 == /etc/companion-lab ]]; then
    printf '%s\n' disposable-desktop-lab
  else
    command cat "$@"
  fi
}
systemd-detect-virt() { echo kvm; }
function . {
  [[ $1 == /etc/os-release ]] || return 99
  ID=ubuntu VERSION_ID=24.04
}
java() { echo 'stub Java'; }
tar() {
  /usr/bin/python3 -c 'import json,os; print(json.dumps(dict(os.environ)))' > "$HOME/probe.json"
  return 42
}
source "$1" "${@:2}"
'''


@unittest.skipIf(os.geteuid() == 0, "guest packaging deliberately refuses root")
class PackageTests(unittest.TestCase):
    def run_package(self, args, missing=None, nonexecutable=None):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            files = ["package-phone.apk", *("android-build-tools/" + name for name in TOOLS)]
            for name in files:
                if name == missing:
                    continue
                path = home / "lab" / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(b"fixture")
                if name in ("android-build-tools/apksigner", "android-build-tools/zipalign"):
                    path.chmod(0o600 if name == nonexecutable else 0o700)
            old_work = home / "lab/package/old-build"
            old_work.parent.mkdir()
            old_work.write_bytes(b"do not discard before preflight")
            inherited_config = home / "inherited-build.sh"
            inherited_config.write_text(
                "export OPHIO_SOURCE_COMMIT=" + "f" * 40 + "\n", encoding="utf-8"
            )
            environment = {
                "PATH": "/usr/bin:/bin", "HOME": str(home), "OPHIO_SOURCE_COMMIT": "f" * 40,
                "BUILD_ENV_FILE": str(inherited_config),
                **{name: "inherited-value" for name in VARIABLES},
            }
            result = subprocess.run(
                ["bash", "-c", PRELUDE, "test-package", str(PACKAGE.resolve()), *args],
                env=environment, capture_output=True, text=True, encoding="utf-8",
            )
            probe = home / "probe.json"
            exported = json.loads(probe.read_text(encoding="utf-8")) if probe.exists() else None
            return result, exported, old_work.exists(), home

    def test_public_exports_only_the_staged_signing_inputs(self):
        result, exported, _, home = self.run_package(["1700000000", COMMIT, "public"])
        self.assertEqual(result.returncode, 42, result.stderr)
        self.assertEqual(exported["OPHIO_PUBLIC_RELEASE"], "1")
        self.assertEqual(exported["OPHIO_PHONE_APK"], str(home / "lab/package-phone.apk"))
        self.assertEqual(exported["OPHIO_ANDROID_BUILD_TOOLS"], str(home / "lab/android-build-tools"))
        self.assertNotIn("OPHIO_PERSONAL_SIGNING", exported)
        self.assertEqual(exported["SOURCE_DATE_EPOCH"], "1700000000")
        self.assertEqual(exported["OPHIO_SOURCE_COMMIT"], COMMIT)

    def test_ordinary_build_cannot_inherit_public_or_personal_inputs(self):
        result, exported, _, _ = self.run_package(["1700000000", COMMIT])
        self.assertEqual(result.returncode, 42, result.stderr)
        self.assertEqual(exported["OPHIO_SOURCE_COMMIT"], COMMIT)
        for name in VARIABLES:
            self.assertNotIn(name, exported)

    def test_inherited_build_config_cannot_override_the_selected_commit(self):
        for mode in ([], ["public"]):
            with self.subTest(mode=mode):
                result, exported, _, _ = self.run_package(["1", COMMIT, *mode])
                self.assertEqual(result.returncode, 42, result.stderr)
                self.assertEqual(exported["OPHIO_SOURCE_COMMIT"], COMMIT)
                # package-common.sh sources this file after the guest preflight.
                self.assertNotIn("BUILD_ENV_FILE", exported)

    def test_both_commit_lengths_reach_public_and_ordinary_builds(self):
        for commit in (COMMIT, "0123456789abcdef" * 4):
            for mode in ([], ["public"]):
                with self.subTest(commit=commit, mode=mode):
                    result, exported, _, _ = self.run_package(["1", commit, *mode])
                    self.assertEqual(result.returncode, 42, result.stderr)
                    self.assertEqual(exported["OPHIO_SOURCE_COMMIT"], commit)

    def test_invalid_arguments_stop_before_source_extraction(self):
        for args in (
            [], ["bad-epoch", COMMIT], ["1"], ["1", COMMIT[:12]], ["1", COMMIT.upper()],
            ["1", COMMIT + "0"], ["1", "a" * 63], ["1", "a" * 65], ["1", "g" * 40],
            ["1", COMMIT, "private"], ["1", COMMIT, ""],
            ["1", COMMIT, "public", "extra"], ["1", "public"],
        ):
            with self.subTest(args=args):
                result, exported, preserved, _ = self.run_package(args)
                self.assertNotEqual(result.returncode, 0)
                self.assertNotIn("unbound variable", result.stderr)
                self.assertIsNone(exported)
                self.assertTrue(preserved)

    def test_missing_public_input_stops_before_source_extraction(self):
        for name in ("package-phone.apk", *("android-build-tools/" + name for name in TOOLS)):
            with self.subTest(missing=name):
                result, exported, preserved, _ = self.run_package(["1", COMMIT, "public"], missing=name)
                self.assertNotEqual(result.returncode, 0)
                self.assertIsNone(exported)
                self.assertTrue(preserved)

    def test_nonexecutable_tool_stops_before_source_extraction(self):
        for name in ("apksigner", "zipalign"):
            with self.subTest(tool=name):
                result, exported, preserved, _ = self.run_package(
                    ["1", COMMIT, "public"], nonexecutable="android-build-tools/" + name,
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertIsNone(exported)
                self.assertTrue(preserved)
