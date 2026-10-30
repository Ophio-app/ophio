// SPDX-License-Identifier: Apache-2.0
//! Bounded Git pack decoding. No external process interprets untrusted objects.
//! Format: https://git-scm.com/docs/pack-format . Delta depth is capped at 32.
use super::{Code, validate};
use flate2::{Decompress, FlushDecompress, Status};
use sha1_checked::{Digest, Sha1};
use std::collections::{BTreeMap, BTreeSet};

pub(super) const EXPANDED: usize = 512 * 1024 * 1024;
pub(super) const ENTRIES: usize = 20_000;
pub(super) const OBJECT: usize = 64 * 1024 * 1024;
pub(super) type Check<'a> = &'a dyn Fn() -> Result<(), Code>;
#[derive(Debug)]
pub(super) struct Object {
    pub kind: u8,
    pub bytes: Vec<u8>,
    depth: u8,
}
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug)]
pub(super) struct File {
    pub oid: String,
    pub executable: bool,
}
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug)]
pub(super) struct Tree {
    pub objects: BTreeMap<String, Object>,
    pub files: BTreeMap<String, File>,
}

pub(super) fn sha1(parts: &[&[u8]], check: Check<'_>) -> Result<[u8; 20], Code> {
    let mut hash = Sha1::new();
    for bytes in parts {
        for chunk in bytes.chunks(65_536) {
            check()?;
            hash.update(chunk);
        }
    }
    let result = hash.try_finalize();
    if result.has_collision() {
        return Err(Code::UnsafeObject);
    }
    Ok((*result.hash()).into())
}
pub(super) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
pub(super) fn kind(k: u8) -> Result<&'static str, Code> {
    match k {
        1 => Ok("commit"),
        2 => Ok("tree"),
        3 => Ok("blob"),
        4 => Ok("tag"),
        _ => Err(Code::UnsafeObject),
    }
}
fn oid(o: &Object, check: Check<'_>) -> Result<String, Code> {
    let head = format!("{} {}\0", kind(o.kind)?, o.bytes.len());
    Ok(hex(&sha1(&[head.as_bytes(), &o.bytes], check)?))
}
fn byte(bytes: &[u8], at: &mut usize) -> Result<u8, Code> {
    let b = *bytes.get(*at).ok_or(Code::UnsafeObject)?;
    *at += 1;
    Ok(b)
}
fn varint(bytes: &[u8], at: &mut usize) -> Result<usize, Code> {
    let mut n = 0;
    for shift in (0..28).step_by(7) {
        let b = byte(bytes, at)?;
        n |= ((b & 127) as usize) << shift;
        if b & 128 == 0 {
            return Ok(n);
        }
    }
    Err(Code::LimitExceeded)
}
fn inflate(bytes: &[u8], expected: usize, check: Check<'_>) -> Result<(Vec<u8>, usize), Code> {
    if expected > OBJECT {
        return Err(Code::LimitExceeded);
    }
    let mut decoder = Decompress::new(true);
    let mut out = Vec::new();
    loop {
        check()?;
        let mut chunk = [0; 65_536];
        let (before_in, before_out) = (decoder.total_in(), decoder.total_out());
        let status = decoder
            .decompress(
                &bytes[before_in as usize..],
                &mut chunk,
                FlushDecompress::None,
            )
            .map_err(|_| Code::UnsafeObject)?;
        let n = (decoder.total_out() - before_out) as usize;
        if n > expected.saturating_sub(out.len()) {
            return Err(Code::LimitExceeded);
        }
        out.extend_from_slice(&chunk[..n]);
        if status == Status::StreamEnd {
            if out.len() != expected {
                return Err(Code::UnsafeObject);
            }
            return Ok((out, decoder.total_in() as usize));
        }
        if before_in == decoder.total_in() && n == 0 {
            return Err(Code::UnsafeObject);
        }
    }
}
#[derive(Debug)]
enum Base {
    Offset(usize),
    Ref(String),
}
#[derive(Debug)]
struct Delta {
    offset: usize,
    base: Base,
    bytes: Vec<u8>,
}
fn apply_delta(
    base: &[u8],
    delta: &[u8],
    remaining: usize,
    check: Check<'_>,
) -> Result<Vec<u8>, Code> {
    let mut at = 0;
    if varint(delta, &mut at)? != base.len() {
        return Err(Code::UnsafeObject);
    }
    let size = varint(delta, &mut at)?;
    if size > OBJECT || size > remaining {
        return Err(Code::LimitExceeded);
    }
    let mut out = Vec::new();
    while at < delta.len() {
        check()?;
        let op = byte(delta, &mut at)?;
        let src = if op & 128 != 0 {
            let mut offset = 0;
            let mut count = 0;
            for i in 0..4 {
                if op & (1 << i) != 0 {
                    offset |= (byte(delta, &mut at)? as usize) << (8 * i);
                }
            }
            for i in 0..3 {
                if op & (1 << (i + 4)) != 0 {
                    count |= (byte(delta, &mut at)? as usize) << (8 * i);
                }
            }
            if count == 0 {
                count = 65_536;
            }
            base.get(offset..offset.checked_add(count).ok_or(Code::UnsafeObject)?)
                .ok_or(Code::UnsafeObject)?
        } else {
            if op == 0 {
                return Err(Code::UnsafeObject);
            }
            let src = delta.get(at..at + op as usize).ok_or(Code::UnsafeObject)?;
            at += op as usize;
            src
        };
        if src.len() > size.saturating_sub(out.len()) {
            return Err(Code::LimitExceeded);
        }
        out.extend_from_slice(src);
    }
    if out.len() != size {
        return Err(Code::UnsafeObject);
    }
    Ok(out)
}

