// SPDX-License-Identifier: Apache-2.0
//! Which changes wait for the person to approve their exact revision before
//! they merge: built-in sensitive areas, the project's own protected paths
//! from its trusted review rules, and any area a reviewer raises. A model
//! can add an area, never take one away.
//!
//! The built-in areas are read from paths alone, generically for any
//! project: folder and file names are split into words, so `keystore.jks`
//! is signing and `keyboard.ts` is nothing. It errs toward asking, since
//! asking costs the person one tap. While a pull request Ophio makes needs
//! approval, nothing merges it but the person's own Merge, pinned to the
//! commit it was worked out for.
use crate::github::{project_tasks, refers_to, repo_key};
use crate::review_rules::{LoadedRules, glob_matches};
use crate::{CompanionHub, error};
use ports::github::Method;
use protocol::event::Event;
use protocol::github::TaskGithub;
use protocol::review::{ApprovalNeed, SensitiveArea};
use protocol::{ErrorCode, ProjectId, ProtocolError};

/// Most changed paths an approval names.
const MAX_PATHS: usize = 5;
/// Most reasons reviewers can add to one approval.
const MAX_RAISED: usize = 4;
/// Longest reason or label kept, in characters.
const MAX_REASON: usize = 200;
/// What each pull request's approval was last worked out as, apart from
/// what reviewers raised, with the base and commit it was worked out for,
/// by project, repository and number. It is worked out once for each: a
/// pull request moved onto another base, or whose base moved on, is worked
/// out again, and so is one whose files or rules couldn't be read for a
/// while.
pub(crate) const APPROVAL_WEIGHED: &str = "github_approval_weighed";
/// Pages of a pull request's files read, a hundred to a page. One with
/// more is too big to judge, and waits for the person.
const FILE_PAGES: usize = 3;

/// A built-in area, as the table below names it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Area {
    Permissions,
    PairingOrLogin,
    Credentials,
    Sandboxing,
    Updates,
    Signing,
    ReleaseOrBuild,
    Installers,
    DataChanges,
    ReviewPolicy,
}

impl Area {
    fn sensitive(self) -> SensitiveArea {
        match self {
            Area::Permissions => SensitiveArea::Permissions,
            Area::PairingOrLogin => SensitiveArea::PairingOrLogin,
            Area::Credentials => SensitiveArea::Credentials,
            Area::Sandboxing => SensitiveArea::Sandboxing,
            Area::Updates => SensitiveArea::Updates,
            Area::Signing => SensitiveArea::Signing,
            Area::ReleaseOrBuild => SensitiveArea::ReleaseOrBuild,
            Area::Installers => SensitiveArea::Installers,
            Area::DataChanges => SensitiveArea::DataChanges,
            Area::ReviewPolicy => SensitiveArea::ReviewPolicy,
        }
    }
}

/// One built-in area, each list separated by spaces: words that mark it
/// anywhere in a path, names a folder or file is exactly (without its
/// extension), and globs as `.gitignore` reads them.
struct Rule {
    area: Area,
    words: &'static str,
    stems: &'static str,
    globs: &'static str,
}

