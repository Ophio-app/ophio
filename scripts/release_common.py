# SPDX-License-Identifier: Apache-2.0
"""Shared, dependency-free release identity and manifest helpers."""

from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import sys
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parents[1]

TARGET_TRIPLES = {
    "linux": "x86_64-unknown-linux-gnu",
    "windows": "x86_64-pc-windows-gnullvm",
    "android": "aarch64-linux-android",
}


class ReleaseError(Exception):
    """An input cannot support a trustworthy release artifact."""


def readable_output() -> None:
    """Release output includes the non-ASCII display name; a locale that cannot
    show it gets an escape rather than a failed build."""
    sys.stdout.reconfigure(errors="backslashreplace")


def _prefix_forms(path: Path) -> list[Path]:
    """The absolute path as given and with symlinks resolved, without repeats."""
    given = Path(os.path.abspath(path))
    resolved = path.resolve()
    return [given] if given == resolved else [given, resolved]


def release_environment(
    artifact: str, root: Path = ROOT, staged: Path | None = None
) -> dict[str, str]:
    """Preserve target flags while removing builder paths from diagnostics.

    The target folder may be anywhere, including a private temporary folder
    outside the checkout or around it, so it is remapped as well. `staged` is a
    copy of the checkout built in place of it (Android builds one under the
    target folder); it is remapped like the checkout itself. Where folders nest
    or share a leading name, the longest matching prefix applies (see
    `_ordered`).
    """
    from sign_android import public_build_check

    public_build_check(root)
    environment = dict(os.environ)
    target_key = (
        "CARGO_TARGET_"
        + TARGET_TRIPLES[artifact].upper().replace("-", "_")
        + "_RUSTFLAGS"
    )
    if "CARGO_ENCODED_RUSTFLAGS" in environment:
        flags = environment["CARGO_ENCODED_RUSTFLAGS"].split("\x1f")
    else:
        flags = shlex.split(
            environment.get("RUSTFLAGS", environment.get(target_key, ""))
        )
    flags = [flag for flag in flags if flag]
    # Tauri normally supplies these target flags. Encoded flags take precedence,
    # so retain the Android system libraries explicitly in this release profile.
    if artifact == "android":
        flags.extend(
            ["-Clink-arg=-landroid", "-Clink-arg=-llog", "-Clink-arg=-lOpenSLES"]
        )
    roots = [(root, "/src")]
    for variable, suffix, replacement in (
        ("CARGO_HOME", ".cargo", "/cargo"),
        ("RUSTUP_HOME", ".rustup", "/rustup"),
    ):
        roots.append(
            (Path(environment.get(variable, str(Path.home() / suffix))), replacement)
        )
    roots.append((Path(environment.get("CARGO_TARGET_DIR", str(root / "target"))), "/target"))
    if staged is not None:
        roots.append((staged, "/src"))
    prefixes = _ordered(roots)
    for source, replacement in prefixes:
        flags.append(f"--remap-path-prefix={source}={replacement}")
    environment["CARGO_ENCODED_RUSTFLAGS"] = "\x1f".join(flags)
    # Native C/C++ __FILE__ strings need the equivalent mapping.
    _add_c_flags(
        environment, [f"-ffile-prefix-map={source}={replacement}" for source, replacement in prefixes]
    )
    return environment


def _ordered(roots: list[tuple[Path, str]]) -> list[tuple[Path, str]]:
    """Mappings ordered by the length of their prefix text, shortest first.

    rustc applies the last mapping whose prefix matches a path by whole
    folders, GCC the last whose prefix matches its text (so `/a/work` also
    matches `/a/work-copy/x.c`), and Clang the most specific one. Any prefix
    that matches a path, by folders or by text, is shorter than the path, so
    of two matching prefixes the longer is the more specific and comes later:
    it wins in all three. Two different prefixes of the same length cannot
    both match one path. For the same folder named twice, the later root wins.
    """
    chosen: dict[Path, str] = {}
    for source, replacement in roots:
        for form in _prefix_forms(source):
            chosen.pop(form, None)
            chosen[form] = replacement
    return sorted(chosen.items(), key=lambda item: len(str(item[0])))


# cc-rs reads these through one parser: CFLAGS, CFLAGS_<target>,
# TARGET_CFLAGS and HOST_CFLAGS, and likewise for the others.
_CC_FLAG_VARIABLE = re.compile(r"(?:(?:TARGET|HOST)_)?(?:C|CXX|AR|RANLIB)FLAGS(?:_.+)?")
_ASCII_WHITESPACE = re.compile(r"[ \t\n\x0c\r]+")


