// SPDX-License-Identifier: Apache-2.0
use super::*;
use std::os::unix::fs::symlink;

struct Home {
    _dir: tempfile::TempDir,
    home: PathBuf,
    cwd: PathBuf,
    private: Vec<PathBuf>,
}
impl Home {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        // A Mac's temporary folder is reached through a link.
        let home = dir.path().canonicalize().unwrap().join("home");
        let cwd = home.join("Projects/app");
        for path in [
            &cwd,
            &home.join(".claude"),
            &home.join(".codex"),
            &home.join(".ssh"),
            &home.join(".local/share/ophio/sandboxes/t"),
        ] {
            std::fs::create_dir_all(path).unwrap();
        }
        let private = private(&home, [home.join(".ssh"), home.join(".local/share/ophio")]);
        Self {
            _dir: dir,
            home,
            cwd,
            private,
        }
    }
    fn select(&self, kind: HarnessKind, writes: bool) -> Vec<Shared> {
        self.select_leaving_out(kind, writes, &[])
    }
    fn select_leaving_out(
        &self,
        kind: HarnessKind,
        writes: bool,
        withheld: &[PathBuf],
    ) -> Vec<Shared> {
        let config = self.home.join(match kind {
            HarnessKind::Codex => ".codex",
            _ => ".claude",
        });
        select(
            kind,
            &config,
            &Bounds {
                home: &self.home,
                cwd: &self.cwd,
                private: &self.private,
                writes,
                withheld,
            },
        )
    }
    fn names(&self, kind: HarnessKind, writes: bool) -> Vec<String> {
        self.select(kind, writes)
            .iter()
            .map(|s| s.relative.to_string_lossy().into_owned())
            .collect()
    }
    /// A skill folder at `path` whose `SKILL.md` holds `text`.
    fn skill(&self, path: &Path, text: &str) {
        std::fs::create_dir_all(path).unwrap();
        std::fs::write(path.join("SKILL.md"), text).unwrap();
    }
    fn write(&self, path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
}

const PLAIN: &str = "---\nname: x\ndescription: y\n---\nbody\n";

#[test]
fn claude_gets_its_instructions_rules_and_skills_and_nothing_else() {
    let h = Home::new();
    let config = h.home.join(".claude");
    h.write(&config.join("CLAUDE.md"), "be kind");
    h.write(&config.join("rules/style.md"), "short");
    h.write(&config.join("rules/team/review.md"), "careful");
    h.skill(&config.join("skills/mine"), PLAIN);
    let elsewhere = h.home.join("Projects/tools/skills/linked");
    h.skill(&elsewhere, PLAIN);
    symlink(&elsewhere, config.join("skills/linked")).unwrap();
    // A folder of skills, shared whole; and the one Claude Code fills from
    // the person's account itself, left to the conversation's copy.
    h.skill(&config.join("skills/team/review"), PLAIN);
    h.skill(&config.join("skills/synced/bucket/pdf"), PLAIN);
    h.write(&config.join("skills/README.md"), "not a skill");
    for never in [
        "settings.json",
        ".credentials.json",
        "history.jsonl",
        "agents/a.md",
        "commands/c.md",
        "plugins/p/SKILL.md",
        "projects/p/s.jsonl",
    ] {
        h.write(&config.join(never), "{}");
    }
    h.write(&h.home.join(".claude.json"), "{}");
    let shared = h.select(HarnessKind::ClaudeCode, true);
    let names: Vec<_> = shared
        .iter()
        .map(|s| s.relative.to_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "CLAUDE.md",
            "rules/style.md",
            "rules/team",
            "skills/linked",
            "skills/mine",
            "skills/team"
        ]
    );
    let linked = shared
        .iter()
        .find(|s| s.relative == Path::new("skills/linked"))
        .unwrap();
    assert_eq!(linked.source, elsewhere);
    assert!(linked.directory);
    let metadata = std::fs::metadata(&elsewhere).unwrap();
    assert_eq!((linked.dev, linked.ino), (metadata.dev(), metadata.ino()));
    assert!(!shared[0].directory);
}

#[test]
fn codex_gets_both_instruction_files_and_skills_but_not_its_own_system_skills_or_rules() {
    let h = Home::new();
    let config = h.home.join(".codex");
    h.write(&config.join("AGENTS.md"), "a");
    h.write(&config.join("AGENTS.override.md"), "b");
    // Approval policy, not instructions.
    h.write(&config.join("rules/default.rules"), "allow");
    h.write(&config.join("config.toml"), "");
    h.write(&config.join("auth.json"), "{}");
    h.skill(&config.join("skills/.system/imagegen"), PLAIN);
    h.skill(&config.join("skills/mine"), PLAIN);
    // Claude Code's names mean nothing to Codex.
    h.write(&config.join("CLAUDE.md"), "c");
    assert_eq!(
        h.names(HarnessKind::Codex, true),
        ["AGENTS.md", "AGENTS.override.md", "skills/mine"]
    );
}

