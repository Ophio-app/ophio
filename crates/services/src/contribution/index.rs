// SPDX-License-Identifier: Apache-2.0
//! Compare staged contents while ignoring Git's disposable stat cache.
//! https://git-scm.com/docs/index-format (SHA-1 index versions 2, 3 and 4).
use super::{Code, pack, validate};
use std::collections::BTreeMap;

fn take<'a>(bytes: &mut &'a [u8], n: usize) -> Result<&'a [u8], Code> {
    let (head, tail) = bytes.split_at_checked(n).ok_or(Code::WorkspaceConflict)?;
    *bytes = tail;
    Ok(head)
}
fn number(bytes: &mut &[u8]) -> Result<u32, Code> {
    Ok(u32::from_be_bytes(take(bytes, 4)?.try_into().unwrap()))
}

/// Rebuild the writer's deterministic index representation from staged
/// entries. `sizes` comes from files verified against the manifest, never
/// from the index stat cache. This retains compatibility with saved manifests
/// containing the digest of the original, zero-stat index.
pub(super) fn unchanged(
    index: &[u8],
    expected: &str,
    sizes: &BTreeMap<String, u32>,
    check: pack::Check<'_>,
) -> Result<bool, Code> {
    let Some(entries) = entries(index, check)? else {
        return Ok(false);
    };
    if entries.len() != sizes.len() {
        return Ok(false);
    }
    let mut canonical = b"DIRC\0\0\0\x02".to_vec();
    canonical.extend_from_slice(&(entries.len() as u32).to_be_bytes());
    for (path, entry) in entries {
        let Some(size) = sizes.get(&path) else {
            return Ok(false);
        };
        let start = canonical.len();
        canonical.extend_from_slice(&[0; 24]);
        canonical.extend_from_slice(&entry[24..28]);
        canonical.extend_from_slice(&[0; 8]);
        canonical.extend_from_slice(&size.to_be_bytes());
        canonical.extend_from_slice(&entry[40..60]);
        canonical.extend_from_slice(&(path.len() as u16).to_be_bytes());
        canonical.extend_from_slice(path.as_bytes());
        canonical.push(0);
        while !(canonical.len() - start).is_multiple_of(8) {
            canonical.push(0);
        }
    }
    canonical.extend_from_slice(&pack::sha1(&[&canonical], check)?);
    Ok(validate::digest(canonical) == expected)
}

pub(super) fn entries(
    index: &[u8],
    check: pack::Check<'_>,
) -> Result<Option<BTreeMap<String, [u8; 62]>>, Code> {
    let end = index.len().checked_sub(20).ok_or(Code::WorkspaceConflict)?;
    let (mut input, checksum) = index.split_at(end);
    if pack::sha1(&[input], check)? != checksum {
        return Err(Code::WorkspaceConflict);
    }
    if take(&mut input, 4)? != b"DIRC" {
        return Err(Code::WorkspaceConflict);
    }
    let version = number(&mut input)?;
    if !(2..=4).contains(&version) {
        return Err(Code::WorkspaceConflict);
    }
    let count = number(&mut input)? as usize;
    if count > pack::ENTRIES {
        return Err(Code::LimitExceeded);
    }
    let mut entries = BTreeMap::new();
    let mut previous = String::new();
    for _ in 0..count {
        check()?;
        let entry_start = input.len();
        let entry = take(&mut input, 62)?;
        let flags = u16::from_be_bytes(entry[60..62].try_into().unwrap());
        // Unmerged entries cannot describe the original stage-zero tree.
        if flags & 0x3000 != 0 {
            return Ok(None);
        }
        if flags & 0x4000 != 0 {
            if version == 2 {
                return Err(Code::WorkspaceConflict);
            }
            // Sparse/intent-to-add entries require reconciliation. The
            // assume-valid cache bit is safe to ignore: files were all read.
            if take(&mut input, 2)? != [0, 0] {
                return Ok(None);
            }
        }
        let mut path = String::new();
        if version == 4 {
            let mut byte = take(&mut input, 1)?[0];
            let mut remove = usize::from(byte & 127);
            while byte & 128 != 0 {
                byte = take(&mut input, 1)?[0];
                remove = remove
                    .checked_add(1)
                    .and_then(|n| n.checked_mul(128))
                    .and_then(|n| n.checked_add(usize::from(byte & 127)))
                    .ok_or(Code::WorkspaceConflict)?;
            }
            let keep = previous
                .len()
                .checked_sub(remove)
                .ok_or(Code::WorkspaceConflict)?;
            path.push_str(previous.get(..keep).ok_or(Code::WorkspaceConflict)?);
        }
        let length = input
            .iter()
            .take(513)
            .position(|&b| b == 0)
            .ok_or(Code::WorkspaceConflict)?;
        path.push_str(
            std::str::from_utf8(take(&mut input, length)?).map_err(|_| Code::WorkspaceConflict)?,
        );
        take(&mut input, 1)?;
        if path.len() > 512 || usize::from(flags & 0x0fff) != path.len() || path <= previous {
            return Err(Code::WorkspaceConflict);
        }
        if version != 4 {
            let padding = (8 - (entry_start - input.len()) % 8) % 8;
            if take(&mut input, padding)?.iter().any(|&b| b != 0) {
                return Err(Code::WorkspaceConflict);
            }
        }
        entries.insert(path.clone(), entry.try_into().unwrap());
        previous = path;
    }
    while !input.is_empty() {
        check()?;
        let signature = take(&mut input, 4)?;
        let length = number(&mut input)? as usize;
        // Optional cache extensions can be ignored. Never interpret a split
        // or sparse index as a complete index, or follow its external files.
        if !signature[0].is_ascii_uppercase() {
            return Ok(None);
        }
        take(&mut input, length)?;
    }
    Ok(Some(entries))
}
