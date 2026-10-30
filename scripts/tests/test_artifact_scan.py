# SPDX-License-Identifier: Apache-2.0
"""Built-file scan checks with synthetic archives and a made-up private term."""

from __future__ import annotations

import gzip
import io
from pathlib import Path
import re
import sys
import tarfile
import time
import unittest
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import artifact_scan

TERM = re.compile(r"(?<![^\W_])(?:Quillwort)(?![^\W_])", re.IGNORECASE)
HOME = ("/" + "home/").encode()


def zipped(files: dict[str, bytes]) -> bytes:
    out = io.BytesIO()
    with zipfile.ZipFile(out, "w") as archive:
        for name, data in files.items():
            archive.writestr(name, data)
    return out.getvalue()


def tarred(files: dict[str, bytes]) -> bytes:
    out = io.BytesIO()
    with tarfile.open(fileobj=out, mode="w:gz") as archive:
        for name, data in files.items():
            info = tarfile.TarInfo(name)
            info.size = len(data)
            archive.addfile(info, io.BytesIO(data))
    return out.getvalue()


class ArtifactScan(unittest.TestCase):
    def rules(self, name: str, data: bytes) -> list[tuple[str, str]]:
        return [(h["rule"], h["path"]) for h in artifact_scan.scan_bytes(name, data, TERM)]

    def test_finds_a_term_inside_an_app_inside_a_download(self) -> None:
        app = zipped({"classes.dex": b"\x00\x01built by quillwort\x00", "res/a.xml": b"fine"})
        download = tarred({"ophio/ophio.apk": app, "ophio/README": b"fine"})
        self.assertIn(
            ("private-term", "ophio.tar.gz!ophio/ophio.apk!classes.dex"),
            self.rules("ophio.tar.gz", download),
        )

    def test_finds_home_folders_but_not_examples_or_a_bare_prefix(self) -> None:
        self.assertEqual(self.rules("ophio", b"x" + HOME + b"pat/.cargo/registry"), [("absolute-home-path", "ophio")])
        self.assertEqual(self.rules("ophio", b"x" + HOME + b"sam/project " + HOME + b"/users"), [])

    def test_finds_temporary_build_roots_in_nested_binary_assets(self) -> None:
        build_root = b"/tmp/" + b"build-session-A1/atarget/packaging/android/project/" + b"client/src-tauri"
        app = zipped({"lib/arm64-v8a/libphone_shell.so": b"\x7fELF\x00" + build_root + b"\x00"})
        download = tarred({"ophio/ophio.apk": app})
        self.assertIn(
            ("absolute-build-path", "ophio.tar.gz!ophio/ophio.apk!lib/arm64-v8a/libphone_shell.so"),
            self.rules("ophio.tar.gz", download),
        )
        self.assertIn(
            ("absolute-build-path", "resources.arsc"),
            self.rules("resources.arsc", build_root.decode().encode("utf-16le")),
        )

    def test_finds_temporary_build_roots_in_concatenated_binary_strings(self) -> None:
        manifest = b"/tmp/" + b"build-1/ophio/client/src-tauri" + b"phone_shell::alerts"
        location = b"handle.rs:244" + b"/tmp/" + b"build-1/ophio/crates/protocol/src/reports.rs"
        for value in [manifest, location]:
            with self.subTest(value=value, format="nested"):
                app = zipped({"lib/arm64-v8a/libphone_shell.so": b"\x7fELF" + value})
                download = tarred({"ophio/ophio.apk": app})
                self.assertIn(
                    ("absolute-build-path", "ophio.tar.gz!ophio/ophio.apk!lib/arm64-v8a/libphone_shell.so"),
                    self.rules("ophio.tar.gz", download),
                )
            for encoding in ["utf-16le", "utf-16be"]:
                with self.subTest(value=value, encoding=encoding):
                    self.assertIn(
                        ("absolute-build-path", "resources.arsc"),
                        self.rules("resources.arsc", b"x" + value.decode().encode(encoding)),
                    )

    def test_finds_spaced_and_deep_temporary_build_roots(self) -> None:
        prefix = b"/tmp/" + b"build session/project/"
        spaced = prefix + b"crates/protocol/src/reports.rs"
        deep = b"/var/tmp/" + b"/".join(b"folder" + str(n).encode() for n in range(12))
        deep += b"/client/src-tauri/src/main.rs"
        for value in (spaced, deep):
            with self.subTest(value=value, format="nested"):
                app = zipped({"lib/arm64-v8a/libphone_shell.so": b"\x7fELF" + b"handle.rs:244" + value + b"handler"})
                download = tarred({"ophio/ophio.apk": app})
                self.assertIn(
                    ("absolute-build-path", "ophio.tar.gz!ophio/ophio.apk!lib/arm64-v8a/libphone_shell.so"),
                    self.rules("ophio.tar.gz", download),
                )
            with self.subTest(value=value, format="packed"):
                self.assertIn("absolute-build-path", [r for r, _ in self.rules("binary", b"prefix" + value + b"suffix")])
            for encoding in ("utf-16le", "utf-16be"):
                with self.subTest(value=value, encoding=encoding):
                    self.assertIn("absolute-build-path", [r for r, _ in self.rules("resources.arsc", b"x" + value.decode().encode(encoding))])

    def test_large_binary_with_many_temporary_runtime_paths_scans_promptly(self) -> None:
        runtime = b"/tmp/" + b"downloads/cache/runtime.sock\x00"
        data = runtime * 100_000
        start = time.monotonic()
        self.assertEqual(self.rules("binary", data), [])
        self.assertLess(time.monotonic() - start, 8.0)

    def test_normal_temporary_runtime_paths_remain_allowed(self) -> None:
        self.assertEqual(
            self.rules("ophio", b"/tmp/ophio-runtime.sock /tmp/ophio/cache/index /var/tmp/my-file /tmp/my downloads/cache/index"),
            [],
        )
        self.assertEqual(
            self.rules("app.apk", zipped({"assets/log.txt": b"/tmp/downloads/file.tar.gz"})),
            [],
        )
        self.assertEqual(
            self.rules("app.apk", zipped({
                "assets/log.txt": b"/data/local/tmp/" + b"build session/ophio/client/src-tauri"
            })),
            [],
        )

    def test_finds_keys_and_terms_in_names_and_compressed_files(self) -> None:
        key = b"-----BEGIN " + b"PRIVATE KEY-----\n"
        hits = self.rules("app.zip", zipped({"Quillwort/notes.gz": gzip.compress(key)}))
        self.assertIn("private-term-in-name", [r for r, _ in hits])
        self.assertIn("private-key-block", [r for r, _ in hits])
        self.assertTrue(all("Quillwort" not in path for _, path in hits))

    def test_this_test_source_passes_the_public_source_audit(self) -> None:
        from source_candidate import inspect
        self.assertEqual(inspect("scripts/tests/test_artifact_scan.py", Path(__file__).read_bytes(), []), [])

    def test_archive_metadata_is_scanned(self) -> None:
        out = io.BytesIO()
        with zipfile.ZipFile(out, "w") as archive:
            archive.comment = b"Quillwort"
            archive.writestr("clean", b"fine")
        self.assertTrue(self.rules("app.apk", out.getvalue()))
        out = io.BytesIO()
        with tarfile.open(fileobj=out, mode="w:gz") as archive:
            info = tarfile.TarInfo("clean")
            info.uname = "Quillwort"
            archive.addfile(info)
        self.assertTrue(self.rules("download.tar.gz", out.getvalue()))

    def test_home_paths_and_private_terms_in_non_file_entries(self) -> None:
        path = HOME.decode() + "builder/notes"
        for name in [path, "Quillwort/"]:
            with self.subTest(name=name):
                hits = self.rules("app.zip", zipped({name: b"fine"}))
                self.assertTrue(hits)
                self.assertTrue(all("builder" not in p and "Quillwort" not in p for _, p in hits))
        out = io.BytesIO()
        with tarfile.open(fileobj=out, mode="w:gz") as archive:
            info = tarfile.TarInfo("link")
            info.type = tarfile.SYMTYPE
            info.linkname = path
            archive.addfile(info)
        self.assertTrue(self.rules("download.tgz", out.getvalue()))

    def test_utf16_strings_at_either_byte_alignment(self) -> None:
        for encoding in ["utf-16le", "utf-16be"]:
            for prefix in [b"", b"x"]:
                for text in ["Quillwort", HOME.decode() + "builder/file", "-----BEGIN " + "PRIVATE KEY-----"]:
                    with self.subTest(encoding=encoding, prefix=prefix, text=text):
                        self.assertTrue(self.rules("resources.arsc", prefix + text.encode(encoding)))

    def test_nested_archives_are_recognized_without_extensions(self) -> None:
        out = io.BytesIO()
        with tarfile.open(fileobj=out, mode="w") as archive:
            info = tarfile.TarInfo("nested.gz")
            content = gzip.compress(b"Quillwort")
            info.size = len(content)
            archive.addfile(info, io.BytesIO(content))
        self.assertTrue(self.rules("blob", zipped({"nested": out.getvalue()})))
        self.assertTrue(self.rules("blob", gzip.compress(b"Quillwort")))

    def test_archive_errors_do_not_echo_private_names(self) -> None:
        broken = bytearray(zipped({"Quillwort": b"original"}))
        offset = broken.index(b"original")
        broken[offset] ^= 1
        try:
            self.rules("app.zip", bytes(broken))
        except Exception as error:
            self.assertNotIn("Quillwort", str(error))
        else:
            self.fail("A corrupt archive must not pass its scan")

    def test_deeper_archives_are_refused(self) -> None:
        data = b"fine"
        for _ in range(artifact_scan.DEPTH + 1):
            data = zipped({"next.zip": data})
        self.assertIn("archive-too-deep", [r for r, _ in self.rules("app.zip", data)])

    def test_a_clean_download_has_no_findings(self) -> None:
        self.assertEqual(self.rules("ophio.tar.gz", tarred({"ophio/ophio": b"\x7fELF fine"})), [])


if __name__ == "__main__":
    unittest.main()