#[test]
fn nothing_is_shared_when_there_is_nothing() {
    let h = Home::new();
    assert!(h.names(HarnessKind::ClaudeCode, true).is_empty());
    assert!(h.names(HarnessKind::Codex, true).is_empty());
}

#[test]
fn links_that_would_reach_private_or_broad_places_are_refused_and_the_rest_kept() {
    let h = Home::new();
    let skills = h.home.join(".claude/skills");
    std::fs::create_dir_all(&skills).unwrap();
    // Each of these holds a SKILL.md, planted to look like a skill.
    let ssh = h.home.join(".ssh");
    let ophio = h.home.join(".local/share/ophio");
    let codex = h.home.join(".codex");
    let plugins = h.home.join(".claude/plugins/p");
    let projects = h.home.join("Projects");
    for place in [&h.home, &ssh, &ophio, &codex, &plugins, &projects, &h.cwd] {
        h.skill(place, PLAIN);
    }
    for (name, target) in [
        ("home", h.home.clone()),
        ("ssh", ssh.clone()),
        ("ssh-inside", ssh.join("inside")),
        ("ophio", ophio.join("sandboxes")),
        ("other-agent", codex.clone()),
        ("own-settings", plugins.clone()),
        ("above-work", projects.clone()),
        ("root", PathBuf::from("/")),
        ("proc", PathBuf::from("/proc/self")),
        ("dangling", h.home.join("gone")),
    ] {
        symlink(&target, skills.join(name)).unwrap();
    }
    h.skill(&ssh.join("inside"), PLAIN);
    h.skill(&ophio.join("sandboxes"), PLAIN);
    // The working folder itself is fine.
    symlink(&h.cwd, skills.join("work")).unwrap();
    // A folder that holds something private.
    let holder = h.home.join("holder");
    h.skill(&holder, PLAIN);
    symlink(&holder, skills.join("holder")).unwrap();
    let h = Home {
        private: private(
            &h.home,
            h.private.iter().cloned().chain([holder.join("secret")]),
        ),
        ..h
    };
    assert_eq!(h.names(HarnessKind::ClaudeCode, true), ["skills/work"]);
}

#[test]
fn a_link_must_lead_straight_to_a_real_skill_folder() {
    let h = Home::new();
    let skills = h.home.join(".claude/skills");
    std::fs::create_dir_all(&skills).unwrap();
    let real = h.home.join("Projects/tools/real");
    h.skill(&real, PLAIN);
    // Through a second link someone else could turn.
    symlink(h.home.join("Projects/tools"), h.home.join("Projects/alias")).unwrap();
    symlink(h.home.join("Projects/alias/real"), skills.join("twice")).unwrap();
    // A relative link is fine.
    symlink("../../Projects/tools/real", skills.join("relative")).unwrap();
    // No SKILL.md, or one that is a link or a folder.
    let empty = h.home.join("Projects/empty");
    std::fs::create_dir_all(&empty).unwrap();
    symlink(&empty, skills.join("empty")).unwrap();
    let linked_file = h.home.join("Projects/linked-file");
    std::fs::create_dir_all(&linked_file).unwrap();
    symlink(real.join("SKILL.md"), linked_file.join("SKILL.md")).unwrap();
    symlink(&linked_file, skills.join("linked-file")).unwrap();
    let folder = h.home.join("Projects/folder");
    std::fs::create_dir_all(folder.join("SKILL.md")).unwrap();
    symlink(&folder, skills.join("folder")).unwrap();
    // A file, linked as a skill.
    symlink(real.join("SKILL.md"), skills.join("file")).unwrap();
    assert_eq!(h.names(HarnessKind::ClaudeCode, true), ["skills/relative"]);
}

#[test]
fn instruction_files_must_be_plain_files_with_one_name() {
    let h = Home::new();
    let config = h.home.join(".claude");
    let dotfiles = h.home.join("dotfiles");
    h.write(&dotfiles.join("CLAUDE.md"), "mine");
    std::fs::hard_link(dotfiles.join("CLAUDE.md"), dotfiles.join("copy.md")).unwrap();
    // A second name could be anything's, so a hard link is refused.
    std::fs::hard_link(dotfiles.join("CLAUDE.md"), config.join("CLAUDE.md")).unwrap();
    assert!(h.names(HarnessKind::ClaudeCode, true).is_empty());
    std::fs::remove_file(config.join("CLAUDE.md")).unwrap();
    std::fs::remove_file(dotfiles.join("copy.md")).unwrap();
    // A link to one is followed.
    symlink(dotfiles.join("CLAUDE.md"), config.join("CLAUDE.md")).unwrap();
    let shared = h.select(HarnessKind::ClaudeCode, true);
    assert_eq!(shared.len(), 1);
    assert_eq!(shared[0].source, dotfiles.join("CLAUDE.md"));
    // A folder by that name is not instructions.
    std::fs::remove_file(config.join("CLAUDE.md")).unwrap();
    std::fs::create_dir(config.join("CLAUDE.md")).unwrap();
    assert!(h.names(HarnessKind::ClaudeCode, true).is_empty());
}

