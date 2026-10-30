// SPDX-License-Identifier: Apache-2.0
use super::*;
use protocol::{launchers::SkillCopy, time::Timestamp};

fn catalog_entry(kind: CatalogKind, source: CatalogSource) -> CatalogEntry {
    CatalogEntry {
        kind,
        source,
        id: "folder".into(),
        name: "Named".into(),
        description: None,
        digest: String::new(),
        host_path: "/tmp/folder/SKILL.md".into(),
        library: None,
        harness: None,
        model: None,
        effort: None,
        launch_policy: None,
        copy: None,
        disable: None,
    }
}

fn agent_file(
    group: AgentFileGroup,
    harness: Option<HarnessKind>,
    scope: &str,
    shown: &str,
) -> AgentFile {
    AgentFile {
        root_id: protocol::LibraryRootId::new(),
        path: shown.into(),
        shown: shown.into(),
        title: None,
        description: None,
        group,
        scope: scope.into(),
        everywhere: false,
        project_id: None,
        harness,
        size: 0,
        modified_at: None,
        host_path: String::new(),
        disable: None,
    }
}

#[test]
fn catalog_entries_are_classified_without_allowing_builtin_entries_or_copies() {
    let entry = catalog_entry(CatalogKind::Skill, CatalogSource::OphioSetup);
    assert_eq!(entry_block(&entry), Err(BUILT_IN));
    let mut copy = catalog_entry(CatalogKind::Skill, CatalogSource::ClaudeUser);
    copy.copy = Some(SkillCopy { newer: false });
    assert_eq!(entry_block(&copy), Err(BUILT_IN));
    for (kind, source, expected) in [
        (CatalogKind::Agent, CatalogSource::Ophio, Block::Ophio),
        (
            CatalogKind::Agent,
            CatalogSource::ClaudeUser,
            Block::ClaudeAgent {
                name: "Named".into(),
            },
        ),
        (
            CatalogKind::Agent,
            CatalogSource::ClaudeProject,
            Block::ClaudeAgent {
                name: "Named".into(),
            },
        ),
        (
            CatalogKind::Skill,
            CatalogSource::ClaudeUser,
            Block::ClaudeSkill {
                name: "folder".into(),
            },
        ),
        (
            CatalogKind::Skill,
            CatalogSource::ClaudeProject,
            Block::ClaudeSkill {
                name: "folder".into(),
            },
        ),
        (
            CatalogKind::Skill,
            CatalogSource::CodexUser,
            Block::CodexSkill {
                name: "Named".into(),
            },
        ),
        (
            CatalogKind::Skill,
            CatalogSource::CodexProject,
            Block::CodexSkill {
                name: "Named".into(),
            },
        ),
    ] {
        assert_eq!(entry_block(&catalog_entry(kind, source)), Ok(expected));
    }
}

