// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::github_watch::{Handover, Watch};
use ports::github::{Github, GithubError, Method};
use protocol::capability::{Availability, HarnessInfo, ModeSupport};
use protocol::github::*;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Mutex;

/// GitHub as recorded: REST answers by method and path, GraphQL answers by
/// a word of the query. Anything else is GitHub's 404.
#[derive(Debug, Default)]
struct FakeGithub {
    rest: Mutex<HashMap<String, Result<Value, GithubError>>>,
    /// Answers to calls starting with a prefix, where none is exact.
    prefixed: Mutex<Vec<(String, Value)>>,
    graphql: Mutex<Vec<(&'static str, Result<Value, GithubError>)>>,
    calls: Mutex<Vec<(String, Value)>>,
    meanwhile: Mutex<Vec<(&'static str, Meanwhile)>>,
    paused: Mutex<Vec<(&'static str, Paused)>>,
    /// The repository GitHub's copy of a commit's files is read from,
    /// where no answer is recorded.
    files_from: Mutex<Option<PathBuf>>,
}
#[derive(Debug)]
struct Paused {
    started: tokio::sync::oneshot::Sender<()>,
    release: tokio::sync::oneshot::Receiver<()>,
}
/// What happens while a query is answered: the person, acting meanwhile.
struct Meanwhile(Box<dyn FnOnce() + Send>);
impl std::fmt::Debug for Meanwhile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Meanwhile")
    }
}
impl FakeGithub {
    /// Does `then` once, while the next call or query containing `word`
    /// is asked.
    fn meanwhile(&self, word: &'static str, then: impl FnOnce() + Send + 'static) {
        (self.meanwhile.lock().unwrap()).push((word, Meanwhile(Box::new(then))));
    }
    fn pause(
        &self,
        word: &'static str,
    ) -> (
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (started, arrived) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        self.paused.lock().unwrap().push((
            word,
            Paused {
                started,
                release: released,
            },
        ));
        (arrived, release)
    }
    async fn then(&self, asked: &str) {
        let then = {
            let mut hooks = self.meanwhile.lock().unwrap();
            (hooks.iter().position(|(word, _)| asked.contains(word))).map(|i| hooks.remove(i).1)
        };
        if let Some(Meanwhile(then)) = then {
            then();
        }
        let paused = {
            let mut hooks = self.paused.lock().unwrap();
            hooks
                .iter()
                .position(|(word, _)| asked.contains(word))
                .map(|i| hooks.remove(i).1)
        };
        if let Some(paused) = paused {
            paused.started.send(()).unwrap();
            paused.release.await.unwrap();
        }
    }
    fn answer(&self, call: &str, value: Value) {
        self.rest.lock().unwrap().insert(call.into(), Ok(value));
    }
    /// The answer to calls starting with `prefix` that have none of
    /// their own, ahead of earlier ones.
    fn answer_prefixed(&self, prefix: &str, value: Value) {
        (self.prefixed.lock().unwrap()).insert(0, (prefix.into(), value));
    }
    fn refuse(&self, call: &str, code: u16) {
        let message = format!("Synthetic {code}");
        (self.rest.lock().unwrap()).insert(call.into(), Err(GithubError::Status { code, message }));
    }
    /// The answer to queries containing `word`, ahead of earlier ones.
    fn query(&self, word: &'static str, data: Value) {
        self.graphql.lock().unwrap().insert(0, (word, Ok(data)));
    }
    /// Queries containing `word` fail, ahead of earlier answers.
    fn fail_query(&self, word: &'static str) {
        let failed = Err(GithubError::Status {
            code: 502,
            message: "Synthetic 502".into(),
        });
        self.graphql.lock().unwrap().insert(0, (word, failed));
    }
    fn called(&self, call: &str) -> Option<Value> {
        let calls = self.calls.lock().unwrap();
        calls
            .iter()
            .find(|(c, _)| c == call)
            .map(|(_, v)| v.clone())
    }
}
#[async_trait::async_trait]
impl Github for FakeGithub {
    async fn report_git(
        &self,
        repository: &str,
        _mirror: &Path,
        operation: ports::github::ReportGit,
    ) -> Result<(), GithubError> {
        let call = format!("GIT {repository} {operation:?}");
        self.calls.lock().unwrap().push((call.clone(), Value::Null));
        self.then(&call).await;
        Ok(())
    }
    async fn rest(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, GithubError> {
        let call = format!("{} {path}", method.as_str());
        (self.calls.lock().unwrap()).push((call.clone(), body.unwrap_or(Value::Null)));
        self.then(&call).await;
        if let Some(answer) = self.rest.lock().unwrap().get(&call).cloned() {
            return answer;
        }
        if method == Method::Get
            && (path.contains("/comments?")
                || path.contains("/reviews?")
                || path.contains("/files?"))
        {
            return Ok(json!([]));
        }
        let prefixed = self.prefixed.lock().unwrap();
        (prefixed
            .iter()
            .find(|(prefix, _)| call.starts_with(prefix.as_str())))
        .map(|(_, value)| Ok(value.clone()))
        .unwrap_or(Err(GithubError::Status {
            code: 404,
            message: "Not Found".into(),
        }))
    }
    async fn graphql(&self, query: &str, variables: Value) -> Result<Value, GithubError> {
        (self.calls.lock().unwrap()).push((format!("GRAPHQL {query}"), variables.clone()));
        self.then(query).await;
        // As GitHub does: a commit's file that isn't there is an error when
        // asked for by path, and nothing when asked for by expression.
        if query.contains("file(path:") {
            return Err(GithubError::Status {
                code: 200,
                message: "Could not resolve file for path".into(),
            });
        }
        let answers = self.graphql.lock().unwrap();
        let recorded = (answers.iter().find(|(word, _)| query.contains(word)))
            .map(|(_, answer)| answer.clone());
        let from = self.files_from.lock().unwrap().clone();
        match (recorded, from) {
            (Some(answer), _) => answer,
            (None, Some(dir)) if query.contains("isTruncated") => Ok(files_at(&dir, &variables)),
            _ => Err(GithubError::Status {
                code: 200,
                message: "Synthetic: no answer recorded".into(),
            }),
        }
    }
}

/// GitHub's answer to which files are at commit `at` in `variables`, read
/// from the repository at `dir`: at that commit where it has it, else at
/// `origin/main` as fetched. One that isn't there, or isn't a file, is
/// nothing or not a blob, as GitHub answers.
fn files_at(dir: &Path, variables: &Value) -> Value {
    let git = |args: &[&str]| {
        let out = (std::process::Command::new("git")
            .args(args)
            .current_dir(dir))
        .output();
        out.ok().filter(|o| o.status.success()).map(|o| o.stdout)
    };
    let at = variables["at"].as_str().unwrap_or_default();
    let known = at.len() == 40 && git(&["cat-file", "-e", &format!("{at}^{{commit}}")]).is_some();
    let rev = if known {
        at
    } else {
        "refs/remotes/origin/main"
    };
    let mut repository = serde_json::Map::new();
    repository.insert("c".into(), json!({ "oid": at }));
    for (key, expression) in variables.as_object().unwrap() {
        let Some(n) = key.strip_prefix('p') else {
            continue;
        };
        let (_, path) = expression.as_str().unwrap().split_once(':').unwrap();
        let object = format!("{rev}:{path}");
        let kind = git(&["cat-file", "-t", &object])
            .map(|t| String::from_utf8_lossy(&t).trim().to_owned());
        let entry = match (kind.as_deref(), git(&["cat-file", "blob", &object])) {
            (Some("blob"), Some(b)) => json!({ "__typename": "Blob", "byteSize": b.len(),
                "isBinary": false, "isTruncated": false, "text": String::from_utf8_lossy(&b) }),
            (Some(_), _) => json!({ "__typename": "Tree" }),
            (None, _) => Value::Null,
        };
        repository.insert(format!("f{n}"), entry);
    }
    json!({ "repository": repository })
}

struct Repo {
    f: Fixture,
    gh: Arc<FakeGithub>,
    project_id: ProjectId,
}

/// A project whose `origin` is octo/app on GitHub, administered by `me`.
async fn repo(admin: bool) -> Repo {
    repo_with(admin, |b| {
        b.sandboxes(Arc::new(
            crate::tests::conversation_access::Backend::default(),
        ))
    })
    .await
}

/// [`repo`], with the hub built as `configure` says too.
async fn repo_with(admin: bool, configure: impl FnOnce(HubBuilder) -> HubBuilder) -> Repo {
    let gh = Arc::new(FakeGithub::default());
    let port = gh.clone();
    let f = Fixture::with(move |b| configure(b.github(port))).await;
    let project_id = f.project().await;
    let dir = f.project_dir();
    *gh.files_from.lock().unwrap() = Some(dir.clone());
    for args in [
        &["init", "-q"][..],
        &["remote", "add", "origin", "git@github.com:octo/app.git"],
    ] {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(&dir)
            .status()
            .unwrap();
        assert!(status.success());
    }
    fetched_base(&dir, &[]);
    f.hub
        .write(|tx| {
            let mut infos: Vec<HarnessInfo> = tx.read().require("meta", "harnesses")?;
            infos[0].modes = [TaskMode::Ask, TaskMode::Plan, TaskMode::Work]
                .into_iter()
                .map(|mode| ModeSupport {
                    mode,
                    availability: Availability::Available,
                    enforcement: "Synthetic fixture restriction".into(),
                })
                .collect();
            tx.put("meta", "harnesses", &infos)
        })
        .unwrap();
    gh.answer("GET /user", json!({ "login": "me" }));
    gh.answer(
        "GET /repos/octo/app",
        json!({
            "default_branch": "main",
            "private": false,
            "html_url": "https://github.com/octo/app",
            "has_pages": false,
            "owner": { "login": "octo" },
            "permissions": { "admin": admin, "push": true },
        }),
    );
    gh.answer("GET /repos/octo/app/rules/branches/main", json!([]));
    gh.refuse("GET /repos/octo/app/branches/main/protection", 404);
    gh.answer(
        "GET /repos/octo/app/branches/main",
        json!({ "protected": false }),
    );
    gh.answer(
        "GET /repos/octo/app/deployments?ref=main&per_page=1",
        json!([]),
    );
    gh.answer(
        "GET /repos/octo/app/private-vulnerability-reporting",
        json!({ "enabled": true }),
    );
    // `main`'s head, and every branch up to date with it.
    gh.answer(
        "GET /repos/octo/app/git/ref/heads/main",
        json!({ "object": { "sha": "main0" } }),
    );
    gh.answer_prefixed(
        "GET /repos/octo/app/compare/main...",
        json!({ "behind_by": 0 }),
    );
    gh.query(
        "object(expression:$expression",
        json!({ "repository": { "object": { "entries": [
            { "name": "ci.yml", "object": { "text": "on:\n  pull_request:\njobs:\n  test: {}\n" } },
        ] } } }),
    );
    Repo { f, gh, project_id }
}

impl Repo {
    async fn act(&self, caller: &Caller, action: GithubAction) -> Result<(), ErrorCode> {
        let command = CommandEnvelope::new(Command::Github { action });
        let id = command.command_id;
        let r = self.f.hub.submit(caller, command).await;
        if let Some(e) = r.error {
            return Err(e.code);
        }
        settle(&self.f.hub, id).await;
        let stored = self.f.hub.read(|r| r.command(id)).unwrap().unwrap();
        stored.error.map_or(Ok(()), |e| Err(e.code))
    }
    async fn set(&self, settings: GithubSettings) -> Result<(), ErrorCode> {
        let project_id = self.project_id;
        self.act(
            &Caller::local_owner(),
            GithubAction::SetSettings {
                project_id,
                settings,
            },
        )
        .await
    }
    fn settings(&self) -> GithubSettings {
        self.f.hub.github_settings(self.project_id).unwrap()
    }
    /// Changes a task's link, as the watcher would.
    fn alter(&self, task_id: TaskId, change: impl FnOnce(&mut TaskGithub)) {
        let now = self.f.hub.inner.clock.now();
        (self.f.hub)
            .write(|tx| crate::github_watch::change_link(tx, task_id, now, change))
            .unwrap();
    }
    /// Points the project's `origin` at `url`.
    fn origin(&self, url: &str) {
        let status = std::process::Command::new("git")
            .args(["remote", "set-url", "origin", url])
            .current_dir(self.f.project_dir())
            .status()
            .unwrap();
        assert!(status.success());
    }
    /// GraphQL queries asked so far that contain `word`.
    fn queried(&self, word: &str) -> usize {
        let calls = self.gh.calls.lock().unwrap();
        (calls.iter())
            .filter(|(c, _)| c.starts_with("GRAPHQL ") && c.contains(word))
            .count()
    }
    async fn link(&self, link: GithubLink, prompt: &str) -> Result<Task, ProtocolError> {
        let r = (self.f.hub)
            .submit(
                &Caller::local_owner(),
                CommandEnvelope::new(Command::CreateTask {
                    github: Some(Box::new(link)),
                    agent_chosen: None,
                    mode: None,
                    project_id: self.project_id,
                    kind: TaskKind::Coding,
                    title: None,
                    prompt: prompt.into(),
                    bundle: None,
                    harness: HarnessKind::ClaudeCode,
                    model: None,
                    effort: None,
                    launch_policy: None,
                    branch: None,
                    mission: None,
                    target_id: None,
                    placement: TaskPlacement::Queue {
                        position: None,
                        authorization: QueueAuthorization::AskBeforeStart,
                    },
                    start: None,
                    place: None,
                    browser: None,
                    conversation: None,
                    access: None,
                    review: None,
                }),
            )
            .await;
        if let Some(e) = r.error {
            return Err(e);
        }
        let Some(CommandResult::TaskCreated { task_id, .. }) = r.result else {
            panic!("{r:?}")
        };
        Ok(self.f.hub.read(|r| r.require("task", task_id)).unwrap())
    }
}

fn claude() -> LaunchChoice {
    LaunchChoice {
        harness: HarnessKind::ClaudeCode,
        model: None,
        effort: None,
        launch_policy: None,
    }
}

/// A round charged with the check's findings on PR #7, as a restart
/// leaves it.
fn handover(task_id: TaskId, revision: u64, now: Timestamp) -> Handover {
    Handover {
        trusted: true,
        task_id,
        command_id: CommandId::new(),
        text: "Pull request #7 has news: the check before merging found a lost rename.".into(),
        revision,
        number: 7,
        review: Some("> - Rename isn't fsynced".into()),
        merge_check: None,
        before: Watch {
            head: String::new(),
            failed: Default::default(),
            threads: Default::default(),
            conflict: None,
            polled_at: Timestamp(0),
            progress_at: now,
            pending: false,
            updated: None,
        },
        stage: Some(PrStage::Babysitting),
    }
}
fn drafts() -> GithubSettings {
    GithubSettings {
        mode: GithubMode::Drafts,
        ..Default::default()
    }
}

#[tokio::test]
async fn a_project_reads_its_repository_and_says_what_through_merge_risks() {
    let r = repo(true).await;
    let QueryData::GithubProject { view } = (r.f.hub)
        .query(
            &Caller::local_owner(),
            Query::GithubProject {
                project_id: r.project_id,
            },
        )
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(view.login.as_deref(), Some("me"));
    assert_eq!(view.problem, None);
    let repo = view.repo.unwrap();
    assert_eq!((repo.owner.as_str(), repo.name.as_str()), ("octo", "app"));
    assert!(repo.admin && repo.push && !repo.owned);
    assert_eq!(repo.protected, Some(false));
    assert!(repo.runs_checks == Some(true) && !repo.deploys);
    assert_eq!(repo.private_reporting, Some(true));
    assert_eq!(
        view.warnings,
        ["`main` isn't protected, so nothing stops a push that skips the PR."]
    );

    // A ruleset that asks for a pull request protects it.
    (r.gh).answer(
        "GET /repos/octo/app/rules/branches/main",
        json!([{ "type": "pull_request" }]),
    );
    let repo = r.f.hub.github_repo(r.project_id).await.unwrap();
    assert_eq!(repo.protected, Some(true));

    // Signed out, the view says so rather than failing.
    r.gh.refuse("GET /user", 401);
    let view = r.f.hub.github_project(r.project_id).await.unwrap();
    assert_eq!((view.login, view.repo), (None, None));
    assert!(view.problem.unwrap().contains("Sign in to GitHub"));
    r.f.close().await;
}

/// Makes `origin/main` in the repository at `dir` a commit of `files`, as
/// a fetch would, leaving the branch checked out alone: the trusted base
/// review rules are read from.
pub(super) fn fetched_base(dir: &std::path::Path, files: &[(&str, &str)]) -> String {
    let git = |args: &[&str], input: Option<&str>| {
        use std::io::Write;
        let mut child = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
            ])
            .args(args)
            .current_dir(dir)
            .env("GIT_INDEX_FILE", dir.join(".git/fetched-base-index"))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.unwrap_or_default().as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "{args:?}");
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    };
    // A separate index, so the project's own is untouched.
    git(&["read-tree", "--empty"], None);
    for (path, text) in files {
        let blob = git(&["hash-object", "-w", "--stdin"], Some(text));
        let entry = format!("100644,{blob},{path}");
        git(&["update-index", "--add", "--cacheinfo", &entry], None);
    }
    let tree = git(&["write-tree"], None);
    let commit = git(&["commit-tree", &tree, "-m", "base"], None);
    git(&["update-ref", "refs/remotes/origin/main", &commit], None);
    commit
}