#[test]
fn a_linked_skills_folder_is_not_followed() {
    let h = Home::new();
    let elsewhere = h.home.join("dotfiles/skills");
    h.skill(&elsewhere.join("one"), PLAIN);
    symlink(&elsewhere, h.home.join(".claude/skills")).unwrap();
    assert!(h.names(HarnessKind::ClaudeCode, true).is_empty());
}

#[test]
fn a_read_only_run_is_not_shown_skills_that_say_they_change_files() {
    let h = Home::new();
    let skills = h.home.join(".claude/skills");
    h.skill(&skills.join("plain"), PLAIN);
    h.skill(
        &skills.join("tools"),
        "---\nname: tools\nallowed-tools: Read, Write(docs/**)\n---\n",
    );
    h.skill(
        &skills.join("marked"),
        "---\nname: marked\nmetadata:\n  author: me\n  ophio-access: write\n---\n",
    );
    h.skill(
        &skills.join("reads"),
        "---\nname: reads\nallowed-tools: Read Grep Bash(git status *)\nmetadata:\n  ophio-access: read\n---\n",
    );
    let elsewhere = h.home.join("Projects/linked");
    h.skill(&elsewhere, "---\nallowed-tools:\n  - Read\n  - Edit\n---\n");
    symlink(&elsewhere, skills.join("linked")).unwrap();
    assert_eq!(
        h.names(HarnessKind::ClaudeCode, true),
        [
            "skills/linked",
            "skills/marked",
            "skills/plain",
            "skills/reads",
            "skills/tools"
        ]
    );
    assert_eq!(
        h.names(HarnessKind::ClaudeCode, false),
        ["skills/plain", "skills/reads"]
    );
}

#[test]
fn front_matter_that_is_malformed_or_too_long_declares_nothing() {
    let h = Home::new();
    let skills = h.home.join(".codex/skills");
    h.skill(
        &skills.join("unclosed"),
        "---\nname: unclosed\nallowed-tools: Write\n",
    );
    h.skill(
        &skills.join("late"),
        "intro\n---\nallowed-tools: Write\n---\n",
    );
    h.skill(&skills.join("binary"), "\u{0}\u{1}\u{2}");
    let long = format!(
        "---\ndescription: {}\nallowed-tools: Write\n---\n",
        "x".repeat(HEAD as usize)
    );
    h.skill(&skills.join("long"), &long);
    // A big body after a short front matter is fine.
    let big = format!(
        "---\nmetadata: {{ophio-access: write}}\n---\n{}",
        "y".repeat(4 * HEAD as usize)
    );
    h.skill(&skills.join("big"), &big);
    assert_eq!(
        h.names(HarnessKind::Codex, false),
        [
            "skills/binary",
            "skills/late",
            "skills/long",
            "skills/unclosed"
        ]
    );
}

#[test]
fn declarations_are_read_from_the_usual_spellings() {
    for (text, writes) in [
        ("---\nallowed-tools: Write\n---", true),
        ("---\nallowed-tools: \"Read Edit\"\n---", true),
        ("---\nallowed-tools: [Read, MultiEdit]\n---", true),
        ("---\nallowed-tools: NotebookEdit\n---", true),
        ("---\nallowed-tools: apply_patch\n---", true),
        ("---\nallowed-tools:\n- Bash\n- Write\n---", true),
        ("---\nallowed-tools: Bash Read WebFetch\n---", false),
        ("---\nallowed-tools: Rewrite\n---", false),
        ("---\nmetadata:\n  \"ophio-access\": \"write\"\n---", true),
        ("---\nmetadata: {a: b, ophio-access: write}\n---", true),
        ("---\nophio-access: write\n---", false),
        ("---\ndescription: uses Write\n---", false),
        ("---\nmetadata:\n  ophio-access: write\n", false),
        ("---\r\nallowed-tools: Edit\r\n---\r\n", true),
        ("\u{feff}---\nallowed-tools: Write\n---", true),
        ("---\n\"allowed-tools\": [Edit]\n---", true),
        ("---\n'metadata':\n  ophio-access: write\n---", true),
    ] {
        assert_eq!(declares_writing(text), writes, "{text:?}");
    }
}

fn place_in(h: &Home, kind: HarnessKind, link: bool) -> (PathBuf, Vec<Shared>) {
    let state = h.home.join(".local/share/ophio/sandboxes/t");
    let native = state.join("native");
    crate::sys::directories(&native).unwrap();
    let placed = place(&native, &state, h.select(kind, true), link);
    (native, placed)
}

