#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Disposable Linux compositor guests. No host display or global configuration."""

from __future__ import annotations

import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import signal
import socket
import subprocess
import sys
import tarfile
import tempfile
import time
import uuid

ROOT = Path(__file__).resolve().parents[2]
LAB = (ROOT / "target/lab").resolve()
EVIDENCE = (ROOT / "target/lab-evidence").resolve()
run_label = os.environ.get("LAB_EVIDENCE_RUN")
if run_label:
    if not re.fullmatch(r"[a-zA-Z0-9_-]{1,40}", run_label):
        raise ValueError("Invalid evidence run label")
    EVIDENCE = EVIDENCE / run_label
TARGETS = ("hyprland", "gnome", "plasma", "builder")
# The builder has no desktop. It compiles release archives on the oldest
# supported userspace, so their glibc requirement matches the documented floor.
DISTROS = {"hyprland": "arch", "gnome": "ubuntu", "plasma": "arch", "builder": "ubuntu"}
# Loopback forwards, overridable so a second checkout's lab can run beside the
# connected review environment: SSH, Øphio on the host, Øphio in the guest.
FORWARDS = {
    "ssh": ("LAB_SSH_PORT", 47222),
    "ophio": ("LAB_OPHIO_PORT", 47112),
    "guest_ophio": ("LAB_GUEST_OPHIO_PORT", 47112),
}
ARCH_KEY = "1B9A16984A4E8CB448712D2AE0B78BF4326C6F8F"
UBUNTU_KEY = "D2EB44626FDDC30B513D5BB71A5D6C4C7DB87C81"
DISPLAY_ENV = ("DISPLAY", "WAYLAND_DISPLAY", "DBUS_SESSION_BUS_ADDRESS")
# Only what the public phone app check runs: apksigner's launcher and jar, and
# zipalign with the C++ runtime its RUNPATH finds beside it.
BUILD_TOOLS = ("apksigner", "zipalign", "lib/apksigner.jar", "lib64/libc++.so")
EXECUTABLE_TOOLS = ("apksigner", "zipalign")


class LabError(Exception):
    """A failed precondition or lab operation; never a qualification pass."""


def environment():
    return {k: v for k, v in os.environ.items() if k not in DISPLAY_ENV}


def forwards():
    ports = {}
    for key, (variable, default) in FORWARDS.items():
        value = os.environ.get(variable, str(default))
        if not re.fullmatch(r"[0-9]{4,5}", value) or not 1024 <= int(value) <= 65535:
            raise LabError(f"{variable} must be an unprivileged TCP port")
        ports[key] = int(value)
    if ports["ssh"] == ports["ophio"]:
        raise LabError("SSH and Øphio need different host ports")
    return ports


def adb_serial():
    """The isolated emulator, named by its console port; never a physical device."""
    serial = os.environ.get("LAB_ADB_SERIAL", "emulator-5562")
    match = re.fullmatch(r"emulator-([0-9]{4})", serial)
    if not match or not 5554 <= int(match[1]) <= 5584 or int(match[1]) % 2:
        raise LabError("LAB_ADB_SERIAL must name an emulator console port")
    return serial


def run(args, **kwargs):
    return subprocess.run(
        [str(a) for a in args], env=environment(), check=True, **kwargs
    )


def download(url, path):
    if not url.startswith("https://"):
        raise LabError("Downloads must use HTTPS")
    temporary = path.with_suffix(path.suffix + ".partial")
    run(
        [
            "curl",
            "--fail",
            "--location",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--tlsv1.2",
            "--retry",
            "3",
            "--connect-timeout",
            "30",
            "--max-time",
            "1800",
            "--silent",
            "--show-error",
            url,
            "-o",
            temporary,
        ]
    )
    temporary.replace(path)


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def expected_digest(text, filename):
    matches = []
    for line in text.splitlines():
        match = re.fullmatch(r"([0-9a-fA-F]{64})\s+\*?(.+)", line)
        if match and match[2] == filename:
            matches.append(match[1].lower())
    if len(matches) != 1:
        raise LabError("Publisher checksum must name the exact image once")
    return matches[0]