const RULES: &[Rule] = &[
    Rule {
        area: Area::Permissions,
        words: "permission permissions perm perms grant grants acl acls rbac abac authz \
            authorization authorisation authorize authorise authorizer access entitlement \
            entitlements privilege privileges capability capabilities sudo sudoers polkit \
            allowlist denylist whitelist blacklist firewall csp cors",
        stems: "",
        globs: "androidmanifest.xml info.plist *.entitlements",
    },
    Rule {
        area: Area::PairingOrLogin,
        words: "pair pairing paired login logins logout logon signin signup signout auth authn \
            authenticate authentication authenticator oauth oauth2 oidc saml sso session \
            sessions passkey passkeys webauthn fido fido2 mfa totp otp 2fa twofactor jwt \
            cookie cookies",
        stems: "",
        globs: "",
    },
    Rule {
        area: Area::Credentials,
        words: "credential credentials creds secret secrets token tokens apikey apikeys password \
            passwords passwd passphrase key keys keypair keyring keychain vault kms \
            privatekey ssh dotenv htpasswd",
        stems: "",
        globs: ".env .env.* *.env .npmrc .netrc .pypirc .pgpass id_rsa* id_ed25519* id_ecdsa* \
            id_dsa* *.pem *.key",
    },
    Rule {
        area: Area::Sandboxing,
        words: "sandbox sandboxed sandboxes sandboxing seccomp landlock bwrap bubblewrap \
            apparmor selinux seatbelt jail chroot firejail nsjail gvisor isolation \
            containment",
        stems: "",
        globs: "",
    },
    Rule {
        area: Area::Updates,
        words: "updater updaters autoupdate autoupdater selfupdate upgrader ota",
        stems: "update updates upgrade upgrades appupdate softwareupdate",
        globs: "",
    },
    Rule {
        area: Area::Signing,
        words: "sign signs signing signer signers signed signature signatures keystore keystores \
            notarize notarise notarization notary codesign codesigning gpg pgp certificate \
            certificates cert certs provisioning cosign sigstore minisign signtool apksigner \
            jarsigner",
        stems: "",
        globs: "*.jks *.keystore *.p12 *.pfx *.mobileprovision *.provisionprofile *.cer *.crt \
            *.der *.asc *.sig *.minisig *.gpg *.pbxproj *.xcconfig",
    },
    Rule {
        area: Area::ReleaseOrBuild,
        words: "release releases releasing publish publishing deploy deploys deployment \
            deployments ci buildscript toolchain",
        stems: "build builds",
        globs: "/.github/workflows/ /.github/actions/ /.gitlab/ .gitlab-ci.yml /.circleci/ \
            /.buildkite/ jenkinsfile azure-pipelines.yml /.azure-pipelines/ .travis.yml \
            appveyor.yml .appveyor.yml bitbucket-pipelines.yml .drone.yml /.woodpecker/ \
            .woodpecker.yml .ci/ build.rs makefile gnumakefile *.mk cmakelists.txt *.cmake \
            justfile dockerfile dockerfile.* *.dockerfile containerfile docker-compose*.yml \
            docker-compose*.yaml compose.yml compose.yaml *.gradle *.gradle.kts \
            gradle.properties gradle-wrapper.* gradlew gradlew.bat gradle/ /.mvn/ pom.xml \
            cargo.toml /.cargo/ rust-toolchain rust-toolchain.toml package.json .yarnrc \
            .yarnrc.yml pyproject.toml setup.py setup.cfg go.mod gemfile *.gemspec \
            /.github/dependabot.yml renovate.json .goreleaser.yml .goreleaser.yaml \
            package.swift package.resolved podfile cartfile cartfile.resolved *.pbxproj \
            *.xcconfig *.xcscheme *.lock *.lockb *.lockfile *.lock.json \
            package-lock.json npm-shrinkwrap.json pnpm-lock.yaml go.sum *requirements*.txt \
            **/requirements/*.txt constraints*.txt *requirements*.in pipfile environment.yml \
            environment.yaml pubspec.yaml mix.exs *.csproj *.fsproj *.vbproj nuget.config \
            directory.packages.props directory.build.props directory.build.targets \
            packages.config .pnpmfile.cjs pnpm-workspace.yaml deno.json deno.jsonc \
            bunfig.toml go.work flake.nix *.nix meson.build vcpkg.json conanfile.py \
            conanfile.txt *.podspec *.cabal cabal.project stack.yaml composer.json .gitmodules",
    },
    Rule {
        area: Area::Installers,
        words: "install installs installer installers uninstall uninstaller packaging msi msix \
            appx nsis wix appimage flatpak snapcraft homebrew winget chocolatey dmg pkgbuild \
            deb rpm",
        stems: "",
        globs: "/debian/ *.spec *.nsi *.nsh *.wxs *.iss *.desktop *.service",
    },
    Rule {
        area: Area::DataChanges,
        words: "migration migrations migrate migrator ddl alembic flyway liquibase backfill \
            dbmate",
        stems: "",
        globs: "*.sql *.prisma /db/migrate/ /db/schema.*",
    },
    Rule {
        area: Area::ReviewPolicy,
        words: "",
        stems: "",
        globs: "review.md codeowners",
    },
];