/// Runs `git` in `dir`, which must succeed.
fn git_in(dir: &std::path::Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(status.success());
}

#[tokio::test]
async fn a_folder_that_isnt_a_repository_names_the_ones_in_it() {
    let gh = Arc::new(FakeGithub::default());
    let port = gh.clone();
    let f = Fixture::with(move |b| b.github(port)).await;
    let project_id = f.project().await;
    // The fixture's folder is in this repository's checkout, so the
    // project's is moved out of any.
    let dir = std::env::temp_dir().join(uuid::Uuid::now_v7().to_string());
    std::fs::create_dir_all(&dir).unwrap();
    let mut project: Project = f.hub.read(|r| r.require("project", project_id)).unwrap();
    project.host_path = dir.to_string_lossy().into_owned();
    (f.hub.write(|tx| tx.put("project", project_id, &project))).unwrap();
    gh.answer("GET /user", json!({ "login": "me" }));
    let view = f.hub.github_project(project_id).await.unwrap();
    assert_eq!(view.problem.as_deref(), Some(crate::github::NOT_A_REPO));
    assert!(view.repo_folders.is_empty() && view.repo.is_none());

    // The site's repository, a worktree of it, one not on GitHub, a
    // hidden one and a plain folder: only the site is offered.
    let make = |name: &str, origin: Option<&str>| {
        let sub = dir.join(name);
        std::fs::create_dir_all(&sub).unwrap();
        git_in(&sub, &["init", "-q"]);
        if let Some(url) = origin {
            git_in(&sub, &["remote", "add", "origin", url]);
        }
        sub
    };
    make("site", Some("https://github.com/octo/site.git"));
    make("other", Some("https://gitlab.com/octo/other.git"));
    make(".hidden", Some("git@github.com:octo/hidden.git"));
    std::fs::create_dir_all(dir.join("planning")).unwrap();
    std::fs::create_dir_all(dir.join("site-worktree")).unwrap();
    std::fs::write(
        dir.join("site-worktree/.git"),
        "gitdir: ../site/.git/worktrees/w\n",
    )
    .unwrap();
    let view = f.hub.github_project(project_id).await.unwrap();
    let names: Vec<_> = view.repo_folders.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["site"]);
    let site = &view.repo_folders[0];
    assert_eq!(site.path, dir.join("site").to_string_lossy());
    assert_eq!(site.project_id, None);
    let problem = view.problem.unwrap();
    assert!(
        problem.contains("isn't a Git repository")
            && problem.contains("The folder site inside it is one"),
        "{problem}"
    );

    // Signed out, what's wrong with the folder is still what's said.
    gh.refuse("GET /user", 401);
    let view = f.hub.github_project(project_id).await.unwrap();
    assert_eq!(view.login, None);
    assert!(
        view.problem
            .unwrap()
            .contains("The folder site inside it is one")
    );

    // Once the site is a project, it's named as that one.
    project.project_id = ProjectId::new();
    project.host_path = site.path.clone();
    let added = project.project_id;
    (f.hub.write(|tx| tx.put("project", added, &project))).unwrap();
    let view = f.hub.github_project(project_id).await.unwrap();
    assert_eq!(view.repo_folders[0].project_id, Some(added));
    std::fs::remove_dir_all(&dir).unwrap();
    f.close().await;
}