def verify_signature(signature, data, keyfile, fingerprint):
    home = LAB / "gnupg"
    home.mkdir(exist_ok=True, mode=0o700)
    gpg = ["gpg", "--homedir", home, "--batch", "--no-auto-key-retrieve"]
    run([*gpg, "--import", keyfile], capture_output=True)
    checked = run(
        [*gpg, "--status-fd", "1", "--verify", signature, data],
        capture_output=True,
        text=True,
        encoding="utf-8",
    )
    valid = [
        line.split()
        for line in checked.stdout.splitlines()
        if line.startswith("[GNUPG:] VALIDSIG ")
    ]
    if not any(line[-1] == fingerprint or line[2] == fingerprint for line in valid):
        raise LabError("Signature did not match the pinned publisher fingerprint")
    return checked.stdout


def fetch(distro):
    folder = LAB / "images" / distro
    folder.mkdir(parents=True, exist_ok=True)
    # Overlays depend on the exact backing bytes. Never refresh a cached base in
    # place when the publisher advances its moving "latest" URL.
    if (folder / "verified.json").exists():
        record = json.loads((folder / "verified.json").read_text(encoding="utf-8"))
        image = folder / record["url"].rsplit("/", 1)[1]
        if not image.is_file() or digest(image) != record["sha256"]:
            raise LabError(
                "Cached verified image changed; preserve overlays and investigate"
            )
        print(f"{distro}: immutable cached image checksum verified")
        return
    if distro == "arch":
        base = "https://geo.mirror.pkgbuild.com/images/latest/"
        name = "Arch-Linux-x86_64-cloudimg.qcow2"
        download(base + name + ".SHA256", folder / "SHA256SUMS")
        download(base + name + ".sig", folder / "image.sig")
        download(
            "https://raw.githubusercontent.com/archlinux/arch-boxes/master/README.md",
            folder / "publisher.txt",
        )
        match = re.search(
            r"-----BEGIN PGP PUBLIC KEY BLOCK-----.*?-----END PGP PUBLIC KEY BLOCK-----",
            (folder / "publisher.txt").read_text(encoding="utf-8"),
            re.S,
        )
        if not match:
            raise LabError("Publisher signing key missing")
        (folder / "publisher.asc").write_text(match[0] + "\n", encoding="utf-8")
        signature, signed, key = folder / "image.sig", folder / name, ARCH_KEY
    else:
        base = "https://cloud-images.ubuntu.com/noble/current/"
        name = "noble-server-cloudimg-amd64.img"
        download(base + "SHA256SUMS", folder / "SHA256SUMS")
        download(base + "SHA256SUMS.gpg", folder / "image.sig")
        download(
            "https://keyserver.ubuntu.com/pks/lookup?op=get&search=0x" + UBUNTU_KEY,
            folder / "publisher.asc",
        )
        signature, signed, key = folder / "image.sig", folder / "SHA256SUMS", UBUNTU_KEY
    sums = (folder / "SHA256SUMS").read_text(encoding="utf-8")
    expected = expected_digest(sums, name)
    image = folder / name
    if not image.exists() or digest(image) != expected:
        download(base + name, image)
    if digest(image) != expected:
        raise LabError("Image checksum mismatch; image must not be booted")
    status = verify_signature(signature, signed, folder / "publisher.asc", key)
    record = {
        "url": base + name,
        "sha256": expected,
        "publisher_key": key,
        "gpg_status": status,
    }
    (folder / "verified.json").write_text(
        json.dumps(record, indent=2) + "\n", encoding="utf-8"
    )
    print(f"{distro}: publisher checksum and pinned-key signature verified")


def ssh_args(target, port):
    return [
        "ssh",
        "-F",
        "/dev/null",
        "-i",
        LAB / "id_ed25519",
        "-p",
        str(port),
        "-o",
        "IdentitiesOnly=yes",
        "-o",
        "IdentityAgent=none",
        "-o",
        "BatchMode=yes",
        "-o",
        "ForwardAgent=no",
        "-o",
        "ForwardX11=no",
        "-o",
        "ClearAllForwardings=yes",
        "-o",
        "StrictHostKeyChecking=accept-new",
        "-o",
        "ConnectTimeout=3",
        "-o",
        f"UserKnownHostsFile={LAB / target / 'known_hosts'}",
        "lab@127.0.0.1",
    ]