/// Extensions of prose files: documentation, not code or configuration.
const PROSE: &[&str] = &[".md", ".mdx", ".markdown", ".rst", ".adoc", ".txt"];

/// Words that, followed by another, mean something else: `sign in` is
/// signing in, and `key frames` aren't keys.
const NOT_BEFORE: &[(&str, &[&str])] = &[
    ("sign", &["in", "up", "out", "on", "off"]),
    (
        "key",
        &[
            "binding", "bindings", "board", "boards", "frame", "frames", "map", "maps", "press",
            "down", "up", "code", "codes", "word", "words", "stroke", "strokes", "value", "values",
            "event", "events", "handler", "handlers",
        ],
    ),
];

/// The words of a folder or file name: split at anything not a letter or
/// digit and where lower case turns upper, lowercased. Each pair of words
/// next to each other is a word too, so `sign_in` and `SignIn` are
/// `signin` as well.
fn words(name: &str) -> Vec<String> {
    let mut plain = Vec::new();
    let mut word = String::new();
    let chars: Vec<char> = name.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_alphanumeric() {
            if !word.is_empty() {
                plain.push(std::mem::take(&mut word));
            }
            continue;
        }
        let upper = c.is_uppercase();
        let after_lower = i > 0 && chars[i - 1].is_lowercase();
        // `HTTPServer` is `http` and `server`.
        let acronym_end = i > 0
            && chars[i - 1].is_uppercase()
            && chars.get(i + 1).is_some_and(|n| n.is_lowercase());
        if upper && (after_lower || acronym_end) && !word.is_empty() {
            plain.push(std::mem::take(&mut word));
        }
        word.extend(c.to_lowercase());
    }
    if !word.is_empty() {
        plain.push(word);
    }
    let mut all = Vec::new();
    for (i, w) in plain.iter().enumerate() {
        let next = plain.get(i + 1);
        let other = next.is_some_and(|n| {
            NOT_BEFORE
                .iter()
                .any(|(word, after)| word == w && after.contains(&n.as_str()))
        });
        if !other {
            all.push(w.clone());
        }
        if let Some(n) = next {
            all.push(format!("{w}{n}"));
        }
    }
    all
}