#[tokio::test]
async fn through_merge_starts_unanswered_and_its_answer_keeps_it_or_falls_back() {
    let r = repo(true).await;
    // Refused at the receipt, before GitHub is asked anything.
    let bad = GithubSettings {
        branch_prefix: "bad prefix/".into(),
        ..drafts()
    };
    assert_eq!(r.set(bad).await, Err(ErrorCode::InvalidRequest));

    let through = GithubSettings {
        mode: GithubMode::ThroughMerge,
        // A client can't answer the warning by saying so.
        warning_answered: true,
        ..Default::default()
    };
    r.set(through.clone()).await.unwrap();
    assert!(!r.settings().warning_answered);
    let answer_for = |repo: &str, keep| GithubAction::AnswerWarning {
        project_id: r.project_id,
        repo: repo.into(),
        keep_through_merge: keep,
    };
    let answer = |keep| answer_for("Octo/App", keep);
    // A warning read for another repository answers nothing here.
    assert_eq!(
        r.act(&Caller::local_owner(), answer_for("octo/other", true))
            .await,
        Err(ErrorCode::Conflict)
    );
    assert!(!r.settings().warning_answered);
    r.act(&Caller::local_owner(), answer(true)).await.unwrap();
    assert!(r.settings().warning_answered);
    // It answers for this repository, whatever the case of its name.
    let mut read: GithubRepo = (r.f.hub)
        .read(|x| x.require(crate::github::REPO, r.project_id))
        .unwrap();
    assert_eq!(r.settings().warned_repo.as_deref(), Some("octo/app"));
    read.owner = "Octo".into();
    assert!(crate::github::warning_answered(&r.settings(), &read));
    read.name = "other".into();
    assert!(!crate::github::warning_answered(&r.settings(), &read));
    // Changing another setting keeps the answer; leaving and coming back
    // asks again.
    r.set(GithubSettings {
        branch_prefix: "bot/".into(),
        ..through.clone()
    })
    .await
    .unwrap();
    assert!(r.settings().warning_answered);
    assert_eq!(r.settings().warned_repo.as_deref(), Some("octo/app"));
    r.set(drafts()).await.unwrap();
    r.set(through).await.unwrap();
    assert!(!r.settings().warning_answered);
    r.act(&Caller::local_owner(), answer(false)).await.unwrap();
    assert_eq!(r.settings().mode, GithubMode::Drafts);
    assert!(r.settings().warning_answered);
    // There's nothing left to answer.
    assert_eq!(
        r.act(&Caller::local_owner(), answer(true)).await,
        Err(ErrorCode::Conflict)
    );
    r.f.close().await;

    let r = repo(false).await;
    let through = GithubSettings {
        mode: GithubMode::ThroughMerge,
        ..Default::default()
    };
    assert_eq!(r.set(through).await, Err(ErrorCode::PermissionDenied));
    r.set(drafts()).await.unwrap();
    r.f.close().await;
}

