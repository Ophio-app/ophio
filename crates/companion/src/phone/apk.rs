// SPDX-License-Identifier: Apache-2.0
//! Who signed an Android app: the certificates its APK Signature Scheme v2
//! and v3 blocks name, read without unpacking the app.
//!
//! This reads the claim, not the proof. Android checks the signature of the
//! newest scheme it knows that the app carries, and refuses one that does
//! not match its certificate. So the app must carry v2, which every Android
//! from 7 on checks before the old JAR signature, and every signer of every
//! scheme must be the release key: whichever one a phone proves is its own.
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

const EOCD: u32 = 0x0605_4b50;
const EOCD_SIZE: u64 = 22;
const MAGIC: &[u8; 16] = b"APK Sig Block 42";
/// Scheme v2, v3 and v3.1.
const SCHEMES: [u32; 3] = [V2, 0xf053_68c0, 0x1b93_ad61];
const V2: u32 = 0x7109_871a;
/// Far larger than any signing block Øphio's releases carry.
const MOST: u64 = 16 << 20;

/// Why an app's signers could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Unreadable {
    /// Not a ZIP archive, so not an Android app.
    NotApp,
    /// Signed only the old way, not at all, or without scheme v2.
    Unsigned,
    /// Its signing block is damaged.
    Damaged,
}

/// The SHA-256 of each signer's certificate, in the v2, v3 and v3.1 blocks
/// alike.
pub(crate) fn signers(path: &Path) -> std::io::Result<Result<Vec<[u8; 32]>, Unreadable>> {
    let mut file = File::open(path)?;
    let size = file.metadata()?.len();
    let Some(directory) = central_directory(&mut file, size)? else {
        return Ok(Err(Unreadable::NotApp));
    };
    if directory < 32 {
        return Ok(Err(Unreadable::Unsigned));
    }
    let mut footer = [0; 24];
    file.seek(SeekFrom::Start(directory - 24))?;
    file.read_exact(&mut footer)?;
    if &footer[8..] != MAGIC {
        return Ok(Err(Unreadable::Unsigned));
    }
    let length = u64::from_le_bytes(footer[..8].try_into().expect("eight bytes"));
    if !(24..=MOST).contains(&length) || length + 8 > directory {
        return Ok(Err(Unreadable::Damaged));
    }
    let mut block =
        vec![0; usize::try_from(length + 8).map_err(|_| std::io::ErrorKind::OutOfMemory)?];
    file.seek(SeekFrom::Start(directory - length - 8))?;
    file.read_exact(&mut block)?;
    if block[..8] != footer[..8] {
        return Ok(Err(Unreadable::Damaged));
    }
    Ok(read_block(&block[8..block.len() - 24]))
}

/// Where the central directory starts, from the end-of-directory record.
fn central_directory(file: &mut File, size: u64) -> std::io::Result<Option<u64>> {
    if size < EOCD_SIZE {
        return Ok(None);
    }
    let tail_size = size.min(EOCD_SIZE + u64::from(u16::MAX));
    let mut tail = vec![0; usize::try_from(tail_size).expect("under 64 KiB")];
    file.seek(SeekFrom::Start(size - tail_size))?;
    file.read_exact(&mut tail)?;
    // The record is followed by its comment and nothing else.
    for at in (0..=tail.len() - 22).rev() {
        let field = |from: usize, to: usize| &tail[at + from..at + to];
        if u32::from_le_bytes(field(0, 4).try_into().expect("four bytes")) != EOCD {
            continue;
        }
        let comment = u16::from_le_bytes(field(20, 22).try_into().expect("two bytes"));
        if at + 22 + usize::from(comment) != tail.len() {
            continue;
        }
        let length = u64::from(u32::from_le_bytes(
            field(12, 16).try_into().expect("four bytes"),
        ));
        let start = u64::from(u32::from_le_bytes(
            field(16, 20).try_into().expect("four bytes"),
        ));
        let record = size - tail_size + at as u64;
        // Android insists the directory runs right up to the record.
        return Ok((start + length == record).then_some(start));
    }
    Ok(None)
}