#[test]
fn places_are_made_in_the_conversation_folder_and_follow_skills_added_and_removed() {
    let h = Home::new();
    let config = h.home.join(".claude");
    h.write(&config.join("CLAUDE.md"), "be kind");
    h.skill(&config.join("skills/one"), PLAIN);
    h.skill(&config.join("skills/two"), PLAIN);
    let (native, placed) = place_in(&h, HarnessKind::ClaudeCode, false);
    assert_eq!(placed.len(), 3);
    assert!(native.join("skills/one").is_dir());
    assert!(!native.join("skills/one/SKILL.md").exists());
    assert_eq!(std::fs::read(native.join("CLAUDE.md")).unwrap(), b"");
    // Nothing is made in the person's own folder.
    let mut own: Vec<_> = std::fs::read_dir(&config)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    own.sort();
    assert_eq!(own, ["CLAUDE.md", "skills"]);
    // The next turn: one skill gone, another new, and the instructions removed.
    std::fs::remove_dir_all(config.join("skills/one")).unwrap();
    std::fs::remove_file(config.join("CLAUDE.md")).unwrap();
    h.skill(&config.join("skills/three"), PLAIN);
    // Something of the agent's own under a name that was shared stays.
    std::fs::write(native.join("skills/two/notes.md"), "mine").unwrap();
    let (_, placed) = place_in(&h, HarnessKind::ClaudeCode, false);
    let names: Vec<_> = placed.iter().map(|s| s.relative.clone()).collect();
    assert_eq!(names, [Path::new("skills/three"), Path::new("skills/two")]);
    assert!(!native.join("skills/one").exists());
    assert!(!native.join("CLAUDE.md").exists());
    assert!(native.join("skills/two/notes.md").exists());
    assert!(native.join("skills/three").is_dir());
}

#[test]
fn places_an_earlier_turn_turned_into_links_or_files_are_replaced() {
    let h = Home::new();
    let config = h.home.join(".claude");
    h.write(&config.join("CLAUDE.md"), "be kind");
    h.skill(&config.join("skills/one"), PLAIN);
    let state = h.home.join(".local/share/ophio/sandboxes/t");
    let native = state.join("native");
    let outside = h.home.join("Projects/app/target");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::create_dir_all(&native).unwrap();
    // The agent made its skills folder a link into the project, and its
    // instructions a link to a file there.
    symlink(&outside, native.join("skills")).unwrap();
    symlink(outside.join("x.md"), native.join("CLAUDE.md")).unwrap();
    let placed = place(
        &native,
        &state,
        h.select(HarnessKind::ClaudeCode, true),
        false,
    );
    assert_eq!(placed.len(), 2);
    assert!(
        !std::fs::symlink_metadata(native.join("skills"))
            .unwrap()
            .is_symlink()
    );
    assert!(
        std::fs::symlink_metadata(native.join("CLAUDE.md"))
            .unwrap()
            .is_file()
    );
    assert!(std::fs::read_dir(&outside).unwrap().next().is_none());
    // A file where a skill folder goes, and a folder of the agent's where the
    // instructions go.
    std::fs::remove_dir(native.join("skills/one")).unwrap();
    std::fs::write(native.join("skills/one"), "x").unwrap();
    std::fs::remove_file(native.join("CLAUDE.md")).unwrap();
    std::fs::create_dir(native.join("CLAUDE.md")).unwrap();
    std::fs::write(native.join("CLAUDE.md/keep"), "x").unwrap();
    let placed = place(
        &native,
        &state,
        h.select(HarnessKind::ClaudeCode, true),
        false,
    );
    // The agent's own file and folder are moved aside, not deleted.
    let names: Vec<_> = placed.iter().map(|s| s.relative.clone()).collect();
    assert_eq!(names, [Path::new("CLAUDE.md"), Path::new("skills/one")]);
    assert!(native.join("skills/one").is_dir());
    assert_eq!(
        std::fs::read(native.join("skills/.ophio-aside-one")).unwrap(),
        b"x"
    );
    assert!(native.join("CLAUDE.md").is_file());
    assert!(native.join(".ophio-aside-CLAUDE.md/keep").exists());
}

#[test]
fn a_mac_gets_links_to_the_real_places_and_loses_them_when_they_go() {
    let h = Home::new();
    let config = h.home.join(".codex");
    h.write(&config.join("AGENTS.md"), "a");
    let elsewhere = h.home.join("Projects/tools/s");
    h.skill(&elsewhere, PLAIN);
    std::fs::create_dir_all(config.join("skills")).unwrap();
    symlink(&elsewhere, config.join("skills/s")).unwrap();
    let (native, placed) = place_in(&h, HarnessKind::Codex, true);
    assert_eq!(placed.len(), 2);
    assert_eq!(
        std::fs::read_link(native.join("skills/s")).unwrap(),
        elsewhere
    );
    assert_eq!(
        std::fs::read_link(native.join("AGENTS.md")).unwrap(),
        config.join("AGENTS.md")
    );
    // Codex's own skills beside them are left alone.
    h.skill(&native.join("skills/.system/imagegen"), PLAIN);
    std::fs::remove_file(config.join("skills/s")).unwrap();
    let (_, placed) = place_in(&h, HarnessKind::Codex, true);
    assert_eq!(placed.len(), 1);
    assert!(std::fs::symlink_metadata(native.join("skills/s")).is_err());
    assert!(native.join("skills/.system/imagegen/SKILL.md").exists());
}