#[tokio::test]
async fn protecting_the_branch_requires_the_checks_every_recent_pr_ran() {
    let r = repo(true).await;
    let rollup = |names: &[&str]| {
        json!({ "commits": { "nodes": [{ "commit": { "statusCheckRollup": { "contexts": { "nodes":
            names.iter().map(|n| json!({ "__typename": "CheckRun", "name": n })).collect::<Vec<_>>()
        } } } }] } })
    };
    r.gh.query(
        "pullRequests(last:10",
        json!({ "repository": { "pullRequests": { "nodes": [
            rollup(&["test", "lint", "docs"]),
            rollup(&["test", "lint"]),
            rollup(&[]),
        ] } } }),
    );
    r.gh.answer("PUT /repos/octo/app/branches/main/protection", json!({}));
    let protect = GithubAction::ProtectBranch {
        project_id: r.project_id,
    };
    r.act(&Caller::local_owner(), protect).await.unwrap();
    let body =
        r.gh.called("PUT /repos/octo/app/branches/main/protection")
            .unwrap();
    assert_eq!(
        body["required_status_checks"],
        json!({ "strict": false, "checks": [{ "context": "lint" }, { "context": "test" }] })
    );
    assert_eq!(
        body["required_pull_request_reviews"]["required_approving_review_count"],
        0
    );
    r.f.close().await;

    let r = repo(false).await;
    let protect = GithubAction::ProtectBranch {
        project_id: r.project_id,
    };
    assert_eq!(
        r.act(&Caller::local_owner(), protect).await,
        Err(ErrorCode::PermissionDenied)
    );
    r.f.close().await;
}

#[tokio::test]
async fn protecting_the_branch_keeps_what_it_required_and_reads_it_first() {
    let r = repo(true).await;
    let protect = || GithubAction::ProtectBranch {
        project_id: r.project_id,
    };
    let put = "PUT /repos/octo/app/branches/main/protection";
    r.gh.answer(put, json!({}));
    let ran = json!({ "repository": { "pullRequests": { "nodes": [
        { "commits": { "nodes": [{ "commit": { "statusCheckRollup": { "contexts": { "nodes": [
            { "__typename": "CheckRun", "name": "test" },
            { "__typename": "CheckRun", "name": "ci" },
        ] } } } }] } },
    ] } } });
    // Checks that couldn't be read leave the branch as it was.
    r.gh.fail_query("pullRequests(last:10");
    assert!(r.act(&Caller::local_owner(), protect()).await.is_err());
    assert!(r.gh.called(put).is_none());
    // So does protection that couldn't be read.
    r.gh.query("pullRequests(last:10", ran);
    r.gh.refuse("GET /repos/octo/app/branches/main/protection", 502);
    assert!(r.act(&Caller::local_owner(), protect()).await.is_err());
    assert!(r.gh.called(put).is_none());
    // A pull request is required on top of what the branch had.
    r.gh.answer(
        "GET /repos/octo/app/branches/main/protection",
        json!({
            "required_status_checks": {
                "strict": true,
                "contexts": ["ci"],
                "checks": [{ "context": "ci", "app_id": 15368 }],
            },
            "restrictions": { "users": [{ "login": "octo" }], "teams": [{ "slug": "core" }], "apps": [] },
            "required_linear_history": { "enabled": true },
            "allow_force_pushes": { "enabled": false },
        }),
    );
    r.act(&Caller::local_owner(), protect()).await.unwrap();
    let body = r.gh.called(put).unwrap();
    assert_eq!(
        body["required_status_checks"],
        json!({ "strict": true, "checks": [{ "context": "ci", "app_id": 15368 }, { "context": "test" }] })
    );
    assert_eq!(
        body["restrictions"],
        json!({ "users": ["octo"], "teams": ["core"], "apps": [] })
    );
    assert_eq!(body["required_linear_history"], true);
    assert_eq!(body["allow_force_pushes"], false);
    assert_eq!(
        body["required_pull_request_reviews"]["required_approving_review_count"],
        0
    );
    r.f.close().await;
}

