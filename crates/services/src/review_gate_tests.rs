// SPDX-License-Identifier: Apache-2.0
use super::*;
use SensitiveArea::*;
use protocol::review::ReviewRulesUsed;

fn rules() -> LoadedRules {
    LoadedRules {
        used: ReviewRulesUsed {
            revision: Some("base".into()),
            source: Some("origin/main".into()),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn paths(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

fn areas(path: &str) -> Vec<SensitiveArea> {
    classify(&paths(&[path]), &rules()).0
}

#[test]
fn finds_each_built_in_area() {
    let cases: &[(&str, SensitiveArea)] = &[
        (".github/workflows/checks.yml", ReleaseOrBuild),
        (".gitlab-ci.yml", ReleaseOrBuild),
        ("scripts/release.sh", ReleaseOrBuild),
        ("tools/build.mjs", ReleaseOrBuild),
        ("crates/app/build.rs", ReleaseOrBuild),
        ("android/app/build.gradle.kts", ReleaseOrBuild),
        ("Dockerfile", ReleaseOrBuild),
        ("client/package.json", ReleaseOrBuild),
        ("Cargo.toml", ReleaseOrBuild),
        ("deploy/prod.yaml", ReleaseOrBuild),
        ("packaging/arch/PKGBUILD", Installers),
        ("scripts/install.sh", Installers),
        ("installer/windows/setup.nsi", Installers),
        ("debian/control", Installers),
        ("dist/ophio.desktop", Installers),
        ("android/app/release.keystore", Signing),
        ("certs/server.crt", Signing),
        ("src/signing/mod.rs", Signing),
        ("scripts/notarize.sh", Signing),
        ("crates/companion/src/update.rs", Updates),
        ("src/auto_update/check.rs", Updates),
        ("src/AppUpdater.kt", Updates),
        ("crates/sandbox/src/lib.rs", Sandboxing),
        ("profiles/seccomp.json", Sandboxing),
        ("src/permissions.rs", Permissions),
        ("crates/domain/src/grants.rs", Permissions),
        ("src/AccessControl.kt", Permissions),
        ("android/app/src/main/AndroidManifest.xml", Permissions),
        ("ios/App/Info.plist", Permissions),
        ("src/pairing.rs", PairingOrLogin),
        ("src/screens/SignIn.tsx", PairingOrLogin),
        ("web/login/page.tsx", PairingOrLogin),
        ("src/oauth/callback.rs", PairingOrLogin),
        ("src/auth.rs", PairingOrLogin),
        ("src/credentials.rs", Credentials),
        ("src/secret_store.rs", Credentials),
        ("src/api_key.rs", Credentials),
        (".env.production", Credentials),
        ("config/id_ed25519.pub", Credentials),
        ("src/tokens.rs", Credentials),
        ("db/migrations/0003_add_users.sql", DataChanges),
        ("src/store/migrate.rs", DataChanges),
        ("prisma/schema.prisma", DataChanges),
        ("db/schema.rb", DataChanges),
        ("REVIEW.md", ReviewPolicy),
        ("crates/billing/REVIEW.md", ReviewPolicy),
        (".github/CODEOWNERS", ReviewPolicy),
    ];
    for (path, area) in cases {
        assert!(
            areas(path).contains(area),
            "{path} should be {area:?}, got {:?}",
            areas(path)
        );
    }
}

#[test]
fn lockfiles_and_what_else_pulls_in_code_are_how_it_is_built() {
    for path in [
        "Cargo.lock",
        "client/package-lock.json",
        "npm-shrinkwrap.json",
        "web/yarn.lock",
        "pnpm-lock.yaml",
        "go.sum",
        "poetry.lock",
        "Pipfile.lock",
        "uv.lock",
        "Gemfile.lock",
        "composer.lock",
        "requirements.txt",
        "backend/requirements-dev.txt",
        "dev-requirements.txt",
        "test-requirements.txt",
        "requirements/prod.txt",
        "backend/requirements/base.txt",
        "constraints.txt",
        "bun.lock",
        "bun.lockb",
        "deno.lock",
        "ios/Podfile.lock",
        "ios/Podfile",
        "app/gradle.lockfile",
        "src/App/packages.lock.json",
        "pubspec.lock",
        "mix.lock",
        "flake.lock",
        "ios/App/Package.resolved",
        "Package.swift",
        "Cartfile.resolved",
        "android/gradle/libs.versions.toml",
        "client/src-tauri/gen/android/gradle/wrapper/gradle-wrapper.properties",
        "android/gradle/wrapper/gradle-wrapper.jar",
        "android/app/build.gradle.kts",
        "android/settings.gradle",
        "Pipfile",
        "app/pubspec.yaml",
        "mix.exs",
        "src/App/App.csproj",
        "Directory.Packages.props",
        "src/App/packages.config",
        ".pnpmfile.cjs",
        "pnpm-workspace.yaml",
        "flake.nix",
        "shell.nix",
        "meson.build",
        "environment.yml",
        "requirements.in",
        "requirements-dev.in",
        "go.work",
        "bunfig.toml",
        "vcpkg.json",
        "conanfile.py",
        "deno.json",
        "composer.json",
        ".gitmodules",
    ] {
        assert!(
            areas(path).contains(&ReleaseOrBuild),
            "{path}: {:?}",
            areas(path)
        );
    }
}

#[test]
fn the_iphone_project_is_how_it_is_built_and_signed() {
    for path in [
        "ios/App.xcodeproj/project.pbxproj",
        "ios/Config/Release.xcconfig",
    ] {
        let found = areas(path);
        assert!(
            found.contains(&ReleaseOrBuild) && found.contains(&Signing),
            "{path}: {found:?}"
        );
    }
    let scheme = "ios/App.xcodeproj/xcshareddata/xcschemes/App.xcscheme";
    assert!(areas(scheme).contains(&ReleaseOrBuild));
}

#[test]
fn near_misses_are_not_sensitive() {
    for path in [
        "client/src/keyboard.ts",
        "src/ui/KeyboardShortcuts.tsx",
        "src/keyBindings.ts",
        "src/keyframes.css",
        "src/author.rs",
        "docs/BUILDING.md",
        "src/designer.rs",
        "src/updateUser.ts",
        "src/accessibility.ts",
        "src/tokenizer.rs",
        "src/sessionless_widget.rs",
        "src/signal.rs",
        "src/assign.rs",
        "src/pairs_of_cards.rs",
        "src/user.spec.ts",
        "src/release_notes_view_test_data.json.bak",
        "src/README.md",
        "docs/requirements.md",
        "docs/notes.txt",
        "notes/review_notes.md",
        "src/screens/Settings.tsx",
        "src/monkey.rs",
        "src/turkey_trot.rs",
        "src/install_count_chart.svg",
        "docs/agent-logins.md",
        "crates/sandbox/README.md",
        "client/src/diagnostics/stateSchema.json",
        "crates/report-pipeline/src/schema.rs",
        "src/lock.rs",
        "src/lockfile.rs",
        "src/clock.rs",
        "src/gradle_helper.rs",
        "docs/package-swift-notes.md",
        "docs/requirements-gathering.md",
        "src/environment.ts",
        "src/flake.rs",
        "src/mix.ts",
    ] {
        let found = areas(path);
        let allowed = match path {
            // Words worth asking about, kept on purpose.
            "src/release_notes_view_test_data.json.bak" => vec![ReleaseOrBuild],
            "src/install_count_chart.svg" => vec![Installers],
            _ => vec![],
        };
        assert_eq!(found, allowed, "{path}");
    }
}

#[test]
fn signing_in_is_not_signing() {
    assert_eq!(areas("src/sign_in.rs"), [PairingOrLogin]);
    assert_eq!(areas("src/SignUpForm.tsx"), [PairingOrLogin]);
    assert!(areas("src/sign_payload.rs").contains(&Signing));
}

#[test]
fn names_a_few_of_the_paths_and_keeps_areas_in_order() {
    let changed = paths(&[
        "src/lib.rs",
        "REVIEW.md",
        ".github/workflows/a.yml",
        ".github/workflows/b.yml",
        ".github/workflows/c.yml",
        ".github/workflows/d.yml",
        ".github/workflows/e.yml",
        "src/permissions.rs",
    ]);
    let (areas, named) = classify(&changed, &rules());
    assert_eq!(areas, [Permissions, ReleaseOrBuild, ReviewPolicy]);
    assert_eq!(named.len(), MAX_PATHS);
    assert_eq!(named[0], "REVIEW.md");
    assert!(!named.contains(&"src/lib.rs".to_owned()));
}

#[test]
fn adds_the_projects_protected_paths() {
    let mut rules = rules();
    rules.protected = vec![
        ("payments".into(), "crates/billing/**".into()),
        ("the public API".into(), "api/*.proto".into()),
        ("payments".into(), "billing.md".into()),
    ];
    let (areas, named) = classify(
        &paths(&[
            "crates/billing/src/charge.rs",
            "docs/billing.md",
            "api/v1/x.proto",
        ]),
        &rules,
    );
    assert_eq!(
        areas,
        [ProjectRule {
            label: "payments".into()
        }]
    );
    assert_eq!(named, ["crates/billing/src/charge.rs", "docs/billing.md"]);
}

#[test]
fn needs_nothing_for_an_ordinary_change() {
    let changed = paths(&["src/lib.rs", "client/src/screens/Home.tsx", "README.md"]);
    assert_eq!(need("abc", &changed, &rules(), &[]), None);
    assert_eq!(need("abc", &[], &rules(), &[]), None);
}

#[test]
fn a_reviewer_adds_reasons_bounded() {
    let changed = paths(&["src/lib.rs"]);
    let raised: Vec<String> = (0..10)
        .map(|n| format!("reason   {n}\nsecond line {}", "x".repeat(300)))
        .collect();
    let need = need("abc", &changed, &rules(), &raised).unwrap();
    assert_eq!(need.head, "abc");
    assert_eq!(need.areas.len(), MAX_RAISED);
    match &need.areas[0] {
        Reviewer { reason } => {
            assert!(reason.starts_with("reason 0 second line"));
            assert!(!reason.contains('\n'));
            assert_eq!(reason.chars().count(), MAX_REASON);
        }
        other => panic!("{other:?}"),
    }
    // Blank and repeated reasons add nothing.
    let need = super::need(
        "abc",
        &changed,
        &rules(),
        &["  ".into(), "a".into(), "a".into()],
    );
    assert_eq!(need.unwrap().areas, [Reviewer { reason: "a".into() }]);
}

#[test]
fn rules_that_couldnt_be_read_count_against_the_change() {
    let mut unread = rules();
    unread.used.revision = None;
    unread.used.missing = Some("The base branch `origin/x` isn't on this computer.".into());
    let need = need("abc", &paths(&["src/lib.rs"]), &unread, &[]).unwrap();
    match &need.areas[..] {
        [Unchecked { reason }] => {
            assert!(reason.contains("protected paths"));
            assert!(reason.contains("origin/x"));
        }
        other => panic!("{other:?}"),
    }
    // Read from the base, but not all of them: the paths they protect may
    // be missing some.
    let mut part = rules();
    part.unknown = Some("`a/REVIEW.md` on `origin/main` couldn't be read.".into());
    let need = super::need("abc", &paths(&["src/lib.rs"]), &part, &[]).unwrap();
    match &need.areas[..] {
        [Unchecked { reason }] => assert!(reason.contains("a/REVIEW.md"), "{reason}"),
        other => panic!("{other:?}"),
    }
    let need = unchecked(
        "abc",
        "The change's files couldn't be listed.",
        &["r".into()],
    );
    assert_eq!(need.areas.len(), 2);
}

fn github(head: &str) -> TaskGithub {
    serde_json::from_value(serde_json::json!({
        "link": { "type": "pr", "number": 3 },
        "pr": {
            "number": 3, "url": "", "state": "open", "draft": false, "head": head,
            "passed": 0, "failed": 0, "pending": 0, "unresolved": 0, "conflict": false
        }
    }))
    .unwrap()
}

#[test]
fn raising_only_adds_and_follows_the_current_commit() {
    let mut g = github("new");
    raise(&mut g, "new", &[]);
    assert_eq!(g.approval, None);
    raise(&mut g, "new", &["It changes who can approve.".into()]);
    let need = g.approval.clone().unwrap();
    assert_eq!(need.head, "new");
    assert_eq!(
        need.areas,
        [Reviewer {
            reason: "It changes who can approve.".into()
        }]
    );

    // Raised about an older commit, it holds the current one.
    g.approval = Some(ApprovalNeed {
        areas: vec![Updates],
        paths: vec!["src/update.rs".into()],
        head: "new".into(),
    });
    raise(&mut g, "old", &["Also risky.".into()]);
    let need = g.approval.clone().unwrap();
    assert_eq!(need.head, "new");
    assert_eq!(
        need.areas,
        [
            Updates,
            Reviewer {
                reason: "Also risky.".into()
            }
        ]
    );

    // A need for an older commit gives way to the current one, keeping
    // only what reviewers raised until it is worked out again.
    g.approval = Some(ApprovalNeed {
        areas: vec![
            Updates,
            Reviewer {
                reason: "Kept.".into(),
            },
        ],
        paths: vec![],
        head: "older".into(),
    });
    raise(&mut g, "new", &["More.".into()]);
    let need = g.approval.unwrap();
    assert_eq!(need.head, "new");
    assert_eq!(
        need.areas,
        [
            Reviewer {
                reason: "Kept.".into()
            },
            Reviewer {
                reason: "More.".into()
            }
        ]
    );
}

#[test]
fn a_new_commit_keeps_what_reviewers_raised() {
    let old = ApprovalNeed {
        areas: vec![
            Signing,
            Reviewer {
                reason: "Kept.".into(),
            },
        ],
        paths: vec!["a.jks".into()],
        head: "old".into(),
    };
    assert_eq!(merged(None, "new", None), None);
    let kept = merged(None, "new", Some(&old)).unwrap();
    assert_eq!(kept.head, "new");
    assert_eq!(
        kept.areas,
        [Reviewer {
            reason: "Kept.".into()
        }]
    );
    let fresh = need("new", &paths(&["src/update.rs"]), &rules(), &[]);
    let both = merged(fresh, "new", Some(&old)).unwrap();
    assert_eq!(
        both.areas,
        [
            Updates,
            Reviewer {
                reason: "Kept.".into()
            }
        ]
    );
}

#[test]
fn says_why_in_plain_sentences() {
    let only = ApprovalNeed {
        areas: vec![ProjectRule {
            label: "payments".into(),
        }],
        paths: vec![],
        head: "abc".into(),
    };
    assert_eq!(
        why(&only),
        "This change touches payments, which the project's review rules protect. It doesn't merge by itself: merging it yourself approves commit abc."
    );
    let need = ApprovalNeed {
        areas: vec![
            Updates,
            Signing,
            ProjectRule {
                label: "payments".into(),
            },
            Reviewer {
                reason: "It changes refunds.".into(),
            },
        ],
        paths: vec![],
        head: "0123456789".into(),
    };
    assert_eq!(
        why(&need),
        "This change touches software updates and signing. It also touches payments, which the project's review rules protect. The review asked for your approval: It changes refunds. It doesn't merge by itself: merging it yourself approves commit 0123456."
    );
}