#[test]
fn the_record_names_only_places_this_could_have_made() {
    for (name, ours) in [
        ("skills/x", true),
        ("CLAUDE.md", true),
        ("AGENTS.override.md", true),
        ("rules/a.md", true),
        ("skills", false),
        ("skills/x/y", false),
        ("../skills/x", false),
        ("/skills/x", false),
        ("settings.json", false),
        ("projects/x", false),
    ] {
        assert_eq!(super::ours(Path::new(name)), ours, "{name}");
    }
}

#[test]
fn the_sandbox_refuses_to_start_when_a_place_is_not_what_was_checked() {
    let h = Home::new();
    let config = h.home.join(".claude");
    h.skill(&config.join("skills/one"), PLAIN);
    let shared = h.select(HarnessKind::ClaudeCode, true);
    // Checked in place, as a mount would show it.
    let list = expected(&config, &shared);
    verify(&list).unwrap();
    // Swapped for something else after the check.
    std::fs::rename(config.join("skills/one"), h.home.join("moved")).unwrap();
    h.skill(&config.join("skills/one"), PLAIN);
    assert!(verify(&list).is_err());
    assert!(verify("garbage").is_err());
    verify("").unwrap();
}

#[test]
fn what_is_disabled_in_ophio_is_not_shared() {
    let h = Home::new();
    let claude = h.home.join(".claude");
    let codex = h.home.join(".codex");
    h.write(&claude.join("CLAUDE.md"), "be kind");
    h.write(&claude.join("rules/style.md"), "short");
    h.write(&claude.join("rules/team/review.md"), "careful");
    h.skill(&claude.join("skills/kept"), PLAIN);
    h.skill(&codex.join("skills/off"), PLAIN);
    h.skill(&codex.join("skills/kept"), PLAIN);
    let elsewhere = h.home.join("Projects/tools/skills/linked");
    h.skill(&elsewhere, PLAIN);
    symlink(&elsewhere, codex.join("skills/linked")).unwrap();
    // Disabled as the person's agent finds them: by a spelling of the home
    // folder with a link in it, by a file in a folder of rules, and by where
    // a linked skill really is.
    let alias = h.home.parent().unwrap().join("alias");
    symlink(&h.home, &alias).unwrap();
    let disabled = ports::Disabled {
        files: vec![
            alias.join(".claude/CLAUDE.md"),
            claude.join("rules/team/review.md"),
        ],
        skills: [
            codex.join("skills/off/SKILL.md"),
            elsewhere.join("SKILL.md"),
        ]
        .into_iter()
        .map(|path| ports::DisabledEntry {
            path,
            name: "x".into(),
            harness: Some(HarnessKind::Codex),
        })
        .collect(),
        ..Default::default()
    };
    let withheld = withheld(&disabled);
    let names = |kind| -> Vec<String> {
        h.select_leaving_out(kind, true, &withheld)
            .iter()
            .map(|s| s.relative.to_string_lossy().into_owned())
            .collect()
    };
    assert_eq!(
        names(HarnessKind::ClaudeCode),
        ["rules/style.md", "skills/kept"]
    );
    assert_eq!(names(HarnessKind::Codex), ["skills/kept"]);
    assert_eq!(h.names(HarnessKind::Codex, true).len(), 3);
}

/// Everything under `path`, by kind, without following a link.
fn tree(path: &Path) -> Vec<(PathBuf, String)> {
    let mut found = Vec::new();
    let mut stack = vec![path.to_owned()];
    while let Some(next) = stack.pop() {
        let meta = std::fs::symlink_metadata(&next).unwrap();
        let what = if meta.is_symlink() {
            format!("link {}", std::fs::read_link(&next).unwrap().display())
        } else if meta.is_dir() {
            for entry in std::fs::read_dir(&next).unwrap() {
                stack.push(entry.unwrap().path());
            }
            "folder".into()
        } else {
            format!(
                "file {}",
                std::fs::read_to_string(&next).unwrap_or_default()
            )
        };
        found.push((next.strip_prefix(path).unwrap().to_owned(), what));
    }
    found.sort();
    found
}

/// The person's Claude Code folder as the owner keeps it: skills and a rule
/// that are links to where they really are.
fn linked_setup(h: &Home) -> PathBuf {
    let config = h.home.join(".claude");
    let tools = h.home.join("Projects/tools");
    for name in ["a", "b"] {
        h.skill(&tools.join(name), PLAIN);
    }
    h.write(&tools.join("x.md"), "x");
    h.write(&tools.join("y.md"), "y");
    std::fs::create_dir_all(config.join("skills")).unwrap();
    std::fs::create_dir_all(config.join("rules")).unwrap();
    for (from, to) in [
        ("skills/a", "a"),
        ("skills/b", "b"),
        ("rules/x.md", "x.md"),
        ("rules/y.md", "y.md"),
    ] {
        symlink(tools.join(to), config.join(from)).unwrap();
    }
    config
}