#[test]
fn agent_files_are_classified_by_where_the_agent_discovers_them() {
    use AgentFileGroup::*;
    use HarnessKind::*;
    let dirs = LauncherDirs::default();
    let check = |group, harness, scope, shown, path: &str, expected| {
        assert_eq!(
            file_block(
                &agent_file(group, harness, scope, shown),
                Path::new(path),
                &dirs,
                &["builtin"]
            ),
            expected,
            "{path}"
        );
    };
    check(
        Skills,
        Some(ClaudeCode),
        "project",
        "alpha/SKILL.md",
        "/project/.claude/skills/alpha/SKILL.md",
        Ok(Block::ClaudeSkill {
            name: "alpha".into(),
        }),
    );
    check(
        Skills,
        Some(ClaudeCode),
        "Claude Code",
        "alpha/SKILL.md",
        "/home/sam/.claude/skills/alpha/SKILL.md",
        Ok(Block::ClaudeSkill {
            name: "alpha".into(),
        }),
    );
    check(
        Skills,
        Some(ClaudeCode),
        "project",
        "alpha/notes.md",
        "/project/.claude/skills/alpha/notes.md",
        Err(PART_OF_SKILL),
    );
    check(
        Skills,
        Some(ClaudeCode),
        "x plugin",
        "alpha/SKILL.md",
        "/plugin/alpha/SKILL.md",
        Err(PLUGIN),
    );
    check(
        Agents,
        Some(ClaudeCode),
        "x plugin",
        "name.md",
        "/plugin/agents/name.md",
        Ok(Block::ClaudeAgent {
            name: "x:name".into(),
        }),
    );
    check(
        Commands,
        Some(ClaudeCode),
        "project",
        "sub/do.md",
        "/project/.claude/commands/sub/do.md",
        Ok(Block::ClaudeSkill {
            name: "sub:do".into(),
        }),
    );
    check(
        Commands,
        Some(Codex),
        "Codex",
        "do.md",
        "/home/sam/.codex/prompts/do.md",
        Err(CODEX_PROMPTS),
    );
    check(
        Instructions,
        Some(ClaudeCode),
        "project",
        "CLAUDE.md",
        "/project/CLAUDE.md",
        Ok(Block::ClaudeFile),
    );
    check(
        Rules,
        Some(ClaudeCode),
        "project",
        "rule.md",
        "/project/.claude/rules/rule.md",
        Ok(Block::ClaudeFile),
    );
    check(
        Rules,
        Some(ClaudeCode),
        "Claude Code",
        "style.md",
        "/home/sam/.claude/output-styles/style.md",
        Ok(Block::ClaudeOutputStyle {
            name: "style".into(),
        }),
    );
    check(
        Instructions,
        Some(Codex),
        "project",
        "AGENTS.md",
        "/project/AGENTS.md",
        Ok(Block::CodexDoc),
    );
    let mut global = agent_file(Instructions, Some(Codex), "Codex", "AGENTS.md");
    global.everywhere = true;
    assert_eq!(
        file_block(&global, Path::new("/home/sam/.codex/AGENTS.md"), &dirs, &[]),
        Err(CODEX_GLOBAL)
    );
    check(
        Memory,
        Some(ClaudeCode),
        "Claude Code",
        "MEMORY.md",
        "/home/sam/.claude/projects/-project/memory/MEMORY.md",
        Ok(Block::ClaudeMemory {
            slug: "-project".into(),
        }),
    );
    check(
        Memory,
        Some(ClaudeCode),
        "Claude Code",
        "topic.md",
        "/home/sam/.claude/projects/-project/memory/topic.md",
        Err(MEMORY_TOPIC),
    );
    check(
        Skills,
        Some(Hermes),
        "Hermes",
        "alpha/SKILL.md",
        "/home/sam/.hermes/alpha/SKILL.md",
        Err(NOT_RUN),
    );
    check(
        Agents,
        None,
        "Ophio",
        "builtin.md",
        "/home/sam/Ophio/Agents/builtin.md",
        Err(BUILT_IN),
    );
}

fn stored(block: Block, path: &Path) -> Stored {
    Stored {
        item: DisabledItem {
            kind: block.kind(),
            host_path: path.to_string_lossy().into_owned(),
            name: "name".into(),
            description: None,
            scope: String::new(),
            harness: None,
            project_id: None,
            group: None,
            library: None,
            disabled_at: Timestamp(1),
            found: true,
        },
        block,
        listed: Vec::new(),
    }
}

/// A file with `text` at `path`, its folders made.
fn file(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn place(cwd: &Path) -> RunPlace<'_> {
    RunPlace {
        cwd,
        project_ids: Vec::new(),
        project_settings: false,
    }
}