/// A folder's name, or a file's without its extensions, letters and
/// digits only: `auto_update.rs` is `autoupdate`.
fn stem(name: &str) -> String {
    let base = name.trim_start_matches('.');
    let base = base.split('.').next().unwrap_or_default();
    base.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// The built-in areas one path is in.
fn areas_of(path: &str) -> Vec<Area> {
    let names: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    // Prose changes no behaviour, so its names alone don't make it
    // sensitive: only the globs, such as `REVIEW.md`, apply to it.
    let prose = (names.last()).is_some_and(|n| {
        let n = n.to_ascii_lowercase();
        PROSE.iter().any(|ext| n.ends_with(ext))
    });
    let (words, stems): (Vec<String>, Vec<String>) = if prose {
        Default::default()
    } else {
        (
            names.iter().flat_map(|n| words(n)).collect(),
            names.iter().map(|n| stem(n)).collect(),
        )
    };
    RULES
        .iter()
        .filter(|rule| {
            let listed = |list: &str, s: &str| list.split_whitespace().any(|w| w == s);
            words.iter().any(|w| listed(rule.words, w))
                || stems.iter().any(|s| listed(rule.stems, s))
                || rule.globs.split_whitespace().any(|g| glob_matches(g, path))
        })
        .map(|rule| rule.area)
        .collect()
}

/// The sensitive areas `changed` (repository paths) touches, with the
/// paths that made each so.
pub(crate) fn classify(
    changed: &[String],
    rules: &LoadedRules,
) -> (Vec<SensitiveArea>, Vec<String>) {
    let mut built_in = Vec::new();
    let mut labels: Vec<&str> = Vec::new();
    let mut paths = Vec::new();
    for path in changed {
        let mut hit = false;
        for area in areas_of(path) {
            hit = true;
            if !built_in.contains(&area) {
                built_in.push(area);
            }
        }
        for (label, glob) in &rules.protected {
            if glob_matches(glob, path) {
                hit = true;
                if !labels.contains(&label.as_str()) {
                    labels.push(label);
                }
            }
        }
        if hit && paths.len() < MAX_PATHS && !paths.contains(path) {
            paths.push(path.clone());
        }
    }
    // In the order they are declared, whatever order the paths came in.
    let mut areas: Vec<SensitiveArea> = RULES
        .iter()
        .filter(|r| built_in.contains(&r.area))
        .map(|r| r.area.sensitive())
        .collect();
    areas.extend(labels.into_iter().map(|label| SensitiveArea::ProjectRule {
        label: line(label, MAX_REASON),
    }));
    (areas, paths)
}

/// What a change at `head` touching `changed` needs, adding the areas a
/// reviewer `raised`, in its words. None when nothing is sensitive. Rules
/// that couldn't all be read count against it: the paths they protect are
/// unknown.
pub(crate) fn need(
    head: &str,
    changed: &[String],
    rules: &LoadedRules,
    raised: &[String],
) -> Option<ApprovalNeed> {
    let (mut areas, paths) = classify(changed, rules);
    let unknown = match rules.read() {
        true => rules.unknown.as_deref(),
        false => Some(
            rules
                .used
                .missing
                .as_deref()
                .unwrap_or("They couldn't be read."),
        ),
    };
    if let Some(why) = unknown {
        areas.push(SensitiveArea::Unchecked {
            reason: line(
                &format!("Ophio couldn't check the project's protected paths. {why}"),
                MAX_REASON,
            ),
        });
    }
    let mut need = ApprovalNeed {
        areas,
        paths,
        head: head.to_owned(),
    };
    add_raised(&mut need, raised);
    (!need.areas.is_empty()).then_some(need)
}

/// What a change at `head` needs when what it touches couldn't be told:
/// `reason`, with what reviewers `raised`.
pub(crate) fn unchecked(head: &str, reason: &str, raised: &[String]) -> ApprovalNeed {
    let mut need = ApprovalNeed {
        areas: vec![SensitiveArea::Unchecked {
            reason: line(reason, MAX_REASON),
        }],
        paths: Vec::new(),
        head: head.to_owned(),
    };
    add_raised(&mut need, raised);
    need
}

/// The reasons reviewers raised in `need`, to carry to a newer commit.
pub(crate) fn raised(need: Option<&ApprovalNeed>) -> Vec<String> {
    need.into_iter()
        .flat_map(|n| &n.areas)
        .filter_map(|a| match a {
            SensitiveArea::Reviewer { reason } => Some(reason.clone()),
            _ => None,
        })
        .collect()
}

/// Adds what a reviewer raised about pull request commit `head`, in its
/// words, to `github`'s approval. Reasons raised about an older commit
/// still hold for the newer one the pull request is at now. What is
/// raised only ever adds: nothing here can lift an approval.
pub(crate) fn raise(github: &mut TaskGithub, head: &str, reasons: &[String]) {
    let current = (github.pr.as_ref())
        .map(|p| p.head.clone())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| head.to_owned());
    let mut need = match github.approval.take() {
        Some(need) if need.head == current => need,
        // The commit changed: its other reasons are worked out again for
        // the new one, and what reviewers raised is kept meanwhile.
        old => ApprovalNeed {
            areas: (raised(old.as_ref()).into_iter())
                .map(|reason| SensitiveArea::Reviewer { reason })
                .collect(),
            paths: Vec::new(),
            head: current,
        },
    };
    add_raised(&mut need, reasons);
    github.approval = (!need.areas.is_empty()).then_some(need);
}

/// `fresh` for its commit, keeping what reviewers raised in `kept`.
#[cfg(test)]
pub(crate) fn merged(
    fresh: Option<ApprovalNeed>,
    head: &str,
    kept: Option<&ApprovalNeed>,
) -> Option<ApprovalNeed> {
    with_raised(fresh, head, &raised(kept))
}

/// `fresh` for its commit `head`, with the reasons reviewers raised,
/// `carried`.
fn with_raised(
    fresh: Option<ApprovalNeed>,
    head: &str,
    carried: &[String],
) -> Option<ApprovalNeed> {
    match fresh {
        Some(mut need) => {
            add_raised(&mut need, carried);
            Some(need)
        }
        None if carried.is_empty() => None,
        None => {
            let mut need = ApprovalNeed {
                areas: Vec::new(),
                paths: Vec::new(),
                head: head.to_owned(),
            };
            add_raised(&mut need, carried);
            Some(need)
        }
    }
}