/// The signers in a block's ID-value pairs.
fn read_block(mut pairs: &[u8]) -> Result<Vec<[u8; 32]>, Unreadable> {
    let mut found = Vec::new();
    let mut v2 = false;
    while !pairs.is_empty() {
        let length = take_u64(&mut pairs).ok_or(Unreadable::Damaged)?;
        let length = usize::try_from(length).map_err(|_| Unreadable::Damaged)?;
        if length < 4 || length > pairs.len() {
            return Err(Unreadable::Damaged);
        }
        let (pair, rest) = pairs.split_at(length);
        pairs = rest;
        let id = u32::from_le_bytes(pair[..4].try_into().expect("four bytes"));
        if SCHEMES.contains(&id) {
            v2 |= id == V2;
            match scheme_signers(&pair[4..]) {
                Some(signers) if !signers.is_empty() => found.extend(signers),
                _ => return Err(Unreadable::Damaged),
            }
        }
    }
    // Without v2, an Android too old for the newer blocks checks only the
    // JAR signature, which is not read here.
    if !v2 {
        return Err(Unreadable::Unsigned);
    }
    Ok(found)
}

/// Each signer's first certificate, its own; any after it are its chain.
fn scheme_signers(mut value: &[u8]) -> Option<Vec<[u8; 32]>> {
    let mut signers = take(&mut value)?;
    let mut found = Vec::new();
    while !signers.is_empty() {
        let mut signer = take(&mut signers)?;
        let mut signed = take(&mut signer)?;
        let _digests = take(&mut signed)?;
        let mut certificates = take(&mut signed)?;
        let certificate = take(&mut certificates)?;
        if certificate.is_empty() {
            return None;
        }
        found.push(Sha256::digest(certificate).into());
    }
    Some(found)
}

/// A part that starts with its length as four little-endian bytes.
fn take<'a>(from: &mut &'a [u8]) -> Option<&'a [u8]> {
    let length = usize::try_from(u32::from_le_bytes(from.get(..4)?.try_into().ok()?)).ok()?;
    let part = from.get(4..4 + length)?;
    *from = &from[4 + length..];
    Some(part)
}

fn take_u64(from: &mut &[u8]) -> Option<u64> {
    let value = u64::from_le_bytes(from.get(..8)?.try_into().ok()?);
    *from = &from[8..];
    Some(value)
}