#[test]
fn a_link_the_agent_planted_in_place_of_a_folder_is_never_followed() {
    for link in [false, true] {
        let h = Home::new();
        let config = linked_setup(&h);
        let tools = h.home.join("Projects/tools");
        let state = h.home.join(".local/share/ophio/sandboxes/t");
        let native = state.join("native");
        crate::sys::directories(&native).unwrap();
        let placed = place(
            &native,
            &state,
            h.select(HarnessKind::ClaudeCode, true),
            link,
        );
        assert_eq!(placed.len(), 4, "link {link}");
        // During the turn the agent moves its own folders aside and leaves
        // links to the person's real ones in their place.
        for folder in ["skills", "rules"] {
            std::fs::rename(native.join(folder), native.join(format!("{folder}-mine"))).unwrap();
            symlink(config.join(folder), native.join(folder)).unwrap();
        }
        let before = tree(&h.home.join("Projects"));
        let own = tree(&config);
        // Before the next turn `b` and `y.md` stop being shared, here by
        // being disabled; `a` and `x.md` still are.
        let disabled = ports::Disabled {
            files: vec![tools.join("y.md")],
            skills: vec![ports::DisabledEntry {
                path: tools.join("b/SKILL.md"),
                name: "b".into(),
                harness: None,
            }],
            ..Default::default()
        };
        let shared = h.select_leaving_out(HarnessKind::ClaudeCode, true, &withheld(&disabled));
        let placed = place(&native, &state, shared, link);
        assert_eq!(tree(&config), own, "link {link}");
        assert_eq!(tree(&h.home.join("Projects")), before, "link {link}");
        // The planted links are replaced by folders of the chat's own.
        let names: Vec<_> = placed.iter().map(|s| s.relative.clone()).collect();
        assert_eq!(names, [Path::new("rules/x.md"), Path::new("skills/a")]);
        for folder in ["skills", "rules"] {
            assert!(
                std::fs::symlink_metadata(native.join(folder))
                    .unwrap()
                    .is_dir()
            );
        }
    }
}

#[test]
fn something_the_agent_left_at_a_shared_place_on_a_mac_is_moved_aside() {
    let h = Home::new();
    let config = linked_setup(&h);
    let state = h.home.join(".local/share/ophio/sandboxes/t");
    let native = state.join("native");
    crate::sys::directories(&native).unwrap();
    place(
        &native,
        &state,
        h.select(HarnessKind::ClaudeCode, true),
        true,
    );
    std::fs::remove_file(native.join("skills/a")).unwrap();
    h.write(&native.join("skills/a/notes.md"), "mine");
    std::fs::remove_file(native.join("rules/x.md")).unwrap();
    h.write(&native.join("rules/x.md"), "mine");
    let outside = tree(&h.home.join("Projects"));
    let own = tree(&config);
    let placed = place(
        &native,
        &state,
        h.select(HarnessKind::ClaudeCode, true),
        true,
    );
    assert_eq!(placed.len(), 4);
    let tools = h.home.join("Projects/tools");
    assert_eq!(
        std::fs::read_link(native.join("skills/a")).unwrap(),
        tools.join("a")
    );
    assert_eq!(
        std::fs::read_link(native.join("rules/x.md")).unwrap(),
        tools.join("x.md")
    );
    assert_eq!(
        std::fs::read_to_string(native.join("skills/.ophio-aside-a/notes.md")).unwrap(),
        "mine"
    );
    assert_eq!(
        std::fs::read_to_string(native.join("rules/.ophio-aside-x.md")).unwrap(),
        "mine"
    );
    assert_eq!(tree(&h.home.join("Projects")), outside);
    assert_eq!(tree(&config), own);
}

#[test]
fn a_folder_swapped_for_a_link_after_it_was_opened_redirects_nothing() {
    let h = Home::new();
    let native = h.home.join(".local/share/ophio/sandboxes/t/native");
    crate::sys::directories(&native).unwrap();
    let root = crate::sys::folder(&native).unwrap();
    let skills = folder_of_its_own(&root, OsStr::new("skills")).unwrap();
    // The person's project, whose folders share names with skills.
    let victim = h.home.join("Projects/b");
    h.write(&victim.join("b/src/main.rs"), "fn main() {}");
    h.write(&victim.join("c"), "data");
    h.write(&victim.join("d/keep"), "keep");
    std::fs::create_dir_all(victim.join("e")).unwrap();
    let before = tree(&victim);
    // A process the agent left behind swaps the folder for a link.
    std::fs::rename(native.join("skills"), native.join("moved")).unwrap();
    symlink(&victim, native.join("skills")).unwrap();
    let source = h.home.join("Projects/tools/b");
    linked(&skills, OsStr::new("b"), &source).unwrap();
    linked(&skills, OsStr::new("c"), &source).unwrap();
    mount_point(&skills, OsStr::new("d"), true).unwrap();
    mount_point(&skills, OsStr::new("e"), false).unwrap();
    assert_eq!(tree(&victim), before);
    let moved = native.join("moved");
    assert_eq!(std::fs::read_link(moved.join("b")).unwrap(), source);
    assert_eq!(std::fs::read_link(moved.join("c")).unwrap(), source);
    assert!(moved.join("d").is_dir());
    assert!(moved.join("e").is_file());
    // And a name that isn't one plain part is refused.
    for name in ["..", "x/y", ""] {
        assert!(
            mount_point(&skills, OsStr::new(name), true).is_err(),
            "{name}"
        );
    }
}

