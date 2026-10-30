# SPDX-License-Identifier: Apache-2.0
"""Tests for the packaging ABI ceiling; no executables or desktop are started."""

import sys
from pathlib import Path
import unittest
from unittest.mock import patch
import subprocess

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "scripts"))
from package_linux import required_glibc, check_glibc  # noqa: E402
from release_common import ReleaseError  # noqa: E402


class AbiTests(unittest.TestCase):
    def test_numeric_order_and_only_requirements(self):
        text = "Version definition section\nName: GLIBC_9.0\nVersion needs section\nName: GLIBC_2.9\nName: GLIBC_2.39\nName: GLIBC_2.3.4\n"
        self.assertEqual(required_glibc(text), (2, 39))

    def test_uninspectable_input_is_rejected(self):
        for text in ("", "Version needs section\nName: OTHER_2.43"):
            with self.assertRaises(ReleaseError):
                required_glibc(text)

    def test_newer_abi_is_refused_before_packaging(self):
        with patch(
            "package_linux.subprocess.run",
            return_value=subprocess.CompletedProcess(
                [], 0, stdout="Version needs section\nName: GLIBC_2.43\n"
            ),
        ):
            with self.assertRaisesRegex(ReleaseError, "requires GLIBC_2.43"):
                check_glibc(Path("ophio"))