fn add_raised(need: &mut ApprovalNeed, reasons: &[String]) {
    for reason in reasons {
        let reason = line(reason, MAX_REASON);
        let count = (need.areas.iter())
            .filter(|a| matches!(a, SensitiveArea::Reviewer { .. }))
            .count();
        if reason.is_empty() || count >= MAX_RAISED {
            continue;
        }
        let area = SensitiveArea::Reviewer { reason };
        if !need.areas.contains(&area) {
            need.areas.push(area);
        }
    }
}

/// One line of at most `max` characters.
fn line(s: &str, max: usize) -> String {
    let joined = s.split_whitespace().collect::<Vec<_>>().join(" ");
    match joined.chars().count() > max {
        true => format!("{}…", joined.chars().take(max - 1).collect::<String>()),
        false => joined,
    }
}

/// What a built-in area is, in plain words for a sentence.
fn plain(area: &SensitiveArea) -> Option<&'static str> {
    Some(match area {
        SensitiveArea::Permissions => "permissions or access control",
        SensitiveArea::PairingOrLogin => "pairing or signing in",
        SensitiveArea::Credentials => "credentials or secrets",
        SensitiveArea::Sandboxing => "sandboxing",
        SensitiveArea::Updates => "software updates",
        SensitiveArea::Signing => "signing",
        SensitiveArea::ReleaseOrBuild => "how it is built or released",
        SensitiveArea::Installers => "installers or packaging",
        SensitiveArea::DataChanges => "stored data, such as migrations",
        SensitiveArea::ReviewPolicy => "the review rules",
        SensitiveArea::ProjectRule { .. }
        | SensitiveArea::Reviewer { .. }
        | SensitiveArea::Unchecked { .. } => return None,
    })
}

/// What makes `need` sensitive, in plain sentences.
fn reasons(need: &ApprovalNeed) -> Vec<String> {
    let touched: Vec<String> = (need.areas.iter())
        .filter_map(plain)
        .map(str::to_owned)
        .collect();
    let labels: Vec<String> = (need.areas.iter())
        .filter_map(|a| match a {
            SensitiveArea::ProjectRule { label } => Some(label.clone()),
            _ => None,
        })
        .collect();
    let listed = crate::github_tools::listed;
    let mut said = Vec::new();
    if !touched.is_empty() {
        said.push(format!("This change touches {}.", listed(&touched)));
    }
    if !labels.is_empty() {
        let also = if touched.is_empty() {
            "This change"
        } else {
            "It also"
        };
        said.push(format!(
            "{also} touches {}, which the project's review rules protect.",
            listed(&labels)
        ));
    }
    for area in &need.areas {
        match area {
            SensitiveArea::Reviewer { reason } => {
                said.push(format!("The review asked for your approval: {reason}"));
            }
            SensitiveArea::Unchecked { reason } => said.push(reason.clone()),
            _ => {}
        }
    }
    said
}

/// Why `need` holds a merge for the person, in plain sentences.
pub(crate) fn why(need: &ApprovalNeed) -> String {
    let mut said = reasons(need);
    said.push(format!(
        "It doesn't merge by itself: merging it yourself approves commit {}.",
        short(&need.head)
    ));
    said.join(" ")
}

/// What an agent, or anything else merging without the person, is told
/// when `need` holds the merge.
pub(crate) fn refused(need: &ApprovalNeed) -> ProtocolError {
    let mut said = reasons(need);
    said.push(
        "Only the person can approve it, by merging this exact commit themselves. Stop, and say it's green."
            .to_owned(),
    );
    error(ErrorCode::PermissionDenied, said.join(" "))
}

/// What the person is told when their Merge comes before Ophio had worked
/// out why pull request `number` at its commit needs approval: they look
/// again, and merging then approves it.
pub(crate) fn look_again(number: u64, need: &ApprovalNeed) -> ProtocolError {
    let mut said = vec![format!(
        "Ophio has only just checked what pull request #{number} touches."
    )];
    said.extend(reasons(need));
    said.push(format!(
        "Look at it again before merging: merging it then approves commit {}.",
        short(&need.head)
    ));
    error(ErrorCode::Conflict, said.join(" "))
}

