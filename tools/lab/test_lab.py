# SPDX-License-Identifier: Apache-2.0
"""Host isolation and publisher-verification regression checks; no VM required."""

import hashlib
import io
import os
import json
from pathlib import Path
import subprocess
import sys
import re
import tarfile
import tempfile
import unittest
from unittest.mock import call as mock_call, patch

import lab

# Independent expectations: shrinking the implementation list must fail tests.
EXPECTED_TOOLS = ("apksigner", "zipalign", "lib/apksigner.jar", "lib64/libc++.so")

COMMIT = "0123456789abcdef" * 2 + "01234567"
CLEAR_PHONE_APP = ["rm", "-rf", "lab/package-phone.apk", "lab/android-build-tools"]


class PersonHistoryTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.project = self.root / "a project-é_字"
        self.project.mkdir()
        self.agent = self.root / "claude.py"
        repository = Path(__file__).resolve().parents[2]
        script = (repository / "tools/lab/guest/person.sh").read_text(encoding="utf-8")
        # Exercise the actual transformation, never the root-only guest setup.
        self.transform = script.split("<<'PY'\n", 1)[1].split("\nPY\n", 1)[0]
        self.source = (repository / "crates/harness-claude/tests/fake_claude.py").read_text(encoding="utf-8")

    def transform_fixture(self, source):
        fixture = self.root / "fixture.py"
        fixture.write_text(source, encoding="utf-8")
        return subprocess.run(
            [sys.executable, "-c", self.transform, str(fixture), str(self.agent)],
            capture_output=True, text=True, encoding="utf-8", timeout=10,
        )

    def test_saves_in_the_config_override_and_default_home(self):
        result = self.transform_fixture(self.source)
        self.assertEqual(result.returncode, 0, result.stderr)
        home = self.root / "home"
        home.mkdir()
        for override in (None, self.root / "custom-config"):
            with self.subTest(override=override):
                env = {"HOME": str(home), "PATH": os.defpath}
                if override is not None:
                    env["CLAUDE_CONFIG_DIR"] = str(override)
                message = {"type": "user", "message": {
                    "content": [{"type": "text", "text": "complete"}]
                }}
                result = subprocess.run(
                    [sys.executable, str(self.agent)], cwd=self.project, env=env,
                    input=json.dumps(message) + "\n", capture_output=True,
                    text=True, encoding="utf-8", timeout=10,
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                config = override if override is not None else home / ".claude"
                slug = re.sub(r"[^a-zA-Z0-9]", "-", str(self.project))
                saved = config / "projects" / slug / "native-session.jsonl"
                self.assertEqual([json.loads(line) for line in saved.read_text(encoding="utf-8").splitlines()], [message])
                self.assertFalse((self.project / "stdin.jsonl").exists())
                self.assertFalse((self.project / "argv.json").exists())

    def test_changed_or_ambiguous_fixture_shape_fails_before_installing(self):
        marker = '        Path("fixture.pid").write_text(str(os.getpid()))\n'
        self.assertEqual(self.source.count(marker), 1)
        for replacement in ("", marker + marker):
            result = self.transform_fixture(self.source.replace(marker, replacement))
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("fake_claude.py changed", result.stderr)
            self.assertFalse(self.agent.exists())


class IsolationTests(unittest.TestCase):
    def test_wake_refuses_absent_or_provisioning_guests(self):
        for state in (None, {"restricted": False}):
            with (
                patch.object(lab, "state", return_value=state),
                patch.object(lab, "qmp") as qmp,
            ):
                with self.assertRaises(lab.LabError):
                    lab.wake("gnome")
                qmp.assert_not_called()

    def test_wake_sends_only_motion_to_the_selected_guest(self):
        with (
            patch.object(lab, "state", return_value={"restricted": True}),
            patch.object(lab, "qmp") as qmp,
        ):
            lab.wake("gnome")
        self.assertEqual(qmp.call_count, 2)
        for call in qmp.call_args_list:
            self.assertEqual(call.args[0], "gnome")
            (command,) = call.args[1]
            self.assertEqual(command["execute"], "input-send-event")
            for event in command["arguments"]["events"]:
                self.assertEqual(event["type"], "abs")
                self.assertIn(event["data"]["axis"], ("x", "y"))
                self.assertTrue(0 <= event["data"]["value"] <= 32767)

    def test_cached_backing_image_is_never_replaced_by_a_new_publisher_release(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            folder = root / "images/arch"
            folder.mkdir(parents=True)
            image = folder / "image.qcow2"
            image.write_bytes(b"verified immutable base")
            (folder / "verified.json").write_text(
                json.dumps(
                    {
                        "url": "https://publisher.invalid/latest/image.qcow2",
                        "sha256": lab.digest(image),
                    }
                ),
                encoding="utf-8",
            )
            with (
                patch.object(lab, "LAB", root),
                patch.object(lab, "download") as download,
            ):
                lab.fetch("arch")
                download.assert_not_called()
                image.write_bytes(b"changed")
                with self.assertRaises(lab.LabError):
                    lab.fetch("arch")
                download.assert_not_called()
                self.assertEqual(image.read_bytes(), b"changed")

    def test_moved_backing_image_is_verified_before_relocation(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            folder = root / "gnome"
            folder.mkdir()
            image = root / "images/ubuntu/base.qcow2"
            image.parent.mkdir(parents=True)
            image.write_bytes(b"original verified image")
            (folder / "image.json").write_text(
                json.dumps(
                    {
                        "url": "https://publisher.invalid/base.qcow2",
                        "sha256": lab.digest(image),
                    }
                ),
                encoding="utf-8",
            )
            result = type(
                "Result",
                (),
                {
                    "stdout": json.dumps(
                        {
                            "backing-filename": "/old-worktree/base.qcow2",
                            "backing-filename-format": "qcow2",
                        }
                    )
                },
            )()
            with (
                patch.object(lab, "LAB", root),
                patch.object(lab, "run", return_value=result) as run,
            ):
                lab.restore_backing_path("gnome")
                self.assertEqual(
                    run.call_args.args[0],
                    [
                        "qemu-img",
                        "rebase",
                        "-u",
                        "-f",
                        "qcow2",
                        "-F",
                        "qcow2",
                        "-b",
                        image,
                        folder / "disk.qcow2",
                    ],
                )
                run.reset_mock()
                image.write_bytes(b"different base")
                with self.assertRaises(lab.LabError):
                    lab.restore_backing_path("gnome")
                self.assertEqual(run.call_count, 1)

    def test_ssh_never_connects_to_a_different_running_guest(self):
        with (
            patch.object(lab, "state", return_value=None),
            patch.object(lab, "run") as run,
        ):
            with self.assertRaises(lab.LabError):
                lab.guest("gnome", ["true"])
            run.assert_not_called()

    def test_environment_removes_desktop_endpoints(self):
        with patch.dict(os.environ, {key: "do-not-use" for key in lab.DISPLAY_ENV}):
            self.assertFalse(set(lab.DISPLAY_ENV) & set(lab.environment()))

    def test_qualification_has_only_two_loopback_forwards_and_no_display(self):
        with (
            patch.object(lab, "firmware", return_value=Path("/firmware")),
            patch.dict(os.environ, clear=True),
        ):
            args = lab.qemu_args("hyprland", lab.forwards())
        self.assertEqual(args[args.index("-display") + 1], "none")
        self.assertIn("-enable-kvm", args)
        self.assertNotIn("-vnc", args)
        self.assertNotIn("-spice", args)
        self.assertEqual(
            args[args.index("-netdev") + 1],
            "user,id=labnet,restrict=on,hostfwd=tcp:127.0.0.1:47222-:22,"
            "hostfwd=tcp:127.0.0.1:47112-:47112",
        )
        self.assertLessEqual(int(args[args.index("-smp") + 1]), 8)
        self.assertLessEqual(int(args[args.index("-m") + 1]), 8192)

    def test_forwards_and_serial_default_to_the_original_lab_endpoints(self):
        with patch.dict(os.environ, clear=True):
            self.assertEqual(
                lab.forwards(),
                {"ssh": 47222, "ophio": 47112, "guest_ophio": 47112},
            )
            self.assertEqual(lab.adb_serial(), "emulator-5562")

    def test_a_concurrent_lab_selects_its_own_forwards_and_emulator(self):
        with patch.dict(
            os.environ,
            {
                "LAB_SSH_PORT": "47224",
                "LAB_OPHIO_PORT": "47114",
                "LAB_GUEST_OPHIO_PORT": "47100",
                "LAB_ADB_SERIAL": "emulator-5564",
            },
            clear=True,
        ):
            ports = lab.forwards()
            self.assertEqual(lab.adb_serial(), "emulator-5564")
            with patch.object(lab, "firmware", return_value=Path("/firmware")):
                args = lab.qemu_args("gnome", ports)
        self.assertEqual(
            args[args.index("-netdev") + 1],
            "user,id=labnet,restrict=on,hostfwd=tcp:127.0.0.1:47224-:22,"
            "hostfwd=tcp:127.0.0.1:47114-:47100",
        )

    def test_the_phone_pairs_through_the_forward_a_fresh_guest_recorded(self):
        module = (Path(__file__).parent / "offer.mjs").resolve().as_uri()
        script = f"""
          import fs from "node:fs";
          import {{ guestAddress }} from {json.dumps(module)};
          const address = file => {{
            try {{ return guestAddress(JSON.parse(fs.readFileSync(file, "utf8"))); }}
            catch {{ return null; }}
          }};
          console.log(JSON.stringify(process.argv.slice(1).map(address)));
        """
        with tempfile.TemporaryDirectory() as directory:
            folder = Path(directory)
            with patch.dict(
                os.environ,
                {"LAB_OPHIO_PORT": "47116", "LAB_GUEST_OPHIO_PORT": "47100"},
                clear=True,
            ):
                lab.record_process(folder, 1234, "5678", True, lab.forwards())
            stale = folder / "stale.json"
            stale.write_text(
                json.dumps({"forwards": {"companion": 47116}}), encoding="utf-8"
            )
            result = subprocess.run(
                [
                    "node",
                    "--input-type=module",
                    "-e",
                    script,
                    "--",
                    str(folder / "process.json"),
                    str(stale),
                ],
                check=True,
                capture_output=True,
                text=True,
                encoding="utf-8",
            )
        # A record without the forward is refused, never paired as "undefined".
        self.assertEqual(json.loads(result.stdout), ["10.0.2.2:47116", None])

    def test_invalid_forwards_or_serials_are_refused(self):
        for variables in (
            {"LAB_SSH_PORT": "22"},
            {"LAB_OPHIO_PORT": "70000"},
            {"LAB_GUEST_OPHIO_PORT": "47100 "},
            {"LAB_SSH_PORT": "47300", "LAB_OPHIO_PORT": "47300"},
        ):
            with patch.dict(os.environ, variables, clear=True):
                with self.assertRaises(lab.LabError):
                    lab.forwards()
        for serial in (
            "emulator-5563",
            "emulator-5600",
            "R5CT1234567",
            "emulator-5564x",
        ):
            with patch.dict(os.environ, {"LAB_ADB_SERIAL": serial}, clear=True):
                with self.assertRaises(lab.LabError):
                    lab.adb_serial()

    def test_guest_commands_use_the_ssh_forward_the_guest_started_with(self):
        record = {"restricted": True, "forwards": {"ssh": 47224}}
        with (
            patch.object(lab, "state", return_value=record),
            patch.object(lab, "run") as run,
            patch.dict(os.environ, clear=True),
        ):
            lab.guest("gnome", ["true"])
        args = [str(arg) for arg in run.call_args.args[0]]
        self.assertEqual(args[args.index("-p") + 1], "47224")

    def test_release_copies_only_a_built_archive_to_a_restricted_desktop(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            built = root / "release/0123456789ab/ophio-0.1.0-x86_64-linux.tar.gz"
            built.parent.mkdir(parents=True)
            built.write_bytes(b"archive")
            outside = root / "ophio-0.1.0-x86_64-linux.tar.gz"
            outside.write_bytes(b"archive")
            with (
                patch.object(lab, "LAB", root),
                patch.object(lab, "copy_file") as copy,
                patch.object(lab, "guest") as guest,
            ):
                for target, archive, record in (
                    ("gnome", outside, {"restricted": True}),
                    ("gnome", built, {"restricted": False}),
                    ("gnome", built, None),
                    ("builder", built, {"restricted": True}),
                ):
                    with patch.object(lab, "state", return_value=record):
                        with self.assertRaises(lab.LabError):
                            lab.release(target, archive)
                copy.assert_not_called()
                guest.assert_not_called()
                with patch.object(lab, "state", return_value={"restricted": True}):
                    lab.release("gnome", built)
            copy.assert_called_once_with("gnome", built, "lab/incoming.tar.gz")
            guest.assert_called_once_with(
                "gnome",
                [
                    "sudo",
                    "bash",
                    "lab/person.sh",
                    "receive",
                    "ophio-0.1.0-x86_64-linux.tar.gz",
                ],
            )

    def test_archives_are_built_only_in_the_builder_and_users_made_only_on_desktops(
        self,
    ):
        with (
            patch.object(lab, "state", return_value={"restricted": False}),
            patch.object(lab, "run") as run,
            patch.object(lab, "guest") as guest,
        ):
            for target in ("gnome", "hyprland", "plasma"):
                with self.assertRaises(lab.LabError):
                    lab.archive(target, "HEAD")
            with self.assertRaises(lab.LabError):
                lab.newuser("builder")
        run.assert_not_called()
        guest.assert_not_called()
        with (
            patch.object(lab, "state", return_value={"restricted": True}),
            patch.object(lab, "guest") as guest,
        ):
            with self.assertRaises(lab.LabError):
                lab.newuser("gnome")
        guest.assert_not_called()
        self.assertEqual(lab.DISTROS["builder"], "ubuntu")
        self.assertEqual(set(lab.DISTROS), set(lab.TARGETS))

    def build_archive(self, root, phone_app=None, build_tools=None, bundled=None, failure=None):
        """Run archive() with a fake builder that packs `bundled` as the phone app."""
        name = "ophio-0.1.0-x86_64-linux"
        made = root / "made.tar.gz"
        with tarfile.open(made, "w:gz") as archive:
            for member, data in (("ophio", b"elf"), ("ophio.apk", bundled)):
                if data is not None:
                    entry = tarfile.TarInfo(f"{name}/{member}")
                    entry.size = len(data)
                    archive.addfile(entry, io.BytesIO(data))
        if failure == "malformed":
            made.write_bytes(b"not a tar archive")
        sha = hashlib.sha256(made.read_bytes()).hexdigest()

        def run(args, **kwargs):
            if args[:2] == ["git", "rev-parse"]:
                return subprocess.CompletedProcess(args, 0, COMMIT + "\n")
            if args[:2] == ["git", "log"]:
                return subprocess.CompletedProcess(args, 0, "1700000000\n")
            return subprocess.CompletedProcess(args, 0, "")

        def guest(target, args, stdout=None, **kwargs):
            if args[:2] == ["bash", "lab/package.sh"]:
                stdout.write(f"{sha}  {name}.tar.gz\n".encode())
            elif args[0] == "cat":
                self.assertFalse((root / "lab/release/0123456789ab/build.json").exists())
                if failure in ("interrupted", "checksum"):
                    stdout.write(b"partial download")
                    if failure == "interrupted":
                        raise subprocess.CalledProcessError(255, args)
                else:
                    stdout.write(made.read_bytes())

        with (
            patch.object(lab, "ROOT", root),
            patch.object(lab, "LAB", root / "lab"),
            patch.object(lab, "EVIDENCE", root / "evidence"),
            patch.object(lab, "state", return_value={"restricted": False}),
            patch.object(lab, "run", side_effect=run),
            patch.object(lab, "guest", side_effect=guest) as guests,
            patch.object(lab, "copy_file") as copies,
        ):
            guests.attach_mock(copies, "copy")
            lab.archive("builder", "HEAD", phone_app, build_tools)
        destination = root / "lab/release/0123456789ab"
        return guests, copies, destination

    def android_tools(self, root):
        tools = root / "sdk/build-tools/37.0.0"
        for name in EXPECTED_TOOLS:
            (tools / name).parent.mkdir(parents=True, exist_ok=True)
            (tools / name).write_bytes(name.encode())
        return tools

    def test_public_archive_carries_the_given_phone_app_checked_in_the_builder(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            app = root / "signed.apk"
            app.write_bytes(b"signed app")
            tools = self.android_tools(root)
            guests, copies, destination = self.build_archive(
                root, app, tools, bundled=b"signed app"
            )
            commands = [call.args[1] for call in guests.call_args_list]
            build_command = ["bash", "lab/package.sh", "1700000000", COMMIT, "public"]
            self.assertLess(
                commands.index(CLEAR_PHONE_APP), commands.index(build_command)
            )
            sent = {call.args[2]: call.args[1] for call in copies.call_args_list}
            phone_app = Path(sent["lab/package-phone.apk"])
            self.assertEqual(phone_app.read_bytes(), b"signed app")
            self.assertTrue(phone_app.is_relative_to(root / "lab"))
            self.assertEqual(
                set(sent),
                {"lab/package-source.tar", "lab/package-vendor.tar", "lab/package.sh",
                 "lab/package-phone.apk", *("lab/android-build-tools/" + name for name in EXPECTED_TOOLS)},
            )
            self.assertLess(
                guests.mock_calls.index(mock_call("builder", CLEAR_PHONE_APP)),
                guests.mock_calls.index(mock_call.copy("builder", phone_app, "lab/package-phone.apk")),
            )
            for name in EXPECTED_TOOLS:
                staged = Path(sent["lab/android-build-tools/" + name])
                # Staged inside the lab, never sent straight from the SDK.
                self.assertTrue(staged.is_relative_to(root / "lab"))
                self.assertEqual(staged.read_bytes(), name.encode())
            tools_folder = "lab/android-build-tools/"
            self.assertIn(
                ["chmod", "700", tools_folder + "apksigner", tools_folder + "zipalign"],
                commands,
            )
            build = json.loads((destination / "build.json").read_text(encoding="utf-8"))
            self.assertTrue(build["public"])
            self.assertEqual(build["revision"], COMMIT)
            expected = hashlib.sha256(b"signed app").hexdigest()
            self.assertEqual(build["phone_app_sha256"], expected)
            self.assertEqual(
                build["android_build_tools"],
                {name: hashlib.sha256(name.encode()).hexdigest() for name in EXPECTED_TOOLS},
            )

    def test_public_archive_without_the_given_phone_app_is_not_kept(self):
        for bundled in (None, b"another app"):
            with (
                self.subTest(bundled=bundled),
                tempfile.TemporaryDirectory() as directory,
            ):
                root = Path(directory)
                app = root / "signed.apk"
                app.write_bytes(b"signed app")
                tools = self.android_tools(root)
                destination = root / "lab/release/0123456789ab"
                destination.mkdir(parents=True)
                (destination / "build.json").write_text("{}", encoding="utf-8")
                with self.assertRaisesRegex(lab.LabError, "does not carry"):
                    self.build_archive(root, app, tools, bundled=bundled)
                self.assertFalse(list(destination.glob("*.tar.gz")))
                self.assertFalse((destination / "build.json").exists())

    def test_failed_retrieval_leaves_neither_archive_nor_stale_record(self):
        for failure in ("interrupted", "checksum", "malformed"):
            for previous in (False, True):
                with (
                    self.subTest(failure=failure, previous=previous),
                    tempfile.TemporaryDirectory() as directory,
                ):
                    root = Path(directory)
                    app = root / "signed.apk"
                    app.write_bytes(b"signed app")
                    destination = root / "lab/release/0123456789ab"
                    destination.mkdir(parents=True)
                    if previous:
                        (destination / "build.json").write_text("{}", encoding="utf-8")
                        (destination / "ophio-0.1.0-x86_64-linux.tar.gz").write_bytes(b"old archive")
                    with self.assertRaises((lab.LabError, subprocess.CalledProcessError)):
                        self.build_archive(
                            root, app, self.android_tools(root), bundled=b"signed app",
                            failure=failure,
                        )
                    self.assertEqual(list(destination.iterdir()), [])

    def test_public_archive_inputs_are_refused_before_any_build(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            app = root / "signed.apk"
            app.write_bytes(b"signed app")
            link = root / "link.apk"
            link.symlink_to(app)
            tools = self.android_tools(root)
            partial = root / "partial"
            partial.mkdir()
            (partial / "apksigner").write_bytes(b"")
            for phone_app, build_tools in (
                (app, None),
                (None, tools),
                (link, tools),
                (root / "missing.apk", tools),
                (tools / "apksigner", tools),
                (app, partial),
            ):
                with (
                    self.subTest(phone_app=phone_app, build_tools=build_tools),
                    patch.object(lab, "state", return_value={"restricted": False}),
                    patch.object(lab, "run") as run,
                    patch.object(lab, "guest") as guest,
                ):
                    with self.assertRaises((lab.LabError, OSError)):
                        lab.archive("builder", "HEAD", phone_app, build_tools)
                    run.assert_not_called()
                    guest.assert_not_called()

    def test_each_missing_tool_is_refused_before_any_build(self):
        for missing in EXPECTED_TOOLS:
            with self.subTest(missing=missing), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                app = root / "signed.apk"
                app.write_bytes(b"signed app")
                tools = self.android_tools(root)
                (tools / missing).unlink()
                with (
                    patch.object(lab, "state", return_value={"restricted": False}),
                    patch.object(lab, "run") as run,
                    patch.object(lab, "guest") as guest,
                ):
                    with self.assertRaises(lab.LabError):
                        lab.archive("builder", "HEAD", app, tools)
                    run.assert_not_called()
                    guest.assert_not_called()

    def test_ordinary_archive_clears_any_earlier_phone_app_in_the_builder(self):
        with tempfile.TemporaryDirectory() as directory:
            guests, copies, destination = self.build_archive(Path(directory))
            commands = [call.args[1] for call in guests.call_args_list]
            self.assertLess(
                commands.index(CLEAR_PHONE_APP),
                commands.index(["bash", "lab/package.sh", "1700000000", COMMIT]),
            )
            sent = [call.args[2] for call in copies.call_args_list]
            self.assertEqual(set(sent), {"lab/package-source.tar", "lab/package-vendor.tar", "lab/package.sh"})
            build = json.loads((destination / "build.json").read_text(encoding="utf-8"))
            self.assertFalse(build["public"])
            self.assertNotIn("phone_app_sha256", build)

    def test_ssh_cannot_use_personal_configuration_agent_or_forwarding(self):
        args = [str(arg) for arg in lab.ssh_args("hyprland", 47222)]
        self.assertEqual(args[args.index("-F") + 1], "/dev/null")
        for option in (
            "IdentitiesOnly=yes",
            "IdentityAgent=none",
            "ForwardAgent=no",
            "ForwardX11=no",
            "ClearAllForwardings=yes",
        ):
            self.assertIn(option, args)
        self.assertTrue(Path(args[args.index("-i") + 1]).is_relative_to(lab.LAB))

    def test_exact_checksum_selection_rejects_missing_duplicate_or_malformed(self):
        value = "a" * 64
        self.assertEqual(
            lab.expected_digest(value + " *image.qcow2\n", "image.qcow2"), value
        )
        for text in (
            value + " image.qcow2.old",
            value + " image.qcow2\n" + value + " image.qcow2",
            "invalid image.qcow2",
        ):
            with self.assertRaises(lab.LabError):
                lab.expected_digest(text, "image.qcow2")

    def test_wrong_signature_identity_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            result = type(
                "Result", (), {"stdout": "[GNUPG:] VALIDSIG BAD 1 2 3 BAD\n"}
            )()
            with (
                patch.object(lab, "LAB", Path(directory)),
                patch.object(lab, "run", return_value=result),
            ):
                with self.assertRaises(lab.LabError):
                    lab.verify_signature("signature", "image", "key", lab.ARCH_KEY)


if __name__ == "__main__":
    unittest.main()