pub(super) fn decode(bytes: &[u8], commit: &str, check: Check<'_>) -> Result<Tree, Code> {
    if bytes.len() > super::network::TRANSFER {
        return Err(Code::LimitExceeded);
    }
    if bytes.len() < 32
        || &bytes[..4] != b"PACK"
        || ![2, 3].contains(&u32::from_be_bytes(bytes[4..8].try_into().unwrap()))
    {
        return Err(Code::UnsafeObject);
    }
    let count = u32::from_be_bytes(bytes[8..12].try_into().unwrap()) as usize;
    if count > ENTRIES * 2 + 1 {
        return Err(Code::LimitExceeded);
    }
    let end = bytes.len() - 20;
    if sha1(&[&bytes[..end]], check)? != bytes[end..] {
        return Err(Code::UnsafeObject);
    }
    let bytes = &bytes[..end];
    let mut at = 12;
    let mut total = 0usize;
    let mut objects = BTreeMap::new();
    let mut offsets = BTreeMap::new();
    let mut deltas = Vec::new();
    for _ in 0..count {
        check()?;
        let offset = at;
        let mut b = byte(bytes, &mut at)?;
        let kind = (b >> 4) & 7;
        let mut size = (b & 15) as usize;
        let mut shift = 4;
        while b & 128 != 0 {
            if shift > 25 {
                return Err(Code::LimitExceeded);
            }
            b = byte(bytes, &mut at)?;
            size |= ((b & 127) as usize) << shift;
            shift += 7;
        }
        total = total
            .checked_add(size)
            .filter(|n| *n <= EXPANDED)
            .ok_or(Code::LimitExceeded)?;
        let base = match kind {
            6 => {
                let mut b = byte(bytes, &mut at)?;
                let mut distance = (b & 127) as usize;
                for _ in 0..8 {
                    if b & 128 == 0 {
                        break;
                    }
                    b = byte(bytes, &mut at)?;
                    distance = distance
                        .checked_add(1)
                        .and_then(|n| n.checked_mul(128))
                        .and_then(|n| n.checked_add((b & 127) as usize))
                        .ok_or(Code::UnsafeObject)?;
                }
                if b & 128 != 0 || distance == 0 {
                    return Err(Code::UnsafeObject);
                }
                Some(Base::Offset(
                    offset.checked_sub(distance).ok_or(Code::UnsafeObject)?,
                ))
            }
            7 => {
                let hash = hex(bytes.get(at..at + 20).ok_or(Code::UnsafeObject)?);
                at += 20;
                Some(Base::Ref(hash))
            }
            1..=4 => None,
            _ => return Err(Code::UnsafeObject),
        };
        let (data, consumed) = inflate(&bytes[at..], size, check)?;
        at += consumed;
        if let Some(base) = base {
            deltas.push(Delta {
                offset,
                base,
                bytes: data,
            });
        } else {
            let o = Object {
                kind,
                bytes: data,
                depth: 0,
            };
            let hash = oid(&o, check)?;
            offsets.insert(offset, hash.clone());
            objects.insert(hash, o);
        }
    }
    if at != bytes.len() {
        return Err(Code::UnsafeObject);
    }
    // Ref deltas can precede their base. Bounded passes prevent cycles and long chains.
    for _ in 0..33 {
        if deltas.is_empty() {
            break;
        }
        let before = deltas.len();
        let mut pending = Vec::new();
        for delta in deltas {
            check()?;
            let hash = match &delta.base {
                Base::Offset(n) => offsets.get(n),
                Base::Ref(h) => Some(h),
            };
            if let Some(base) = hash.and_then(|h| objects.get(h)) {
                if base.depth >= 32 {
                    return Err(Code::LimitExceeded);
                }
                let data = apply_delta(
                    &base.bytes,
                    &delta.bytes,
                    EXPANDED.saturating_sub(total),
                    check,
                )?;
                total = total
                    .checked_add(data.len())
                    .filter(|n| *n <= EXPANDED)
                    .ok_or(Code::LimitExceeded)?;
                let o = Object {
                    kind: base.kind,
                    bytes: data,
                    depth: base.depth + 1,
                };
                let hash = oid(&o, check)?;
                offsets.insert(delta.offset, hash.clone());
                objects.insert(hash, o);
            } else {
                pending.push(delta);
            }
        }
        deltas = pending;
        if deltas.len() == before {
            return Err(Code::UnsafeObject);
        }
    }
    if !deltas.is_empty() {
        return Err(Code::LimitExceeded);
    }
    let c = objects
        .get(commit)
        .filter(|c| c.kind == 1)
        .ok_or(Code::IdentityMismatch)?;
    let line = c
        .bytes
        .split(|b| *b == b'\n')
        .next()
        .ok_or(Code::UnsafeObject)?;
    let root = std::str::from_utf8(line)
        .ok()
        .and_then(|s| s.strip_prefix("tree "))
        .filter(|s| validate::hex(s, 40))
        .ok_or(Code::UnsafeObject)?;
    let mut files = BTreeMap::new();
    let mut seen = BTreeSet::new();
    let mut bytes = 0;
    walk(&objects, root, "", &mut files, &mut seen, &mut bytes, check)?;
    Ok(Tree { objects, files })
}
fn walk(
    objects: &BTreeMap<String, Object>,
    oid: &str,
    prefix: &str,
    files: &mut BTreeMap<String, File>,
    seen: &mut BTreeSet<String>,
    total: &mut usize,
    check: Check<'_>,
) -> Result<(), Code> {
    check()?;
    let data = &objects
        .get(oid)
        .filter(|o| o.kind == 2)
        .ok_or(Code::UnsafeObject)?
        .bytes;
    let mut at = 0;
    while at < data.len() {
        check()?;
        let end = data[at..]
            .iter()
            .position(|b| *b == 0)
            .map(|n| n + at)
            .ok_or(Code::UnsafeObject)?;
        let header = std::str::from_utf8(&data[at..end]).map_err(|_| Code::UnsafePath)?;
        let (mode, name) = header.split_once(' ').ok_or(Code::UnsafeObject)?;
        if name.contains('/') {
            return Err(Code::UnsafePath);
        }
        let path = format!("{prefix}{name}");
        validate::path(&path)?;
        if seen.len() >= ENTRIES || !seen.insert(path.to_ascii_lowercase()) {
            return Err(Code::UnsafePath);
        }
        at = end + 1;
        let oid = hex(data.get(at..at + 20).ok_or(Code::UnsafeObject)?);
        at += 20;
        match mode {
            "40000" => walk(
                objects,
                &oid,
                &format!("{path}/"),
                files,
                seen,
                total,
                check,
            )?,
            "100644" | "100755" => {
                let o = objects
                    .get(&oid)
                    .filter(|o| o.kind == 3)
                    .ok_or(Code::UnsafeObject)?;
                if o.bytes
                    .starts_with(b"version https://git-lfs.github.com/spec/v1")
                {
                    return Err(Code::CompatibilityUnsupported);
                }
                *total = total
                    .checked_add(o.bytes.len())
                    .filter(|n| *n <= EXPANDED)
                    .ok_or(Code::LimitExceeded)?;
                files.insert(
                    path,
                    File {
                        oid,
                        executable: mode == "100755",
                    },
                );
            }
            _ => return Err(Code::UnsafeObject), // symlinks, gitlinks, exotic modes
        }
    }
    Ok(())
}