def guest(target, args, **kwargs):
    record = state(target)
    if not record:
        raise LabError("The requested guest is not running")
    port = record["forwards"]["ssh"]
    return run([*ssh_args(target, port), shlex.join(str(a) for a in args)], **kwargs)


def state(target):
    path = LAB / target / "process.json"
    if not path.exists():
        return None
    record = json.loads(path.read_text(encoding="utf-8"))
    proc = Path(f"/proc/{record['pid']}")
    try:
        fields = (proc / "stat").read_text(encoding="utf-8").split(") ", 1)[1].split()
        if fields[0] == "Z":
            return None
        start = fields[19]
        command = (proc / "cmdline").read_bytes().split(b"\0")
        if start != record["start"] or str(
            LAB / target / "disk.qcow2"
        ).encode() not in b" ".join(command):
            return None
        if b"-display" not in command or b"none" not in command:
            raise LabError("Recorded process does not match the headless guest")
    except FileNotFoundError:
        return None
    return record


def create(target):
    folder = LAB / target
    if folder.exists():
        raise LabError(
            "Guest already exists; preserve evidence and choose a new checkout to reset"
        )
    distro = DISTROS[target]
    images = LAB / "images" / distro
    record = json.loads((images / "verified.json").read_text(encoding="utf-8"))
    image = images / record["url"].rsplit("/", 1)[1]
    if digest(image) != record["sha256"]:
        raise LabError("Verified base image changed")
    if not (LAB / "id_ed25519").exists():
        run(
            [
                "ssh-keygen",
                "-q",
                "-t",
                "ed25519",
                "-N",
                "",
                "-C",
                "disposable-desktop-lab",
                "-f",
                LAB / "id_ed25519",
            ]
        )
    folder.mkdir()
    seed = folder / "seed"
    seed.mkdir()
    public_key = (LAB / "id_ed25519.pub").read_text(encoding="utf-8").strip()
    (seed / "meta-data").write_text(
        f"instance-id: desktop-lab-{uuid.uuid4()}\nlocal-hostname: desktop-lab-{target}\n",
        encoding="utf-8",
    )
    # Only this generated lab public key is copied; SSH ignores all user config/agents.
    (seed / "user-data").write_text(f"""#cloud-config
users:
  - name: lab
    shell: /bin/bash
    lock_passwd: true
    sudo: ALL=(ALL) NOPASSWD:ALL
    ssh_authorized_keys:
      - {public_key}
ssh_pwauth: false
disable_root: true
write_files:
  - path: /etc/companion-lab
    content: disposable-desktop-lab
  - path: /etc/ssh/sshd_config.d/00-lab.conf
    content: |
      PasswordAuthentication no
      KbdInteractiveAuthentication no
      PermitRootLogin no
      AllowUsers lab
runcmd:
  - [systemctl, enable, --now, {"ssh" if distro == "ubuntu" else "sshd"}]
""", encoding="utf-8")
    run(
        [
            "xorriso",
            "-as",
            "mkisofs",
            "-quiet",
            "-V",
            "cidata",
            "-o",
            folder / "seed.iso",
            "-J",
            "-r",
            seed,
        ],
        capture_output=True,
    )
    run(
        [
            "qemu-img",
            "create",
            "-f",
            "qcow2",
            "-F",
            "qcow2",
            "-b",
            image,
            folder / "disk.qcow2",
            "24G",
        ]
    )
    shutil.copyfile(firmware("vars"), folder / "OVMF_VARS.fd")
    (folder / "image.json").write_text(
        json.dumps(record, indent=2) + "\n", encoding="utf-8"
    )


def firmware(kind):
    names = {
        "code": [
            "/usr/share/edk2/x64/OVMF_CODE.4m.fd",
            "/usr/share/OVMF/OVMF_CODE_4M.fd",
        ],
        "vars": [
            "/usr/share/edk2/x64/OVMF_VARS.4m.fd",
            "/usr/share/OVMF/OVMF_VARS_4M.fd",
        ],
    }
    for name in names[kind]:
        if Path(name).is_file():
            return Path(name)
    raise LabError("OVMF firmware not installed; do not install system packages")