/// Whether `need` asks for more than the person was `shown`, when their
/// phone said what it showed.
pub(crate) fn unshown(need: &ApprovalNeed, shown: Option<&[SensitiveArea]>) -> bool {
    shown.is_some_and(|shown| need.areas.iter().any(|a| !shown.contains(a)))
}

fn short(head: &str) -> String {
    head.chars().take(7).collect()
}

/// The newest task making pull request `number` on `here`, linked to it or
/// having opened it: a pull request Ophio only reads is the person's.
fn making<'a>(
    tasks: &'a [protocol::model::Task],
    here: &str,
    number: u64,
) -> Option<&'a protocol::model::Task> {
    (tasks.iter())
        .filter(|t| t.state != protocol::model::TaskState::Archived)
        .filter(|t| {
            (t.github.as_deref())
                .is_some_and(|g| !g.link.read_only() && refers_to(g, here, number, true))
        })
        .max_by_key(|t| t.created_at)
}

/// An approval worked out for a pull request's commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Weighed {
    pub need: Option<ApprovalNeed>,
    /// The newest making task's approval changed just now: the person
    /// can't have seen it yet.
    pub fresh: bool,
}

/// What a pull request's approval was worked out as, apart from what
/// reviewers raised, and the base and commit it was worked out for.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Weighing {
    pub base: String,
    pub head: String,
    pub need: Option<ApprovalNeed>,
    /// The commit GitHub said the base branch was at, whose review rules
    /// it was worked out with. Once the base moves on, its rules may have
    /// changed, so it is worked out again.
    #[serde(default)]
    pub at: String,
}

/// Where pull request `number` on `here`'s weighing for `project_id` is kept.
pub(crate) fn weighing_key(project_id: ProjectId, here: &str, number: u64) -> String {
    format!("{project_id}#{here}#{number}")
}

/// Why a pull request's files couldn't be listed, and whether that lasts
/// for its commit, or trying again may do better.
struct Unlisted {
    why: String,
    lasting: bool,
}

/// The approval every task about pull request `number` on `here`, at
/// `head`, is to have: `worked` with every reason a reviewer raised on any
/// task about it, in any project, for this commit or an older one. Another
/// project's task counts when it names this repository.
fn settled(
    tasks: &[protocol::model::Task],
    project_id: ProjectId,
    here: &str,
    number: u64,
    head: &str,
    worked: Option<ApprovalNeed>,
) -> Option<ApprovalNeed> {
    let mut carried: Vec<String> = Vec::new();
    for task in tasks {
        let Some(github) = task.github.as_deref() else {
            continue;
        };
        let named = task.project_id == project_id || github.repo.as_deref() == Some(here);
        if !named || !refers_to(github, here, number, true) {
            continue;
        }
        for reason in raised(github.approval.as_ref()) {
            if !carried.contains(&reason) {
                carried.push(reason);
            }
        }
    }
    with_raised(worked, head, &carried)
}

