// SPDX-License-Identifier: Apache-2.0
use library::{FsLibrary, LibraryConfig};
use ports::{
    clock::Clock,
    library::{LibraryFs, RootSpec},
};
use protocol::{
    capability::HarnessKind,
    ids::LibraryRootId,
    library::{InstructionScope, InstructionState, RootKind},
    time::Timestamp,
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};
#[derive(Debug)]
struct FixedClock(Instant);
impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        Timestamp(0)
    }
    fn monotonic(&self) -> Instant {
        self.0
    }
}
fn put(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}
async fn fixture(max: usize) -> (tempfile::TempDir, FsLibrary, PathBuf, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let tree = temp.path().join("tree");
    let home = temp.path().join("home");
    let cwd = tree.join("repo/pkg/src");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir_all(tree.join("repo/.git")).unwrap();
    let mut config = LibraryConfig::new(temp.path().join("data"), home.clone());
    config.instructions.ancestor_boundary = Some(tree.clone());
    config.instructions.project_doc_max_bytes = max;
    config.instructions.project_doc_fallback_filenames = vec!["TEAM.md".into()];
    let library = FsLibrary::new(config, Arc::new(FixedClock(Instant::now()))).unwrap();
    library
        .set_roots(vec![RootSpec {
            root_id: LibraryRootId::new(),
            path: tree,
            label: "Fixture".into(),
            kind: RootKind::Project,
            writable: true,
            project_id: None,
            harness: None,
            only: None,
        }])
        .await
        .unwrap();
    (temp, library, home, cwd)
}
#[tokio::test]
async fn claude_order_imports_scopes_missing_and_reported_loaded_are_distinct() {
    let (temp, library, home, cwd) = fixture(32768).await;
    let tree = temp.path().join("tree");
    put(&home, ".claude/CLAUDE.md", "user");
    put(&tree, "CLAUDE.md", "parent");
    put(
        &tree,
        "repo/CLAUDE.md",
        "@docs/guide.md `@ignored.md`\n```\n@also-ignored.md\n```\n",
    );
    put(&tree, "repo/docs/guide.md", "@next.md");
    put(&tree, "repo/docs/next.md", "@guide.md");
    put(&tree, "repo/.claude/CLAUDE.md", "project alternate");
    put(&tree, "repo/CLAUDE.local.md", "personal");
    put(&tree, "repo/pkg/CLAUDE.md", "nested");
    put(&tree, "repo/pkg/src/CLAUDE.md", "cwd");
    put(&home, ".claude/skills/global/SKILL.md", "skill");
    put(&tree, "repo/.claude/agents/review.md", "agent");
    let loaded = tree.join("repo/.claude/CLAUDE.md");
    let sources = library
        .instruction_sources(
            &cwd,
            HarnessKind::ClaudeCode,
            std::slice::from_ref(&loaded),
            true,
        )
        .await;
    let present: Vec<_> = sources
        .iter()
        .filter(|s| s.current_revision.is_some())
        .collect();
    let paths: Vec<_> = present
        .iter()
        .map(|s| PathBuf::from(s.host_path.as_ref().unwrap()))
        .collect();
    assert_eq!(
        &paths[..9],
        &[
            home.join(".claude/CLAUDE.md"),
            tree.join("CLAUDE.md"),
            tree.join("repo/CLAUDE.md"),
            tree.join("repo/docs/guide.md"),
            tree.join("repo/docs/next.md"),
            loaded.clone(),
            tree.join("repo/CLAUDE.local.md"),
            tree.join("repo/pkg/CLAUDE.md"),
            cwd.join("CLAUDE.md")
        ]
    );
    assert_eq!(present[0].scope, InstructionScope::User);
    assert_eq!(present[2].scope, InstructionScope::Project);
    assert_eq!(present[7].scope, InstructionScope::Directory);
    assert_eq!(present[5].state, InstructionState::Loaded);
    assert!(present[5].revision_at_start.is_none());
    assert!(sources.iter().any(|s| s.state == InstructionState::Missing));
    assert!(sources.iter().any(|s| s.scope == InstructionScope::Skill));
    assert!(sources.iter().any(|s| s.scope == InstructionScope::Agent));
    assert!(
        sources
            .iter()
            .all(|s| !s.host_path.as_ref().unwrap().contains("ignored.md"))
    );
    let roots = library.suggested_roots(&tree.join("repo")).await;
    assert_eq!(roots.len(), 2);
    assert_eq!(library.roots().await.len(), 1);
}
#[tokio::test]
async fn codex_selection_root_to_cwd_fallback_and_byte_budget() {
    let (temp, library, home, cwd) = fixture(7).await;
    let tree = temp.path().join("tree");
    put(&home, ".codex/AGENTS.override.md", "");
    put(&home, ".codex/AGENTS.md", "user");
    put(&tree, "AGENTS.md", "outside repo");
    put(&tree, "repo/AGENTS.md", "unused");
    put(&tree, "repo/AGENTS.override.md", "123456");
    put(&tree, "repo/pkg/TEAM.md", "team");
    put(&tree, "repo/pkg/src/AGENTS.md", "leaf");
    let sources = library
        .instruction_sources(&cwd, HarnessKind::Codex, &[], false)
        .await;
    assert_eq!(sources.len(), 4);
    let paths: Vec<_> = sources
        .iter()
        .map(|s| PathBuf::from(s.host_path.as_ref().unwrap()))
        .collect();
    assert_eq!(
        paths,
        [
            home.join(".codex/AGENTS.md"),
            tree.join("repo/AGENTS.override.md"),
            tree.join("repo/pkg/TEAM.md"),
            cwd.join("AGENTS.md")
        ]
    );
    assert!(sources[1].label.contains("3 of 6"));
    assert_eq!(sources[2].state, InstructionState::Unknown);
    assert_eq!(sources[3].state, InstructionState::Unknown);
    assert_eq!(sources[0].scope, InstructionScope::User);
    assert_eq!(sources[1].scope, InstructionScope::Project);
}
#[tokio::test]
async fn imports_are_bounded_and_outside_roots_remain_unknown() {
    let (temp, library, _, cwd) = fixture(32768).await;
    let tree = temp.path().join("tree");
    put(&tree, "repo/CLAUDE.md", "@i1.md\n@../../not-granted.md");
    put(temp.path(), "not-granted.md", "outside every root");
    for n in 1..=6 {
        put(&tree, &format!("repo/i{n}.md"), &format!("@i{}.md", n + 1));
    }
    let sources = library
        .instruction_sources(&cwd, HarnessKind::ClaudeCode, &[], true)
        .await;
    assert!(
        sources
            .iter()
            .any(|s| s.host_path.as_deref().unwrap().ends_with("i4.md"))
    );
    assert!(
        !sources
            .iter()
            .any(|s| s.host_path.as_deref().unwrap().ends_with("i5.md"))
    );
    let outside = sources
        .iter()
        .find(|s| s.host_path.as_deref().unwrap().ends_with("not-granted.md"))
        .unwrap();
    assert_eq!(outside.state, InstructionState::Unknown);
    assert!(outside.current_revision.is_none());
    assert!(
        library
            .instruction_sources(&cwd, HarnessKind::Hermes, &[], false)
            .await
            .is_empty()
    );
}
#[tokio::test]
async fn without_project_settings_claude_runs_read_only_the_project_folders_shared_instructions() {
    let (temp, library, home, cwd) = fixture(32768).await;
    let tree = temp.path().join("tree");
    put(&home, ".claude/CLAUDE.md", "user");
    put(&home, ".claude/skills/global/SKILL.md", "skill");
    put(&tree, "CLAUDE.md", "above the repository");
    put(&tree, "repo/CLAUDE.md", "repository");
    put(&tree, "repo/.claude/agents/review.md", "agent");
    put(&cwd, "CLAUDE.md", "@notes.md");
    put(&cwd, "notes.md", "imported");
    put(&cwd, ".claude/CLAUDE.md", "project alternate");
    put(&cwd, "CLAUDE.local.md", "@local-notes.md");
    put(&cwd, "local-notes.md", "imported by a local file");
    put(&cwd, ".claude/settings.json", "{\"hooks\":{}}");
    let states = |sources: &[protocol::library::InstructionSource]| {
        sources
            .iter()
            .map(|s| (PathBuf::from(s.host_path.as_ref().unwrap()), s.state))
            .collect::<std::collections::HashMap<_, _>>()
    };
    let ignored = library
        .instruction_sources(&cwd, HarnessKind::ClaudeCode, &[], false)
        .await;
    let settings: Vec<_> = ignored
        .iter()
        .filter(|s| s.scope == InstructionScope::Settings)
        .collect();
    assert_eq!(settings.len(), 2);
    assert!(settings.iter().all(|s| s.current_revision.is_none()));
    let ignored = states(&ignored);
    for (path, state) in [
        (home.join(".claude/CLAUDE.md"), InstructionState::Discovered),
        (
            home.join(".claude/skills/global/SKILL.md"),
            InstructionState::Discovered,
        ),
        (cwd.join("CLAUDE.md"), InstructionState::Discovered),
        (cwd.join("notes.md"), InstructionState::Discovered),
        (cwd.join(".claude/CLAUDE.md"), InstructionState::Discovered),
        (tree.join("CLAUDE.md"), InstructionState::NotApplied),
        (tree.join("repo/CLAUDE.md"), InstructionState::NotApplied),
        (
            tree.join("repo/.claude/agents/review.md"),
            InstructionState::NotApplied,
        ),
        (cwd.join("CLAUDE.local.md"), InstructionState::NotApplied),
        (cwd.join("local-notes.md"), InstructionState::NotApplied),
        (
            cwd.join(".claude/settings.json"),
            InstructionState::NotApplied,
        ),
        (
            cwd.join(".claude/settings.local.json"),
            InstructionState::Missing,
        ),
    ] {
        assert_eq!(ignored[&path], state, "{}", path.display());
    }
    let applied = states(
        &library
            .instruction_sources(&cwd, HarnessKind::ClaudeCode, &[], true)
            .await,
    );
    assert!(
        applied
            .values()
            .all(|state| *state != InstructionState::NotApplied)
    );
    assert_eq!(
        applied[&cwd.join(".claude/settings.json")],
        InstructionState::Discovered
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_root_added_through_a_link_reads_the_instructions_named_the_same_way() {
    let temp = tempfile::tempdir().unwrap();
    let real = temp.path().join("real");
    std::fs::create_dir_all(real.join("tree/repo/.git")).unwrap();
    std::os::unix::fs::symlink(&real, temp.path().join("link")).unwrap();
    // The folders as a person named them, one step removed from where they are.
    let tree = temp.path().join("link/tree");
    put(&tree, "repo/CLAUDE.md", "project");
    put(&tree, "repo/.claude/skills/review/SKILL.md", "skill");
    let mut config = LibraryConfig::new(temp.path().join("data"), temp.path().join("home"));
    config.instructions.ancestor_boundary = Some(tree.clone());
    let library = FsLibrary::new(config, Arc::new(FixedClock(Instant::now()))).unwrap();
    library
        .set_roots(vec![RootSpec {
            root_id: LibraryRootId::new(),
            path: tree.clone(),
            label: "Fixture".into(),
            kind: RootKind::Project,
            writable: true,
            project_id: None,
            harness: None,
            only: None,
        }])
        .await
        .unwrap();
    let sources = library
        .instruction_sources(&tree.join("repo"), HarnessKind::ClaudeCode, &[], true)
        .await;
    let named = |path: PathBuf| {
        sources
            .iter()
            .find(|s| s.host_path.as_deref().map(PathBuf::from) == Some(path.clone()))
            .unwrap_or_else(|| panic!("{} expected", path.display()))
    };
    assert!(
        named(tree.join("repo/CLAUDE.md"))
            .current_revision
            .is_some()
    );
    let skill = named(tree.join("repo/.claude/skills/review/SKILL.md"));
    assert_eq!(skill.scope, InstructionScope::Skill);
    assert!(skill.current_revision.is_some());
}

#[tokio::test]
async fn candidates_outside_every_root_are_missing_only_when_absent() {
    let temp = tempfile::tempdir().unwrap();
    let tree = temp.path().join("tree");
    let cwd = tree.join("repo/pkg");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir_all(tree.join("repo/.git")).unwrap();
    let mut config = LibraryConfig::new(temp.path().join("data"), temp.path().join("home"));
    config.instructions.ancestor_boundary = Some(tree.clone());
    let library = FsLibrary::new(config, Arc::new(FixedClock(Instant::now()))).unwrap();
    put(&tree, "CLAUDE.md", "present but not granted");
    put(&tree, "repo/AGENTS.override.md", "present but not granted");
    #[cfg(unix)]
    std::os::unix::fs::symlink("nowhere.md", tree.join("repo/CLAUDE.local.md")).unwrap();
    let state = |sources: &[protocol::library::InstructionSource], path: PathBuf| {
        let source = sources
            .iter()
            .find(|s| s.host_path.as_deref().map(Path::new) == Some(path.as_path()))
            .unwrap_or_else(|| panic!("{} expected", path.display()));
        assert!(source.current_revision.is_none() && source.size.is_none());
        source.state
    };
    let claude = library
        .instruction_sources(&cwd, HarnessKind::ClaudeCode, &[], true)
        .await;
    assert_eq!(
        state(&claude, tree.join("CLAUDE.md")),
        InstructionState::Unknown
    );
    assert_eq!(
        state(&claude, tree.join(".claude/CLAUDE.md")),
        InstructionState::Missing
    );
    assert_eq!(
        state(&claude, tree.join("repo/CLAUDE.md")),
        InstructionState::Missing
    );
    #[cfg(unix)]
    assert_eq!(
        state(&claude, tree.join("repo/CLAUDE.local.md")),
        InstructionState::Unknown
    );
    let codex = library
        .instruction_sources(&cwd, HarnessKind::Codex, &[], true)
        .await;
    assert_eq!(
        state(&codex, tree.join("repo/AGENTS.override.md")),
        InstructionState::Unknown
    );
    assert_eq!(
        state(&codex, cwd.join("AGENTS.md")),
        InstructionState::Missing
    );
    assert!(
        !codex
            .iter()
            .any(|s| s.host_path.as_deref().map(Path::new) == Some(&tree.join("repo/AGENTS.md")))
    );
    // A path the phone shows uses the computer's own separator throughout.
    for source in claude.iter().chain(&codex) {
        let shown = source.host_path.as_deref().unwrap();
        assert!(
            std::path::MAIN_SEPARATOR == '/' || !shown.contains('/'),
            "{shown}"
        );
    }
}