def qemu_args(target, ports, provision=False):
    folder = LAB / target
    net = "user,id=labnet," + ("" if provision else "restrict=on,")
    net += f"hostfwd=tcp:127.0.0.1:{ports['ssh']}-:22,"
    net += f"hostfwd=tcp:127.0.0.1:{ports['ophio']}-:{ports['guest_ophio']}"
    return [
        "/usr/bin/qemu-system-x86_64",
        "-enable-kvm",
        "-machine",
        "q35",
        "-cpu",
        "host",
        "-smp",
        "8",
        "-m",
        "6144",
        "-display",
        "none",
        "-nodefaults",
        "-drive",
        f"if=pflash,format=raw,readonly=on,file={firmware('code')}",
        "-drive",
        f"if=pflash,format=raw,file={folder / 'OVMF_VARS.fd'}",
        "-drive",
        f"if=virtio,format=qcow2,file={folder / 'disk.qcow2'}",
        "-drive",
        f"if=virtio,format=raw,readonly=on,file={folder / 'seed.iso'}",
        "-device",
        "virtio-vga",
        "-device",
        "virtio-rng-pci",
        "-netdev",
        net,
        "-device",
        "virtio-net-pci,netdev=labnet",
        "-serial",
        f"file:{folder / 'serial.log'}",
        "-monitor",
        "none",
        "-qmp",
        f"unix:{folder / 'qmp.sock'},server=on,wait=off",
    ]


def restore_backing_path(target):
    """Relocate only the backing pathname, after verifying the original bytes."""
    folder = LAB / target
    info = json.loads(
        run(
            ["qemu-img", "info", "--output=json", folder / "disk.qcow2"],
            capture_output=True,
            text=True,
            encoding="utf-8",
        ).stdout
    )
    record = json.loads((folder / "image.json").read_text(encoding="utf-8"))
    image = (
        LAB / "images" / DISTROS[target] / record["url"].rsplit("/", 1)[1]
    ).resolve()
    if info.get("backing-filename") == str(image):
        return
    if not image.is_file() or digest(image) != record["sha256"]:
        raise LabError("Cannot relocate an unverified backing image")
    if info.get("backing-filename-format") != "qcow2":
        raise LabError("Unexpected backing image format")
    run(
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
        ]
    )


def record_process(folder, pid, started, restricted, ports):
    """Later commands, and the phone, reach the guest through the forwards it
    was started with."""
    (folder / "process.json").write_text(
        json.dumps(
            {
                "pid": pid,
                "start": started,
                "restricted": restricted,
                "forwards": ports,
            }
        )
        + "\n",
        encoding="utf-8",
    )


def start(target, provision=False):
    if any(state(name) for name in TARGETS):
        raise LabError(
            "Stop the running lab guest first; the loopback forwards are shared"
        )
    folder = LAB / target
    if not (folder / "disk.qcow2").is_file():
        raise LabError("Create the guest before starting it")
    restore_backing_path(target)
    ports = forwards()
    for port in (ports["ssh"], ports["ophio"]):
        with socket.socket() as probe:
            probe.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            probe.bind(("127.0.0.1", port))
    (folder / "qmp.sock").unlink(missing_ok=True)
    args = qemu_args(target, ports, provision)
    with (folder / "qemu.log").open("ab") as log:
        process = subprocess.Popen(
            args,
            env=environment(),
            stdin=subprocess.DEVNULL,
            stdout=log,
            stderr=log,
            start_new_session=True,
        )
    time.sleep(0.4)
    if process.poll() is not None:
        raise LabError("QEMU failed; inspect the guest's qemu.log")
    stat = Path(f"/proc/{process.pid}/stat").read_text(encoding="utf-8")
    started = stat.split(") ", 1)[1].split()[19]
    record_process(folder, process.pid, started, not provision, ports)
    print(
        f"{target}: started headless; network={'provisioning' if provision else 'restricted'}"
    )


def stop(target):
    record = state(target)
    if record is None:
        print(f"{target}: stopped")
        return
    # A recorded PID alone is insufficient: state() also verifies start time and image.
    try:
        guest(target, ["sudo", "poweroff"], capture_output=True, timeout=10)
    except (LabError, subprocess.SubprocessError, OSError):
        pass
    for _ in range(40):
        if not state(target):
            break
        time.sleep(0.25)
    if state(target):
        os.kill(record["pid"], signal.SIGTERM)
        for _ in range(40):
            if not state(target):
                break
            time.sleep(0.25)
    if state(target):
        raise LabError("Guest did not stop; manual inspection required")
    (LAB / target / "process.json").unlink(missing_ok=True)
    print(f"{target}: stopped")


