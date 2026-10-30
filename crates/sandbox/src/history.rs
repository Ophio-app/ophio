// SPDX-License-Identifier: Apache-2.0
use std::{
    io,
    path::{Path, PathBuf},
};

pub const DIRECTORIES: &[&str] = &[
    "projects",
    "todos",
    "file-history",
    "shell-snapshots",
    "session-env",
    "debug",
];

/// The agent never saved the conversation a turn would carry on from.
#[derive(Debug)]
pub struct Unsaved;
impl std::fmt::Display for Unsaved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("The agent didn't save this conversation's earlier turns, so it can't carry on from them. Start a new conversation to go on.")
    }
}
impl std::error::Error for Unsaved {}

#[derive(Debug)]
pub struct History {
    pub binds: Vec<(PathBuf, PathBuf)>,
    copy_back: Option<(PathBuf, PathBuf)>,
    codex_resumed: bool,
    export: Option<(protocol::capability::HarnessKind, PathBuf, PathBuf, PathBuf)>,
}
impl History {
    /// Native state belongs to this conversation. Only the provider credential
    /// file is shared; global sessions, hooks, MCP servers and databases are not.
    pub fn isolated(
        kind: protocol::capability::HarnessKind,
        config: &Path,
        state: &Path,
        cwd: &Path,
        resume: Option<&str>,
    ) -> io::Result<Self> {
        use protocol::capability::HarnessKind;
        let local = state.join("native");
        crate::sys::directories(&local)?;
        let mut binds = vec![(local.clone(), config.to_owned())];
        let credential = if kind == HarnessKind::Codex {
            "auth.json"
        } else {
            ".credentials.json"
        };
        let auth = config.join(credential);
        if auth.exists() {
            // Validate the file without reading its contents.
            drop(crate::sys::history_file(&auth, false)?);
            binds.push((auth, config.join(credential)));
        }
        if kind == HarnessKind::ClaudeCode {
            let history = Self::claude(config, state, cwd, resume)?;
            binds.extend(history.binds);
            return Ok(Self {
                binds,
                copy_back: history.copy_back,
                codex_resumed: false,
                export: Some((kind, config.to_owned(), state.to_owned(), cwd.to_owned())),
            });
        }
        // A new conversation keeps Codex's sqlite index and rollouts in `local`.
        // For an existing native session, seed only its rollout, never its database.
        if let Some(id) = resume {
            if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
                return Err(io::Error::other("invalid native session id"));
            }
            let sessions = local.join("sessions");
            if session_files(&sessions, id)?.is_empty()
                && !seed_session(&config.join("sessions"), &sessions, id)?
            {
                return Err(io::Error::new(io::ErrorKind::NotFound, Unsaved));
            }
        }
        Ok(Self {
            binds,
            copy_back: None,
            codex_resumed: resume.is_some(),
            export: Some((kind, config.to_owned(), state.to_owned(), cwd.to_owned())),
        })
    }
    pub fn claude(
        config: &Path,
        state: &Path,
        cwd: &Path,
        resume: Option<&str>,
    ) -> io::Result<Self> {
        let root = state.join("claude-history");
        crate::sys::directories(&root)?;
        let mut binds = Vec::new();
        for name in DIRECTORIES {
            let source = root.join(name);
            crate::sys::directories(&source)?;
            binds.push((source, config.join(name)));
        }
        let history = root.join("history.jsonl");
        drop(crate::sys::history_file(&history, true)?);
        binds.push((history, config.join("history.jsonl")));
        let copy_back = if let Some(id) = resume {
            if id.is_empty()
                || id.len() > 128
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            {
                return Err(io::Error::other("invalid native session id"));
            }
            let slug: String = cwd
                .to_string_lossy()
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                .collect();
            let relative = PathBuf::from("projects")
                .join(slug)
                .join(format!("{id}.jsonl"));
            let local = root.join(&relative);
            let real = config.join(relative);
            crate::sys::directories(local.parent().unwrap())?;
            // The conversation copy wins on subsequent turns. Only seed once.
            match crate::sys::history_file(&local, false) {
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    let input =
                        crate::sys::history_file(&real, false).map_err(|e| match e.kind() {
                            io::ErrorKind::NotFound => io::Error::new(e.kind(), Unsaved),
                            _ => e,
                        })?;
                    copy_file(input, &local)?;
                }
                Err(e) => return Err(e),
            }
            Some((local, real))
        } else {
            None
        };
        Ok(Self {
            binds,
            copy_back,
            codex_resumed: false,
            export: None,
        })
    }
    pub fn finished(&self, session: Option<&str>) -> io::Result<()> {
        if let Some((source, destination)) = &self.copy_back {
            crate::sys::directories(destination.parent().unwrap())?;
            copy(source, destination)?;
        }
        if let (Some((kind, config, state, cwd)), Some(id)) = (&self.export, session) {
            // An agent stopped before it saved anything leaves nothing to keep.
            if *kind == protocol::capability::HarnessKind::Codex {
                let sessions = state.join("native/sessions");
                if !seed_session(&sessions, &config.join("sessions"), id)? && self.codex_resumed {
                    // A resumed turn had a transcript before launch. Its loss is
                    // an export failure, not a first turn that saved nothing.
                    return Err(io::Error::other("native session history not found"));
                }
            } else if self.copy_back.is_none() {
                let slug: String = cwd
                    .to_string_lossy()
                    .chars()
                    .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                    .collect();
                let relative = PathBuf::from("projects")
                    .join(slug)
                    .join(format!("{id}.jsonl"));
                let source = state.join("claude-history").join(&relative);
                let input = match crate::sys::history_file(&source, false) {
                    Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
                    opened => opened?,
                };
                let destination = config.join(&relative);
                crate::sys::directories(destination.parent().unwrap())?;
                copy_file(input, &destination)?;
            }
        }
        Ok(())
    }
}
/// Only an absent top-level session directory means nothing was saved.
/// Once discovered, disappearing subdirectories and files are real errors.
fn session_files(root: &Path, id: &str) -> io::Result<Vec<PathBuf>> {
    fn collect(
        root: &Path,
        entries: std::fs::ReadDir,
        suffix: &str,
        files: &mut Vec<PathBuf>,
    ) -> io::Result<()> {
        for entry in entries {
            let entry = entry?;
            let kind = entry.file_type()?;
            let path = root.join(entry.file_name());
            if entry.file_name().to_string_lossy().ends_with(suffix) {
                // Matching links and non-regular files are errors, not absence.
                drop(crate::sys::history_file(&path, false)?);
                files.push(path);
            } else if kind.is_dir() {
                let (_directory, entries) = crate::sys::history_entries(&path)?;
                collect(&path, entries, suffix, files)?;
            }
        }
        Ok(())
    }
    let (_directory, entries) = match crate::sys::history_entries(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut files = Vec::new();
    collect(root, entries, &format!("-{id}.jsonl"), &mut files)?;
    Ok(files)
}
fn seed_session(root: &Path, destination: &Path, id: &str) -> io::Result<bool> {
    let files = session_files(root, id)?;
    for source in &files {
        let target = destination.join(source.strip_prefix(root).map_err(io::Error::other)?);
        crate::sys::directories(target.parent().unwrap())?;
        copy(source, &target)?;
    }
    Ok(!files.is_empty())
}

/// A destination that exists is complete: the copy is published only once it is whole.
fn copy(source: &Path, destination: &Path) -> io::Result<()> {
    copy_file(crate::sys::history_file(source, false)?, destination)
}
fn copy_file(mut input: std::fs::File, destination: &Path) -> io::Result<()> {
    crate::sys::replace_history(destination, |output| {
        #[cfg(test)]
        if let Some(room) = tests::ROOM.get() {
            io::copy(&mut io::Read::take(&mut input, room), output)?;
            return Err(io::ErrorKind::StorageFull.into());
        }
        io::copy(&mut input, output).map(drop)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    thread_local! {
        /// The disk fills after this many bytes of a copy on this thread.
        pub(super) static ROOM: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
    }
    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }
    #[test]
    fn a_turn_stopped_before_the_agent_saved_anything_has_nothing_to_keep() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        for kind in [
            protocol::capability::HarnessKind::ClaudeCode,
            protocol::capability::HarnessKind::Codex,
        ] {
            let state = dir.path().join(format!("{kind:?}"));
            let history =
                History::isolated(kind, &config, &state, Path::new("/repo"), None).unwrap();
            history.finished(None).unwrap();
            history.finished(Some("one")).unwrap();
            assert!(!config.exists());
        }
        assert!(!config.join("projects").exists());
        assert!(!config.join("sessions").exists());
    }
    #[test]
    fn carrying_on_from_a_conversation_never_saved_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        let state = dir.path().join("state");
        let error = History::claude(&config, &state, Path::new("/repo"), Some("one")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(error.get_ref().is_some_and(|e| e.is::<Unsaved>()));
        fs::create_dir_all(config.join("projects/-repo")).unwrap();
        fs::write(config.join("projects/-repo/one.jsonl"), "saved\n").unwrap();
        History::claude(&config, &state, Path::new("/repo"), Some("one")).unwrap();
    }
    #[test]
    fn codex_missing_resume_reports_unsaved_with_or_without_other_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        let state = dir.path().join("state");
        for other in [false, true] {
            if other {
                fs::create_dir_all(config.join("sessions/date")).unwrap();
                fs::write(config.join("sessions/date/rollout-other.jsonl"), "other").unwrap();
            }
            let error = History::isolated(
                protocol::capability::HarnessKind::Codex,
                &config,
                &state,
                Path::new("/repo"),
                Some("one"),
            )
            .unwrap_err();
            assert!(
                error.get_ref().is_some_and(|e| e.is::<Unsaved>()),
                "{error}"
            );
        }
    }
    #[test]
    fn codex_export_reports_history_lookup_errors() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        let config = dir.path().join("config");
        let history = History::isolated(
            protocol::capability::HarnessKind::Codex,
            &config,
            &state,
            Path::new("/repo"),
            None,
        )
        .unwrap();
        std::os::unix::fs::symlink("sessions", state.join("native/sessions")).unwrap();
        assert_eq!(
            history.finished(Some("one")).unwrap_err().raw_os_error(),
            Some(libc::ENOTDIR)
        );
        fs::remove_file(state.join("native/sessions")).unwrap();
        std::os::unix::fs::symlink("missing", state.join("native/sessions")).unwrap();
        assert!(history.finished(Some("one")).is_err());
        assert!(!config.exists());
    }
    #[test]
    fn codex_resumed_history_disappearing_is_not_an_unsaved_first_turn() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        let state = dir.path().join("state");
        fs::create_dir_all(config.join("sessions/date")).unwrap();
        let relative = "sessions/date/rollout-one.jsonl";
        fs::write(config.join(relative), "host").unwrap();
        let history = History::isolated(
            protocol::capability::HarnessKind::Codex,
            &config,
            &state,
            Path::new("/repo"),
            Some("one"),
        )
        .unwrap();
        fs::remove_file(state.join("native").join(relative)).unwrap();
        assert!(history.finished(Some("one")).is_err());
        assert_eq!(fs::read_to_string(config.join(relative)).unwrap(), "host");
    }
    #[test]
    fn resumed_conversations_keep_their_own_copy_after_the_first_seed() {
        for kind in [
            protocol::capability::HarnessKind::ClaudeCode,
            protocol::capability::HarnessKind::Codex,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let config = dir.path().join("config");
            let state = dir.path().join("state");
            let (relative, private) = if kind == protocol::capability::HarnessKind::ClaudeCode {
                ("projects/-repo/one.jsonl", "claude-history")
            } else {
                ("sessions/date/rollout-one.jsonl", "native")
            };
            let host = config.join(relative);
            fs::create_dir_all(host.parent().unwrap()).unwrap();
            fs::write(&host, "original").unwrap();
            let resume =
                || History::isolated(kind, &config, &state, Path::new("/repo"), Some("one"));
            resume().unwrap();
            let local = state.join(private).join(relative);
            fs::write(&local, "conversation's own turns").unwrap();
            fs::write(&host, "stale host copy").unwrap();
            resume().unwrap();
            assert_eq!(
                fs::read_to_string(&local).unwrap(),
                "conversation's own turns"
            );
            fs::remove_file(host).unwrap();
            resume().unwrap();
            assert_eq!(
                fs::read_to_string(local).unwrap(),
                "conversation's own turns"
            );
        }
    }
    #[test]
    fn codex_history_permission_errors_are_not_absence() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let dir = tempfile::tempdir().unwrap();
        // Root bypasses file permissions; the companion runs as its owner.
        if dir.path().metadata().unwrap().uid() == 0 {
            return;
        }
        let config = dir.path().join("config");
        let state = dir.path().join("state");
        let history = History::isolated(
            protocol::capability::HarnessKind::Codex,
            &config,
            &state,
            Path::new("/repo"),
            None,
        )
        .unwrap();
        let native = state.join("native");
        fs::create_dir(native.join("sessions")).unwrap();
        fs::set_permissions(&native, fs::Permissions::from_mode(0o000)).unwrap();
        let result = history.finished(Some("one"));
        fs::set_permissions(native, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        assert!(!config.exists());
    }
    #[test]
    fn codex_matching_links_and_non_regular_files_are_not_missing_history() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        let state = dir.path().join("state");
        let history = History::isolated(
            protocol::capability::HarnessKind::Codex,
            &config,
            &state,
            Path::new("/repo"),
            None,
        )
        .unwrap();
        let sessions = state.join("native/sessions");
        fs::create_dir(&sessions).unwrap();
        let source = sessions.join("rollout-one.jsonl");
        std::os::unix::fs::symlink("missing", &source).unwrap();
        assert!(history.finished(Some("one")).is_err());
        fs::remove_file(&source).unwrap();
        fs::create_dir(&source).unwrap();
        assert!(history.finished(Some("one")).is_err());
        assert!(!config.exists());
    }
    #[test]
    fn finishing_a_new_claude_turn_still_rejects_links_and_non_regular_files() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        let state = dir.path().join("state");
        let history = History::isolated(
            protocol::capability::HarnessKind::ClaudeCode,
            &config,
            &state,
            Path::new("/repo"),
            None,
        )
        .unwrap();
        let project = state.join("claude-history/projects/-repo");
        fs::create_dir(&project).unwrap();
        let source = project.join("one.jsonl");
        std::os::unix::fs::symlink("missing", &source).unwrap();
        assert!(history.finished(Some("one")).is_err());
        fs::remove_file(&source).unwrap();
        fs::create_dir(&source).unwrap();
        assert!(history.finished(Some("one")).is_err());
        assert!(!config.exists());
    }
    #[test]
    fn missing_seed_destination_is_not_reported_as_an_unsaved_conversation() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        let state = dir.path().join("state");
        fs::create_dir_all(config.join("projects/-repo")).unwrap();
        fs::write(config.join("projects/-repo/one.jsonl"), "saved").unwrap();
        let local = state.join("claude-history/projects/-repo");
        crate::sys::BEFORE_LOCK.set(Some(Box::new(move || fs::remove_dir_all(local).unwrap())));
        let error = History::claude(&config, &state, Path::new("/repo"), Some("one")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(!error.get_ref().is_some_and(|e| e.is::<Unsaved>()));
        assert_eq!(
            fs::read_to_string(config.join("projects/-repo/one.jsonl")).unwrap(),
            "saved"
        );
    }
    #[test]
    fn interrupted_claude_seed_is_copied_again() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        let state = dir.path().join("state");
        fs::create_dir_all(config.join("projects/-repo")).unwrap();
        fs::write(config.join("projects/-repo/one.jsonl"), "first\nsecond\n").unwrap();
        ROOM.set(Some(4));
        assert!(History::claude(&config, &state, Path::new("/repo"), Some("one")).is_err());
        let local = state.join("claude-history/projects/-repo");
        assert!(names(&local).is_empty());
        ROOM.set(None);
        History::claude(&config, &state, Path::new("/repo"), Some("one")).unwrap();
        assert_eq!(names(&local), ["one.jsonl"]);
        assert_eq!(
            fs::read_to_string(local.join("one.jsonl")).unwrap(),
            "first\nsecond\n"
        );
    }
    #[test]
    fn interrupted_codex_seed_is_copied_again() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        let state = dir.path().join("state");
        fs::create_dir_all(config.join("sessions/2026/10/11")).unwrap();
        let relative = "sessions/2026/10/11/rollout-date-one.jsonl";
        fs::write(config.join(relative), "first\nsecond\n").unwrap();
        let isolated = || {
            History::isolated(
                protocol::capability::HarnessKind::Codex,
                &config,
                &state,
                Path::new("/repo"),
                Some("one"),
            )
        };
        ROOM.set(Some(4));
        assert!(isolated().is_err());
        let local = state.join("native/sessions/2026/10/11");
        assert!(names(&local).is_empty());
        ROOM.set(None);
        isolated().unwrap();
        assert_eq!(names(&local), ["rollout-date-one.jsonl"]);
        assert_eq!(
            fs::read_to_string(state.join("native").join(relative)).unwrap(),
            "first\nsecond\n"
        );
    }
    #[test]
    fn failed_export_keeps_the_host_transcript() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        let state = dir.path().join("state");
        fs::create_dir_all(config.join("projects/-repo")).unwrap();
        let host = config.join("projects/-repo/one.jsonl");
        fs::write(&host, "host\n").unwrap();
        let history = History::claude(&config, &state, Path::new("/repo"), Some("one")).unwrap();
        fs::write(
            state.join("claude-history/projects/-repo/one.jsonl"),
            "host\nrestricted turn\n",
        )
        .unwrap();
        ROOM.set(Some(3));
        assert!(history.finished(None).is_err());
        assert_eq!(fs::read_to_string(&host).unwrap(), "host\n");
        assert_eq!(names(&config.join("projects/-repo")), ["one.jsonl"]);
        ROOM.set(None);
        history.finished(None).unwrap();
        assert_eq!(
            fs::read_to_string(&host).unwrap(),
            "host\nrestricted turn\n"
        );

        fs::create_dir_all(config.join("sessions/2026/10/11")).unwrap();
        let relative = "sessions/2026/10/11/rollout-date-two.jsonl";
        fs::write(config.join(relative), "host\n").unwrap();
        let history = History::isolated(
            protocol::capability::HarnessKind::Codex,
            &config,
            &state.join("codex"),
            Path::new("/repo"),
            Some("two"),
        )
        .unwrap();
        fs::write(state.join("codex/native").join(relative), "host\nturn\n").unwrap();
        ROOM.set(Some(3));
        assert!(history.finished(Some("two")).is_err());
        assert_eq!(fs::read_to_string(config.join(relative)).unwrap(), "host\n");
        assert_eq!(
            names(&config.join("sessions/2026/10/11")),
            ["rollout-date-two.jsonl"]
        );
    }
    #[test]
    fn staging_left_by_a_killed_copy_is_removed_unless_in_use() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.jsonl");
        fs::write(&source, "whole").unwrap();
        let out = dir.path().join("out");
        fs::create_dir(&out).unwrap();
        let killed = out.join(".one.jsonl.1.0.ophio-partial");
        let live = out.join(".one.jsonl.2.0.ophio-partial");
        let unrelated = out.join(".notes.ophio-partial");
        for path in [&killed, &live, &unrelated] {
            fs::write(path, "part").unwrap();
        }
        let held = fs::File::open(&live).unwrap();
        held.lock().unwrap();
        copy(&source, &out.join("one.jsonl")).unwrap();
        assert_eq!(fs::read_to_string(out.join("one.jsonl")).unwrap(), "whole");
        assert_eq!(
            names(&out),
            [
                ".notes.ophio-partial",
                ".one.jsonl.2.0.ophio-partial",
                "one.jsonl"
            ]
        );
    }
    #[test]
    fn a_copy_whose_staging_is_swept_before_it_locks_still_publishes() {
        let dir = tempfile::tempdir().unwrap();
        let (first, second) = (dir.path().join("first"), dir.path().join("second"));
        fs::write(&first, "first").unwrap();
        fs::write(&second, "second").unwrap();
        let out = dir.path().join("out");
        fs::create_dir(&out).unwrap();
        let destination = out.join("one.jsonl");
        let other = (second.clone(), destination.clone());
        crate::sys::BEFORE_LOCK.set(Some(Box::new(move || copy(&other.0, &other.1).unwrap())));
        copy(&first, &destination).unwrap();
        assert_eq!(fs::read_to_string(&destination).unwrap(), "first");
        assert_eq!(names(&out), ["one.jsonl"]);
    }
    #[test]
    fn export_replaces_a_planted_link_instead_of_writing_through_it() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.jsonl");
        fs::write(&source, "transcript").unwrap();
        let other = dir.path().join("other");
        fs::write(&other, "keep").unwrap();
        let out = dir.path().join("out");
        fs::create_dir(&out).unwrap();
        fs::hard_link(&other, out.join("hard.jsonl")).unwrap();
        std::os::unix::fs::symlink(&other, out.join("soft.jsonl")).unwrap();
        copy(&source, &out.join("hard.jsonl")).unwrap();
        copy(&source, &out.join("soft.jsonl")).unwrap();
        assert_eq!(fs::read_to_string(&other).unwrap(), "keep");
        assert_eq!(
            fs::read_to_string(out.join("hard.jsonl")).unwrap(),
            "transcript"
        );
        assert!(
            fs::symlink_metadata(out.join("soft.jsonl"))
                .unwrap()
                .is_file()
        );
    }
    #[test]
    fn only_resumed_transcript_is_seeded_and_persisted() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        let state = dir.path().join("state");
        fs::create_dir_all(config.join("projects/-repo")).unwrap();
        fs::write(config.join("projects/-repo/one.jsonl"), "one").unwrap();
        fs::write(config.join("projects/-repo/two.jsonl"), "two").unwrap();
        let history = History::claude(&config, &state, Path::new("/repo"), Some("one")).unwrap();
        let local = state.join("claude-history/projects/-repo/one.jsonl");
        assert_eq!(fs::read_to_string(&local).unwrap(), "one");
        assert!(!local.with_file_name("two.jsonl").exists());
        fs::write(&local, "changed").unwrap();
        history.finished(None).unwrap();
        assert_eq!(
            fs::read_to_string(config.join("projects/-repo/one.jsonl")).unwrap(),
            "changed"
        );
        assert!(History::claude(&config, &state, Path::new("/repo"), Some("../bad")).is_err());
    }
    #[test]
    fn codex_isolates_native_state_and_exports_only_the_named_session() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        let state = dir.path().join("state");
        fs::create_dir_all(config.join("sessions/2026/10/11")).unwrap();
        fs::write(config.join("auth.json"), "fixture login").unwrap();
        let relative = "sessions/2026/10/11/rollout-date-one.jsonl";
        fs::write(config.join(relative), "one").unwrap();
        let other = "sessions/2026/10/11/rollout-date-two.jsonl";
        fs::write(config.join(other), "two").unwrap();
        let history = History::isolated(
            protocol::capability::HarnessKind::Codex,
            &config,
            &state,
            Path::new("/repo"),
            Some("one"),
        )
        .unwrap();
        assert_eq!(history.binds[0], (state.join("native"), config.clone()));
        assert!(!state.join("native").join(other).exists());
        fs::write(state.join("native").join(relative), "changed").unwrap();
        fs::write(state.join("native").join(other), "must not export").unwrap();
        history.finished(Some("one")).unwrap();
        assert_eq!(
            fs::read_to_string(config.join(relative)).unwrap(),
            "changed"
        );
        assert_eq!(fs::read_to_string(config.join(other)).unwrap(), "two");
    }
    #[test]
    fn symlinked_history_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("state/claude-history");
        fs::create_dir_all(&root).unwrap();
        std::os::unix::fs::symlink(dir.path(), root.join("projects")).unwrap();
        assert!(
            History::claude(
                &dir.path().join("config"),
                &dir.path().join("state"),
                Path::new("/repo"),
                None
            )
            .is_err()
        );
    }
}
