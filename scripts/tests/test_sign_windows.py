# SPDX-License-Identifier: Apache-2.0
"""Offline orchestration tests. Mock SignTool results are NOT signed artifacts."""

import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import unittest
from unittest import mock
import zipfile

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / filename)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


signing = load("sign_windows", "sign-windows.py")
packaging = load("package_windows", "package-windows.py")


def pe():
    data = bytearray(128)
    data[:2] = b"MZ"
    data[60:64] = (64).to_bytes(4, "little")
    data[64:70] = b"PE\0\0\x64\x86"
    return bytes(data)


class WindowsSigningTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.source = self.root / "unsigned.zip"
        self.output = self.root / "signed.zip"
        self.signtool = self.root / "sdk tools" / "signtool.exe"
        self.dlib = self.root / "client tools" / "Azure.CodeSigning.Dlib.dll"
        for tool in (self.signtool, self.dlib):
            tool.parent.mkdir()
            tool.write_bytes(b"mock, never executed")
        self.contents = {
            "ophio.exe": pe(), "computer-mcp.exe": pe(),
            "runtime/Helper.DLL": pe(), "runtime/nested/extra.EXE": pe(),
            "INSTALL.md": b"documentation\n", "ophio.apk": b"untouched phone artifact",
        }
        self.write_package()
        self.original = self.source.read_bytes()
        self.metadata = signing.signing_metadata(
            "https://eus.codesigning.azure.net", "test-account", "public-profile", "azure-cli",
        )
        self.calls = []
        self.stdout = contextlib.redirect_stdout(io.StringIO())
        self.stdout.__enter__()
        self.addCleanup(self.stdout.__exit__, None, None, None)

    def write_package(self, extras=()):
        contents = {**self.contents, "SHA256SUMS": signing.checksums(self.contents)}
        with zipfile.ZipFile(self.source, "w") as archive:
            for name, data in contents.items():
                member = zipfile.ZipInfo(f"ophio-1.2.3-x86_64-windows/{name}", (2025, 1, 2, 3, 4, 6))
                member.create_system = 3
                member.external_attr = 0o100644 << 16
                archive.writestr(member, data)
            for member, data in extras:
                archive.writestr(member, data)

    def fake_signtool(self, args, **kwargs):
        self.calls.append(args)
        self.assertEqual(args[0], str(self.signtool))
        self.assertTrue(kwargs["check"])
        self.assertEqual(kwargs["timeout"], 300)
        self.assertEqual(kwargs["stdin"], subprocess.DEVNULL)
        binary = Path(args[-1])
        if args[1] == "sign":
            self.assertEqual(args[2:10], [
                "/v", "/fd", "SHA256", "/tr", signing.TIMESTAMP_URL, "/td", "SHA256", "/dlib",
            ])
            self.assertEqual(args[10], str(self.dlib))
            self.assertEqual(args[11], "/dmdf")
            metadata = json.loads(Path(args[12]).read_text(encoding="utf-8"))
            self.assertEqual(metadata, self.metadata)
            binary.write_bytes(binary.read_bytes() + b"mock signature and timestamp")
        else:
            self.assertEqual(args[1:7], ["verify", "/pa", "/all", "/tw", "/v", str(binary)])
            self.assertTrue(binary.read_bytes().endswith(b"mock signature and timestamp"))
        return subprocess.CompletedProcess(args, 0)

    def run_sign(self):
        with mock.patch.object(signing.subprocess, "run", side_effect=self.fake_signtool):
            return signing.sign_package(self.source, self.output, self.signtool, self.dlib, self.metadata)

    def test_signs_every_exe_dll_and_verifies_final_archive_bytes(self):
        self.assertEqual(self.run_sign(), 4)
        self.assertEqual([call[1] for call in self.calls], ["sign"] * 4 + ["verify"] * 4)
        self.assertEqual(self.source.read_bytes(), self.original)
        package, signed, entries = signing.read_package(self.output)
        self.assertEqual(package, "ophio-1.2.3-x86_64-windows")
        for name, original in self.contents.items():
            expected = original + b"mock signature and timestamp" if signing.is_binary(name) else original
            self.assertEqual(signed[name], expected)
            self.assertEqual(entries[name].date_time, (2025, 1, 2, 3, 4, 6))
        self.assertEqual(signed["SHA256SUMS"], signing.checksums(signed))
        self.assertNotEqual(signed["SHA256SUMS"], signing.checksums(self.contents))
        self.assertFalse(Path(self.calls[0][-1]).exists(), "temporary binaries must be removed")
        self.assertFalse(Path(self.calls[0][-2]).exists(), "temporary metadata must be removed")

    def test_output_is_not_published_until_all_verification_passes(self):
        def check_visibility(args, **kwargs):
            self.assertFalse(self.output.exists())
            return self.fake_signtool(args, **kwargs)
        with mock.patch.object(signing.subprocess, "run", side_effect=check_visibility):
            signing.sign_package(self.source, self.output, self.signtool, self.dlib, self.metadata)
        self.assertTrue(self.output.is_file())

    def test_sign_and_verify_errors_and_warnings_never_publish(self):
        for command, code, position in (("sign", 1, 1), ("sign", 2, 3), ("verify", 1, 1), ("verify", 2, 4)):
            with self.subTest(command=command, code=code, position=position):
                counter = 0
                def fail(args, **kwargs):
                    nonlocal counter
                    if args[1] == command:
                        counter += 1
                        if counter == position:
                            raise subprocess.CalledProcessError(code, args)
                    return self.fake_signtool(args, **kwargs)
                with mock.patch.object(signing.subprocess, "run", side_effect=fail):
                    with self.assertRaises(subprocess.CalledProcessError):
                        signing.sign_package(self.source, self.output, self.signtool, self.dlib, self.metadata)
                self.assertFalse(self.output.exists())
                self.assertEqual(self.source.read_bytes(), self.original)

    def test_timeout_never_publishes(self):
        with mock.patch.object(signing.subprocess, "run", side_effect=subprocess.TimeoutExpired("signtool", 300)):
            with self.assertRaises(subprocess.TimeoutExpired):
                signing.sign_package(self.source, self.output, self.signtool, self.dlib, self.metadata)
        self.assertFalse(self.output.exists())

    def test_existing_and_original_outputs_are_never_overwritten(self):
        self.output.write_bytes(b"existing artifact")
        for destination in (self.output, self.source):
            with mock.patch.object(signing.subprocess, "run") as tool:
                with self.assertRaisesRegex(ValueError, "separate output"):
                    signing.sign_package(self.source, destination, self.signtool, self.dlib, self.metadata)
                tool.assert_not_called()
        self.assertEqual(self.output.read_bytes(), b"existing artifact")
        self.assertEqual(self.source.read_bytes(), self.original)

    def test_output_race_preserves_other_process_artifact(self):
        def race(args, **kwargs):
            self.output.write_bytes(b"other process")
            return self.fake_signtool(args, **kwargs)
        with mock.patch.object(signing.subprocess, "run", side_effect=race):
            with self.assertRaises(FileExistsError):
                signing.sign_package(self.source, self.output, self.signtool, self.dlib, self.metadata)
        self.assertEqual(self.output.read_bytes(), b"other process")
        self.assertEqual(set(self.root.glob("*.zip")), {self.source, self.output})

    def test_unsafe_archive_names_rejected_before_signing(self):
        for name in (
            "../escape.exe", "/absolute.exe", "pkg/../escape.dll", "pkg/a\\b.dll",
            "pkg/file:stream.dll", "pkg/NUL.exe", "pkg/COM1.dll", "pkg/file. /x.dll",
            "pkg//empty.dll", "pkg/trailing./x.dll", "different-root/file.exe",
            "ophio-1.2.3-x86_64-windows/OPHIO.EXE",
        ):
            with self.subTest(name=name):
                self.write_package([(name, pe())])
                with mock.patch.object(signing.subprocess, "run") as tool:
                    with self.assertRaises(ValueError):
                        signing.sign_package(self.source, self.output, self.signtool, self.dlib, self.metadata)
                    tool.assert_not_called()
                self.assertFalse(self.output.exists())

    def test_windows_publication_uses_no_replace_rename_without_hardlinks(self):
        with (
            mock.patch.object(signing.os, "name", "nt"),
            mock.patch.object(signing.os, "rename") as rename,
            mock.patch.object(signing.os, "link", side_effect=AssertionError("hardlinks unavailable")),
        ):
            signing.publish_no_replace(self.source, self.output)
        rename.assert_called_once_with(self.source, self.output)

    def test_unsupported_output_publication_refused_before_any_signing(self):
        with (
            mock.patch.object(signing, "publish_no_replace", side_effect=OSError("unsupported filesystem")),
            mock.patch.object(signing.subprocess, "run") as tool,
        ):
            with self.assertRaisesRegex(ValueError, "cannot safely publish"):
                signing.sign_package(self.source, self.output, self.signtool, self.dlib, self.metadata)
            tool.assert_not_called()
        self.assertEqual(self.source.read_bytes(), self.original)
        self.assertFalse(self.output.exists())
        self.assertFalse(list(self.root.glob(".ophio-publication-check-*")))

    def test_preflight_uses_only_sibling_files_in_exact_output_parent(self):
        publish = signing.publish_no_replace
        probed = []
        def inspect(source, destination):
            self.assertEqual(source.parent, self.output.parent)
            self.assertEqual(destination.parent, self.output.parent)
            self.assertEqual(source.read_bytes(), b"publication check")
            self.assertFalse(destination.exists())
            probed.extend((source, destination))
            publish(source, destination)
        with (
            mock.patch.object(signing.tempfile, "TemporaryDirectory", side_effect=AssertionError("no subdirectory permission")),
            mock.patch.object(signing.tempfile, "mkstemp", wraps=signing.tempfile.mkstemp) as create,
            mock.patch.object(signing, "publish_no_replace", side_effect=inspect),
        ):
            signing.preflight_output(self.output)
        self.assertEqual(create.call_count, 1)
        self.assertEqual(create.call_args.kwargs["dir"], self.output.parent)
        self.assertEqual(len(probed), 2)
        self.assertTrue(all(not path.exists() for path in probed))
        self.assertFalse(self.output.exists())

    def test_failed_preflight_write_closes_handle_and_removes_sibling(self):
        with (
            mock.patch.object(signing.os, "write", side_effect=OSError("disk full")),
            mock.patch.object(signing.os, "close", wraps=signing.os.close) as close,
            mock.patch.object(signing, "publish_no_replace") as publish,
        ):
            with self.assertRaisesRegex(ValueError, "cannot safely publish"):
                signing.preflight_output(self.output)
            close.assert_called_once()
            publish.assert_not_called()
        self.assertFalse(list(self.root.glob(".ophio-publication-check-*")))

    def test_preflight_collision_preserves_other_process_file_and_cleans_source(self):
        publish = signing.publish_no_replace
        probed = []
        def race(source, destination):
            probed.extend((source, destination))
            destination.write_bytes(b"other process file")
            publish(source, destination)
        with mock.patch.object(signing, "publish_no_replace", side_effect=race):
            with self.assertRaisesRegex(ValueError, "cannot safely publish"):
                signing.preflight_output(self.output)
        source, destination = probed
        self.assertFalse(source.exists())
        self.assertEqual(destination.read_bytes(), b"other process file")
        self.assertFalse(self.output.exists())

    def test_output_publication_uses_preflight_before_signing(self):
        with mock.patch.object(signing, "publish_no_replace", wraps=signing.publish_no_replace) as publish:
            def check_preflight(args, **kwargs):
                self.assertEqual(publish.call_count, 1, "preflight must precede every signing call")
                return self.fake_signtool(args, **kwargs)
            with mock.patch.object(signing.subprocess, "run", side_effect=check_preflight):
                signing.sign_package(self.source, self.output, self.signtool, self.dlib, self.metadata)
            self.assertEqual(publish.call_count, 2, "preflight and final publication must both run")

    def test_native_publication_never_replaces_an_existing_destination(self):
        self.output.write_bytes(b"existing destination")
        with self.assertRaises(FileExistsError):
            signing.publish_no_replace(self.source, self.output)
        self.assertEqual(self.output.read_bytes(), b"existing destination")
        self.assertEqual(self.source.read_bytes(), self.original)

    def test_reserved_windows_aliases_refused_before_any_signing(self):
        root = "ophio-1.2.3-x86_64-windows"
        for name in (
            "COM¹.dll", "com².dll", "COM³.dll", "LPT¹.dll", "lpt².dll", "LPT³.dll",
            "CON .dll", "con  .dll", "NUL .dll", "COM¹ .dll", "CONIN$.dll", "conout$.dll",
        ):
            with self.subTest(name=name):
                self.write_package([(f"{root}/{name}", pe())])
                with mock.patch.object(signing.subprocess, "run") as tool:
                    with self.assertRaisesRegex(ValueError, "Unsafe archive"):
                        signing.sign_package(self.source, self.output, self.signtool, self.dlib, self.metadata)
                    tool.assert_not_called()
        for name in ("console.dll", "COM10.dll", "lpt10.dll"):
            self.assertFalse(signing.reserved_on_windows(name))

    def test_file_parent_collisions_refused_in_both_orders_and_cases(self):
        original = list(self.contents.items())
        for child in ("ophio.exe/child.txt", "OPHIO.EXE/child.txt"):
            for child_first in (False, True):
                with self.subTest(child=child, child_first=child_first):
                    items = original + [(child, b"not a directory")]
                    self.contents = dict(reversed(items) if child_first else items)
                    self.write_package()
                    with mock.patch.object(signing.subprocess, "run") as tool:
                        with self.assertRaisesRegex(ValueError, "file/directory collision"):
                            signing.sign_package(self.source, self.output, self.signtool, self.dlib, self.metadata)
                        tool.assert_not_called()
                    self.assertFalse(self.output.exists())

    def test_symlink_rejected(self):
        link = zipfile.ZipInfo("ophio-1.2.3-x86_64-windows/link.dll")
        link.create_system = 3
        link.external_attr = (stat.S_IFLNK | 0o777) << 16
        self.write_package([(link, b"somewhere")])
        with self.assertRaisesRegex(ValueError, "Unsafe archive"):
            signing.read_package(self.source)

    def test_crc_corruption_and_bad_checksums_rejected(self):
        self.contents["SHA256SUMS"] = b"not matching"
        # write_package always builds its own sums, so replace one payload in raw ZIP bytes.
        raw = self.source.read_bytes().replace(b"documentation\n", b"DOCUMENTATION\n")
        self.source.write_bytes(raw)
        with self.assertRaises(zipfile.BadZipFile):
            signing.read_package(self.source)
        self.write_package()
        # A valid ZIP CRC is not enough: require the package manifest too.
        with zipfile.ZipFile(self.source) as archive:
            items = [(entry, archive.read(entry)) for entry in archive.infolist()]
        with zipfile.ZipFile(self.source, "w") as archive:
            for entry, data in items:
                archive.writestr(entry, b"wrong sums" if entry.filename.endswith("/SHA256SUMS") else data)
        with self.assertRaisesRegex(ValueError, "SHA256SUMS"):
            signing.read_package(self.source)

    def test_non_pe_binary_and_missing_companion_rejected(self):
        self.contents["runtime/Helper.DLL"] = b"not a binary"
        self.write_package()
        with self.assertRaisesRegex(ValueError, "Expected a PE"):
            signing.read_package(self.source)
        del self.contents["ophio.exe"]
        self.write_package()
        with self.assertRaisesRegex(ValueError, "packaged Windows"):
            signing.read_package(self.source)

    def test_archive_size_limit(self):
        with mock.patch.object(signing, "MAX_MEMBER_BYTES", 32):
            with self.assertRaisesRegex(ValueError, "size limits"):
                signing.read_package(self.source)

    def test_credential_selection_never_enables_interactive_fallback(self):
        for selection, credential in signing.CREDENTIALS.items():
            metadata = signing.signing_metadata("https://weu.codesigning.azure.net/", "account", "profile", selection)
            exclusions = metadata["ExcludeCredentials"]
            self.assertNotIn(credential, exclusions)
            self.assertEqual(set(exclusions), set(signing.ALL_CREDENTIALS) - {credential})
            self.assertIn("InteractiveBrowserCredential", exclusions)

    def test_bad_regional_endpoints_and_empty_profile_rejected(self):
        for endpoint in (
            "http://eus.codesigning.azure.net", "https://example.com",
            "https://eus.codesigning.azure.net.attacker.example", "https://user@eus.codesigning.azure.net",
            "https://eus.codesigning.azure.net/path", "https://eus.codesigning.azure.net/?query",
        ):
            with self.subTest(endpoint=endpoint), self.assertRaises(ValueError):
                signing.signing_metadata(endpoint, "account", "profile", "azure-cli")
        with self.assertRaises(ValueError):
            signing.signing_metadata("https://eus.codesigning.azure.net", "account", " ", "azure-cli")

    def test_non_windows_cli_rejects_without_invoking_tools(self):
        with mock.patch.object(signing.sys, "platform", "linux"), mock.patch.object(signing.subprocess, "run") as run:
            with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
                signing.main([str(self.source), "--signtool", str(self.signtool), "--verify-only"])
            run.assert_not_called()

    def test_verify_only_never_signs_or_creates_output(self):
        self.run_sign()
        self.calls.clear()
        with mock.patch.object(signing.sys, "platform", "win32"), mock.patch.object(signing.subprocess, "run", side_effect=self.fake_signtool):
            signing.main([str(self.output), "--signtool", str(self.signtool), "--verify-only"])
        self.assertEqual([call[1] for call in self.calls], ["verify"] * 4)
        self.assertEqual(self.source.read_bytes(), self.original)

    def test_invoke_checks_real_process_warning_code(self):
        # Exercise subprocess.check=True, rather than only mocking its exception.
        with self.assertRaises(subprocess.CalledProcessError) as failure:
            signing.invoke([sys.executable, "-I", "-c", "raise SystemExit(2)"])
        self.assertEqual(failure.exception.returncode, 2)

    def test_failed_output_copy_leaves_no_partial_archive(self):
        with (
            mock.patch.object(signing.subprocess, "run", side_effect=self.fake_signtool),
            mock.patch.object(signing.shutil, "copyfileobj", side_effect=OSError("disk full")),
        ):
            with self.assertRaisesRegex(OSError, "disk full"):
                signing.sign_package(self.source, self.output, self.signtool, self.dlib, self.metadata)
        self.assertEqual(set(self.root.glob("*.zip")), {self.source})
        self.assertEqual(self.source.read_bytes(), self.original)

    def test_sign_cli_assembles_all_options(self):
        with (
            mock.patch.object(signing.sys, "platform", "win32"),
            mock.patch.object(signing.subprocess, "run", side_effect=self.fake_signtool),
        ):
            signing.main([
                str(self.source), "--output", str(self.output), "--signtool", str(self.signtool),
                "--dlib", str(self.dlib), "--endpoint", self.metadata["Endpoint"],
                "--account", self.metadata["CodeSigningAccountName"],
                "--profile", self.metadata["CertificateProfileName"],
            ])
        self.assertTrue(self.output.is_file())
        self.assertEqual([call[1] for call in self.calls], ["sign"] * 4 + ["verify"] * 4)

    def test_sign_cli_requires_all_inputs_before_any_azure_call(self):
        for arguments in ([str(self.source), "--signtool", str(self.signtool)], [
            str(self.source), "--signtool", str(self.signtool), "--verify-only", "--account", "unexpected",
        ]):
            with mock.patch.object(signing.sys, "platform", "win32"), mock.patch.object(signing.subprocess, "run") as run:
                with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
                    signing.main(arguments)
                run.assert_not_called()

    def test_unsigned_personal_packaging_never_invokes_signtool(self):
        repo, target = self.root / "repo", self.root / "target"
        release = target / "x86_64-pc-windows-gnullvm/release"
        licenses = target / "packaging/licenses/windows"
        release.mkdir(parents=True)
        licenses.mkdir(parents=True)
        (repo / "packaging/windows").mkdir(parents=True)
        (repo / "LICENSE").write_bytes(b"license")
        (repo / "packaging/windows/INSTALL.md").write_bytes(b"install")
        for name in ("NOTICE", "THIRD-PARTY-NOTICES.txt", "sbom.cdx.json", "license-report.json"):
            (licenses / name).write_bytes(b"qualification fixture")
        for name in ("ophio.exe", "computer-mcp.exe"):
            (release / name).write_bytes(pe())
        with (
            mock.patch.object(packaging, "require_licences"),
            mock.patch.object(packaging, "identity", return_value=("Ophio", "ophio", "1.2.3")),
            mock.patch.object(packaging, "phone_app_bytes", return_value=None),
            mock.patch.object(packaging, "require_executable_trust"),
            mock.patch.object(packaging, "manifest", return_value="fixture"),
            mock.patch.object(signing.subprocess, "run") as run,
            mock.patch.dict(os.environ, {"OPHIO_PERSONAL_SIGNING": "1"}),
        ):
            output = packaging.assemble(repo, target, 1700000000)
            run.assert_not_called()
        _, contents, _ = signing.read_package(output)
        self.assertEqual(contents["ophio.exe"], pe())
        self.assertEqual(contents["computer-mcp.exe"], pe())


if __name__ == "__main__":
    unittest.main()