/// `digest` as lower-case hex, as `apksigner` and `keytool` print it.
pub(crate) fn hex(digest: &[u8; 32]) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn prefixed(part: &[u8]) -> Vec<u8> {
        let mut out = u32::try_from(part.len()).unwrap().to_le_bytes().to_vec();
        out.extend_from_slice(part);
        out
    }

    fn signer(certificate: &[u8], v3: bool) -> Vec<u8> {
        let mut signed = prefixed(&prefixed(&prefixed(b"digest")));
        signed.extend(prefixed(&prefixed(certificate)));
        if v3 {
            signed.extend(28u32.to_le_bytes());
            signed.extend(36u32.to_le_bytes());
        }
        signed.extend(prefixed(b""));
        let mut out = prefixed(&signed);
        if v3 {
            out.extend(28u32.to_le_bytes());
            out.extend(36u32.to_le_bytes());
        }
        out.extend(prefixed(&prefixed(b"signature")));
        out.extend(prefixed(b"public key"));
        prefixed(&out)
    }

    /// An app whose signing block holds these ID-value pairs, or none.
    pub(crate) fn app(pairs: Option<&[(u32, Vec<u8>)]>, comment: &[u8]) -> Vec<u8> {
        let mut out = b"PK\x03\x04 entries".to_vec();
        if let Some(pairs) = pairs {
            let mut body = Vec::new();
            for (id, value) in pairs {
                body.extend((value.len() as u64 + 4).to_le_bytes());
                body.extend(id.to_le_bytes());
                body.extend(value);
            }
            let length = (body.len() + 24) as u64;
            out.extend(length.to_le_bytes());
            out.extend(body);
            out.extend(length.to_le_bytes());
            out.extend(MAGIC);
        }
        let directory = out.len() as u32;
        let entries = b"PK\x01\x02 directory";
        out.extend(entries);
        out.extend(EOCD.to_le_bytes());
        out.extend([0; 8]);
        out.extend((entries.len() as u32).to_le_bytes());
        out.extend(directory.to_le_bytes());
        out.extend((comment.len() as u16).to_le_bytes());
        out.extend(comment);
        out
    }

    /// An app signed in v2 and v3 by `certificate`.
    pub(crate) fn signed_app(certificate: &[u8]) -> Vec<u8> {
        let v2 = prefixed(&signer(certificate, false));
        let v3 = prefixed(&signer(certificate, true));
        app(
            Some(&[
                (SCHEMES[0], v2),
                (0x4272_7a6e, b"padding".to_vec()),
                (SCHEMES[1], v3),
            ]),
            b"",
        )
    }

    fn read(bytes: &[u8]) -> Result<Vec<[u8; 32]>, Unreadable> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.apk");
        std::fs::write(&path, bytes).unwrap();
        signers(&path).unwrap()
    }

    #[test]
    fn reads_each_scheme_s_signer() {
        let digest: [u8; 32] = Sha256::digest(b"release certificate").into();
        assert_eq!(
            read(&signed_app(b"release certificate")),
            Ok(vec![digest, digest])
        );
    }

    #[test]
    fn finds_the_record_behind_a_comment() {
        let v2 = prefixed(&signer(b"cert", false));
        let digest: [u8; 32] = Sha256::digest(b"cert").into();
        let bytes = app(
            Some(&[(SCHEMES[0], v2)]),
            b"a comment with PK\x05\x06 in it",
        );
        assert_eq!(read(&bytes), Ok(vec![digest]));
    }

    #[test]
    fn a_claim_only_newer_phones_check_is_not_enough() {
        // An older phone would check only the JAR signature beside these.
        for scheme in [SCHEMES[1], SCHEMES[2]] {
            let claim = prefixed(&signer(b"release", true));
            assert_eq!(
                read(&app(Some(&[(scheme, claim)]), b"")),
                Err(Unreadable::Unsigned)
            );
        }
    }

    #[test]
    fn every_scheme_s_signer_is_read() {
        let release: [u8; 32] = Sha256::digest(b"release").into();
        let other: [u8; 32] = Sha256::digest(b"other").into();
        let bytes = app(
            Some(&[
                (SCHEMES[0], prefixed(&signer(b"release", false))),
                (SCHEMES[1], prefixed(&signer(b"release", true))),
                (SCHEMES[2], prefixed(&signer(b"other", true))),
            ]),
            b"",
        );
        assert_eq!(read(&bytes), Ok(vec![release, release, other]));
        // A scheme that names no signer is not passed over.
        let bytes = app(
            Some(&[
                (SCHEMES[0], prefixed(&signer(b"release", false))),
                (SCHEMES[1], prefixed(b"")),
            ]),
            b"",
        );
        assert_eq!(read(&bytes), Err(Unreadable::Damaged));
    }

    #[test]
    fn tells_what_is_wrong() {
        assert_eq!(
            read(b"not an archive at all, just some text"),
            Err(Unreadable::NotApp)
        );
        assert_eq!(read(&app(None, b"")), Err(Unreadable::Unsigned));
        assert_eq!(
            read(&app(Some(&[(0x4272_7a6e, b"padding".to_vec())]), b"")),
            Err(Unreadable::Unsigned)
        );
        let cut = prefixed(&signer(b"cert", false)[..20]);
        assert_eq!(
            read(&app(Some(&[(SCHEMES[0], cut)]), b"")),
            Err(Unreadable::Damaged)
        );
        // A directory that does not reach the record is not trusted.
        let mut moved = signed_app(b"cert");
        let at = moved.len() - 6;
        moved[at] = moved[at].wrapping_sub(1);
        assert_eq!(read(&moved), Err(Unreadable::NotApp));
    }

    #[test]
    fn hex_is_lower_case() {
        assert_eq!(hex(&[0xab; 32]), "ab".repeat(32));
    }
}
