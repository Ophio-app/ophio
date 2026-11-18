// SPDX-License-Identifier: Apache-2.0

//! Recoverable Windows replacement, run by a helper outside both installations.
//!
//! The caller holds the installation lock and has stopped its programs. Nothing
//! here modifies an executable in place. Copies and a flushed, immutable journal
//! precede the first rename. Recovery recognizes completed renames by content,
//! including a crash after a rename but before the caller observes its success.
//! This is not a transaction against concurrent filesystem writers.

use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const NAMES: [&str; 3] = ["bin", "app", "installed.json"];
const RECORD: &str = "replacement.json";
const DONE: &str = "replacement.done";
const WORK: &str = "replacement";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
enum Identity {
    File { len: u64, sha256: [u8; 32] },
    Directory(BTreeMap<String, Identity>),
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
enum InstallationBinding {
    Absent,
    Present(Identity),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    version: u32,
    installation: InstallationBinding,
    incoming: [Option<Identity>; 3],
    current: [Option<Identity>; 3],
    previous: Option<Identity>,
    retained: Option<Identity>,
    programs_last: bool,
}

fn invalid(message: &str) -> Error {
    Error::Config(format!(
        "Cannot recover the interrupted Windows update: {message}"
    ))
}

fn metadata(path: &Path) -> Result<Option<fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(value) => {
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                if value.file_attributes() & 0x400 != 0 {
                    return Err(invalid("a reparse point was found"));
                }
            }
            if value.file_type().is_symlink() || (!value.is_file() && !value.is_dir()) {
                return Err(invalid("a link or special file was found"));
            }
            Ok(Some(value))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn ancestors(path: &Path) -> Result<()> {
    for ancestor in path.ancestors() {
        if !ancestor.as_os_str().is_empty() {
            metadata(ancestor)?;
        }
    }
    Ok(())
}

fn identity(path: &Path) -> Result<Option<Identity>> {
    let Some(meta) = metadata(path)? else {
        return Ok(None);
    };
    if meta.is_dir() {
        let mut children = BTreeMap::new();
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| invalid("a filename is not Unicode"))?;
            let value = identity(&entry.path())?.ok_or_else(|| invalid("a file disappeared"))?;
            children.insert(name, value);
        }
        Ok(Some(Identity::Directory(children)))
    } else {
        let mut file = File::open(path)?;
        let mut hash = Sha256::new();
        let mut len = 0;
        let mut bytes = [0; 65536];
        loop {
            let count = file.read(&mut bytes)?;
            if count == 0 {
                break;
            }
            len += count as u64;
            hash.update(&bytes[..count]);
        }
        Ok(Some(Identity::File {
            len,
            sha256: hash.finalize().into(),
        }))
    }
}

/// Reject links, reparse points and special files in a tree or its ancestors.
/// This examines metadata only, without opening or hashing file contents.
/// A missing leaf is permitted so callers can validate a destination before
/// creating it. Callers still need the installation lock against concurrent writes.
pub(super) fn validate_tree(path: &Path) -> Result<()> {
    ancestors(path)?;
    validate_descendants(path)
}

fn validate_descendants(path: &Path) -> Result<()> {
    let Some(meta) = metadata(path)? else {
        return Ok(());
    };
    if meta.is_dir() {
        for entry in fs::read_dir(path)? {
            validate_descendants(&entry?.path())?;
        }
    }
    Ok(())
}

fn directory(path: &Path) -> Result<()> {
    match metadata(path)? {
        Some(meta) if meta.is_dir() => Ok(()),
        _ => Err(invalid(
            "an installation folder is missing or is not a directory",
        )),
    }
}

fn sync_directory(path: &Path) -> Result<()> {
    // Windows does not expose directory FlushFileBuffers through std. Files
    // are explicitly flushed; directory fsync additionally hardens Unix tests.
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    let meta = metadata(from)?.ok_or_else(|| invalid("a snapshot source disappeared"))?;
    if meta.is_dir() {
        fs::create_dir(to)?;
        for entry in fs::read_dir(from)? {
            let entry = entry?;
            copy_tree(&entry.path(), &to.join(entry.file_name()))?;
        }
        sync_directory(to)?;
    } else {
        // Never use hard links: the retained installation must be independent.
        let mut source = File::open(from)?;
        let mut target = OpenOptions::new().write(true).create_new(true).open(to)?;
        std::io::copy(&mut source, &mut target)?;
        target.sync_all()?;
        #[cfg(test)]
        tests::after_copy(from)?;
    }
    boundary()?;
    Ok(())
}

fn remove_tree(path: &Path) -> Result<()> {
    // Validate the whole tree before deleting anything. The caller's installation
    // lock excludes concurrent writers; this is not a hostile-writer boundary.
    identity(path)?;
    remove_validated(path)
}

fn remove_validated(path: &Path) -> Result<()> {
    let Some(meta) = metadata(path)? else {
        return Ok(());
    };
    if meta.is_dir() {
        for entry in fs::read_dir(path)? {
            remove_validated(&entry?.path())?;
        }
        fs::remove_dir(path)?;
    } else {
        fs::remove_file(path)?;
    }
    boundary()?;
    Ok(())
}