def _add_c_flags(environment: dict[str, str], added: list[str]) -> None:
    """Append flags to CFLAGS and CXXFLAGS as cc-rs will split them.

    cc-rs splits these variables on ASCII whitespace unless
    CC_SHELL_ESCAPED_FLAGS is set, when it parses them as shell words. A flag
    containing whitespace needs shell parsing; existing flags are then quoted
    so that each keeps the meaning whitespace splitting gave it.
    """
    escaped = environment.get("CC_SHELL_ESCAPED_FLAGS", "") not in ("", "0", "false", "no")
    if not escaped and any(_ASCII_WHITESPACE.search(flag) for flag in added):
        for name, value in list(environment.items()):
            if _CC_FLAG_VARIABLE.fullmatch(name):
                words = [word for word in _ASCII_WHITESPACE.split(value) if word]
                environment[name] = " ".join(shlex.quote(word) for word in words)
        environment["CC_SHELL_ESCAPED_FLAGS"] = "1"
        escaped = True
    words = " ".join(shlex.quote(flag) if escaped else flag for flag in added)
    for variable in ("CFLAGS", "CXXFLAGS"):
        environment[variable] = (environment.get(variable, "") + " " + words).strip()


def identity(root: Path = ROOT) -> tuple[str, str, str]:
    # This is the single source of the generated distribution's display name.
    brand = (root / "client/src/brand.ts").read_text(encoding="utf-8")
    match = re.search(r'export const PRODUCT_NAME = "([^"\n]+)";', brand)
    if not match:
        raise ReleaseError("Cannot read PRODUCT_NAME from the brand module")
    name = match[1]
    # File names use the command's name: the display name need not be ASCII.
    program = re.search(r'export const PROGRAM_NAME = "([a-z0-9-]+)";', brand)
    if not program:
        raise ReleaseError("Cannot read PROGRAM_NAME from the brand module")
    slug = program[1]
    workspace = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
    version = workspace["workspace"]["package"]["version"]
    if not slug or not re.fullmatch(r"\d+\.\d+\.\d+(?:[-+][A-Za-z0-9.-]+)?", version):
        raise ReleaseError("Invalid distribution name/version")
    for path in ("client/package.json", "client/src-tauri/tauri.conf.json"):
        data = json.loads((root / path).read_text(encoding="utf-8"))
        if data["version"] != version:
            raise ReleaseError(f"Version differs from workspace: {path}")
    if (
        json.loads(
            (root / "client/src-tauri/tauri.conf.json").read_text(encoding="utf-8")
        )["productName"]
        != name
    ):
        raise ReleaseError("Tauri productName differs from the brand module")
    native = tomllib.loads(
        (root / "client/src-tauri/Cargo.toml").read_text(encoding="utf-8")
    )
    if native["package"]["version"] != version:
        raise ReleaseError("Native client version differs from workspace")
    return name, slug, version


def notice(root: Path = ROOT) -> str:
    name, _, _ = identity(root)
    return (
        f"{name}\nCopyright 2026 the contributors\n\n"
        "Product-owned source code is licensed under the Apache License, Version 2.0.\n"
        "See LICENSE. Third-party components retain their own licences and notices;\n"
        "see THIRD-PARTY-NOTICES.txt and sbom.cdx.json in each distribution.\n\n"
        "This software is based in part on the work of the Independent JPEG Group.\n\n"
        "Its table of HTML named character references is the HTML Living Standard's,\n"
        "https://html.spec.whatwg.org/multipage/named-characters.html,\n"
        "Copyright WHATWG (Apple, Google, Mozilla, Microsoft), licensed under the\n"
        "Creative Commons Attribution 4.0 International License,\n"
        "https://creativecommons.org/licenses/by/4.0/. It is reproduced in Rust form.\n"
    )