#[test]
fn a_name_with_a_control_character_is_left_out_and_the_rest_still_checks() {
    let h = Home::new();
    let config = h.home.join(".claude");
    h.write(&config.join("CLAUDE.md"), "be kind");
    h.skill(&config.join("skills/odd\r"), PLAIN);
    h.skill(&config.join("skills/tab\there"), PLAIN);
    h.skill(&config.join("skills/fine"), PLAIN);
    assert_eq!(
        h.names(HarnessKind::ClaudeCode, true),
        ["CLAUDE.md", "skills/fine"]
    );
    verify(&expected(&config, &h.select(HarnessKind::ClaudeCode, true))).unwrap();
}

#[test]
fn the_check_list_is_split_on_line_feeds_only() {
    let dir = tempfile::tempdir().unwrap();
    let odd = dir.path().join("odd\r");
    std::fs::write(&odd, "x").unwrap();
    let meta = std::fs::symlink_metadata(&odd).unwrap();
    let line = format!("{} {} {}\n", meta.dev(), meta.ino(), odd.to_str().unwrap());
    verify(&line).unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn a_settings_folder_whose_path_is_not_text_shares_nothing() {
    use std::os::unix::ffi::OsStrExt;
    let h = Home::new();
    let config = h.home.join(OsStr::from_bytes(b"conf\xff"));
    h.write(&config.join("CLAUDE.md"), "be kind");
    h.skill(&config.join("skills/fine"), PLAIN);
    let bounds = Bounds {
        home: &h.home,
        cwd: &h.cwd,
        private: &h.private,
        writes: true,
        withheld: &[],
    };
    assert!(select(HarnessKind::ClaudeCode, &config, &bounds).is_empty());
}

/// A conversation folder as the Mac's sign-in and history links use it, with
/// the person's real sign-in and history beside it.
fn history_setup(h: &Home) -> (PathBuf, File, PathBuf, PathBuf) {
    let native = h.home.join(".local/share/ophio/sandboxes/t/native");
    crate::sys::directories(&native).unwrap();
    let root = crate::sys::folder(&native).unwrap();
    let sign_in = h.home.join(".claude/.credentials.json");
    h.write(&sign_in, "secret");
    let history = h.home.join(".local/share/ophio/sandboxes/t/claude-history");
    h.write(&history.join("projects/app/one.jsonl"), "turn");
    (native, root, sign_in, history)
}

#[test]
fn history_links_are_made_kept_and_moved_on_through_the_folder_handle() {
    let h = Home::new();
    let (native, root, sign_in, history) = history_setup(&h);
    let deep = Path::new("projects/app/one.jsonl");
    // A fresh folder, to any depth.
    link_within(&root, Path::new(".credentials.json"), &sign_in, true).unwrap();
    link_within(&root, deep, &history.join(deep), false).unwrap();
    assert_eq!(
        std::fs::read_link(native.join(".credentials.json")).unwrap(),
        sign_in
    );
    assert_eq!(
        std::fs::read_link(native.join(deep)).unwrap(),
        history.join(deep)
    );
    // A repeated turn keeps the very same link.
    use std::os::unix::fs::MetadataExt;
    let before = std::fs::symlink_metadata(native.join(deep)).unwrap().ino();
    link_within(&root, deep, &history.join(deep), false).unwrap();
    assert_eq!(
        std::fs::symlink_metadata(native.join(deep)).unwrap().ino(),
        before
    );
    // A changed source moves the link on.
    let other = history.join("projects/app/two.jsonl");
    h.write(&other, "turn");
    link_within(&root, deep, &other, false).unwrap();
    assert_eq!(std::fs::read_link(native.join(deep)).unwrap(), other);
    // Only plain names are walked.
    for bad in ["../x", "/x", "./x", "a/../b", ""] {
        assert!(
            link_within(&root, Path::new(bad), &sign_in, false).is_err(),
            "{bad}"
        );
    }
    assert_eq!(std::fs::read_to_string(&sign_in).unwrap(), "secret");
}

#[test]
fn a_link_planted_where_a_history_folder_should_be_is_replaced_not_followed() {
    let h = Home::new();
    let (native, root, _, history) = history_setup(&h);
    // The person's own project, reachable through a link the agent plants.
    let victim = h.home.join("Projects/victim");
    h.write(&victim.join("app/one.jsonl"), "precious");
    h.write(&victim.join("app/keep/notes.md"), "keep");
    let outside = tree(&h.home.join("Projects"));
    let config = tree(&h.home.join(".claude"));
    symlink(&victim, native.join("projects")).unwrap();
    let deep = Path::new("projects/app/one.jsonl");
    link_within(&root, deep, &history.join(deep), false).unwrap();
    assert!(
        !std::fs::symlink_metadata(native.join("projects"))
            .unwrap()
            .is_symlink()
    );
    assert_eq!(
        std::fs::read_link(native.join(deep)).unwrap(),
        history.join(deep)
    );
    // One level down too: a real `projects` holding a planted `app`.
    std::fs::remove_dir_all(native.join("projects")).unwrap();
    std::fs::create_dir(native.join("projects")).unwrap();
    symlink(victim.join("app"), native.join("projects/app")).unwrap();
    link_within(&root, deep, &history.join(deep), false).unwrap();
    assert!(native.join("projects/app").is_dir());
    assert!(
        !std::fs::symlink_metadata(native.join("projects/app"))
            .unwrap()
            .is_symlink()
    );
    assert_eq!(tree(&h.home.join("Projects")), outside);
    assert_eq!(tree(&h.home.join(".claude")), config);
}

#[test]
fn a_real_folder_or_file_at_a_history_link_is_moved_aside_not_deleted() {
    let h = Home::new();
    let (native, root, sign_in, history) = history_setup(&h);
    let projects = history.join("projects");
    h.write(&native.join("projects/app/notes.md"), "mine");
    h.write(&native.join("history.jsonl"), "the agent's");
    // An empty folder and an empty file are simply cleared.
    std::fs::create_dir_all(native.join("todos")).unwrap();
    h.write(&native.join("session.jsonl"), "");
    link_within(&root, Path::new("projects"), &projects, false).unwrap();
    link_within(&root, Path::new("history.jsonl"), &history.join("h"), false).unwrap();
    link_within(&root, Path::new("todos"), &history.join("todos"), false).unwrap();
    link_within(&root, Path::new("session.jsonl"), &history.join("s"), false).unwrap();
    assert_eq!(
        std::fs::read_link(native.join("projects")).unwrap(),
        projects
    );
    assert_eq!(
        std::fs::read_to_string(native.join(".ophio-aside-projects/app/notes.md")).unwrap(),
        "mine"
    );
    assert_eq!(
        std::fs::read_to_string(native.join(".ophio-aside-history.jsonl")).unwrap(),
        "the agent's"
    );
    assert!(std::fs::read_link(native.join("todos")).is_ok());
    assert!(std::fs::read_link(native.join("history.jsonl")).is_ok());
    assert!(std::fs::read_link(native.join("session.jsonl")).is_ok());
    let names: Vec<_> = std::fs::read_dir(&native)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(".ophio-aside-"))
        .collect();
    assert_eq!(names.len(), 2, "{names:?}");
    assert_eq!(std::fs::read_to_string(&sign_in).unwrap(), "secret");
}