fn pull(number: u64, head_repo: &str, author: &str) -> Value {
    json!({
        "number": number,
        "state": "open",
        "title": "Make saving atomic",
        "html_url": format!("https://github.com/octo/app/pull/{number}"),
        "draft": false,
        "user": { "login": author },
        "author_association": if matches!(author, "me" | "teammate") { "COLLABORATOR" } else { "CONTRIBUTOR" },
        "head": { "ref": "save-fix", "sha": "bbb", "repo": { "full_name": head_repo } },
    })
}

#[tokio::test]
async fn a_linked_task_starts_from_its_issue_or_pull_request() {
    let r = repo(true).await;
    // Off, nothing links.
    let issue = GithubLink::Issue {
        number: 12,
        review_with: None,
    };
    let e = r.link(issue.clone(), "Fix it").await.unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidRequest);
    r.set(drafts()).await.unwrap();

    // A stranger's issue: its own title and branch, and Plan first.
    r.gh.answer(
        "GET /repos/octo/app/issues/12",
        json!({
            "number": 12, "state": "open", "title": "Crash on save: ÜTF names",
            "user": { "login": "visitor" }, "author_association": "NONE",
        }),
    );
    r.gh.answer(
        "GET /repos/octo/app/collaborators/visitor/permission",
        json!({ "permission": "read" }),
    );
    let task = r.link(issue, "Fix it").await.unwrap();
    assert_eq!(task.title, "Crash on save: ÜTF names");
    assert_eq!(task.mode, TaskMode::Plan);
    let branch = task.branch.unwrap();
    assert_eq!(
        (branch.name.as_str(), branch.new),
        ("ophio/12-crash-on-save-tf-names", true)
    );
    let github = task.github.unwrap();
    assert_eq!(github.issue, Some(12));
    assert!(github.from_stranger);

    // A collaborator's pull request is worked on where it is, in Work.
    r.gh.answer(
        "GET /repos/octo/app/pulls/7",
        pull(7, "octo/app", "teammate"),
    );
    r.gh.answer(
        "GET /repos/octo/app/collaborators/teammate/permission",
        json!({ "permission": "write" }),
    );
    let task = r
        .link(GithubLink::Pr { number: 7 }, "Finish it")
        .await
        .unwrap();
    assert_eq!(task.mode, TaskMode::Work);
    let branch = task.branch.unwrap();
    assert_eq!((branch.name.as_str(), branch.new), ("save-fix", false));
    let github = task.github.unwrap();
    assert!(!github.from_stranger);
    assert_eq!(github.pr.unwrap().head, "bbb");
    assert_eq!(github.stage, Some(PrStage::Babysitting));

    // A fork's goes on a new branch.
    r.gh.answer(
        "GET /repos/octo/app/pulls/8",
        pull(8, "teammate/app", "teammate"),
    );
    let task = r
        .link(GithubLink::Pr { number: 8 }, "Finish it")
        .await
        .unwrap();
    assert_eq!(task.branch.unwrap().name, "ophio/pr-8-make-saving-atomic");

    // A pull request isn't an issue, and a closed issue has nothing to fix.
    r.gh.answer(
        "GET /repos/octo/app/issues/9",
        json!({ "number": 9, "state": "open", "pull_request": {}, "user": { "login": "me" } }),
    );
    let e = (r
        .link(
            GithubLink::Issue {
                number: 9,
                review_with: None,
            },
            "x",
        )
        .await)
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidRequest);
    r.gh.answer(
        "GET /repos/octo/app/issues/10",
        json!({ "number": 10, "state": "closed", "user": { "login": "me" } }),
    );
    let e = (r
        .link(
            GithubLink::Issue {
                number: 10,
                review_with: None,
            },
            "x",
        )
        .await)
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Conflict);

    // Read-only work is Plan, whoever asked.
    let task = r.link(GithubLink::Triage, "Sort them").await.unwrap();
    assert_eq!(task.mode, TaskMode::Plan);
    assert_eq!(task.branch, None);
    r.f.close().await;
}

#[tokio::test]
async fn items_list_mine_first_and_show_what_the_agents_are_doing() {
    let r = repo(true).await;
    r.set(drafts()).await.unwrap();
    let node = |number: u64, author: &str, at: &str, association: &str| {
        json!({
            "number": number, "title": format!("Item {number}"),
            "url": format!("https://github.com/octo/app/issues/{number}"),
            "updatedAt": at, "authorAssociation": association,
            "author": { "login": author }, "labels": { "nodes": [{ "name": "bug" }] },
        })
    };
    r.gh.query(
        "issues(states:OPEN",
        json!({
            "viewer": { "login": "me" },
            "repository": { "issues": { "totalCount": 120, "nodes": [
                node(3, "visitor", "2026-10-02T10:00:00Z", "NONE"),
                node(2, "me", "2026-10-01T10:00:00Z", "OWNER"),
                node(12, "visitor", "2026-10-02T09:00:00Z", "NONE"),
            ] } },
        }),
    );
    r.gh.answer(
        "GET /repos/octo/app/issues/12",
        json!({
            "number": 12, "state": "open", "title": "Item 12", "html_url": "https://github.com/octo/app/issues/12",
            "updated_at": "2026-10-02T09:00:00Z", "author_association": "NONE",
            "user": { "login": "visitor" }, "labels": [],
        }),
    );
    let task = r
        .link(
            GithubLink::Issue {
                number: 12,
                review_with: None,
            },
            "Fix it",
        )
        .await
        .unwrap();
    let list = |filter: Option<&'static str>| {
        let hub = r.f.hub.clone();
        let project_id = r.project_id;
        async move {
            hub.github_items(project_id, GithubItemKind::Issues, filter)
                .await
        }
    };
    let items = list(None).await.unwrap();
    assert!(items.truncated);
    let numbers: Vec<u64> = items.items.iter().map(|i| i.number).collect();
    assert_eq!(numbers, [2, 3, 12]);
    assert!(items.items[0].mine && items.items[0].author_writes);
    assert!(!items.items[1].author_writes);
    assert_eq!(items.items[0].labels, ["bug"]);
    assert_eq!(items.items[2].task_id, Some(task.task_id));
    assert_eq!(items.items[1].task_id, None);

    let items = list(Some("  item 3 ")).await.unwrap();
    assert_eq!(items.items.len(), 1);
    // By number or link, whether or not it's in the first hundred.
    let items = list(Some("#12")).await.unwrap();
    assert_eq!(items.items[0].task_id, Some(task.task_id));
    assert!(!items.truncated);
    let items = list(Some("https://github.com/octo/app/issues/12")).await;
    assert_eq!(items.unwrap().items.len(), 1);
    assert!(list(Some("#404")).await.unwrap().items.is_empty());
    let e = list(Some("https://github.com/other/app/issues/1")).await;
    assert_eq!(e.unwrap_err().code, ErrorCode::InvalidRequest);
    // A pull request isn't on the issues list.
    r.gh.answer(
        "GET /repos/octo/app/issues/13",
        json!({ "number": 13, "state": "open", "pull_request": {}, "user": { "login": "me" } }),
    );
    assert!(list(Some("13")).await.unwrap().items.is_empty());
    r.f.close().await;
}