#[test]
fn a_run_excludes_the_relevant_native_entries_only() {
    let tmp = tempfile::tempdir().unwrap();
    let root = real(tmp.path());
    let home = root.join("claude");
    file(&home.join("settings.json"), r#"{"outputStyle":"Quiet"}"#);
    let dirs = LauncherDirs {
        claude_home: Some(home.clone()),
        ..LauncherDirs::default()
    };
    let cwd = root.join("project");
    std::fs::create_dir_all(cwd.join(".git")).unwrap();
    let at = |rest: &str| {
        let path = root.join(rest);
        file(&path, "x");
        path
    };
    let entries = [
        stored(
            Block::ClaudeSkill {
                name: "alpha".into(),
            },
            &at("claude/skills/alpha/SKILL.md"),
        ),
        stored(
            Block::CodexSkill {
                name: "beta".into(),
            },
            &at("codex/skills/beta/SKILL.md"),
        ),
        stored(
            Block::ClaudeAgent {
                name: "reviewer".into(),
            },
            &at("claude/agents/reviewer.md"),
        ),
        stored(Block::ClaudeFile, &at("project/CLAUDE.md")),
        stored(Block::CodexDoc, &at("project/AGENTS.md")),
        // Another folder's instructions, which a run here never reads.
        stored(Block::ClaudeFile, &at("elsewhere/CLAUDE.md")),
        // Gone since: nothing to keep from the run.
        stored(Block::ClaudeFile, &root.join("project/gone/CLAUDE.md")),
        stored(
            Block::ClaudeMemory {
                slug: slug(cwd.to_str().unwrap()),
            },
            &at("claude/projects/x/memory/MEMORY.md"),
        ),
        stored(
            Block::ClaudeOutputStyle {
                name: "Quiet".into(),
            },
            &at("claude/output-styles/Quiet.md"),
        ),
        stored(Block::Ophio, &at("ophio/Agents/ophio.md")),
    ];
    let result = for_run(&entries, &place(&cwd), &dirs);
    let names: Vec<_> = result
        .skills
        .iter()
        .map(|s| (s.name.as_str(), s.harness))
        .collect();
    assert_eq!(
        names,
        [
            ("alpha", Some(HarnessKind::ClaudeCode)),
            ("beta", Some(HarnessKind::Codex))
        ]
    );
    assert_eq!(result.agents.len(), 1);
    assert_eq!(result.files, [cwd.join("CLAUDE.md"), cwd.join("AGENTS.md")]);
    assert!(result.memory);
    assert!(result.output_style);
    let different = for_run(&entries, &place(&root.join("elsewhere")), &dirs);
    assert!(!different.memory);
    assert_eq!(different.files, [root.join("elsewhere/CLAUDE.md")]);
    file(&home.join("settings.json"), r#"{"outputStyle":"Other"}"#);
    assert!(!for_run(&entries, &place(&cwd), &dirs).output_style);
}

#[test]
fn another_project_s_own_items_are_left_out_of_a_run() {
    let tmp = tempfile::tempdir().unwrap();
    let skill = real(tmp.path()).join("skills/own/SKILL.md");
    file(&skill, "x");
    let mut own = stored(Block::ClaudeSkill { name: "own".into() }, &skill);
    let project = protocol::ProjectId::new();
    own.item.project_id = Some(project);
    let dirs = LauncherDirs::default();
    let elsewhere = RunPlace {
        cwd: tmp.path(),
        project_ids: vec![protocol::ProjectId::new()],
        project_settings: false,
    };
    assert!(for_run(std::slice::from_ref(&own), &elsewhere, &dirs).is_empty());
    let here = RunPlace {
        project_ids: vec![project],
        ..elsewhere
    };
    assert_eq!(for_run(&[own], &here, &dirs).skills.len(), 1);
}

#[test]
fn memory_is_the_main_working_tree_s_for_a_subfolder_and_a_worktree() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = real(tmp.path()).join("repo");
    std::fs::create_dir_all(repo.join(".git/worktrees/wt")).unwrap();
    std::fs::create_dir_all(repo.join("packages/api")).unwrap();
    let worktree = real(tmp.path()).join("wt");
    std::fs::create_dir_all(&worktree).unwrap();
    std::fs::write(
        worktree.join(".git"),
        format!("gitdir: {}\n", repo.join(".git/worktrees/wt").display()),
    )
    .unwrap();
    std::fs::write(repo.join(".git/worktrees/wt/commondir"), "../..\n").unwrap();
    assert_eq!(memory_root(&repo.join("packages/api")), repo);
    assert_eq!(memory_root(&worktree), repo);
    let outside = real(tmp.path()).join("loose");
    std::fs::create_dir_all(&outside).unwrap();
    assert_eq!(memory_root(&outside), outside);

    let memory = repo.join("mem/MEMORY.md");
    file(&memory, "x");
    let kept = [stored(
        Block::ClaudeMemory {
            slug: slug(repo.to_str().unwrap()),
        },
        &memory,
    )];
    let dirs = LauncherDirs::default();
    assert!(for_run(&kept, &place(&repo.join("packages/api")), &dirs).memory);
    assert!(for_run(&kept, &place(&worktree), &dirs).memory);
    assert!(!for_run(&kept, &place(&outside), &dirs).memory);
}

#[test]
fn the_output_style_in_effect_is_the_project_s_when_its_settings_are_read() {
    let tmp = tempfile::tempdir().unwrap();
    let root = real(tmp.path());
    let home = root.join("claude");
    file(&home.join("settings.json"), r#"{"outputStyle":"User"}"#);
    let cwd = root.join("project");
    file(
        &cwd.join(".claude/settings.json"),
        r#"{"outputStyle":"Project"}"#,
    );
    let dirs = LauncherDirs {
        claude_home: Some(home.clone()),
        ..LauncherDirs::default()
    };
    let style = |name: &str| {
        let path = home.join(format!("output-styles/{name}.md"));
        file(&path, "x");
        stored(Block::ClaudeOutputStyle { name: name.into() }, &path)
    };
    let with = |project_settings| RunPlace {
        cwd: &cwd,
        project_ids: Vec::new(),
        project_settings,
    };
    assert!(for_run(&[style("User")], &with(false), &dirs).output_style);
    assert!(!for_run(&[style("User")], &with(true), &dirs).output_style);
    assert!(for_run(&[style("Project")], &with(true), &dirs).output_style);
    file(
        &cwd.join(".claude/settings.local.json"),
        r#"{"outputStyle":"Local"}"#,
    );
    assert!(for_run(&[style("Local")], &with(true), &dirs).output_style);
    assert!(!for_run(&[style("Project")], &with(true), &dirs).output_style);
}

#[cfg(unix)]
#[test]
fn a_linked_skill_is_named_and_kept_as_listed() {
    let tmp = tempfile::tempdir().unwrap();
    let root = real(tmp.path());
    let kit = root.join("src/kit/bar/SKILL.md");
    file(&kit, "x");
    let home = root.join("claude");
    std::fs::create_dir_all(home.join("skills")).unwrap();
    std::os::unix::fs::symlink(root.join("src/kit/bar"), home.join("skills/foo")).unwrap();
    let listed = home.join("skills/foo/SKILL.md");
    let dirs = LauncherDirs {
        claude_home: Some(home.clone()),
        ..LauncherDirs::default()
    };
    let file_entry = agent_file(
        AgentFileGroup::Skills,
        Some(HarnessKind::ClaudeCode),
        "Claude Code",
        "foo/SKILL.md",
    );
    assert_eq!(
        file_block(&file_entry, &listed, &dirs, &[]),
        Ok(Block::ClaudeSkill { name: "foo".into() })
    );
    // Kept by where it leads; the run is told both paths, by its listed name.
    let mut kept = stored(Block::ClaudeSkill { name: "foo".into() }, &kit);
    kept.listed = vec![listed.to_string_lossy().into_owned()];
    let run = for_run(&[kept], &place(&root), &dirs);
    let mut paths: Vec<_> = run.skills.iter().map(|s| s.path.clone()).collect();
    paths.sort();
    let mut want = vec![kit, listed];
    want.sort();
    assert_eq!(paths, want);
    assert!(run.skills.iter().all(|s| s.name == "foo"));
}

#[cfg(unix)]
#[test]
fn a_linked_claude_folder_gives_files_by_both_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let root = real(tmp.path());
    let data = root.join("data/claude");
    file(&data.join("CLAUDE.md"), "x");
    let home = root.join("claude");
    std::os::unix::fs::symlink(&data, &home).unwrap();
    let dirs = LauncherDirs {
        claude_home: Some(home.clone()),
        ..LauncherDirs::default()
    };
    let kept = stored(Block::ClaudeFile, &data.join("CLAUDE.md"));
    let run = for_run(&[kept], &place(&root.join("project")), &dirs);
    assert_eq!(run.files, [home.join("CLAUDE.md"), data.join("CLAUDE.md")]);
}

#[test]
fn rules_in_a_folder_named_output_styles_are_still_rules() {
    let dirs = LauncherDirs::default();
    let rule = agent_file(
        AgentFileGroup::Rules,
        Some(HarnessKind::ClaudeCode),
        "p",
        "output-styles/x.md",
    );
    assert_eq!(
        file_block(
            &rule,
            Path::new("/home/sam/p/.claude/rules/output-styles/x.md"),
            &dirs,
            &[]
        ),
        Ok(Block::ClaudeFile)
    );
    assert!(matches!(
        file_block(
            &rule,
            Path::new("/home/sam/p/.claude/output-styles/x.md"),
            &dirs,
            &[]
        ),
        Ok(Block::ClaudeOutputStyle { .. })
    ));
}

#[cfg(unix)]
#[test]
fn every_link_to_a_disabled_skill_is_blocked_for_each_agent_that_reaches_it() {
    let tmp = tempfile::tempdir().unwrap();
    let root = real(tmp.path());
    let kit = root.join("kit/shared");
    file(&kit.join("SKILL.md"), "x");
    let claude = root.join("claude");
    let agents = root.join("agents");
    std::fs::create_dir_all(claude.join("skills")).unwrap();
    std::fs::create_dir_all(agents.join("skills")).unwrap();
    for name in ["alpha", "beta"] {
        std::os::unix::fs::symlink(&kit, claude.join("skills").join(name)).unwrap();
    }
    std::os::unix::fs::symlink(&kit, agents.join("skills/gamma")).unwrap();
    let dirs = LauncherDirs {
        claude_home: Some(claude.clone()),
        agents_home: Some(agents.clone()),
        ..LauncherDirs::default()
    };
    // Disabled as alpha, a Claude Code skill of another project.
    let mut kept = stored(
        Block::ClaudeSkill {
            name: "alpha".into(),
        },
        &kit.join("SKILL.md"),
    );
    kept.item.project_id = Some(protocol::ProjectId::new());
    let cwd = root.join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    let run = for_run(&[kept], &place(&cwd), &dirs);
    let claude_names: Vec<_> = run
        .skills
        .iter()
        .filter(|s| s.harness == Some(HarnessKind::ClaudeCode))
        .map(|s| s.name.as_str())
        .collect();
    assert!(
        claude_names.contains(&"alpha") && claude_names.contains(&"beta"),
        "{run:?}"
    );
    assert!(
        run.skills
            .iter()
            .any(|s| s.harness == Some(HarnessKind::Codex)
                && s.path == agents.join("skills/gamma/SKILL.md")),
        "{run:?}"
    );
}

#[test]
fn a_recovery_copy_is_matched_by_the_file_it_is_of() {
    let root = protocol::LibraryRootId::new();
    let item = |id: &str, path: &str| protocol::library::LibraryRecoveryItem {
        recovery_id: id.into(),
        root_id: root,
        path: path.into(),
        created_at: Timestamp(1),
    };
    let items = vec![
        item("a", "notes.md"),
        item("b", ".claude/skills/quiet/SKILL.md"),
    ];
    assert_eq!(
        recovered(items.clone(), root, "b"),
        Some((root, ".claude/skills/quiet/SKILL.md".into()))
    );
    assert_eq!(recovered(items.clone(), root, "c"), None);
    assert_eq!(recovered(items, protocol::LibraryRootId::new(), "b"), None);
}