fn publish(path: &Path, bytes: &[u8]) -> Result<()> {
    let temp = path.with_extension("new");
    remove_tree(&temp)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    boundary()?;
    if metadata(path)?.is_some() {
        return Err(invalid("a recovery record already exists"));
    }
    fs::rename(&temp, path)?;
    sync_directory(
        path.parent()
            .ok_or_else(|| invalid("a record has no parent"))?,
    )?;
    boundary()?;
    Ok(())
}

// Test-only failure injection also exercises a denied rename on Linux. Native
// Windows tests must separately prove sharing violations from a running binary.
fn writable_files(path: &Path) -> Result<Vec<File>> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        let mut handles = Vec::new();
        let meta = metadata(path)?.ok_or_else(|| invalid("a rename source disappeared"))?;
        if meta.is_dir() {
            for entry in fs::read_dir(path)? {
                handles.extend(writable_files(&entry?.path())?);
            }
        } else {
            // An image mapped as an executable cannot be opened for writing.
            // Allow ordinary sharing while probing; the caller closes all
            // probes before renaming their containing directory.
            handles.push(OpenOptions::new().write(true).share_mode(7).open(path)?);
        }
        Ok(handles)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Ok(Vec::new())
    }
}

fn move_expected(from: &Path, to: &Path, expected: &Identity) -> Result<()> {
    if identity(to)?.as_ref() == Some(expected) {
        return Ok(());
    }
    if identity(to)?.is_some() || identity(from)?.as_ref() != Some(expected) {
        return Err(invalid("a rename's source or destination changed"));
    }
    #[cfg(test)]
    tests::check_lock(from)?;
    let failure = |error: Error| {
        Error::Ended(format!(
            "Windows update could not move {}: {error}. Stop programs using this installation, then retry; the recovery record and earlier installation have been kept.",
            from.display()
        ))
    };
    // A mapped image must refuse the writable probe before anything moves.
    // Even DELETE-sharing file handles prevent Windows from renaming their
    // containing directory, so close our probes before the rename. The caller
    // holds the installation/launch locks against cooperative launches; hostile
    // concurrent installation writers are outside this module's boundary.
    drop(writable_files(from).map_err(failure)?);
    fs::rename(from, to).map_err(|error| failure(error.into()))?;
    sync_directory(
        from.parent()
            .ok_or_else(|| invalid("a source has no parent"))?,
    )?;
    sync_directory(
        to.parent()
            .ok_or_else(|| invalid("a destination has no parent"))?,
    )?;
    boundary()?;
    Ok(())
}

fn check_slot(path: &Path, allowed: &[Option<&Identity>]) -> Result<()> {
    let actual = identity(path)?;
    if !allowed.contains(&actual.as_ref()) {
        return Err(invalid("the recorded installation files changed"));
    }
    Ok(())
}

fn installation_binding(opt: &Path) -> Result<InstallationBinding> {
    let path = opt.join("installation.json");
    let Some(meta) = metadata(&path)? else {
        return Ok(InstallationBinding::Absent);
    };
    if !meta.is_file() {
        return Err(invalid("the permanent installation identity is not a file"));
    }
    let value = identity(&path)?
        .ok_or_else(|| invalid("the permanent installation identity disappeared"))?;
    Ok(InstallationBinding::Present(value))
}

fn check_installation(opt: &Path, journal: &Journal) -> Result<()> {
    if installation_binding(opt)? != journal.installation {
        return Err(invalid("the permanent installation identity changed"));
    }
    Ok(())
}

fn validate_journal(journal: &Journal) -> Result<()> {
    if matches!(&journal.installation, InstallationBinding::Present(value) if !matches!(value, Identity::File { .. }))
    {
        return Err(invalid(
            "the recorded permanent installation identity is not a file",
        ));
    }
    if journal.version != 1 || !matches!(journal.incoming[0], Some(Identity::Directory(_))) {
        return Err(invalid("the recovery record has an unsupported format"));
    }
    for entries in [&journal.incoming, &journal.current] {
        if entries[0]
            .as_ref()
            .is_some_and(|id| !matches!(id, Identity::Directory(_)))
            || entries[1]
                .as_ref()
                .is_some_and(|id| !matches!(id, Identity::Directory(_)))
            || entries[2]
                .as_ref()
                .is_some_and(|id| !matches!(id, Identity::File { .. }))
        {
            return Err(invalid("the recovery record has invalid entry types"));
        }
    }
    if journal
        .previous
        .as_ref()
        .is_some_and(|id| !matches!(id, Identity::Directory(_)))
        || journal
            .retained
            .as_ref()
            .is_some_and(|id| !matches!(id, Identity::Directory(_)))
    {
        return Err(invalid("the recovery record has invalid snapshot types"));
    }
    // A retained snapshot must exactly describe the original root entries.
    if let Some(retained) = &journal.retained {
        let expected = Identity::Directory(
            NAMES
                .iter()
                .zip(&journal.current)
                .filter_map(|(name, value)| value.clone().map(|value| ((*name).into(), value)))
                .collect(),
        );
        if *retained != expected {
            return Err(invalid("the earlier snapshot does not match the install"));
        }
    }
    Ok(())
}