def wait_ssh(target):
    for _ in range(120):
        if not state(target):
            raise LabError("Guest exited before SSH became ready")
        try:
            guest(
                target,
                ["test", "-f", "/etc/companion-lab"],
                capture_output=True,
                timeout=5,
            )
            print(f"{target}: key-only SSH ready")
            return
        except subprocess.SubprocessError:
            time.sleep(1)
    raise LabError("SSH timeout; inspect serial.log")


def copy_file(target, source, destination):
    source = Path(source).resolve(strict=True)
    if not source.is_file() or not any(
        source.is_relative_to(root) for root in (ROOT, LAB)
    ):
        raise LabError("Copy sources must be files inside this checkout")
    # SSH stdin avoids scp's remote shell/path interpretation and local identities.
    with source.open("rb") as content:
        guest(
            target,
            ["sh", "-c", 'umask 077; cat > "$1"', "lab-copy", destination],
            stdin=content,
        )


def provision(target):
    record = state(target)
    if not record or record["restricted"]:
        raise LabError("Start with --provision before installing guest packages")
    guest(target, ["mkdir", "-p", "lab"])
    for source in sorted((ROOT / "tools/lab/guest").iterdir()):
        if source.is_file():
            copy_file(target, source, "lab/" + source.name)
    guest(target, ["sudo", "bash", "lab/provision.sh", target])


def artifacts(target, guest_build=False):
    guest(target, ["mkdir", "-p", "bin"])
    if guest_build:
        record = state(target)
        if target != "gnome" or not record or record["restricted"]:
            raise LabError("Guest builds require the Ubuntu guest in provisioning mode")
        source = LAB / "source.tar"
        run(
            [
                "git",
                "archive",
                "--format=tar",
                f"--output={source}",
                "HEAD",
                "Cargo.toml",
                "Cargo.lock",
                ".cargo",
                "crates",
                "LICENSE",
            ],
            cwd=ROOT,
        )
        copy_file(target, source, "lab/source.tar")
        for name in ("toolchain.sh", "build.sh"):
            copy_file(target, ROOT / "tools/lab/guest" / name, "lab/" + name)
        guest(target, ["bash", "lab/build.sh"])
    else:
        for name in ("ophio", "computer-mcp"):
            copy_file(target, ROOT / "target/debug" / name, "bin/" + name)
    for name, crate in (("claude", "harness-claude"), ("codex", "harness-codex")):
        source = ROOT / "crates" / crate / "tests" / ("fake_" + name + ".py")
        copy_file(target, source, "lab/fake_" + name + ".py")
    guest(target, ["bash", "lab/install.sh"])