fn pr_answer(head: &str, draft: bool, merge_state: &str) -> Value {
    json!({ "repository": {
        "squashMergeAllowed": true, "mergeCommitAllowed": true, "rebaseMergeAllowed": false,
        "pullRequest": {
            "id": "PR_node", "number": 7, "title": "Make saving atomic",
            "url": "https://github.com/octo/app/pull/7", "body": "Fixes #12",
            "state": "OPEN", "isDraft": draft, "headRefName": "save-fix", "baseRefName": "main",
            "headRefOid": head, "author": { "login": "teammate" },
            "mergeable": "MERGEABLE", "mergeStateStatus": merge_state,
            "reviewThreads": { "nodes": [
                { "id": "T1", "isResolved": false, "path": "src/save.rs", "line": 4,
                  "comments": { "nodes": [{ "author": { "login": "me" }, "body": "Why?", "url": "u" }] } },
                { "id": "T2", "isResolved": true, "comments": { "nodes": [] } },
            ] },
            "commits": { "nodes": [{ "commit": { "statusCheckRollup": { "contexts": { "nodes": [
                { "__typename": "CheckRun", "name": "test", "status": "COMPLETED", "conclusion": "SUCCESS", "isRequired": true },
                { "__typename": "CheckRun", "name": "lint", "status": "IN_PROGRESS", "isRequired": false },
                { "__typename": "CheckRun", "name": "skip", "status": "COMPLETED", "conclusion": "SKIPPED" },
                { "__typename": "StatusContext", "context": "ci/legacy", "state": "ERROR", "targetUrl": "t" },
            ] } } } }] },
        },
    } })
}

#[test]
fn a_pull_request_reads_as_its_sheet_shows_it() {
    let answer = pr_answer("abc", false, "BLOCKED");
    let view = crate::github::pr_view(&answer["repository"]).unwrap();
    let s = &view.summary;
    assert_eq!((s.passed, s.failed, s.pending, s.unresolved), (1, 1, 1, 1));
    assert!(!s.conflict && !s.draft);
    assert_eq!(view.checks[2].state, CheckState::Other);
    assert!(view.checks[0].required);
    assert_eq!(view.threads[0].path.as_deref(), Some("src/save.rs"));
    assert!(!view.mergeable);
    assert!(view.blocked.unwrap().contains("required check or review"));
    assert_eq!(view.method, Some(MergeMethod::Squash));
    let clean = pr_answer("abc", false, "CLEAN");
    assert!(
        crate::github::pr_view(&clean["repository"])
            .unwrap()
            .mergeable
    );
    assert!(crate::github::pr_view(&json!({ "pullRequest": null })).is_none());
}