fn check_work_names(opt: &Path) -> Result<()> {
    let work = opt.join(WORK);
    if metadata(&work)?.is_none() {
        return Ok(());
    }
    directory(&work)?;
    for entry in fs::read_dir(&work)? {
        let name = entry?.file_name();
        if name != "incoming" && name != "displaced" && name != "retired-previous" {
            return Err(invalid("an unexpected recovery file was found"));
        }
    }
    for folder in ["incoming", "displaced"] {
        let path = work.join(folder);
        if metadata(&path)?.is_some() {
            directory(&path)?;
            for entry in fs::read_dir(path)? {
                if !NAMES
                    .iter()
                    .any(|name| entry.as_ref().is_ok_and(|entry| entry.file_name() == *name))
                {
                    return Err(invalid("an unexpected recovery entry was found"));
                }
            }
        }
    }
    Ok(())
}

/// Prepare independent snapshots, publish the recovery record, then rename.
/// Callers must serialize this with startup recovery using the install lock.
pub(super) fn replace(
    opt: &Path,
    source: &Path,
    keep_previous: bool,
    programs_last: bool,
) -> Result<()> {
    ancestors(opt)?;
    ancestors(source)?;
    directory(opt)?;
    directory(source)?;
    finish_replacement(opt)?;
    let installation = installation_binding(opt)?;
    let incoming: [Option<Identity>; 3] = [
        identity(&source.join(NAMES[0]))?,
        identity(&source.join(NAMES[1]))?,
        identity(&source.join(NAMES[2]))?,
    ];
    let current: [Option<Identity>; 3] = [
        identity(&opt.join(NAMES[0]))?,
        identity(&opt.join(NAMES[1]))?,
        identity(&opt.join(NAMES[2]))?,
    ];
    let previous = identity(&opt.join("previous"))?;
    let retained = if keep_previous && current[0].is_some() {
        Some(Identity::Directory(
            NAMES
                .iter()
                .zip(&current)
                .filter_map(|(name, value)| value.clone().map(|value| ((*name).into(), value)))
                .collect(),
        ))
    } else {
        None
    };
    let journal = Journal {
        version: 1,
        installation,
        incoming,
        current,
        previous,
        retained,
        programs_last,
    };
    validate_journal(&journal)?;
    // No pending journal means these are abandoned preparation/cleanup only.
    // Snapshot source may be opt/previous (rollback), but not these work paths.
    let canonical_source = fs::canonicalize(source)?;
    for path in [opt.join(WORK), opt.join("previous.partial")] {
        if let Ok(canonical_work) = fs::canonicalize(&path)
            && canonical_source.starts_with(canonical_work)
        {
            return Err(invalid("the snapshot source is a recovery work folder"));
        }
        remove_tree(&path)?;
    }
    fs::create_dir(opt.join(WORK))?;
    fs::create_dir(opt.join(WORK).join("incoming"))?;
    fs::create_dir(opt.join(WORK).join("displaced"))?;
    for (index, name) in NAMES.iter().enumerate() {
        if journal.incoming[index].is_some() {
            copy_tree(
                &source.join(name),
                &opt.join(WORK).join("incoming").join(name),
            )?;
        }
    }
    if journal.retained.is_some() {
        fs::create_dir(opt.join("previous.partial"))?;
        for (index, name) in NAMES.iter().enumerate() {
            if journal.current[index].is_some() {
                copy_tree(&opt.join(name), &opt.join("previous.partial").join(name))?;
            }
        }
        sync_directory(&opt.join("previous.partial"))?;
    }
    // Reject changed sources/copies before committing any recovery operation.
    for (index, name) in NAMES.iter().enumerate() {
        check_slot(
            &opt.join(WORK).join("incoming").join(name),
            &[journal.incoming[index].as_ref()],
        )?;
        check_slot(&source.join(name), &[journal.incoming[index].as_ref()])?;
        check_slot(&opt.join(name), &[journal.current[index].as_ref()])?;
    }
    ancestors(source)?;
    ancestors(opt)?;
    check_installation(opt, &journal)?;
    check_slot(&opt.join("previous.partial"), &[journal.retained.as_ref()])?;
    check_slot(&opt.join("previous"), &[journal.previous.as_ref()])?;
    sync_directory(&opt.join(WORK))?;
    sync_directory(opt)?;
    let bytes =
        serde_json::to_vec(&journal).map_err(|_| invalid("cannot encode the recovery record"))?;
    publish(&opt.join(RECORD), &bytes)?;
    finish_replacement(opt)
}

fn operations<'a>(opt: &Path, journal: &'a Journal) -> Vec<(PathBuf, PathBuf, &'a Identity)> {
    let mut previous = Vec::new();
    if let Some(id) = &journal.previous {
        previous.push((
            opt.join("previous"),
            opt.join(WORK).join("retired-previous"),
            id,
        ));
    }
    if let Some(id) = &journal.retained {
        previous.push((opt.join("previous.partial"), opt.join("previous"), id));
    }
    let mut moves = Vec::new();
    let order = if journal.programs_last {
        [1, 2, 0]
    } else {
        [0, 1, 2]
    };
    for index in order {
        if index == 0 && journal.programs_last {
            moves.append(&mut previous);
        }
        let name = NAMES[index];
        if let Some(id) = &journal.current[index] {
            moves.push((
                opt.join(name),
                opt.join(WORK).join("displaced").join(name),
                id,
            ));
        }
        if let Some(id) = &journal.incoming[index] {
            moves.push((
                opt.join(WORK).join("incoming").join(name),
                opt.join(name),
                id,
            ));
        }
    }
    moves.append(&mut previous);
    moves
}