#[test]
fn a_sign_in_left_at_the_sign_in_link_is_removed_not_kept_aside() {
    let h = Home::new();
    let (native, root, sign_in, _) = history_setup(&h);
    let name = Path::new(".credentials.json");
    h.write(&native.join(name), "a newer token");
    link_within(&root, name, &sign_in, true).unwrap();
    assert_eq!(std::fs::read_link(native.join(name)).unwrap(), sign_in);
    let aside: Vec<_> = std::fs::read_dir(&native)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(".ophio-aside-"))
        .collect();
    assert!(aside.is_empty(), "{aside:?}");
    assert_eq!(std::fs::read_to_string(&sign_in).unwrap(), "secret");
    // A folder there still follows the general rule.
    std::fs::remove_file(native.join(name)).unwrap();
    h.write(&native.join(name).join("notes.md"), "mine");
    link_within(&root, name, &sign_in, true).unwrap();
    assert_eq!(std::fs::read_link(native.join(name)).unwrap(), sign_in);
    assert_eq!(
        std::fs::read_to_string(native.join(".ophio-aside-.credentials.json/notes.md")).unwrap(),
        "mine"
    );
}

#[test]
fn a_record_of_the_longest_list_is_read_to_its_last_entries() {
    // The largest Claude list: 513 names, each as long as a name can be.
    assert!(RECORD_MOST >= 513 * ("skills/".len() + 255 + 1) as u64);
    let h = Home::new();
    let state = h.home.join(".local/share/ophio/sandboxes/t");
    let native = state.join("native");
    crate::sys::directories(&native.join("skills")).unwrap();
    crate::sys::directories(&native.join("rules")).unwrap();
    let mut record = String::new();
    for folder in ["rules", "skills"] {
        for n in 0..256 {
            record.push_str(&format!("{folder}/{n:03}-{}\n", "x".repeat(250)));
        }
    }
    // The last entries, past 64 KiB, are what an earlier turn left.
    for last in ["skills/zy", "skills/zz"] {
        record.push_str(last);
        record.push('\n');
        symlink(h.home.join("Projects"), native.join(last)).unwrap();
    }
    assert!(record.len() > 128 * 1024, "{}", record.len());
    std::fs::write(state.join(RECORD), record).unwrap();
    place(&native, &state, Vec::new(), true);
    for last in ["skills/zy", "skills/zz"] {
        assert!(
            std::fs::symlink_metadata(native.join(last)).is_err(),
            "{last}"
        );
    }
}
