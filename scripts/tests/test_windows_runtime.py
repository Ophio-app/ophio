# SPDX-License-Identifier: Apache-2.0
from __future__ import annotations

from pathlib import Path
import shutil
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from release_common import ROOT, ReleaseError
import windows_runtime

TOOLCHAIN = "/opt/llvm-mingw/x86_64-w64-mingw32/lib"
MAP = f"""\
Address  Size     Align Out     In      Symbol
00001000 000bf406  4096 .text
00001000 0000045f    16         {TOOLCHAIN}/crt2.o:(.text)
00001000 00000000     0                 mainCRTStartup
00001460 00000000     4         {TOOLCHAIN}/crtbegin.o:(.text)
00001460 00000055    16         /build/target/deps/app.rcgu.o:(.text$_ZN3app4main)
000ba1d4 00000000     4         lib64_libmingwex_a-mingw_matherr.o:(.text)
000bad50 00002b26    16         lib64_libmingwex_a-mingw_pformat.o:(.text)
000bad80 00000010    16         a0330e891e733f4e-crt_shim.o:(.text)
000bad90 00000000     0                 lookalike:(.text)
"""


class WindowsRuntimeTests(unittest.TestCase):
    def check(self, text: str, root: Path = ROOT) -> set[str]:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "app.map"
            path.write_text(text, encoding="utf-8")
            return windows_runtime.check([path], root)

    def test_map_inputs_are_read_and_symbols_ignored(self):
        self.assertEqual(
            windows_runtime.linked(MAP),
            {
                "crt2.o",
                "crtbegin.o",
                "lib64_libmingwex_a-mingw_matherr.o",
                "lib64_libmingwex_a-mingw_pformat.o",
            },
        )

    def test_reviewed_objects_pass(self):
        self.assertIn("lib64_libmingwex_a-mingw_pformat.o", self.check(MAP))

    def test_unreviewed_runtime_object_fails(self):
        cephes = MAP + "000bb000 00000400    16         lib64_libmingwex_a-lgamma.o:(.text)\n"
        with self.assertRaisesRegex(ReleaseError, "lgamma"):
            self.check(cephes)

    def test_unread_map_fails(self):
        with self.assertRaisesRegex(ReleaseError, "startup object"):
            self.check("Address  Size     Align Out     In      Symbol\n")

    def test_component_must_declare_every_linked_licence(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name in ("components.toml", "windows/runtime-objects.toml"):
                (root / "packaging" / name).parent.mkdir(parents=True, exist_ok=True)
                shutil.copy(ROOT / "packaging" / name, root / "packaging" / name)
            components = root / "packaging/components.toml"
            components.write_text(
                components.read_text(encoding="utf-8").replace(
                    " AND LicenseRef-gdtoa-Lucent", "", 1
                ),
                encoding="utf-8",
            )
            with self.assertRaisesRegex(ReleaseError, "gdtoa"):
                self.check(MAP, root)


if __name__ == "__main__":
    unittest.main()
