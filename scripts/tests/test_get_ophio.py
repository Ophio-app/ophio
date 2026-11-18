# SPDX-License-Identifier: Apache-2.0
"""One-line installers against fake releases, without installing Ophio."""
from __future__ import annotations

import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import time
import unittest
import zipfile

SCRIPT = Path(__file__).resolve().parents[1] / "get-ophio.sh"

# A stand-in for the release's `ophio`: says its version, or fails the way a
# program built for a newer glibc does.
FAKE_OPHIO = """#!/bin/sh
if [ -n "${FAKE_BROKEN:-}" ]; then
  echo "./ophio: /lib/x86_64-linux-gnu/libc.so.6: version \\`GLIBC_2.39' not found" >&2
  exit 1
fi
echo "ophio VERSION"
"""
# A stand-in for the archive's installer: records how it was run.
FAKE_INSTALL = """#!/bin/sh
printf 'installed VERSION from %s with [%s]\\n' "$(dirname "$0")" "$*" >> "$FAKE_LOG"
"""


def archive(version: str, target: str, *, member_prefix: str | None = None) -> bytes:
    folder = member_prefix or f"ophio-{version}-{target}"
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w:gz") as tar:
        for name, text, mode in (
            ("ophio", FAKE_OPHIO.replace("VERSION", version), 0o755),
            ("install.sh", FAKE_INSTALL.replace("VERSION", version), 0o755),
            ("INSTALL.md", "Install guide\n", 0o644),
        ):
            data = text.encode()
            info = tarfile.TarInfo(f"{folder}/{name}")
            info.size = len(data)
            info.mode = mode
            tar.addfile(info, io.BytesIO(data))
    return buffer.getvalue()


class GetOphioTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.releases = self.root / "releases"
        self.log = self.root / "install.log"
        self.home = self.root / "home"
        self.home.mkdir()
        self.bin = self.root / "bin"
        self.bin.mkdir()
        # uname and id answer as the test says, so every computer can be tried.
        self.fake("uname", 'case "$1" in -s) echo "${FAKE_OS:-Linux}";; -m) echo "${FAKE_MACHINE:-x86_64}";; esac')
        self.fake("id", 'case "$1" in -u) echo "${FAKE_UID:-1000}";; -un) echo sam;; esac')
        self.fake("sysctl", 'echo "${FAKE_ARM64:-0}"')
        self.publish("9.9.8", ["x86_64-linux", "aarch64-macos"])
        self.publish("9.9.9", ["x86_64-linux"], latest=True)

    def fake(self, name: str, body: str) -> None:
        path = self.bin / name
        path.write_text(f"#!/bin/sh\n{body}\n", encoding="utf-8")
        path.chmod(0o755)

    def publish(self, version: str, targets: list[str], *, latest: bool = False, tamper: str | None = None) -> None:
        folder = self.releases / "download" / f"v{version}"
        folder.mkdir(parents=True, exist_ok=True)
        lines = []
        for target in targets:
            name = f"ophio-{version}-{target}.tar.gz"
            data = archive(version, target)
            (folder / name).write_bytes(data)
            lines.append(f"{hashlib.sha256(data).hexdigest()}  {name}")
        source = f"ophio-{version}-source.tar.gz"
        (folder / source).write_bytes(b"source")
        lines.append(f"{hashlib.sha256(b'source').hexdigest()}  {source}")
        lines.append(f"{'0' * 64}  ophio-{version}-arm64.apk")
        (folder / "SHA256SUMS").write_text("\n".join(lines) + "\n", encoding="utf-8")
        if tamper:
            (folder / tamper).write_bytes(b"changed on the way")
        if latest:
            link = self.releases / "latest"
            link.mkdir(exist_ok=True)
            target = link / "download"
            if target.is_symlink():
                target.unlink()
            target.symlink_to(folder)

    def run_script(self, *args: str, **env: str) -> subprocess.CompletedProcess:
        environment = {
            "PATH": f"{self.bin}{os.pathsep}{os.environ['PATH']}",
            "HOME": str(self.home),
            "TMPDIR": str(self.root),
            "OPHIO_RELEASES": self.releases.as_uri(),
            "FAKE_LOG": str(self.log),
            "SHELL": "/bin/zsh",
        }
        environment.update(env)
        return subprocess.run(
            ["sh", str(SCRIPT), *args],
            env=environment,
            stdin=subprocess.DEVNULL,
            capture_output=True,
            text=True,
            encoding="utf-8",
            timeout=60,
        )

    def installed(self) -> str:
        return self.log.read_text(encoding="utf-8") if self.log.exists() else ""

    def leftovers(self) -> list[str]:
        return sorted(p.name for p in self.root.glob("ophio-get.*"))

    def test_installs_the_newest_release_after_checking_it(self):
        result = self.run_script()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Checked ophio-9.9.9-x86_64-linux.tar.gz against the release's SHA256SUMS.", result.stdout)
        self.assertRegex(self.installed(), r"^installed 9\.9\.9 from .*/ophio-9\.9\.9-x86_64-linux with \[\]\n$")
        self.assertEqual(self.leftovers(), [], "the download and unpacked files are removed")

    def test_says_how_to_put_ophio_on_the_path(self):
        result = self.run_script(SHELL="/bin/zsh")
        self.assertIn(f"echo 'export PATH=\"$HOME/.local/bin:$PATH\"' >> {self.home}/.zshrc", result.stdout)
        on_path = self.run_script(PATH=f"{self.home}/.local/bin{os.pathsep}{self.bin}{os.pathsep}{os.environ['PATH']}")
        self.assertNotIn("add ~/.local/bin to your PATH", on_path.stdout)

    def test_a_chosen_version_and_no_setup_reach_the_installer(self):
        result = self.run_script("--version", "v9.9.8", "--no-setup")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertRegex(self.installed(), r"^installed 9\.9\.8 .* with \[--no-setup\]\n$")
        pinned = self.run_script(OPHIO_VERSION="9.9.8")
        self.assertEqual(pinned.returncode, 0, pinned.stderr)
        self.assertIn("Finding Øphio's release 9.9.8", pinned.stdout)

    def test_a_mac_with_apple_silicon_gets_the_mac_download(self):
        result = self.run_script("--version", "9.9.8", FAKE_OS="Darwin", FAKE_MACHINE="arm64")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("ophio-9.9.8-aarch64-macos", self.installed())
        self.assertRegex(self.installed(), r"with \[--no-setup\]\n$", "a Mac follows the Mac guide, not setup")
        self.assertIn("~/.local/bin/ophio service install", result.stdout)
        rosetta = self.run_script("--version", "9.9.8", FAKE_OS="Darwin", FAKE_MACHINE="x86_64", FAKE_ARM64="1")
        self.assertEqual(rosetta.returncode, 0, rosetta.stderr)
        self.assertEqual(self.installed().count("aarch64-macos"), 2)
        self.assertIn("packaging/macos/INSTALL.md", rosetta.stdout)

    def test_a_release_without_this_computers_download_says_so(self):
        result = self.run_script(FAKE_OS="Darwin", FAKE_MACHINE="arm64")
        self.assertEqual(result.returncode, 1)
        self.assertIn("Øphio's 9.9.9, the newest release, has no download for a Mac with Apple silicon.", result.stderr)
        self.assertEqual(self.installed(), "")

    def test_a_download_that_does_not_match_its_checksum_is_not_installed(self):
        self.publish("9.9.9", ["x86_64-linux"], latest=True, tamper="ophio-9.9.9-x86_64-linux.tar.gz")
        result = self.run_script()
        self.assertEqual(result.returncode, 1)
        self.assertIn("doesn't match the release's SHA256SUMS", result.stderr)
        self.assertIn("Nothing was installed.", result.stderr)
        self.assertEqual(self.installed(), "")
        self.assertEqual(self.leftovers(), [])

    def test_the_installed_version_is_not_downloaded_again(self):
        local_bin = self.home / ".local" / "bin"
        local_bin.mkdir(parents=True)
        (local_bin / "ophio").write_text(FAKE_OPHIO.replace("VERSION", "9.9.9"), encoding="utf-8")
        (local_bin / "ophio").chmod(0o755)
        result = self.run_script()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Øphio 9.9.9 is already installed", result.stdout)
        self.assertNotIn("Downloading", result.stdout)
        self.assertEqual(self.installed(), "")
        older = self.run_script("--version", "9.9.8")
        self.assertEqual(older.returncode, 0, older.stderr)
        self.assertIn("installed 9.9.8", self.installed())

    def test_a_missing_release_is_a_plain_failure(self):
        result = self.run_script("--version", "1.0.0")
        self.assertEqual(result.returncode, 1)
        self.assertIn("couldn't download the release 1.0.0's SHA256SUMS", result.stderr)

    def test_a_program_that_cannot_run_here_explains_why(self):
        result = self.run_script(FAKE_BROKEN="1")
        self.assertEqual(result.returncode, 1)
        self.assertIn("needs glibc 2.39 or later", result.stderr)
        self.assertIn("Øphio 9.9.9 can't run on this computer.", result.stderr)
        self.assertEqual(self.installed(), "")

    def test_refuses_root_and_computers_without_a_download(self):
        cases = [
            ({"FAKE_UID": "0"}, "not as root or with sudo"),
            ({"FAKE_OS": "Darwin", "FAKE_MACHINE": "x86_64"}, "Intel Macs aren't supported"),
            ({"FAKE_MACHINE": "aarch64"}, "no Øphio download for Linux on aarch64"),
            ({"FAKE_OS": "MINGW64_NT-10.0"}, "get-ophio.ps1 | iex"),
            ({"FAKE_OS": "FreeBSD"}, "no Øphio download for FreeBSD"),
        ]
        for env, message in cases:
            with self.subTest(env=env):
                result = self.run_script(**env)
                self.assertEqual(result.returncode, 1)
                self.assertIn(message, result.stderr)
                self.assertNotIn("Finding", result.stdout)
        self.assertEqual(self.installed(), "")

    def test_rejects_unknown_options_and_odd_versions(self):
        self.assertIn("unknown option --now", self.run_script("--now").stderr)
        self.assertIn("isn't a version number", self.run_script("--version", "../x").stderr)
        self.assertEqual(self.installed(), "")

    @unittest.skipUnless(shutil.which("bash"), "needs bash")
    def test_runs_under_a_strict_posix_shell(self):
        # macOS's /bin/sh is bash in POSIX mode.
        result = subprocess.run(["bash", "--posix", "-n", str(SCRIPT)], capture_output=True, text=True, encoding="utf-8")
        self.assertEqual(result.returncode, 0, result.stderr)