impl CompanionHub {
    /// What merging pull request `number` on `owner/name` at `head` into
    /// `base` needs from the person. It is worked out once for each commit
    /// and each commit of the base, and put on every task about the pull
    /// request, with what reviewers raised on any of them, and None when
    /// nothing is sensitive or no task of Ophio's makes the pull request.
    /// What can't be read for good counts against the change. A failure that
    /// may pass, such as GitHub not answering, changes nothing and is an
    /// error, so nothing merges on that pass and it is asked again next
    /// time.
    pub(crate) async fn approval_for(
        &self,
        project_id: ProjectId,
        (owner, name): (&str, &str),
        number: u64,
        head: &str,
        base: &str,
    ) -> Result<Weighed, ProtocolError> {
        let here = repo_key(owner, name);
        let key = weighing_key(project_id, &here, number);
        let unmade = Weighed {
            need: None,
            fresh: false,
        };
        let made =
            self.read(|r| Ok(making(&project_tasks(r, project_id)?, &here, number).is_some()))?;
        if !made {
            return Ok(unmade);
        }
        // Where the base is now, from GitHub: what agents can change in the
        // project's folder never decides it.
        let at = (self.base_at(owner, name, base).await).map_err(|e| passing(&e.why))?;
        let known = self.read(|r| {
            let all = linked(r)?;
            let tasks: Vec<_> = (all.iter())
                .filter(|t| t.project_id == project_id)
                .cloned()
                .collect();
            let Some(speaker) = making(&tasks, &here, number) else {
                return Ok(None);
            };
            let shown = (speaker.github.as_deref()).and_then(|g| g.approval.clone());
            let kept = (r.get::<Weighing>(APPROVAL_WEIGHED, &key)?)
                .filter(|w| w.base == base && w.head == head && w.at == at);
            // Worked out already, and every task has it: nothing to do.
            if let Some(kept) = &kept {
                let next = settled(&all, project_id, &here, number, head, kept.need.clone());
                let every = (tasks.iter())
                    .filter_map(|t| t.github.as_deref())
                    .filter(|g| refers_to(g, &here, number, true))
                    .all(|g| g.approval == next);
                if every {
                    return Ok(Some(Err(shown)));
                }
            }
            Ok(Some(Ok(kept)))
        })?;
        let kept = match known {
            None => return Ok(unmade),
            Some(Err(shown)) => {
                return Ok(Weighed {
                    need: shown,
                    fresh: false,
                });
            }
            Some(Ok(known)) => known,
        };
        let (worked, lasting) = match kept {
            Some(kept) => (kept.need, false),
            None => match self.pr_files(owner, name, number, head).await {
                Err(e) if e.lasting => (Some(unchecked(head, &e.why, &[])), true),
                Err(e) => return Err(passing(&e.why)),
                Ok(changed) => {
                    let gh = (self.github()).map_err(|_| {
                        passing("Ophio couldn't reach GitHub to read the review rules.")
                    })?;
                    let rules = (crate::review_rules::protected_on_github)(
                        &*gh,
                        (owner, name),
                        &at,
                        &changed,
                    )
                    .await;
                    if rules.retry {
                        let why = (rules.unknown.as_deref())
                            .or(rules.used.missing.as_deref())
                            .unwrap_or("The review rules couldn't be read.");
                        return Err(passing(why));
                    }
                    (need(head, &changed, &rules, &[]), true)
                }
            },
        };
        let now = self.inner.clock.now();
        self.write(|tx| {
            let all = linked(&tx.read())?;
            let tasks: Vec<_> = (all.iter())
                .filter(|t| t.project_id == project_id)
                .cloned()
                .collect();
            let Some(speaker) = making(&tasks, &here, number).map(|t| t.task_id) else {
                return Ok(Weighed {
                    need: None,
                    fresh: false,
                });
            };
            let next = settled(&all, project_id, &here, number, head, worked.clone());
            let mut changed = false;
            let mut fresh = false;
            for mut task in tasks {
                let Some(github) = task.github.as_deref_mut() else {
                    continue;
                };
                if !refers_to(github, &here, number, true) || github.approval == next {
                    continue;
                }
                fresh |= task.task_id == speaker;
                github.approval = next.clone();
                task.revision += 1;
                task.updated_at = now;
                crate::runs::carry_permits(tx, &task, task.revision - 1)?;
                tx.put("task", task.task_id, &task)?;
                tx.emit(Event::TaskUpserted { task })?;
                changed = true;
            }
            if changed {
                tx.emit(Event::PrChanged {
                    project_id,
                    number,
                    task_id: Some(speaker),
                })?;
            }
            if lasting {
                let weighing = Weighing {
                    base: base.to_owned(),
                    head: head.to_owned(),
                    need: worked.clone(),
                    at: at.clone(),
                };
                tx.put(APPROVAL_WEIGHED, &key, &weighing)?;
            }
            Ok(Weighed { need: next, fresh })
        })
    }

    /// The commit GitHub says `base` is at now, or why it can't be told.
    async fn base_at(&self, owner: &str, name: &str, base: &str) -> Result<String, Unlisted> {
        let unread = || Unlisted {
            why: format!(
                "Ophio couldn't ask GitHub where `{base}` is now, so it can't read the review rules there."
            ),
            lasting: false,
        };
        let gh = self.github().map_err(|_| unread())?;
        let path = format!("/repos/{owner}/{name}");
        let at = (crate::github_train::head_of(&*gh, &path, base).await).map_err(|_| unread())?;
        // Only a commit's name goes on into a link or a question to GitHub.
        (!at.is_empty() && at.chars().all(|c| c.is_ascii_alphanumeric()))
            .then_some(at)
            .ok_or_else(unread)
    }