fn preflight(opt: &Path, journal: &Journal, moves: &[(PathBuf, PathBuf, &Identity)]) -> Result<()> {
    // Simulate the entire replay before touching the live install. Missing
    // payloads (including identical old/new content) must not strand bin.
    let mut slots = BTreeMap::new();
    for (from, to, _) in moves {
        for path in [from, to] {
            if !slots.contains_key(path) {
                slots.insert(path.clone(), identity(path)?);
            }
        }
    }
    for (from, to, wanted) in moves {
        if slots.get(to).and_then(Option::as_ref) == Some(*wanted) {
            continue;
        }
        if slots.get(to).and_then(Option::as_ref).is_some()
            || slots.get(from).and_then(Option::as_ref) != Some(*wanted)
        {
            return Err(invalid("a required rename cannot be recovered"));
        }
        directory(
            to.parent()
                .ok_or_else(|| invalid("a destination has no parent"))?,
        )?;
        slots.insert(from.clone(), None);
        slots.insert(to.clone(), Some((*wanted).clone()));
    }
    for (index, name) in NAMES.iter().enumerate() {
        let path = opt.join(name);
        let expected = journal.incoming[index].as_ref();
        let actual = if let Some(value) = slots.get(&path) {
            value.clone()
        } else {
            identity(&path)?
        };
        if actual.as_ref() != expected {
            return Err(invalid("the replacement cannot reach a complete install"));
        }
    }
    let previous = opt.join("previous");
    let actual = if let Some(value) = slots.get(&previous) {
        value.clone()
    } else {
        identity(&previous)?
    };
    if actual.as_ref() != journal.retained.as_ref() {
        return Err(invalid("the earlier installation cannot be recovered"));
    }
    Ok(())
}

fn check_final(opt: &Path, journal: &Journal) -> Result<()> {
    for (index, name) in NAMES.iter().enumerate() {
        check_slot(&opt.join(name), &[journal.incoming[index].as_ref()])?;
    }
    check_slot(&opt.join("previous"), &[journal.retained.as_ref()])?;
    Ok(())
}

/// Retry an interrupted replacement; locks and changed files remain visible
/// errors, and never cause the journal or the retained installation to be lost.
pub(super) fn finish_replacement(opt: &Path) -> Result<()> {
    ancestors(opt)?;
    directory(opt)?;
    let record = opt.join(RECORD);
    if metadata(&record)?.is_none() {
        // A crash after removing the journal can leave only the completion marker.
        remove_tree(&opt.join(DONE))?;
        return Ok(());
    }
    if !metadata(&record)?.is_some_and(|meta| meta.is_file()) {
        return Err(invalid("the recovery record is not a file"));
    }
    let bytes = fs::read(&record)?;
    let journal: Journal =
        serde_json::from_slice(&bytes).map_err(|_| invalid("cannot read the recovery record"))?;
    validate_journal(&journal)?;
    // Bind both rename replay and completed-transaction cleanup to the same
    // permanent install. Never create a missing identity during recovery.
    check_installation(opt, &journal)?;
    check_work_names(opt)?;
    let digest: [u8; 32] = Sha256::digest(&bytes).into();
    let completed = metadata(&opt.join(DONE))?.is_some();
    if completed {
        if !metadata(&opt.join(DONE))?.is_some_and(|meta| meta.is_file())
            || fs::read(opt.join(DONE))? != digest
        {
            return Err(invalid("the completion marker changed"));
        }
        check_final(opt, &journal)?;
    } else {
        // Validate every live slot before doing even the first rename. Both
        // sides of each interrupted rename are allowed, but changed bytes aren't.
        for (index, name) in NAMES.iter().enumerate() {
            check_slot(
                &opt.join(name),
                &[
                    None,
                    journal.current[index].as_ref(),
                    journal.incoming[index].as_ref(),
                ],
            )?;
            check_slot(
                &opt.join(WORK).join("incoming").join(name),
                &[None, journal.incoming[index].as_ref()],
            )?;
            check_slot(
                &opt.join(WORK).join("displaced").join(name),
                &[None, journal.current[index].as_ref()],
            )?;
        }
        check_slot(
            &opt.join("previous"),
            &[None, journal.previous.as_ref(), journal.retained.as_ref()],
        )?;
        check_slot(
            &opt.join("previous.partial"),
            &[None, journal.retained.as_ref()],
        )?;
        check_slot(
            &opt.join(WORK).join("retired-previous"),
            &[None, journal.previous.as_ref()],
        )?;
        let moves = operations(opt, &journal);
        preflight(opt, &journal, &moves)?;
        for (from, to, expected) in moves {
            move_expected(&from, &to, expected)?;
        }
        check_final(opt, &journal)?;
        publish(&opt.join(DONE), &digest)?;
    }
    // The marker makes interrupted recursive cleanup replayable. In particular,
    // partially deleted displaced trees no longer need to match old identities.
    remove_tree(&opt.join(WORK))?;
    remove_tree(&opt.join("previous.partial"))?;
    fs::remove_file(&record)?;
    sync_directory(opt)?;
    boundary()?;
    remove_tree(&opt.join(DONE))?;
    Ok(())
}

