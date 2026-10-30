// SPDX-License-Identifier: Apache-2.0
use super::*;
#[cfg(unix)]
use protocol::LibraryRootId;

#[cfg(unix)]
fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

#[cfg(unix)]
fn link(target: &Path, at: &Path) {
    std::fs::create_dir_all(at.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(target, at).unwrap();
}

/// Agent folders as a computer keeps them, with settings, credentials and
/// past sessions beside the files agents are given.
#[cfg(unix)]
struct Computer {
    base: PathBuf,
    claude: PathBuf,
    codex: PathBuf,
    shared: PathBuf,
    project: PathBuf,
}

#[cfg(unix)]
impl Computer {
    fn new() -> Self {
        let base = ports::library::canonical(&std::env::temp_dir())
            .unwrap()
            .join(format!("ophio-agent-files-{}", uuid::Uuid::now_v7()));
        let computer = Self {
            claude: base.join("claude"),
            codex: base.join("codex"),
            shared: base.join("shared"),
            project: base.join("work").join("app"),
            base,
        };
        let (claude, codex) = (&computer.claude, &computer.codex);
        write(&claude.join("CLAUDE.md"), "# Everywhere\n");
        write(&claude.join("settings.json"), "{}");
        write(&claude.join(".credentials.json"), "{}");
        write(
            &claude.join("agents").join("reviewer.md"),
            "---\nname: reviewer\ndescription: \"Reviews changes\"\n---\nBody\n",
        );
        write(
            &claude.join("skills").join("notes").join("SKILL.md"),
            "---\ndescription: Keeps notes\n---\n",
        );
        write(&claude.join("skills").join("notes").join("run.sh"), "");
        let slug = slug(computer.project.to_str().unwrap());
        let sessions = claude.join("projects").join(&slug);
        write(&sessions.join("memory").join("MEMORY.md"), "- remembered\n");
        write(&sessions.join("session.jsonl"), "{}\n");
        // A folder renamed and linked from its old name, its sessions too.
        let old = computer.base.join("work").join("old");
        link(&computer.project, &old);
        link(
            &sessions,
            &claude
                .join("projects")
                .join(super::slug(old.to_str().unwrap())),
        );
        write(&claude.join("history.jsonl"), "{}\n");
        let plugin = claude
            .join("plugins")
            .join("cache")
            .join("tools")
            .join("1.0");
        write(&plugin.join("skills").join("lint").join("SKILL.md"), "");
        write(&plugin.join("plugin.json"), "{}");
        write(
            &claude.join("plugins").join("installed_plugins.json"),
            &serde_json::json!({
                "version": 2,
                "plugins": { "tools@market": [{ "installPath": plugin }] }
            })
            .to_string(),
        );
        write(&codex.join("AGENTS.md"), "# Codex\n");
        write(&codex.join("auth.json"), "{}");
        write(&codex.join("skills").join("plan").join("SKILL.md"), "");
        let shared = &computer.shared;
        write(&shared.join("browser").join("SKILL.md"), "");
        write(&shared.join("browser").join("guide").join("use.md"), "");
        write(&shared.join("browser").join("secret.env"), "");
        write(&shared.join("astra.md"), "");
        write(&shared.join("other.md"), "");
        link(
            &shared.join("browser"),
            &claude.join("skills").join("browser"),
        );
        link(
            &shared.join("astra.md"),
            &claude.join("agents").join("astra.md"),
        );
        let project = &computer.project;
        write(&project.join("CLAUDE.md"), "");
        write(&project.join("AGENTS.md"), "");
        write(&project.join("README.md"), "");
        write(&project.join("api").join("AGENTS.md"), "");
        write(
            &project.join("node_modules").join("x").join("AGENTS.md"),
            "",
        );
        write(&project.join(".claude").join("settings.json"), "{}");
        write(
            &project.join(".claude").join("commands").join("ship.md"),
            "",
        );
        computer
    }

    fn dirs(&self) -> LauncherDirs {
        LauncherDirs {
            claude_home: Some(self.claude.clone()),
            codex_home: Some(self.codex.clone()),
            ..LauncherDirs::default()
        }
    }

    fn project(&self) -> Project {
        Project {
            project_id: ProjectId::new(),
            name: "App".into(),
            host_path: self.project.to_string_lossy().into_owned(),
            default_harness: None,
            default_model: None,
            default_effort: None,
            active_writer: None,
            vcs: None,
            created_at: Timestamp(0),
            stage: None,
            one_off: false,
            home: false,
        }
    }
}

#[cfg(unix)]
impl Drop for Computer {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

#[cfg(unix)]
fn library_roots(roots: &[RootSpec]) -> Vec<LibraryRoot> {
    roots
        .iter()
        .map(|r| LibraryRoot {
            root_id: r.root_id,
            label: r.label.clone(),
            host_path: r.path.to_string_lossy().into_owned(),
            kind: r.kind,
            writable: r.writable,
            project_id: r.project_id,
            harness: r.harness,
            only: r.only.clone(),
        })
        .collect()
}

#[cfg(unix)]
#[test]
fn every_file_agents_are_given_is_reached_and_nothing_beside_them() {
    let computer = Computer::new();
    let project = computer.project();
    let found = find(
        &computer.dirs(),
        std::slice::from_ref(&project),
        Some(&computer.base),
    );
    let roots = roots(&found, &[], &[]);
    let only = |path: &Path| {
        roots
            .iter()
            .find(|r| r.path == path)
            .and_then(|r| r.only.clone())
            .unwrap_or_default()
    };
    let slug = slug(computer.project.to_str().unwrap());
    assert_eq!(
        only(&computer.claude),
        [
            "CLAUDE.md".to_owned(),
            "agents".into(),
            "plugins/cache/tools/1.0/skills".into(),
            format!("projects/{slug}/memory"),
            "skills".into(),
        ]
    );
    assert_eq!(only(&computer.codex), ["AGENTS.md", "skills"]);
    // Linked skills and agents are reached where they are kept, and only
    // their Markdown files.
    assert_eq!(
        only(&computer.shared.join("browser")),
        ["SKILL.md", "guide/use.md"]
    );
    assert_eq!(only(&computer.shared), ["astra.md"]);
    assert_eq!(
        only(&computer.project),
        [
            ".claude/commands",
            "AGENTS.md",
            "CLAUDE.md",
            "api/AGENTS.md"
        ]
    );
    let held = roots.iter().find(|r| r.path == computer.project).unwrap();
    assert_eq!(held.project_id, Some(project.project_id));
    assert!(
        roots
            .iter()
            .all(|r| r.kind == RootKind::AgentFiles && r.writable)
    );

    // A folder Ophio keeps its own data in, or one around it, is never
    // reached, and nor is a folder already the person's own.
    let taken = RootSpec {
        root_id: LibraryRootId::new(),
        label: "App".into(),
        path: computer.project.clone(),
        kind: RootKind::Project,
        writable: true,
        project_id: Some(project.project_id),
        harness: None,
        only: None,
    };
    let fewer = self::roots(
        &found,
        std::slice::from_ref(&taken),
        std::slice::from_ref(&computer.codex),
    );
    assert!(
        fewer
            .iter()
            .all(|r| r.path != computer.codex && r.path != computer.project)
    );
    assert_eq!(fewer.len(), roots.len() - 2);

    let mut all = vec![taken];
    all.extend(fewer);
    let (files, truncated) = list(
        &found.places,
        &library_roots(&all),
        &LauncherDirs::default(),
        &[],
    );
    assert!(!truncated);
    let shown: Vec<(AgentFileGroup, &str, &str)> = files
        .iter()
        .map(|f| (f.group, f.scope.as_str(), f.shown.as_str()))
        .collect();
    use AgentFileGroup::*;
    assert_eq!(
        shown,
        [
            (Instructions, "Claude Code", "CLAUDE.md"),
            (Instructions, "App", "AGENTS.md"),
            (Instructions, "App", "api/AGENTS.md"),
            (Instructions, "App", "CLAUDE.md"),
            (Agents, "Claude Code", "astra.md"),
            (Agents, "Claude Code", "reviewer.md"),
            (Skills, "Claude Code", "browser/guide/use.md"),
            (Skills, "Claude Code", "browser/SKILL.md"),
            (Skills, "Claude Code", "notes/SKILL.md"),
            (Skills, "tools plugin", "lint/SKILL.md"),
            (Commands, "App", "ship.md"),
            (Memory, "App", "MEMORY.md"),
        ]
    );
    let reviewer = files.iter().find(|f| f.shown == "reviewer.md").unwrap();
    assert_eq!(reviewer.title.as_deref(), Some("reviewer"));
    assert_eq!(reviewer.description.as_deref(), Some("Reviews changes"));
    let notes = files.iter().find(|f| f.shown == "notes/SKILL.md").unwrap();
    assert_eq!(notes.title.as_deref(), Some("notes"));
    assert_eq!(notes.description.as_deref(), Some("Keeps notes"));
    // A file in a person's own root is opened through it.
    let ship = files.iter().find(|f| f.shown == "ship.md").unwrap();
    assert_eq!(ship.root_id, all[0].root_id);
    assert_eq!(ship.path, ".claude/commands/ship.md");
    let memory = files.iter().find(|f| f.group == Memory).unwrap();
    assert_eq!(memory.project_id, Some(project.project_id));
    assert!(!memory.everywhere);
}

#[cfg(unix)]
#[test]
fn memory_for_a_folder_that_is_not_a_project_names_the_folder() {
    let computer = Computer::new();
    let found = find(&computer.dirs(), &[], Some(&computer.base));
    let (files, _) = list(
        &found.places,
        &library_roots(&roots(&found, &[], &[])),
        &LauncherDirs::default(),
        &[],
    );
    let memory = files
        .iter()
        .find(|f| f.group == AgentFileGroup::Memory)
        .unwrap();
    assert_eq!(memory.scope, "~/work/app");
    assert_eq!(memory.project_id, None);

    // Memory for a folder no longer there isn't listed.
    std::fs::remove_dir_all(&computer.project).unwrap();
    let found = find(&computer.dirs(), &[], Some(&computer.base));
    assert!(
        found
            .places
            .iter()
            .all(|p| p.kind.group != AgentFileGroup::Memory)
    );
}

#[test]
fn names_come_from_frontmatter_or_where_the_file_is() {
    assert_eq!(
        named(AgentFileGroup::Skills, "a/b/SKILL.md").as_deref(),
        Some("b")
    );
    assert_eq!(
        named(AgentFileGroup::Agents, "x/helper.md").as_deref(),
        Some("helper")
    );
    assert_eq!(named(AgentFileGroup::Rules, "style.md"), None);
    assert_eq!(slug("/srv/a.b/c_d"), "-srv-a-b-c-d");
    assert!(nameable("a/b.md") && !nameable("a/../b") && !nameable("/a") && !nameable("a:b"));
}

#[test]
fn a_fallback_file_codex_is_set_to_read_is_an_agent_file() {
    let tmp = tempfile::tempdir().unwrap();
    let codex = tmp.path().join("codex");
    std::fs::create_dir_all(&codex).unwrap();
    std::fs::write(
        codex.join("config.toml"),
        "project_doc_fallback_filenames = [\"NOTES.md\", \"../escape.md\", \"TEAM.txt\"]\n",
    )
    .unwrap();
    assert_eq!(codex_fallbacks(Some(&codex)), ["NOTES.md"]);
    assert!(codex_fallbacks(None).is_empty());
}
