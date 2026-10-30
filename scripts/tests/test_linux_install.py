# SPDX-License-Identifier: Apache-2.0
"""The archive's install.sh, run against a temporary home only.

The install itself is `ophio install`, tested in crates/companion/src/update.rs;
install.sh only hands over to it.
"""
from __future__ import annotations

from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

INSTALLER = Path(__file__).resolve().parents[2] / "packaging/linux/install.sh"


class InstallScriptTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.home = self.root / "home"
        self.home.mkdir()
        self.archive = self.root / "ophio 1.2.3-x86_64-linux"
        self.archive.mkdir()
        shutil.copy2(INSTALLER, self.archive / "install.sh")

    def run_installer(self, *args: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["/bin/sh", str(self.archive / "install.sh"), *args],
            cwd=self.root,
            env={"HOME": str(self.home), "PATH": "/usr/bin:/bin"},
            stdin=subprocess.DEVNULL,
            capture_output=True,
            text=True,
            timeout=30,
            encoding="utf-8",
        )

    def test_hands_over_to_the_ophio_beside_it(self):
        ophio = self.archive / "ophio"
        ophio.write_text(
            '#!/bin/sh\nprintf "%s\\n" "$0" "$@" > "$HOME/ran"\nexit 4\n', encoding="utf-8"
        )
        ophio.chmod(0o755)
        result = self.run_installer("--now", "--no-setup")
        # Its outcome is the install's.
        self.assertEqual(result.returncode, 4)
        self.assertEqual(
            (self.home / "ran").read_text(encoding="utf-8").splitlines(),
            [str(ophio), "install", "--now", "--no-setup"],
        )

    def test_says_what_is_missing(self):
        result = self.run_installer()
        self.assertEqual(result.returncode, 1)
        self.assertIn("ophio is missing beside install.sh", result.stderr)
        self.assertEqual(list(self.home.iterdir()), [])


if __name__ == "__main__":
    unittest.main()