fn boundary() -> Result<()> {
    #[cfg(test)]
    tests::boundary()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct Faults {
        remaining: Option<usize>,
        count: usize,
        locked: Option<PathBuf>,
        mutate_after_copy: Option<PathBuf>,
    }
    thread_local! { static FAULTS: RefCell<Faults> = RefCell::new(Faults::default()); }

    pub(super) fn boundary() -> Result<()> {
        FAULTS.with(|faults| {
            let mut faults = faults.borrow_mut();
            faults.count += 1;
            if let Some(remaining) = faults.remaining.as_mut() {
                if *remaining == 0 {
                    return Err(invalid("injected interruption"));
                }
                *remaining -= 1;
            }
            Ok(())
        })
    }
    pub(super) fn after_copy(path: &Path) -> Result<()> {
        FAULTS.with(|faults| {
            let mut faults = faults.borrow_mut();
            if faults.mutate_after_copy.as_deref() == Some(path) {
                faults.mutate_after_copy = None;
                fs::write(path, "source changed after its snapshot")?;
            }
            Ok(())
        })
    }

    pub(super) fn check_lock(path: &Path) -> Result<()> {
        FAULTS.with(|faults| {
            if faults
                .borrow()
                .locked
                .as_ref()
                .is_some_and(|locked| locked.starts_with(path))
            {
                Err(Error::Ended(
                    "injected sharing violation; retry after stopping the program".into(),
                ))
            } else {
                Ok(())
            }
        })
    }
    fn reset(remaining: Option<usize>) {
        FAULTS.with(|faults| {
            *faults.borrow_mut() = Faults {
                remaining,
                ..Faults::default()
            }
        });
    }
    fn install(path: &Path, version: &str, optional: bool) {
        fs::create_dir_all(path.join("bin")).unwrap();
        fs::write(path.join("bin/ophio.exe"), format!("host-{version}")).unwrap();
        fs::write(path.join("bin/computer-mcp.exe"), format!("mcp-{version}")).unwrap();
        if optional {
            fs::create_dir_all(path.join("app/nested")).unwrap();
            fs::write(path.join("app/nested/main.js"), format!("app-{version}")).unwrap();
            fs::write(path.join("installed.json"), format!("metadata-{version}")).unwrap();
        }
    }
    fn fixture(old: bool, new_optional: bool) -> (tempfile::TempDir, PathBuf, PathBuf) {
        reset(None);
        let temp = tempfile::tempdir().unwrap();
        let opt = temp.path().join("opt");
        let source = temp.path().join("source");
        fs::create_dir(&opt).unwrap();
        if old {
            install(&opt, "old", true);
        }
        install(&source, "new", new_optional);
        (temp, opt, source)
    }
    fn assert_version(path: &Path, version: &str, optional: bool) {
        assert_eq!(
            fs::read_to_string(path.join("bin/ophio.exe")).unwrap(),
            format!("host-{version}")
        );
        assert_eq!(
            fs::read_to_string(path.join("bin/computer-mcp.exe")).unwrap(),
            format!("mcp-{version}")
        );
        assert_eq!(path.join("app").exists(), optional);
        assert_eq!(path.join("installed.json").exists(), optional);
        if optional {
            assert_eq!(
                fs::read_to_string(path.join("app/nested/main.js")).unwrap(),
                format!("app-{version}")
            );
        }
    }
    fn pause_after_journal(opt: &Path, source: &Path) {
        // Denying the first rename leaves a fully durable prepared transaction.
        FAULTS.with(|faults| faults.borrow_mut().locked = Some(opt.join("bin/computer-mcp.exe")));
        assert!(replace(opt, source, true, false).is_err());
        reset(None);
        assert!(opt.join(RECORD).is_file());
    }

    #[test]
    fn installation_binding_rejects_added_removed_or_changed_identity() {
        for change in ["added", "removed", "changed"] {
            let (_temp, opt, source) = fixture(true, true);
            let permanent = opt.join("installation.json");
            let original = b"{\"installation_id\":\"original\"}";
            if change != "added" {
                fs::write(&permanent, original).unwrap();
            }
            pause_after_journal(&opt, &source);
            match change {
                "removed" => fs::remove_file(&permanent).unwrap(),
                _ => fs::write(&permanent, b"{\"installation_id\":\"other\"}").unwrap(),
            }
            let before = identity(&opt).unwrap();
            let record = fs::read(opt.join(RECORD)).unwrap();
            assert!(finish_replacement(&opt).is_err(), "{change}");
            assert_eq!(identity(&opt).unwrap(), before, "{change}");
            assert_eq!(fs::read(opt.join(RECORD)).unwrap(), record);
            assert_version(&opt, "old", true);
            if change == "added" {
                fs::remove_file(&permanent).unwrap();
            } else {
                fs::write(&permanent, original).unwrap();
            }
            finish_replacement(&opt).unwrap();
            assert_version(&opt, "new", true);
        }
    }

    #[test]
    fn installation_binding_is_checked_before_completed_cleanup() {
        let mut covered = false;
        for stop in 0..100 {
            let (_temp, opt, source) = fixture(true, true);
            let permanent = opt.join("installation.json");
            fs::write(&permanent, b"original identity").unwrap();
            reset(Some(stop));
            let _ = replace(&opt, &source, true, false);
            reset(None);
            if !opt.join(DONE).is_file() || !opt.join(RECORD).is_file() {
                continue;
            }
            fs::write(&permanent, b"different identity").unwrap();
            let before = identity(&opt).unwrap();
            assert!(finish_replacement(&opt).is_err());
            assert_eq!(identity(&opt).unwrap(), before);
            assert!(opt.join(RECORD).is_file());
            fs::write(&permanent, b"original identity").unwrap();
            finish_replacement(&opt).unwrap();
            assert_version(&opt, "new", true);
            covered = true;
            break;
        }
        assert!(covered, "must cover the completion-marker cleanup phase");
    }

    #[test]
    fn installation_binding_is_required_in_the_strict_journal() {
        let (_temp, opt, source) = fixture(true, true);
        pause_after_journal(&opt, &source);
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(opt.join(RECORD)).unwrap()).unwrap();
        value.as_object_mut().unwrap().remove("installation");
        fs::write(opt.join(RECORD), serde_json::to_vec(&value).unwrap()).unwrap();
        let before = identity(&opt).unwrap();
        assert!(finish_replacement(&opt).is_err());
        assert_eq!(identity(&opt).unwrap(), before);
        assert!(opt.join(RECORD).is_file());
    }

    #[test]
    fn standalone_updates_preserve_present_or_absent_installation_identity() {
        for present in [false, true] {
            let (_temp, opt, source) = fixture(true, true);
            let permanent = opt.join("installation.json");
            if present {
                fs::write(&permanent, b"permanent identity").unwrap();
            }
            let original = identity(&permanent).unwrap();
            replace(&opt, &source, true, false).unwrap();
            assert_version(&opt, "new", true);
            assert_eq!(identity(&permanent).unwrap(), original);
            replace(&opt, &opt.join("previous"), false, true).unwrap();
            assert_version(&opt, "old", true);
            assert_eq!(identity(&permanent).unwrap(), original);
        }
    }

    #[test]
    fn copy_source_changes_are_rejected_before_journal_publication() {
        let (_temp, opt, source) = fixture(true, true);
        FAULTS.with(|faults| {
            faults.borrow_mut().mutate_after_copy = Some(source.join("bin/ophio.exe"))
        });
        assert!(replace(&opt, &source, true, false).is_err());
        reset(None);
        assert!(!opt.join(RECORD).exists());
        assert_version(&opt, "old", true);
        // The prepared copy still has the original bytes; checking only that
        // copy would miss the original source changing after it was read.
        assert_eq!(
            fs::read_to_string(opt.join("replacement/incoming/bin/ophio.exe")).unwrap(),
            "host-new"
        );
    }

    #[test]
    fn validates_regular_trees_and_missing_destinations() {
        let (_temp, opt, source) = fixture(true, true);
        validate_tree(&source).unwrap();
        validate_tree(&source.join("bin/ophio.exe")).unwrap();
        validate_tree(&opt.join("not-created-yet/child")).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn validation_rejects_link_ancestors_and_descendants() {
        use std::os::unix::fs::symlink;
        let (temp, opt, source) = fixture(true, true);
        let alias = temp.path().join("alias");
        symlink(&source, &alias).unwrap();
        assert!(validate_tree(&alias.join("bin/ophio.exe")).is_err());
        symlink(&source, opt.join("linked-child")).unwrap();
        assert!(validate_tree(&opt).is_err());
        assert_version(&source, "new", true);
    }

    #[test]
    fn replaces_and_rolls_back_independent_snapshots() {
        let (_temp, opt, source) = fixture(true, true);
        replace(&opt, &source, true, false).unwrap();
        assert_version(&opt, "new", true);
        assert_version(&opt.join("previous"), "old", true);
        fs::write(source.join("bin/ophio.exe"), "mutated source").unwrap();
        assert_version(&opt, "new", true);
        replace(&opt, &opt.join("previous"), false, true).unwrap();
        assert_version(&opt, "old", true);
        assert!(!opt.join("previous").exists());
        finish_replacement(&opt).unwrap();
    }

    #[test]
    fn all_boundaries_replay_upgrade_install_removal_and_rollback() {
        for (old, optional, last, rollback) in [
            (true, true, false, false),
            (false, true, false, false),
            (true, false, false, false),
            (true, true, true, false),
            (true, true, true, true),
        ] {
            let mut reached_end = false;
            for stop in 0..100 {
                let (_temp, opt, mut source) = fixture(old, optional);
                if old {
                    install(&opt.join("previous"), "earlier", true);
                }
                if rollback {
                    source = opt.join("previous");
                }
                reset(Some(stop));
                let result = replace(&opt, &source, !rollback, last);
                reset(None);
                if result.is_ok() {
                    reached_end = true;
                    break;
                }
                if opt.join(RECORD).exists() {
                    finish_replacement(&opt).unwrap();
                } else {
                    // Preparation did not commit, or journal removal committed
                    // the already-complete install. Both are safe to retry.
                    finish_replacement(&opt).unwrap();
                    if fs::read_to_string(opt.join("bin/ophio.exe"))
                        .ok()
                        .as_deref()
                        != Some(if rollback { "host-earlier" } else { "host-new" })
                    {
                        replace(&opt, &source, !rollback, last).unwrap();
                    }
                }
                assert_version(&opt, if rollback { "earlier" } else { "new" }, optional);
                if old && !rollback {
                    assert_version(&opt.join("previous"), "old", true);
                }
                if rollback || !old {
                    assert!(!opt.join("previous").exists());
                }
                assert!(!opt.join(RECORD).exists());
                assert!(!opt.join(WORK).exists());
                finish_replacement(&opt).unwrap();
            }
            assert!(
                reached_end,
                "boundary bound must cover the entire transaction"
            );
        }
    }

    #[test]
    fn locked_extra_executable_keeps_prior_and_retries() {
        let (_temp, opt, source) = fixture(true, true);
        install(&opt.join("previous"), "earlier", true);
        pause_after_journal(&opt, &source);
        assert_version(&opt, "old", true);
        assert_version(&opt.join("previous"), "earlier", true);
        assert_version(&opt.join("previous.partial"), "old", true);
        finish_replacement(&opt).unwrap();
        assert_version(&opt, "new", true);
        assert_version(&opt.join("previous"), "old", true);
    }

    #[test]
    fn recovery_can_be_interrupted_at_every_boundary_again() {
        let mut reached_end = false;
        for stop in 0..100 {
            let (_temp, opt, source) = fixture(true, true);
            install(&opt.join("previous"), "earlier", true);
            pause_after_journal(&opt, &source);
            reset(Some(stop));
            let result = finish_replacement(&opt);
            reset(None);
            finish_replacement(&opt).unwrap();
            assert_version(&opt, "new", true);
            assert_version(&opt.join("previous"), "old", true);
            assert!(!opt.join(RECORD).exists());
            assert!(!opt.join(WORK).exists());
            if result.is_ok() {
                reached_end = true;
                break;
            }
        }
        assert!(reached_end);
    }

    #[test]
    fn identical_entries_replay_at_every_boundary() {
        // Cover equal whole releases, equal bin with changed app/metadata, and
        // an existing previous release identical to the retained snapshot.
        for same_app in [false, true] {
            let mut reached_end = false;
            for stop in 0..100 {
                let (_temp, opt, source) = fixture(true, true);
                fs::write(source.join("bin/ophio.exe"), "host-old").unwrap();
                fs::write(source.join("bin/computer-mcp.exe"), "mcp-old").unwrap();
                if same_app {
                    fs::write(source.join("app/nested/main.js"), "app-old").unwrap();
                    fs::write(source.join("installed.json"), "metadata-old").unwrap();
                }
                install(&opt.join("previous"), "old", true);
                let wanted = NAMES.map(|name| identity(&source.join(name)).unwrap());
                reset(Some(stop));
                let result = replace(&opt, &source, true, false);
                reset(None);
                if opt.join(RECORD).exists() || opt.join(DONE).exists() {
                    finish_replacement(&opt).unwrap();
                } else if result.is_err() && opt.join(WORK).exists() {
                    replace(&opt, &source, true, false).unwrap();
                }
                for (index, name) in NAMES.iter().enumerate() {
                    assert_eq!(identity(&opt.join(name)).unwrap(), wanted[index]);
                }
                assert_version(&opt.join("previous"), "old", true);
                if result.is_ok() {
                    reached_end = true;
                    break;
                }
            }
            assert!(reached_end);
        }
    }

    #[test]
    fn identical_content_still_requires_a_complete_rename_plan() {
        let (_temp, opt, source) = fixture(true, true);
        fs::remove_dir_all(&source).unwrap();
        install(&source, "old", true);
        pause_after_journal(&opt, &source);
        fs::remove_dir_all(opt.join("replacement/incoming/bin")).unwrap();
        assert!(finish_replacement(&opt).is_err());
        assert_version(&opt, "old", true);
        assert!(opt.join(RECORD).exists());
    }

    #[test]
    fn a_second_update_cannot_discard_a_pending_locked_update() {
        let (_temp, opt, source) = fixture(true, true);
        pause_after_journal(&opt, &source);
        let record = fs::read(opt.join(RECORD)).unwrap();
        FAULTS.with(|faults| faults.borrow_mut().locked = Some(opt.join("bin/computer-mcp.exe")));
        assert!(replace(&opt, &source, true, false).is_err());
        reset(None);
        assert_eq!(fs::read(opt.join(RECORD)).unwrap(), record);
        assert_version(&opt, "old", true);
        finish_replacement(&opt).unwrap();
    }

    #[test]
    fn tampering_any_recorded_tree_is_rejected_before_moves() {
        for entry in [
            "bin/ophio.exe",
            "app/nested/main.js",
            "installed.json",
            "replacement/incoming/bin/computer-mcp.exe",
            "previous.partial/bin/ophio.exe",
            "previous/bin/ophio.exe",
        ] {
            let (_temp, opt, source) = fixture(true, true);
            install(&opt.join("previous"), "earlier", true);
            pause_after_journal(&opt, &source);
            fs::write(opt.join(entry), "changed").unwrap();
            assert!(finish_replacement(&opt).is_err(), "{entry}");
            assert!(opt.join(RECORD).exists());
            assert!(opt.join("bin").is_dir());
        }
    }

    #[test]
    fn corrupt_journal_and_missing_payload_are_not_discarded() {
        for corrupt in [false, true] {
            let (_temp, opt, source) = fixture(true, true);
            pause_after_journal(&opt, &source);
            if corrupt {
                fs::write(opt.join(RECORD), b"{bad").unwrap();
            } else {
                fs::remove_dir_all(opt.join(WORK).join("incoming/bin")).unwrap();
            }
            assert!(finish_replacement(&opt).is_err());
            assert!(opt.join(RECORD).exists());
            assert_version(&opt.join("previous.partial"), "old", true);
        }
    }

    #[test]
    fn rejects_bad_entry_types_and_journal_paths() {
        let (_temp, opt, source) = fixture(true, true);
        fs::remove_dir_all(source.join("bin")).unwrap();
        fs::write(source.join("bin"), "not a folder").unwrap();
        assert!(replace(&opt, &source, true, false).is_err());
        assert_version(&opt, "old", true);
        fs::remove_file(source.join("bin")).unwrap();
        install(&source, "new", true);
        pause_after_journal(&opt, &source);
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(opt.join(RECORD)).unwrap()).unwrap();
        value["arbitrary_path"] = serde_json::json!("../outside");
        fs::write(opt.join(RECORD), serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(finish_replacement(&opt).is_err());
        assert!(opt.join(RECORD).exists());
        assert_version(&opt, "old", true);
    }

    // Explicit native qualification only. The parent is the original test
    // runner outside the installation; only its disposable copy runs in bin.
    #[cfg(windows)]
    #[test]
    #[ignore = "native Windows locked-image qualification in a disposable install"]
    fn native_locked_image_preserves_install_and_recovers() {
        use std::process::{Child, Command, Stdio};
        use std::time::{Duration, Instant};

        struct ChildGuard(Child);
        impl Drop for ChildGuard {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }

        let (temp, opt, source) = fixture(true, true);
        install(&opt.join("previous"), "earlier", true);
        let image = opt.join("bin/computer-mcp.exe");
        fs::copy(std::env::current_exe().unwrap(), &image).unwrap();
        let before = identity(&opt.join("bin")).unwrap();
        let earlier = identity(&opt.join("previous")).unwrap();
        let markers = temp.path().join("child-markers");
        fs::create_dir(&markers).unwrap();
        fs::write(markers.join("request"), b"disposable-update-image-test").unwrap();
        // libtest names omit the crate prefix returned by module_path!().
        let module = module_path!().split_once("::").unwrap().1;
        let mut command = Command::new(&image);
        command.args([
            "--exact",
            &format!("{module}::native_locked_image_child"),
            "--ignored",
            "--test-threads=1",
        ]);
        command
            .current_dir(&markers)
            .env_clear()
            .env("OPHIO_UPDATE_IMAGE_TEST_MARKERS", &markers)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // These system-directory values contain no account credentials. The
        // child runs one authored test, not application startup or any agent.
        for variable in ["SystemRoot", "WINDIR"] {
            if let Some(value) = std::env::var_os(variable) {
                command.env(variable, value);
            }
        }
        let mut child = ChildGuard(command.spawn().unwrap());
        let deadline = Instant::now() + Duration::from_secs(20);
        while !markers.join("ready").exists() {
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "disposable image exited before readiness"
            );
            assert!(
                Instant::now() < deadline,
                "disposable image did not become ready"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let error = replace(&opt, &source, true, false).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Stop programs using this installation"),
            "{error}"
        );
        assert!(opt.join(RECORD).is_file());
        assert_eq!(identity(&opt.join("bin")).unwrap(), before);
        assert_eq!(identity(&opt.join("previous.partial/bin")).unwrap(), before);
        assert_eq!(identity(&opt.join("previous")).unwrap(), earlier);
        // Retry while it is still mapped must remain a visible recoverable error.
        assert!(finish_replacement(&opt).is_err());
        assert!(opt.join(RECORD).is_file());
        drop(child);
        finish_replacement(&opt).unwrap();
        assert_version(&opt, "new", true);
        assert_eq!(identity(&opt.join("previous/bin")).unwrap(), before);
        assert!(!opt.join(RECORD).exists());
        assert!(!opt.join(WORK).exists());
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "child fixture; invoked only by the disposable locked-image test"]
    fn native_locked_image_child() {
        use std::time::{Duration, Instant};
        let Some(markers) = std::env::var_os("OPHIO_UPDATE_IMAGE_TEST_MARKERS") else {
            return;
        };
        let markers = PathBuf::from(markers);
        assert_eq!(
            fs::read(markers.join("request")).unwrap(),
            b"disposable-update-image-test"
        );
        fs::write(markers.join("ready"), b"ready").unwrap();
        let deadline = Instant::now() + Duration::from_secs(300);
        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("parent did not stop its disposable image within five minutes");
    }

    #[cfg(unix)]
    #[test]
    fn rejects_links_at_source_destination_and_recovery() {
        use std::os::unix::fs::symlink;
        for entry in ["source/bin/link", "opt/bin/link", "opt/previous/link"] {
            let (temp, opt, source) = fixture(true, true);
            fs::create_dir_all(opt.join("previous")).unwrap();
            symlink(temp.path().join("outside"), temp.path().join(entry)).unwrap();
            assert!(replace(&opt, &source, true, false).is_err(), "{entry}");
            assert!(!opt.join(RECORD).exists());
        }
        let (temp, opt, source) = fixture(true, true);
        pause_after_journal(&opt, &source);
        symlink(
            temp.path().join("outside"),
            opt.join("replacement/incoming/bin/link"),
        )
        .unwrap();
        assert!(finish_replacement(&opt).is_err());
        assert!(opt.join(RECORD).exists());
    }
}