def atomic_write(path: Path, data: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as temp:
        temporary = Path(temp.name)
        temp.write(data)
    try:
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def digest(path: Path) -> str:
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def require_licences(directory: Path, artifact: str, root: Path = ROOT) -> None:
    """Archive entry points cannot accidentally package a failed audit."""
    report = json.loads((directory / "license-report.json").read_text(encoding="utf-8"))
    bom = json.loads((directory / "sbom.cdx.json").read_text(encoding="utf-8"))
    metadata = bom["metadata"]
    properties = {p["name"]: p["value"] for p in metadata["properties"]}
    if (
        report.get("artifact") != artifact
        or report.get("failures") != []
        or properties.get("distribution:inventory-complete") != "true"
        or properties.get("distribution:artifact") != artifact
        or metadata["component"]["version"] != identity(root)[2]
        or (directory / "NOTICE").read_text(encoding="utf-8") != notice(root)
    ):
        raise ReleaseError(
            f"{artifact} licence evidence is failed, stale or incomplete"
        )


def manifest(root: Path = ROOT) -> str:
    dist = root / "dist"
    lines = ["# file\tsize_bytes\tsha256"]
    for path in sorted(dist.rglob("*")):
        if path.is_file() and path.name != "MANIFEST.txt":
            if path.is_symlink():
                raise ReleaseError("Distribution contains a symbolic link")
            lines.append(
                f"{path.relative_to(dist).as_posix()}\t{path.stat().st_size}\t{digest(path)}"
            )
    result = "\n".join(lines) + "\n"
    atomic_write(dist / "MANIFEST.txt", result.encode())
    return result


# APK Signature Scheme v2, v3 and v3.1 block IDs.
APK_SCHEMES = (0x7109871A, 0xF05368C0, 0x1B93AD61)


def _rust_tokens(source: str) -> list[str]:
    """Lex enough Rust to keep comments and literal contents out of pin lookup.

    This is not a Rust evaluator: only plain literal pins and a fixed choice are supported.
    Keep whole string/character literals as tokens, including raw/byte/C strings,
    and handle nested block comments rather than stripping comments with a regex.
    """
    tokens = []
    at = 0
    while at < len(source):
        if source[at].isspace():
            at += 1
        elif source.startswith("//", at):
            end = source.find("\n", at + 2)
            at = len(source) if end < 0 else end + 1
        elif source.startswith("/*", at):
            at += 2
            depth = 1
            while depth and at < len(source):
                if source.startswith("/*", at):
                    depth += 1
                    at += 2
                elif source.startswith("*/", at):
                    depth -= 1
                    at += 2
                else:
                    at += 1
            if depth:
                raise ReleaseError("Unterminated comment in companion certificate source")
        elif raw := re.match(r'(?:br|cr|r)(#*)"', source[at:]):
            end = source.find('"' + raw[1], at + raw.end())
            if end < 0:
                raise ReleaseError("Unterminated raw string in companion certificate source")
            end += 1 + len(raw[1])
            tokens.append(source[at:end])
            at = end
        elif quoted := re.match(r'(?:b|c)?"', source[at:]):
            end = at + quoted.end()
            while end < len(source) and source[end] != '"':
                end += 2 if source[end] == "\\" else 1
            if end >= len(source):
                raise ReleaseError("Unterminated string in companion certificate source")
            tokens.append(source[at:end + 1])
            at = end + 1
        elif character := re.match(r"b?'(?:\\(?:u\{[0-9a-fA-F_]+\}|x[0-9a-fA-F]{2}|[^\n])|[^'\\\n])'", source[at:]):
            # A lifetime such as 'static is not a character literal.
            tokens.append(character[0])
            at += character.end()
        elif identifier := re.match(r"(?:r#)?[A-Za-z_][A-Za-z_0-9]*", source[at:]):
            tokens.append(identifier[0].removeprefix("r#"))
            at += identifier.end()
        else:
            tokens.append(source[at])
            at += 1
    return tokens


# How the companion chooses its pin; crates/companion/build.rs sets the cfg.
CERTIFICATE_CHOICE = [
    "if", ":", ":", "core", ":", ":", "cfg", "!", "(", "ophio_personal_signing", ")",
    "{", "PERSONAL_CERT_SHA256", "}", "else", "{", "PUBLIC_CERT_SHA256", "}",
]


def personal_signing() -> bool:
    """Whether this is a personal build, trusting the development certificate
    as crates/companion/build.rs does for the same setting."""
    mode = os.environ.get("OPHIO_PERSONAL_SIGNING", "0")
    if mode not in {"0", "1"}:
        raise ReleaseError("OPHIO_PERSONAL_SIGNING must be 0 or 1")
    return mode == "1"


def companion_certificates(root: Path = ROOT) -> tuple[str, str]:
    """The public and personal pins in the companion source, read from their
    unique literal declarations, never a comment or string example, with the
    companion's choice between them checked to be the expected one."""
    source = (root / "crates/companion/src/phone/mod.rs").read_text(encoding="utf-8")
    tokens = _rust_tokens(source)
    names = ("PUBLIC_CERT_SHA256", "PERSONAL_CERT_SHA256", "RELEASE_CERT_SHA256")
    declarations: dict[str, list[tuple[int, bool]]] = {name: [] for name in names}
    nesting = []
    closing = {"}": "{", ")": "(", "]": "["}
    for at, token in enumerate(tokens):
        if token == "const" and at + 1 < len(tokens) and tokens[at + 1] in declarations:
            declarations[tokens[at + 1]].append((at, bool(nesting)))
        if token in closing.values():
            nesting.append(token)
        elif token in closing:
            if not nesting or nesting.pop() != closing[token]:
                raise ReleaseError("Unbalanced companion certificate source")
    if nesting:
        raise ReleaseError("Unbalanced companion certificate source")
    values = {}
    for name in names:
        found = declarations[name]
        if len(found) != 1 or found[0][1]:
            raise ReleaseError(f"The companion must declare exactly one top-level {name} constant")
        declaration = found[0][0]
        start = declaration - 1 if declaration and tokens[declaration - 1] == "pub" else declaration
        # Only bare declarations are evaluated here. An attribute can disable a
        # literal and leave an imported or macro-generated replacement in use.
        if start and tokens[start - 1] not in {";", "}"}:
            raise ReleaseError(f"The companion's {name} must be an unattributed plain constant")
        at = declaration + 2
        if tokens[at:at + 4] != [":", "&", "str", "="]:
            raise ReleaseError(f"The companion's {name} must be a &str constant")
        values[name] = at + 4
    pins = []
    for name in names[:2]:
        at = values[name]
        literal = re.fullmatch(r'"([0-9a-f]{64})"', tokens[at]) if at < len(tokens) else None
        if not literal or tokens[at + 1:at + 2] != [";"]:
            raise ReleaseError(f"The companion's {name} must be a lowercase SHA-256 literal")
        pins.append(literal[1])
    at = values["RELEASE_CERT_SHA256"]
    end = at + len(CERTIFICATE_CHOICE)
    if tokens[at:end] != CERTIFICATE_CHOICE or tokens[end:end + 1] != [";"]:
        raise ReleaseError("The companion must choose RELEASE_CERT_SHA256 between its two literal pins")
    public, personal = pins
    if public == personal:
        raise ReleaseError("The companion's public and personal pins must differ")
    return public, personal


def release_certificate(root: Path = ROOT) -> str:
    """The certificate this build's companion trusts: the public pin, or the
    personal one for a build made with OPHIO_PERSONAL_SIGNING=1."""
    public, personal = companion_certificates(root)
    return personal if personal_signing() else public


def require_executable_trust(executable: bytes, root: Path = ROOT) -> None:
    """Refuse stale or mixed companion builds before assembling an archive.

    Release binaries contain the selected certificate's ASCII hex text, not
    the unused pin. Check the exact bytes that the archive will carry.
    """
    selected = release_certificate(root)
    public, personal = companion_certificates(root)
    other = personal if selected == public else public
    if selected.encode("ascii") not in executable or other.encode("ascii") in executable:
        raise ReleaseError(
            "The companion executable was built with a different signing setting; "
            "rebuild it with the same OPHIO_PERSONAL_SIGNING value used for packaging"
        )


def apk_signers(data: bytes) -> list[str]:
    """The SHA-256 of each signer's certificate in an APK's v2, v3 and v3.1
    blocks, as crates/companion/src/phone/apk.rs reads them. A v2 block is
    required: without it an Android too old for the newer blocks checks only
    the JAR signature, which is not read here."""

    def number(at: int, size: int) -> int:
        return int.from_bytes(data[at : at + size], "little")

    record = next(
        (
            at
            for at in range(len(data) - 22, max(-1, len(data) - 22 - 0xFFFF - 1), -1)
            if data[at : at + 4] == b"PK\x05\x06" and at + 22 + number(at + 20, 2) == len(data)
        ),
        None,
    )
    if record is None:
        raise ReleaseError("The phone app is not an Android app")
    start = number(record + 16, 4)
    if start + number(record + 12, 4) != record:
        raise ReleaseError("The phone app is not an Android app")
    if start < 32 or data[start - 16 : start] != b"APK Sig Block 42":
        raise ReleaseError("The phone app is not signed with APK Signature Scheme v2 or v3")
    length = number(start - 24, 8)
    if not 24 <= length <= 16 << 20 or length + 8 > start or number(start - length - 8, 8) != length:
        raise ReleaseError("The phone app's signing block is damaged")

    def take(part: memoryview) -> tuple[memoryview, memoryview]:
        if len(part) < 4:
            raise ReleaseError("The phone app's signing block is damaged")
        size = int.from_bytes(part[:4], "little")
        if size > len(part) - 4:
            raise ReleaseError("The phone app's signing block is damaged")
        return part[4 : 4 + size], part[4 + size :]

    pairs = memoryview(data)[start - length : start - 24]
    found, v2 = [], False
    while pairs:
        size = int.from_bytes(pairs[:8], "little") if len(pairs) >= 8 else 0
        if not 4 <= size <= len(pairs) - 8:
            raise ReleaseError("The phone app's signing block is damaged")
        pair, pairs = pairs[8 : 8 + size], pairs[8 + size :]
        scheme = int.from_bytes(pair[:4], "little")
        if scheme not in APK_SCHEMES:
            continue
        v2 |= scheme == APK_SCHEMES[0]
        signers, _ = take(pair[4:])
        if not signers:
            raise ReleaseError("The phone app's signing block is damaged")
        while signers:
            signer, signers = take(signers)
            signed_data, _ = take(signer)
            _digests, rest = take(signed_data)
            certificates, _ = take(rest)
            certificate, _ = take(certificates)
            if not certificate:
                raise ReleaseError("The phone app's signing block is damaged")
            found.append(hashlib.sha256(certificate).hexdigest())
    if not v2:
        raise ReleaseError("The phone app is not signed with APK Signature Scheme v2")
    return found


def _phone_app_input(root: Path) -> tuple[Path, bytes, str, bool] | None:
    from sign_android import public_build_check

    public = public_build_check(root)
    named = os.environ.get("OPHIO_PHONE_APK")
    if not named:
        return None
    path = Path(named)
    if path.is_symlink() or not path.is_file():
        raise ReleaseError("OPHIO_PHONE_APK is not a file")
    return path, path.read_bytes(), release_certificate(root), public


def _check_phone_signers(data: bytes, certificate: str, root: Path) -> None:
    signers = apk_signers(data)
    if not signers:
        raise ReleaseError("The phone app has no release signer")
    other = [signer for signer in signers if signer != certificate]
    if other and not personal_signing() and other[0] == companion_certificates(root)[1]:
        raise ReleaseError(
            f"The phone app is signed with {other[0]}, the personal development key, "
            "which only a personal build trusts; set OPHIO_PERSONAL_SIGNING=1 for one"
        )
    if other:
        raise ReleaseError(f"The phone app is signed with {other[0]}, not Øphio's release key")


def phone_app(root: Path = ROOT) -> Path | None:
    """Legacy personal-build path lookup; a source path is not a verified copy.

    Public archives must consume phone_app_bytes so replacing the input after
    verification cannot substitute unverified bytes into the archive.
    """
    app = _phone_app_input(root)
    if app is None:
        return None
    path, data, certificate, public = app
    if public:
        raise ReleaseError("Public packaging must consume verified phone_app_bytes, not a source path")
    _check_phone_signers(data, certificate, root)
    return path


def phone_app_bytes(root: Path = ROOT) -> bytes | None:
    """Snapshot the bundled APK; public mode verifies exactly these staged bytes.

    Personal packaging retains its existing certificate-only check and needs no
    Android tools. Public packaging never signs or chooses tools from PATH/SDK
    defaults, and never returns a path that an archive could reopen after checks.
    """
    from sign_android import verify

    app = _phone_app_input(root)
    if app is None:
        return None
    _, data, certificate, public = app
    if not public:
        _check_phone_signers(data, certificate, root)
        return data
    selected = os.environ.get("OPHIO_ANDROID_BUILD_TOOLS", "")
    tools = Path(selected)
    if not selected or not tools.is_absolute():
        raise ReleaseError("Public APK verification requires an absolute OPHIO_ANDROID_BUILD_TOOLS directory")
    try:
        for tool in ("apksigner", "zipalign"):
            if not (tools / tool).is_file():
                raise ReleaseError(f"Public APK verification build-tools missing {tool}")
        with tempfile.TemporaryDirectory(prefix="ophio-verify-phone-") as directory:
            staged = Path(directory) / "phone.apk"
            staged.write_bytes(data)
            # Trusted SDK tools verify the copied input, not a mutable source.
            # verify also requires v3, checks every v2/v3 signer, and checks alignment.
            verify(staged, tools, certificate)
    except (OSError, subprocess.SubprocessError) as error:
        raise ReleaseError("Public APK signature verification failed; Android tools unavailable or APK rejected") from error
    return data