    /// Refuses a merge without the person while the task's pull request
    /// waits for their approval, of whichever commit.
    pub(crate) fn not_awaiting_approval(
        &self,
        task_id: protocol::TaskId,
    ) -> Result<(), ProtocolError> {
        let task: protocol::model::Task = self.read(|r| r.require("task", task_id))?;
        match task.github.as_deref().and_then(|g| g.approval.as_ref()) {
            Some(need) => Err(refused(need)),
            None => Ok(()),
        }
    }

    /// The paths pull request `number` at commit `head` changes, renamed
    /// ones under both names, or why they couldn't be told. A pull request
    /// that moved on while it was read isn't told for `head`.
    async fn pr_files(
        &self,
        owner: &str,
        name: &str,
        number: u64,
        head: &str,
    ) -> Result<Vec<String>, Unlisted> {
        let unread = || {
            Unlisted {
            why: "Ophio couldn't list the files this change touches, so it can't tell whether any are sensitive.".to_owned(),
            lasting: false,
        }
        };
        let moved = || Unlisted {
            why: "The pull request moved to a new commit while Ophio was reading what it touches."
                .to_owned(),
            lasting: false,
        };
        let gh = self.github().map_err(|_| unread())?;
        let mut files = Vec::new();
        for page in 1..=FILE_PAGES {
            let path =
                format!("/repos/{owner}/{name}/pulls/{number}/files?per_page=100&page={page}");
            let list = (gh.rest(Method::Get, &path, None).await).map_err(|_| unread())?;
            let entries = list.as_array().ok_or_else(unread)?;
            for entry in entries {
                let path = entry["filename"].as_str().ok_or_else(unread)?;
                if listed_at(entry).is_some_and(|at| at != head) {
                    return Err(moved());
                }
                files.push(path.to_owned());
                if let Some(old) = entry["previous_filename"].as_str() {
                    files.push(old.to_owned());
                }
            }
            if entries.len() < 100 {
                // A listing over several pages may have moved between them,
                // leaving a later page short or empty: it is told for `head`
                // only if the pull request is still there.
                if page > 1 {
                    let path = format!("/repos/{owner}/{name}/pulls/{number}");
                    let now = (gh.rest(Method::Get, &path, None).await).map_err(|_| unread())?;
                    if now["head"]["sha"].as_str() != Some(head) {
                        return Err(moved());
                    }
                }
                return Ok(files);
            }
        }
        Err(Unlisted {
            why: format!(
                "This change touches more than {} files, too many for Ophio to judge.",
                FILE_PAGES * 100
            ),
            lasting: true,
        })
    }
}

/// What a merge is told while whether a change needs the person's approval
/// can't be told for a moment, `why`: it waits, and nothing changes.
fn passing(why: &str) -> ProtocolError {
    error(
        ErrorCode::TemporarilyUnavailable,
        format!(
            "Ophio can't tell yet whether this change needs your approval. {why} Nothing merges it until Ophio can tell. Try again in a moment."
        ),
    )
}

/// Every task linked to GitHub, in any project.
fn linked(r: &store::Read<'_>) -> store::Result<Vec<protocol::model::Task>> {
    Ok((r.list::<protocol::model::Task>("task")?.into_iter())
        .filter(|t| t.github.is_some())
        .collect())
}

/// The commit GitHub listed a changed file at, read from its contents
/// link. A removed file's is the commit it was removed from, so it says
/// nothing of the pull request's head.
fn listed_at(entry: &serde_json::Value) -> Option<&str> {
    if entry["status"].as_str() == Some("removed") {
        return None;
    }
    let url = entry["contents_url"].as_str()?;
    let (_, query) = url.split_once('?')?;
    (query.split('&')).find_map(|pair| pair.strip_prefix("ref="))
}

#[cfg(test)]
#[path = "review_gate_tests.rs"]
mod tests;