@unittest.skipUnless(shutil.which("pwsh") and os.name == "posix", "needs PowerShell and POSIX fake executables")
class GetOphioWindowsTests(unittest.TestCase):
    """Run the real PowerShell flow, replacing Windows preflight and external I/O.

    A shell executable stands in for ophio.exe. Downloads use local files;
    checksum validation and archive extraction still use PowerShell itself.
    """

    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.local = self.root / "local"
        self.local.mkdir()
        self.installed = self.local / "Programs/Ophio/ophio.exe"
        self.log = self.root / "calls"
        self.release = {"version": "9.9.9", "installed_at": 123}
        self.old = {"id": "previous-attempt", "at": int(time.time()) + 10,
                    "release": self.release, "result": "failed", "message": "old failure"}
        self.fake = """#!/bin/sh
printf '%s\\n' "$*" >> "$FAKE_ROOT/calls"
case "$1" in
  install)
    mkdir -p "$(dirname "$FAKE_INSTALLED")"
    cp "$0" "$FAKE_INSTALLED"
    touch "$FAKE_ROOT/queued"
    ;;
  update)
    if [ ! -f "$FAKE_ROOT/queued" ]; then
      [ "${FAKE_BROKEN:-}" != 1 ] || exit 1
      cat "$FAKE_ROOT/before.json"
    elif [ ! -f "$FAKE_ROOT/polled" ]; then
      touch "$FAKE_ROOT/polled"
      cat "$FAKE_ROOT/pending.json"
    else
      cat "$FAKE_ROOT/finished.json"
    fi
    ;;
esac
"""
        self.name = "ophio-9.9.9-x86_64-windows"
        with zipfile.ZipFile(self.root / f"{self.name}.zip", "w") as archive:
            archive.writestr(f"{self.name}/ophio.exe", self.fake)
        checksum = hashlib.sha256((self.root / f"{self.name}.zip").read_bytes()).hexdigest()
        (self.root / "SHA256SUMS").write_text(f"{checksum}  {self.name}.zip\n", encoding="utf-8")
        self.states()

    def states(self, *, installed=None, last=None, pending=None):
        for name, state in {
            "before": {"installed": installed, "last_update": last},
            "pending": {"installed": None, "last_update": pending},
            "finished": {"installed": self.release, "last_update": None},
        }.items():
            (self.root / f"{name}.json").write_text(json.dumps(state), encoding="utf-8")

    def launcher(self):
        self.installed.parent.mkdir(parents=True, exist_ok=True)
        self.installed.write_text(self.fake, encoding="utf-8")
        self.installed.chmod(0o755)

    def run_script(self, **env):
        source = SCRIPT.with_suffix(".ps1").read_text(encoding="utf-8")
        # Only skip the platform-specific preflight; run the rest unchanged.
        begin = source.index("        $identity =")
        end = source.index('        $version = "$env:OPHIO_VERSION"')
        source = source[:begin] + source[end:]
        harness = r"""
function Invoke-WebRequest {
    param([switch] $UseBasicParsing, $Uri, $OutFile)
    Copy-Item -LiteralPath (Join-Path $env:FAKE_ROOT ($Uri.Split('/')[-1])) -Destination $OutFile
}
function Unblock-File { param($LiteralPath) }
function Start-Sleep { param($Seconds) }
function Expand-Archive {
    param($LiteralPath, $DestinationPath)
    Microsoft.PowerShell.Archive\Expand-Archive -LiteralPath $LiteralPath -DestinationPath $DestinationPath
    Get-ChildItem $DestinationPath -Recurse -Filter ophio.exe | ForEach-Object { & chmod +x $_.FullName }
}
"""
        script = self.root / "run.ps1"
        script.write_text(harness + source + '\nWrite-Host "CALLER STILL RUNNING"\n', encoding="utf-8")
        environment = os.environ.copy()
        environment.update(LOCALAPPDATA=str(self.local), USERNAME="test",
                           TMPDIR=str(self.root), FAKE_ROOT=str(self.root),
                           FAKE_INSTALLED=str(self.installed), OPHIO_RELEASES="https://example.invalid/releases",
                           OPHIO_VERSION="", OPHIO_NO_SETUP="")
        environment.update(env)
        return subprocess.run([shutil.which("pwsh"), "-NoProfile", "-File", str(script)],
                              env=environment, capture_output=True, text=True, encoding="utf-8", timeout=20)

    def assert_finished(self, result):
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Starting setup now", result.stdout)
        self.assertIn("setup", self.log.read_text(encoding="utf-8").splitlines())
        self.assertNotIn("ended without installing it", result.stdout)
        self.assertIn("CALLER STILL RUNNING", result.stdout)
        self.assertEqual(list(self.root.glob("ophio-get-*")), [])

    def test_windows_rerun_ignores_the_previous_attempts_failure(self):
        # No launcher remains, but last-update.json survives the old failure.
        # Even a recent timestamp must not make the same outcome a new one.
        self.states(last=self.old, pending=self.old)
        self.assert_finished(self.run_script())

    def test_windows_half_finished_first_install_waits_and_starts_setup(self):
        self.launcher()
        self.assert_finished(self.run_script())

    def test_windows_unfinished_version_record_still_waits_for_first_install(self):
        self.launcher()
        self.states(installed=dict(self.release, installed_at=0))
        self.assert_finished(self.run_script())

    def test_windows_unrunnable_launcher_still_waits_for_first_install(self):
        self.launcher()
        self.assert_finished(self.run_script(FAKE_BROKEN="1"))

    def test_windows_new_failure_is_reported(self):
        self.states(last=self.old, pending=dict(self.old, id="new-attempt", message="new failure"))
        result = self.run_script()
        self.assertIn("ended without installing it: new failure", result.stdout)
        self.assertNotIn("Starting setup now", result.stdout)
        self.assertIn("CALLER STILL RUNNING", result.stdout)
        self.assertEqual(list(self.root.glob("ophio-get-*")), [])

    def test_windows_old_outcome_is_ignored_when_initial_status_cannot_run(self):
        self.states(pending=dict(self.old, at=1))
        self.assert_finished(self.run_script(FAKE_BROKEN="1"))

    def test_windows_existing_install_queues_an_update(self):
        self.launcher()
        self.states(installed=dict(self.release, version="9.9.8"))
        result = self.run_script()
        self.assertIn("is queued", result.stdout)
        self.assertNotIn("Starting setup now", result.stdout)
        self.assertNotIn("setup", self.log.read_text(encoding="utf-8").splitlines())

    def test_windows_current_install_skips_download(self):
        self.launcher()
        self.states(installed=self.release)
        result = self.run_script()
        self.assertIn("already installed", result.stdout)
        self.assertNotIn("Downloading", result.stdout)

    def test_windows_bad_checksum_never_installs(self):
        (self.root / f"{self.name}.zip").write_bytes(b"tampered")
        result = self.run_script()
        self.assertIn("doesn't match the release's SHA256SUMS", result.stdout)
        self.assertFalse(self.log.exists())
        self.assertEqual(list(self.root.glob("ophio-get-*")), [])


if __name__ == "__main__":
    unittest.main()
