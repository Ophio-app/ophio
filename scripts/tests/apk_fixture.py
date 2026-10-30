# SPDX-License-Identifier: Apache-2.0
"""Android apps whose signing blocks name chosen certificates, as
crates/companion/src/phone/apk.rs's tests build them."""
from __future__ import annotations

import hashlib
from pathlib import Path

V2, V3, V31 = 0x7109871A, 0xF05368C0, 0x1B93AD61


def prefixed(part: bytes) -> bytes:
    return len(part).to_bytes(4, "little") + part


def signer(certificate: bytes, v3: bool) -> bytes:
    versions = (28).to_bytes(4, "little") + (36).to_bytes(4, "little") if v3 else b""
    signed = prefixed(prefixed(prefixed(b"digest"))) + prefixed(prefixed(certificate)) + versions + prefixed(b"")
    body = prefixed(signed) + versions + prefixed(prefixed(b"signature")) + prefixed(b"public key")
    return prefixed(body)


def app(pairs: list[tuple[int, bytes]] | None, comment: bytes = b"") -> bytes:
    out = b"PK\x03\x04 entries"
    if pairs is not None:
        body = b"".join((len(value) + 4).to_bytes(8, "little") + id.to_bytes(4, "little") + value for id, value in pairs)
        length = (len(body) + 24).to_bytes(8, "little")
        out += length + body + length + b"APK Sig Block 42"
    directory = len(out)
    entries = b"PK\x01\x02 directory"
    out += entries + b"PK\x05\x06" + bytes(8) + len(entries).to_bytes(4, "little")
    return out + directory.to_bytes(4, "little") + len(comment).to_bytes(2, "little") + comment


def signed_app(certificate: bytes) -> bytes:
    """An app signed in v2 and v3 by `certificate`."""
    return app([(V2, prefixed(signer(certificate, False))), (0x42727A6E, b"padding"), (V3, prefixed(signer(certificate, True)))])


def certificate_digest(certificate: bytes) -> str:
    return hashlib.sha256(certificate).hexdigest()


def companion_pins(public: str, personal: str) -> str:
    """The companion source's two pins and its choice between them."""
    return (
        f'const PUBLIC_CERT_SHA256: &str =\n    "{public}";\n'
        f'const PERSONAL_CERT_SHA256: &str =\n    "{personal}";\n'
        "pub const RELEASE_CERT_SHA256: &str = if ::core::cfg!(ophio_personal_signing) {\n"
        "    PERSONAL_CERT_SHA256\n} else {\n    PUBLIC_CERT_SHA256\n};\n"
    )


def name_release_certificate(root: Path, certificate: bytes, personal: bytes = b"personal development") -> None:
    """Give a fixture repository the companion source naming `certificate` as
    the public release key and `personal` as the development one."""
    source = root / "crates/companion/src/phone/mod.rs"
    source.parent.mkdir(parents=True, exist_ok=True)
    source.write_text(
        companion_pins(certificate_digest(certificate), certificate_digest(personal)), encoding="utf-8"
    )