#[test]
fn a_check_that_ended_without_passing_failed_unless_it_chose_not_to_judge() {
    let mut answer = pr_answer("abc", false, "CLEAN");
    let ran = |conclusion: &str| json!({ "__typename": "CheckRun", "name": conclusion, "status": "COMPLETED", "conclusion": conclusion });
    answer["repository"]["pullRequest"]["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["contexts"]
        ["nodes"] = json!([
        ran("CANCELLED"),
        ran("STALE"),
        ran("NEUTRAL"),
        ran("SKIPPED"),
        ran("SOMETHING_NEW"),
    ]);
    let view = crate::github::pr_view(&answer["repository"]).unwrap();
    let states: Vec<CheckState> = view.checks.iter().map(|c| c.state).collect();
    use CheckState::{Failed, Other};
    assert_eq!(states, [Failed, Failed, Other, Other, Failed]);
    assert_eq!(view.summary.failed, 3);
    let held = crate::github_tools::not_green(&view, true).unwrap();
    assert!(
        held.contains("failing: `CANCELLED`, `STALE` and `SOMETHING_NEW`"),
        "{held}"
    );
}

#[test]
fn checks_past_the_first_hundred_hold_the_pull_request() {
    let mut answer = pr_answer("abc", false, "CLEAN");
    let contexts = &mut answer["repository"]["pullRequest"]["commits"]["nodes"][0]["commit"]["statusCheckRollup"]
        ["contexts"];
    contexts["nodes"] = json!([{ "__typename": "CheckRun", "name": "test", "status": "COMPLETED",
                                 "conclusion": "SUCCESS", "isRequired": false }]);
    contexts["totalCount"] = json!(101);
    let view = crate::github::pr_view(&answer["repository"]).unwrap();
    let unread = view.checks.last().unwrap();
    assert_eq!(unread.name, "100 more not read here");
    assert_eq!(unread.state, CheckState::Pending);
    assert!(unread.required);
    assert_eq!(
        unread.url.as_deref(),
        Some("https://github.com/octo/app/pull/7/checks")
    );
    assert_eq!((view.summary.passed, view.summary.pending), (1, 1));
    let held = crate::github_tools::not_green(&view, true).unwrap();
    assert!(
        held.contains("still running: `100 more not read here`"),
        "{held}"
    );
    // All of them read, nothing is added.
    answer["repository"]["pullRequest"]["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["contexts"]
        ["totalCount"] = json!(1);
    let view = crate::github::pr_view(&answer["repository"]).unwrap();
    assert_eq!(view.checks.len(), 1);
}

#[tokio::test]
async fn the_person_merging_a_pull_request_changes_only_the_tasks_on_it_now() {
    let r = repo(true).await;
    r.set(drafts()).await.unwrap();
    r.gh.answer("GET /repos/octo/app/pulls/7", pull(7, "octo/app", "me"));
    let moved = (r.link(GithubLink::Pr { number: 7 }, "Finish it").await).unwrap();
    let elsewhere = (r.link(GithubLink::Pr { number: 7 }, "Finish that").await).unwrap();
    let stays = (r.link(GithubLink::Pr { number: 7 }, "And this").await).unwrap();
    // One has opened #22 since; one was linked on another repository.
    r.alter(moved.task_id, |g| {
        let pr = g.pr.as_mut().unwrap();
        pr.number = 22;
        pr.url = "https://github.com/octo/app/pull/22".into();
    });
    r.alter(elsewhere.task_id, |g| g.repo = Some("octo/other".into()));
    r.gh.query("pullRequest(number", pr_answer("def", false, "CLEAN"));
    r.gh.answer(
        "PUT /repos/octo/app/pulls/7/merge",
        json!({ "merged": true }),
    );
    let merge = GithubAction::MergePr {
        project_id: r.project_id,
        repo: "octo/app".into(),
        number: 7,
        expected_head: "def".into(),
        method: MergeMethod::Squash,
        approval_shown: None,
    };
    r.act(&Caller::local_owner(), merge).await.unwrap();
    let github = |task: &Task| {
        let task: Task = r.f.hub.read(|x| x.require("task", task.task_id)).unwrap();
        let github = task.github.unwrap();
        (github.stage, github.pr.unwrap().state)
    };
    let untouched = (Some(PrStage::Babysitting), PrState::Open);
    assert_eq!(github(&moved), untouched);
    assert_eq!(github(&elsewhere), untouched);
    assert_eq!(github(&stays), (Some(PrStage::Merged), PrState::Merged));
    // The sheet of #7 is the task that's on it.
    let view = r.f.hub.github_pr(r.project_id, 7).await.unwrap();
    assert_eq!(view.task_id, Some(stays.task_id));
    r.f.close().await;
}

#[tokio::test]
async fn workflows_that_cant_be_read_leave_whether_checks_run_unknown() {
    let r = repo(true).await;
    let read = async || {
        let query = Query::GithubProject {
            project_id: r.project_id,
        };
        let QueryData::GithubProject { view } =
            (r.f.hub.query(&Caller::local_owner(), query).await).unwrap()
        else {
            panic!()
        };
        view
    };
    r.gh.fail_query("object(expression:$expression");
    let view = read().await;
    assert_eq!(view.repo.unwrap().runs_checks, None);
    assert!(
        (view.warnings.iter()).any(|w| w.contains("workflows couldn't be read")),
        "{:?}",
        view.warnings
    );
    // A repository without any runs none.
    r.gh.query(
        "object(expression:$expression",
        json!({ "repository": { "object": null } }),
    );
    let view = read().await;
    assert_eq!(view.repo.unwrap().runs_checks, Some(false));
    assert!(
        (view.warnings.iter()).any(|w| w.contains("runs no checks")),
        "{:?}",
        view.warnings
    );
    r.f.close().await;
}

#[tokio::test]
async fn a_links_repository_is_read_whatever_origin_names_by_then() {
    let r = repo(true).await;
    r.origin("git@github.com:octo/other.git");
    let read = (r.f.hub.github_repo_or_kept(r.project_id, "octo", "app"))
        .await
        .unwrap();
    assert_eq!((read.owner.as_str(), read.name.as_str()), ("octo", "app"));
    assert!(r.gh.called("GET /repos/octo/app").is_some());
    assert!(r.gh.called("GET /repos/octo/other").is_none());
    r.f.close().await;
}

#[tokio::test]
async fn while_github_cant_be_read_only_this_repositorys_reading_stands_in() {
    let r = repo(true).await;
    let read = r.f.hub.github_repo(r.project_id).await.unwrap();
    r.gh.refuse("GET /repos/octo/app", 502);
    let kept = |owner: &'static str, name: &'static str| {
        let hub = r.f.hub.clone();
        let project_id = r.project_id;
        async move { hub.github_repo_or_kept(project_id, owner, name).await }
    };
    assert_eq!(kept("Octo", "App").await.unwrap().name, "app");
    // What was kept of the repository `origin` named before isn't this
    // one's.
    let other = GithubRepo {
        name: "other".into(),
        ..read
    };
    (r.f.hub
        .write(|tx| tx.put(crate::github::REPO, r.project_id, &other)))
    .unwrap();
    assert!(kept("octo", "app").await.is_err());
    r.f.close().await;
}

#[tokio::test]
async fn merging_needs_the_head_the_person_saw_and_their_phone() {
    let r = repo(true).await;
    r.set(drafts()).await.unwrap();
    r.gh.answer("GET /repos/octo/app/pulls/7", pull(7, "octo/app", "me"));
    let task = r
        .link(GithubLink::Pr { number: 7 }, "Finish it")
        .await
        .unwrap();
    let merge = |head: &str, method| GithubAction::MergePr {
        project_id: r.project_id,
        repo: "Octo/App".into(),
        number: 7,
        expected_head: head.into(),
        method,
        approval_shown: None,
    };
    let owner = Caller::local_owner();

    r.gh.query("pullRequest(number", pr_answer("abc", true, "DRAFT"));
    assert_eq!(
        r.act(&owner, merge("abc", MergeMethod::Squash)).await,
        Err(ErrorCode::Conflict)
    );
    r.gh.query("markPullRequestReadyForReview", json!({}));
    let ready = GithubAction::MarkPrReady {
        project_id: r.project_id,
        repo: "octo/app".into(),
        number: 7,
    };
    r.act(&owner, ready).await.unwrap();
    let marked = r.gh.called("GRAPHQL mutation($id:ID!){markPullRequestReadyForReview(input:{pullRequestId:$id}){pullRequest{isDraft}}}");
    assert_eq!(marked.unwrap()["id"], "PR_node");

    r.gh.query("pullRequest(number", pr_answer("def", false, "CLEAN"));
    // A commit landed after the person looked.
    assert_eq!(
        r.act(&owner, merge("abc", MergeMethod::Squash)).await,
        Err(ErrorCode::Conflict)
    );
    assert_eq!(
        r.act(&owner, merge("def", MergeMethod::Rebase)).await,
        Err(ErrorCode::InvalidRequest)
    );
    assert!(r.gh.called("PUT /repos/octo/app/pulls/7/merge").is_none());
    r.gh.answer(
        "PUT /repos/octo/app/pulls/7/merge",
        json!({ "merged": true }),
    );
    r.act(&owner, merge("def", MergeMethod::Squash))
        .await
        .unwrap();
    assert_eq!(
        r.gh.called("PUT /repos/octo/app/pulls/7/merge").unwrap(),
        json!({ "sha": "def", "merge_method": "squash" })
    );
    let task: Task = r.f.hub.read(|x| x.require("task", task.task_id)).unwrap();
    let github = task.github.unwrap();
    assert_eq!(github.stage, Some(PrStage::Merged));
    assert_eq!(github.pr.unwrap().state, PrState::Merged);

    // Not from a chat, even one allowed everything.
    let chat = Caller {
        principal: Principal::Channel {
            identity_id: ChannelIdentityId::new(),
        },
        grants: Grants::owner(),
    };
    assert_eq!(
        r.f.hub
            .github_action(&chat, &merge("def", MergeMethod::Squash))
            .await
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    r.f.close().await;
}

#[tokio::test]
async fn the_persons_ready_and_merge_act_only_on_the_repository_they_read() {
    let r = repo(true).await;
    r.set(drafts()).await.unwrap();
    r.gh.answer("GET /repos/octo/app/pulls/7", pull(7, "octo/app", "me"));
    (r.link(GithubLink::Pr { number: 7 }, "Finish it").await).unwrap();
    r.gh.query("pullRequest(number", pr_answer("def", true, "DRAFT"));
    let view = r.f.hub.github_pr(r.project_id, 7).await.unwrap();
    assert_eq!(view.repo, "octo/app");
    // `origin` names another repository by the time they confirm.
    r.origin("git@github.com:octo/other.git");
    let owner = Caller::local_owner();
    let ready = GithubAction::MarkPrReady {
        project_id: r.project_id,
        repo: view.repo.clone(),
        number: 7,
    };
    assert_eq!(r.act(&owner, ready).await, Err(ErrorCode::Conflict));
    let merge = GithubAction::MergePr {
        project_id: r.project_id,
        repo: view.repo,
        number: 7,
        expected_head: "def".into(),
        method: MergeMethod::Squash,
        approval_shown: None,
    };
    assert_eq!(r.act(&owner, merge).await, Err(ErrorCode::Conflict));
    assert_eq!(r.queried("markPullRequestReadyForReview"), 0);
    assert!(r.gh.called("PUT /repos/octo/other/pulls/7/merge").is_none());
    assert!(r.gh.called("PUT /repos/octo/app/pulls/7/merge").is_none());
    r.f.close().await;
}

#[tokio::test]
async fn the_person_files_an_issue_in_the_repository_they_were_shown() {
    let r = repo(true).await;
    let owner = Caller::local_owner();
    let file = |repo: &str, title: &str| GithubAction::FileIssue {
        project_id: r.project_id,
        repo: repo.into(),
        title: title.into(),
        body: "Steps to see it\r\n\n".into(),
    };
    let filed = json!({ "number": 12, "html_url": "https://github.com/octo/app/issues/12" });
    assert_eq!(
        r.act(&owner, file("octo/app", "  ")).await,
        Err(ErrorCode::InvalidRequest)
    );
    assert_eq!(
        r.act(&owner, file("octo/other", "Crash on open")).await,
        Err(ErrorCode::Conflict)
    );
    assert!(r.gh.called("POST /repos/octo/app/issues").is_none());
    r.gh.answer("POST /repos/octo/app/issues", filed.clone());
    let done =
        r.f.hub
            .github_action(&owner, &file("octo/app", " Crash on open "))
            .await;
    assert_eq!(
        done.unwrap(),
        Some(CommandResult::IssueFiled {
            number: 12,
            url: "https://github.com/octo/app/issues/12".into()
        })
    );
    assert_eq!(
        r.gh.called("POST /repos/octo/app/issues").unwrap(),
        json!({ "title": "Crash on open", "body": "Steps to see it" })
    );

    // GitHub filed it but the answer was lost: it's found, not filed twice.
    r.gh.refuse("POST /repos/octo/app/issues", 502);
    r.gh.answer(
        "GET /repos/octo/app/issues?creator=me&state=all&sort=created&direction=desc&per_page=20",
        json!([{ "number": 12, "title": "Crash on open", "body": "Steps to see it",
                 "html_url": "https://github.com/octo/app/issues/12" }]),
    );
    let again =
        r.f.hub
            .github_action(&owner, &file("octo/app", "Crash on open"))
            .await;
    assert!(matches!(
        again,
        Ok(Some(CommandResult::IssueFiled { number: 12, .. }))
    ));
    let lost =
        r.f.hub
            .github_action(&owner, &file("octo/app", "Another"))
            .await;
    assert_eq!(lost.unwrap_err().code, ErrorCode::NativeFailure);

    // Not from a chat: the issue would be in the person's name.
    let chat = Caller {
        principal: Principal::Channel {
            identity_id: ChannelIdentityId::new(),
        },
        grants: Grants::owner(),
    };
    let refused =
        r.f.hub
            .github_action(&chat, &file("octo/app", "Crash on open"))
            .await;
    assert_eq!(refused.unwrap_err().code, ErrorCode::PermissionDenied);
    r.f.close().await;
}

#[tokio::test]
async fn without_the_github_port_the_workflow_is_unsupported() {
    let f = Fixture::new().await;
    let project_id = f.project().await;
    let r = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::Github {
                action: GithubAction::ProtectBranch { project_id },
            }),
        )
        .await;
    assert_eq!(r.error.unwrap().code, ErrorCode::Unsupported);
    f.close().await;
}

#[path = "github_tools_tests.rs"]
mod github_tools_tests;

#[path = "report_fixes_tests.rs"]
mod report_fixes_tests;

#[tokio::test]
async fn report_fixes_refuse_automation_merge_together_and_system_callers() {
    let r = repo(true).await;
    r.set(drafts()).await.unwrap();
    let mut read = pr_answer("def", false, "CLEAN");
    read["repository"]["pullRequest"]["headRefName"] = json!("report/a7k3qx");
    r.gh.query("pullRequest(number", read.clone());
    let automation =
        r.f.hub
            .refuse_report_automation("octo", "app", &read["repository"])
            .await;
    assert_eq!(automation.unwrap_err().code, ErrorCode::PermissionDenied);
    let merge = GithubAction::MergePr {
        project_id: r.project_id,
        repo: "octo/app".into(),
        number: 7,
        expected_head: "def".into(),
        method: MergeMethod::Squash,
        approval_shown: None,
    };
    let system = Caller {
        principal: Principal::System,
        grants: Grants::owner(),
    };
    assert_eq!(
        r.act(&system, merge.clone()).await.unwrap_err(),
        ErrorCode::PermissionDenied
    );
    // A report-shaped branch without its gated owner review is refused too.
    assert_eq!(
        r.act(&Caller::local_owner(), merge).await.unwrap_err(),
        ErrorCode::PermissionDenied
    );
    let together = GithubAction::MergeTogether {
        project_id: r.project_id,
        repo: "octo/app".into(),
        prs: vec![ChosenPr {
            number: 7,
            head: "def".into(),
            approval_shown: None,
        }],
    };
    assert_eq!(
        r.act(&Caller::local_owner(), together).await.unwrap_err(),
        ErrorCode::PermissionDenied
    );
    assert!(r.gh.called("PUT /repos/octo/app/pulls/7/merge").is_none());
    r.f.close().await;
}

#[path = "github_after_tests.rs"]
mod github_after_tests;

#[path = "github_busy_wake_tests.rs"]
mod github_busy_wake_tests;

#[path = "app_start_tests.rs"]
mod app_start_tests;

#[path = "work_tools_tests.rs"]
mod work_tools_tests;

#[path = "github_run_tests.rs"]
mod github_run_tests;

#[path = "github_together_tests.rs"]
mod github_together_tests;
#[path = "github_train_tests.rs"]
mod github_train_tests;

#[path = "review_gate_hub_tests.rs"]
mod review_gate_hub_tests;

#[path = "review_loop_tests.rs"]
mod review_loop_tests;

#[path = "github_access_tests.rs"]
mod access;