def archive(target, revision, phone_app=None, build_tools=None):
    """Build the Linux release archive of a commit with the repository's packaging.

    Given the signed phone app, the archive is a public one: the builder checks
    the app against the release key with these Android tools, then bundles it.
    """
    record = state(target)
    if target != "builder" or not record:
        raise LabError("Release archives are built in the running Ubuntu builder")
    if (phone_app is None) != (build_tools is None):
        raise LabError(
            "A public archive needs the signed phone app and Android build-tools"
        )
    public = phone_app is not None
    if public:
        app = Path(phone_app)
        if app.is_symlink() or not app.is_file() or app.suffix != ".apk":
            raise LabError("The signed phone app must be a regular .apk file")
        tools = Path(build_tools).resolve(strict=True)
        for name in BUILD_TOOLS:
            if not (tools / name).is_file():
                raise LabError(f"Android build-tools missing {name}")
    commit = run(
        ["git", "rev-parse", "--verify", revision + "^{commit}"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        encoding="utf-8",
    ).stdout.strip()
    epoch = run(
        ["git", "log", "-1", "--format=%ct", commit],
        cwd=ROOT,
        capture_output=True,
        text=True,
        encoding="utf-8",
    ).stdout.strip()
    work = LAB / "package" / commit[:12]
    shutil.rmtree(work, ignore_errors=True)
    (work / "source").mkdir(parents=True)
    run(
        ["git", "archive", "--format=tar", f"--output={work / 'source.tar'}", commit],
        cwd=ROOT,
    )
    run(["tar", "-xf", work / "source.tar", "-C", work / "source"])
    # The builder has no registry access: vendor exactly this commit's lock file.
    run(
        [
            "cargo",
            "vendor",
            "--locked",
            "--offline",
            "--versioned-dirs",
            work / "vendor",
        ],
        cwd=work / "source",
        capture_output=True,
    )
    run(["tar", "-cf", work / "vendor.tar", "-C", work, "vendor"])
    guest(target, ["mkdir", "-p", "lab"])
    copy_file(target, work / "source.tar", "lab/package-source.tar")
    copy_file(target, work / "vendor.tar", "lab/package-vendor.tar")
    copy_file(target, ROOT / "tools/lab/guest/package.sh", "lab/package.sh")
    # A previous public build's app must never reach a later archive.
    guest(target, ["rm", "-rf", "lab/package-phone.apk", "lab/android-build-tools"])
    staged = {}
    if public:
        # Copies, so the files the builder checks are the ones recorded here.
        shutil.copyfile(app, work / "phone.apk")
        staged["phone.apk"] = digest(work / "phone.apk")
        kit = work / "android-build-tools"
        for name in BUILD_TOOLS:
            (kit / name).parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(tools / name, kit / name)
            staged[name] = digest(kit / name)
        folders = sorted({str(Path(name).parent) for name in BUILD_TOOLS} - {"."})
        guest(
            target,
            ["mkdir", "-p", *("lab/android-build-tools/" + name for name in folders)],
        )
        copy_file(target, work / "phone.apk", "lab/package-phone.apk")
        for name in BUILD_TOOLS:
            copy_file(target, kit / name, "lab/android-build-tools/" + name)
        guest(
            target,
            [
                "chmod",
                "700",
                *("lab/android-build-tools/" + name for name in EXECUTABLE_TOOLS),
            ],
        )
    folder = EVIDENCE / "archives"
    folder.mkdir(parents=True, exist_ok=True, mode=0o700)
    log = folder / f"{commit[:12]}-build.log"
    with log.open("wb") as output:
        guest(
            target,
            ["bash", "lab/package.sh", epoch, commit, *(["public"] if public else [])],
            stdout=output,
            stderr=output,
        )
    match = re.search(
        r"^([0-9a-f]{64})  ([a-z0-9-]+-[0-9.]+-x86_64-linux\.tar\.gz)$",
        log.read_text(encoding="utf-8"),
        re.M,
    )
    if not match:
        raise LabError("The builder reported no archive; inspect its build log")
    destination = LAB / "release" / commit[:12]
    destination.mkdir(parents=True, exist_ok=True)
    # An earlier run's record must not describe this run's archive.
    (destination / "build.json").unlink(missing_ok=True)
    path = destination / match[2]
    path.unlink(missing_ok=True)
    build = {
        "revision": commit,
        "source_date_epoch": int(epoch),
        "archive": match[2],
        "sha256": match[1],
        "public": public,
    }
    # Do not leave interrupted or rejected downloads under a releasable name.
    with tempfile.TemporaryDirectory(dir=destination, prefix="incoming-") as directory:
        incoming = Path(directory) / "archive.partial"
        with incoming.open("wb") as output:
            guest(target, ["cat", "lab/package/dist/linux/" + match[2]], stdout=output)
        if digest(incoming) != match[1]:
            raise LabError("The retrieved archive differs from the one the builder made")
        if public:
            try:
                bundled = bundled_phone_app(incoming, match[2].removesuffix(".tar.gz"))
            except (tarfile.TarError, EOFError) as error:
                raise LabError("The public archive is damaged or unreadable") from error
            if (
                bundled is None
                or hashlib.sha256(bundled).hexdigest() != staged["phone.apk"]
            ):
                raise LabError(
                    "The public archive does not carry the signed phone app it was given"
                )
            build["phone_app_sha256"] = staged.pop("phone.apk")
            build["android_build_tools"] = staged
        incoming.replace(path)
    (destination / "build.json").write_text(
        json.dumps(build, indent=2) + "\n",
        encoding="utf-8",
    )
    print(f"{path.relative_to(ROOT)}: {match[1]}")


def bundled_phone_app(path, name):
    """The phone app inside a release archive, or None when it carries none."""
    with tarfile.open(path) as release_archive:
        try:
            member = release_archive.getmember(f"{name}/ophio.apk")
        except KeyError:
            return None
        if not member.isfile():
            return None
        return release_archive.extractfile(member).read()


def newuser(target):
    """Point the guest's desktop at a new ordinary user who has nothing of Øphio's."""
    record = state(target)
    if target == "builder" or not record or record["restricted"]:
        raise LabError("Set up the new user in a provisioning-mode desktop guest")
    guest(target, ["mkdir", "-p", "lab"])
    for name in ("person.sh", "person-ready.sh", "input-app.py", "accessibility.py"):
        copy_file(target, ROOT / "tools/lab/guest" / name, "lab/" + name)
    copy_file(
        target,
        ROOT / "crates/harness-claude/tests/fake_claude.py",
        "lab/fake_claude.py",
    )
    guest(target, ["sudo", "bash", "lab/person.sh", "setup"])


def release(target, archive_path):
    """Give the new user a release archive, and nothing else from this checkout."""
    source = Path(archive_path).resolve(strict=True)
    if not source.is_relative_to(LAB / "release") or not re.fullmatch(
        r"[a-z0-9-]+-[0-9]+\.[0-9]+\.[0-9]+-x86_64-linux\.tar\.gz", source.name
    ):
        raise LabError("Only an archive made by the archive command can be released")
    record = state(target)
    if target == "builder" or not record or not record["restricted"]:
        raise LabError("Release archives go to a restricted-network desktop guest")
    copy_file(target, source, "lab/incoming.tar.gz")
    guest(target, ["sudo", "bash", "lab/person.sh", "receive", source.name])
    print(f"{target}: ~/Downloads/{source.name} for the new user")


def person(target, args, login=False):
    """Run a command as the new user, in their desktop session unless login."""
    return guest(
        target, ["sudo", "bash", "lab/person.sh", "login" if login else "run", *args]
    )


def collect(target):
    folder = EVIDENCE / target
    folder.mkdir(parents=True, exist_ok=True, mode=0o700)
    for name, args in {
        "doctor.json": ["bash", "lab/in-session.sh", "ophio", "doctor", "--json"],
        "companion-status.json": [
            "bash",
            "lab/in-session.sh",
            "ophio",
            "status",
            "--json",
        ],
        "tuple.txt": ["bash", "lab/tuple.sh"],
    }.items():
        with (
            (folder / name).open("wb") as output,
            (folder / (name + ".stderr.txt")).open("wb") as errors,
        ):
            guest(target, args, stdout=output, stderr=errors, timeout=60)
    # A fixed allowlist excludes configuration, credentials, pairing offers,
    # process environments, SSH material and the general guest filesystem.
    for name in (
        "input-state.json",
        "input-events.jsonl",
        "guest.png",
        "computer-actions.jsonl",
        "computer-observation.png",
        "computer-observation.jpg",
        "session.log",
    ):
        exists = guest(
            target,
            [
                "sh",
                "-c",
                'test ! -f "$1" || printf present',
                "lab-evidence",
                "evidence/" + name,
            ],
            capture_output=True,
        )
        if exists.stdout == b"present":
            with (folder / name).open("wb") as output:
                guest(target, ["cat", "evidence/" + name], stdout=output, timeout=30)


def qmp(target, commands):
    """Use only the verified guest's private QMP socket."""
    if not state(target):
        raise LabError("Guest not running")
    with socket.socket(socket.AF_UNIX) as connection:
        connection.settimeout(10)
        connection.connect(str(LAB / target / "qmp.sock"))
        with connection.makefile("rwb") as stream:
            if "QMP" not in json.loads(stream.readline(1024 * 1024)):
                raise LabError("Unexpected QMP greeting")
            for sequence, command in enumerate(
                [{"execute": "qmp_capabilities"}, *commands]
            ):
                stream.write((json.dumps({**command, "id": sequence}) + "\n").encode())
                stream.flush()
                for _ in range(100):
                    response = json.loads(stream.readline(1024 * 1024))
                    if response.get("id") == sequence:
                        if "error" in response:
                            raise LabError("Guest QMP operation failed")
                        break
                else:
                    raise LabError("No matching QMP response")


def screenshot(target):
    """Capture the guest's virtio display, never the host's desktop."""
    folder = EVIDENCE / target
    folder.mkdir(parents=True, exist_ok=True, mode=0o700)
    qmp(
        target,
        [
            {
                "execute": "screendump",
                "arguments": {
                    "filename": str(folder / "qmp.png"),
                    "format": "png",
                },
            }
        ],
    )
    print(f"{target}: guest virtio framebuffer saved under lab evidence")


def wake(target):
    """Wake the guest output with tablet motion only, after a timed lock probe."""
    record = state(target)
    if not record or not record["restricted"]:
        raise LabError("Waking requires a running restricted-network guest")
    for x in (10000, 10001):
        qmp(
            target,
            [
                {
                    "execute": "input-send-event",
                    "arguments": {
                        "events": [
                            {"type": "abs", "data": {"axis": "x", "value": x}},
                            {"type": "abs", "data": {"axis": "y", "value": 10000}},
                        ]
                    },
                }
            ],
        )
    print(f"{target}: guest tablet moved; no key or button was sent")


def main():
    os.umask(0o077)
    for name in DISPLAY_ENV:
        os.environ.pop(name, None)
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("fetch").add_argument("distro", choices=("arch", "ubuntu"))
    for action in (
        "create",
        "start",
        "stop",
        "wait",
        "provision",
        "artifacts",
        "archive",
        "newuser",
        "release",
        "person",
        "status",
        "ssh",
        "copy",
        "collect",
        "screenshot",
        "wake",
    ):
        child = sub.add_parser(action)
        child.add_argument("target", choices=TARGETS)
        if action == "start":
            child.add_argument("--provision", action="store_true")
        if action == "artifacts":
            child.add_argument("--guest-build", action="store_true")
        if action == "archive":
            child.add_argument("revision")
            child.add_argument("--phone-app", help="signed APK; makes a public archive")
            child.add_argument("--build-tools", help="Android build-tools that check it")
        if action == "release":
            child.add_argument("archive")
        if action == "person":
            child.add_argument("--login", action="store_true")
        if action in ("ssh", "person"):
            child.add_argument("args", nargs=argparse.REMAINDER)
        if action == "copy":
            child.add_argument("source")
            child.add_argument("destination")
    args = parser.parse_args()
    LAB.mkdir(parents=True, exist_ok=True, mode=0o700)
    EVIDENCE.mkdir(parents=True, exist_ok=True, mode=0o700)
    with (LAB / "controller.lock").open("a", encoding="utf-8") as lock:
        # Consent needs a second guest command while the first awaits the dialog.
        fcntl.flock(
            lock,
            fcntl.LOCK_SH
            if args.command
            in (
                "ssh",
                "person",
                "copy",
                "collect",
                "screenshot",
                "wake",
                "status",
                "wait",
            )
            else fcntl.LOCK_EX,
        )
        if args.command == "fetch":
            fetch(args.distro)
        elif args.command == "start":
            start(args.target, args.provision)
        elif args.command == "ssh":
            if not state(args.target):
                raise LabError("Guest not running")
            guest(args.target, args.args)
        elif args.command == "copy":
            if not state(args.target):
                raise LabError("Guest not running")
            copy_file(args.target, args.source, args.destination)
        elif args.command == "status":
            print(json.dumps(state(args.target)))
        elif args.command == "artifacts":
            artifacts(args.target, args.guest_build)
        elif args.command == "archive":
            archive(args.target, args.revision, args.phone_app, args.build_tools)
        elif args.command == "release":
            release(args.target, args.archive)
        elif args.command == "person":
            person(args.target, args.args, args.login)
        else:
            {
                "create": create,
                "stop": stop,
                "wait": wait_ssh,
                "provision": provision,
                "newuser": newuser,
                "collect": collect,
                "screenshot": screenshot,
                "wake": wake,
            }[args.command](args.target)


if __name__ == "__main__":
    try:
        main()
    except (LabError, OSError, ValueError, subprocess.SubprocessError) as error:
        sys.exit(f"Lab failed: {error}")
