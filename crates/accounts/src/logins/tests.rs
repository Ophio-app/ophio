// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::vault::{MemoryVault, VaultError};
use protocol::ProjectId;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Notify;

const PASSWORD_TEXT: &str = "correct horse \"battery\" ü";
const KEY: &str = "JBSWY3DPEHPK3PXPJBSWY3DP";
/// 20 seconds into a 30-second window.
const NOW: u64 = 1_700_000_000 - 1_700_000_000 % 30 + 20;
/// With 2 seconds of its window left.
const LATE: u64 = NOW + 8;

fn now() -> u64 {
    NOW
}
fn late() -> u64 {
    LATE
}

/// A vault that can be made to fail, or to wait in a read until let go.
#[derive(Debug, Default)]
struct Trying {
    inner: MemoryVault,
    fail_set: AtomicBool,
    fail_remove: AtomicBool,
    hold_get: AtomicBool,
    reading: Notify,
    go: Notify,
}
#[async_trait]
impl Vault for Trying {
    fn name(&self) -> String {
        "Test vault".into()
    }
    async fn get(
        &self,
        account: &str,
        item: Item,
    ) -> Result<Option<Zeroizing<String>>, VaultError> {
        if self.hold_get.load(Ordering::SeqCst) {
            self.reading.notify_one();
            self.go.notified().await;
        }
        self.inner.get(account, item).await
    }
    async fn set(&self, account: &str, item: Item, value: &str) -> Result<(), VaultError> {
        if self.fail_set.load(Ordering::SeqCst) {
            return Err(VaultError("the keyring is locked".into()));
        }
        self.inner.set(account, item, value).await
    }
    async fn remove(&self, account: &str, item: Item) -> Result<(), VaultError> {
        if self.fail_remove.load(Ordering::SeqCst) {
            return Err(VaultError("the keyring is locked".into()));
        }
        self.inner.remove(account, item).await
    }
    async fn clear(&self) -> Result<(), VaultError> {
        self.inner.clear().await
    }
}

struct Setup {
    _dir: tempfile::TempDir,
    dir: PathBuf,
    vault: Arc<Trying>,
    logins: ComputerAgentLogins,
}
impl Setup {
    fn new() -> Self {
        Self::with_clock(now)
    }
    fn with_clock(clock: fn() -> u64) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("accounts");
        let vault = Arc::new(Trying::default());
        let logins = open(&dir, vault.clone(), clock);
        Self {
            _dir: temp,
            dir,
            vault,
            logins,
        }
    }
    fn reopened(&self) -> ComputerAgentLogins {
        open(&self.dir, self.vault.clone(), now)
    }
    async fn on(self) -> Self {
        act(
            &self.logins,
            AgentLoginAction::Accept {
                version: DISCLAIMER_VERSION,
            },
        )
        .await
        .unwrap();
        self
    }
    async fn add(&self, details: LoginDetails) -> String {
        self.add_with(details, Some(KEY)).await
    }
    async fn add_with(&self, details: LoginDetails, key: Option<&str>) -> String {
        let name = details.name.clone();
        act(
            &self.logins,
            AgentLoginAction::Add {
                details,
                password: Some(Secret::new(PASSWORD_TEXT)),
                code_key: key.map(Secret::new),
            },
        )
        .await
        .unwrap();
        self.id(&name).await
    }
    async fn id(&self, name: &str) -> String {
        self.login(name).await.login_id
    }
    async fn login(&self, name: &str) -> AgentLogin {
        self.logins
            .view()
            .await
            .logins
            .into_iter()
            .find(|l| l.name == name)
            .unwrap()
    }
    fn saved(&self) -> String {
        std::fs::read_to_string(self.dir.join(FILE)).unwrap()
    }
}

fn open(dir: &std::path::Path, vault: Arc<Trying>, clock: fn() -> u64) -> ComputerAgentLogins {
    ComputerAgentLogins::new(LoginsConfig {
        dir: dir.to_owned(),
        vault,
        browser: Some("Chromium".into()),
        clock,
    })
}

async fn act(
    logins: &ComputerAgentLogins,
    action: AgentLoginAction,
) -> Result<(), AgentLoginsError> {
    logins.act(action, "Pixel").await
}

fn details(name: &str, site: &str) -> LoginDetails {
    LoginDetails {
        name: name.into(),
        sites: vec![site.into()],
        username: "agent@example.com".into(),
        notes: "Use the work profile.".into(),
        who: LoginWho::default(),
        how: LoginHow::Whenever,
        notify: false,
    }
}

fn user(project: ProjectId) -> LoginUser {
    LoginUser {
        task_id: TaskId::new(),
        run_id: RunId::new(),
        project_id: project,
        agent: Some("researcher".into()),
    }
}

fn masked(logins: &ComputerAgentLogins, text: &str) -> String {
    let mut text = text.to_owned();
    ports::accounts::mask(&mut text, &logins.secrets());
    text
}

#[tokio::test]
async fn nothing_but_stopping_works_until_the_notice_is_accepted() {
    let setup = Setup::new();
    let view = setup.logins.view().await;
    assert!(!view.enabled && !setup.logins.enabled());
    assert_eq!(view.accepted_version, None);
    assert_eq!(view.disclaimer_version, DISCLAIMER_VERSION);
    assert_eq!(view.store, "Test vault");
    assert_eq!(view.browser.as_deref(), Some("Chromium"));
    let added = act(
        &setup.logins,
        AgentLoginAction::Add {
            details: details("Forum", "forum.example"),
            password: Some(Secret::new(PASSWORD_TEXT)),
            code_key: None,
        },
    )
    .await;
    assert!(matches!(added, Err(AgentLoginsError::Off(_))), "{added:?}");
    assert!(setup.vault.inner.is_empty());
    assert!(matches!(
        act(&setup.logins, AgentLoginAction::ResumeAll).await,
        Err(AgentLoginsError::Off(_))
    ));
    // Stopping is never refused.
    act(&setup.logins, AgentLoginAction::PauseAll)
        .await
        .unwrap();
    assert!(matches!(
        act(
            &setup.logins,
            AgentLoginAction::Accept {
                version: DISCLAIMER_VERSION + 1
            }
        )
        .await,
        Err(AgentLoginsError::Invalid(_))
    ));
    act(
        &setup.logins,
        AgentLoginAction::Accept {
            version: DISCLAIMER_VERSION,
        },
    )
    .await
    .unwrap();
    let view = setup.logins.view().await;
    assert!(view.enabled);
    assert_eq!(view.accepted_version, Some(DISCLAIMER_VERSION));
    assert_eq!(view.history[0].event, AgentLoginEvent::Accepted);
    assert_eq!(view.history[0].by, "Pixel");
    // Kept across a restart.
    assert!(setup.reopened().enabled());
}

#[tokio::test]
async fn a_password_is_kept_in_the_vault_and_masked_in_every_form() {
    let setup = Setup::new().on().await;
    let id = setup
        .add(details("Forum", "https://www.Forum.example/login"))
        .await;
    let login = setup.login("Forum").await;
    assert_eq!(login.sites, ["forum.example"]);
    assert!(login.has_password && login.has_code_key);
    assert_eq!(login.status, LoginStatus::Ready);
    let saved = setup.saved();
    assert!(
        !saved.contains("horse") && !saved.contains("JBSWY3"),
        "{saved}"
    );
    assert_eq!(
        setup
            .vault
            .get(&account(&id), Item::Password)
            .await
            .unwrap()
            .as_deref()
            .map(String::as_str),
        Some(PASSWORD_TEXT)
    );
    assert!(
        setup
            .vault
            .get(&account(&id), Item::CodeKey)
            .await
            .unwrap()
            .is_some()
    );
    let json = serde_json::to_string(PASSWORD_TEXT).unwrap();
    for shown in [
        PASSWORD_TEXT.to_owned(),
        format!("{{\"password\":{json}}}"),
        r#"{"password":"correct horse \"battery\" ü"}"#.to_owned(),
        r#"{"password":"correct horse \"battery\" ü"}"#.to_owned(),
        "<input value=\"correct horse &quot;battery&quot; ü\">".to_owned(),
        "password=correct+horse+%22battery%22+%C3%BC".to_owned(),
        "password=correct%20horse%20%22battery%22%20%C3%BC".to_owned(),
        KEY.to_lowercase(),
        "JBSW Y3DP EHPK 3PXP JBSW Y3DP".to_owned(),
    ] {
        let hidden = masked(&setup.logins, &shown);
        assert!(
            !hidden.contains("horse") && !hidden.contains("hpk3") && !hidden.contains("EHPK"),
            "{shown} → {hidden}"
        );
    }
    // A restart holds them again before any run.
    let again = setup.reopened();
    assert!(again.secrets().is_empty());
    again.hold_all().await;
    assert!(masked(&again, PASSWORD_TEXT).contains(ports::accounts::MASK));
    assert!(masked(&again, KEY).contains(ports::accounts::MASK));
}

#[tokio::test]
async fn what_cannot_be_kept_is_refused() {
    let setup = Setup::new().on().await;
    setup
        .add_with(details("Forum", "forum.example"), None)
        .await;
    let add = |details: LoginDetails, password: &str, key: Option<&str>| {
        act(
            &setup.logins,
            AgentLoginAction::Add {
                details,
                password: Some(Secret::new(password)),
                code_key: key.map(Secret::new),
            },
        )
    };
    let invalid = |r: Result<(), AgentLoginsError>| matches!(r, Err(AgentLoginsError::Invalid(_)));
    assert!(invalid(
        add(details("Shop", "shop.example"), "short", None).await
    ));
    assert!(invalid(
        add(details("Shop", "shop.example"), "two\nlines here", None).await
    ));
    assert!(invalid(
        add(details("Shop", "shop.example"), "         ", None).await
    ));
    assert!(invalid(
        add(details("FORUM", "other.example"), PASSWORD_TEXT, None).await
    ));
    assert!(invalid(
        add(details(" ", "shop.example"), PASSWORD_TEXT, None).await
    ));
    assert!(invalid(
        add(
            details("Shop", "shop.example"),
            PASSWORD_TEXT,
            Some("not a key!")
        )
        .await
    ));
    let mut notes = details("Shop", "shop.example");
    notes.notes = format!("The password is {PASSWORD_TEXT}");
    assert!(invalid(add(notes, PASSWORD_TEXT, None).await));
    let mut sites = details("Shop", "shop.example");
    sites.sites = (0..11).map(|n| format!("s{n}.example")).collect();
    assert!(invalid(add(sites, PASSWORD_TEXT, None).await));
    let mut nobody = details("Shop", "shop.example");
    nobody.who.projects = Some(Vec::new());
    assert!(invalid(add(nobody, PASSWORD_TEXT, None).await));
    let mut no_agent = details("Shop", "shop.example");
    no_agent.who.agents = Some(Vec::new());
    assert!(invalid(add(no_agent, PASSWORD_TEXT, None).await));
    assert_eq!(setup.logins.view().await.logins.len(), 1);
    // An edit can't put the password in the notes either.
    let id = setup.id("Forum").await;
    let mut edited = details("Forum", "forum.example");
    edited.notes = format!("pw: {PASSWORD_TEXT}");
    assert!(invalid(
        act(
            &setup.logins,
            AgentLoginAction::Edit {
                login_id: id,
                details: edited
            }
        )
        .await
    ));
}

#[test]
fn sites_are_whole_names_on_hosts_of_their_own() {
    assert_eq!(site("Example.com").unwrap(), "example.com");
    assert_eq!(
        site("https://www.example.com/login?next=/").unwrap(),
        "example.com"
    );
    assert_eq!(
        site("http://accounts.example.co.uk.").unwrap(),
        "accounts.example.co.uk"
    );
    assert_eq!(site("example.co.uk").unwrap(), "example.co.uk");
    for refused in [
        "",
        "localhost",
        "192.168.1.1",
        "https://[::1]/",
        "ftp://example.com",
        "https://me:pw@example.com",
        "someone.github.io",
        "github.io",
        "app.vercel.app",
        "bucket.s3.amazonaws.com",
        "co.uk",
        "com.au",
        "javascript:alert(1)",
        "exa mple.com",
    ] {
        assert!(site(refused).is_err(), "{refused} was let through");
    }
    // What was typed is never repeated: it may hold a password.
    for typed in [
        "https://alice:private-password@%zz/",
        "ftp://alice:private-password@example.com",
        "https://alice:private-password@[::1]/",
        "alice:private-password@",
    ] {
        let refused = site(typed).unwrap_err();
        assert!(!refused.contains("private-password"), "{refused}");
    }
}

#[tokio::test]
async fn who_may_use_a_login_is_checked_in_order() {
    let setup = Setup::new().on().await;
    let mine = ProjectId::new();
    let mut ask = details("Forum", "forum.example");
    ask.how = LoginHow::AskFirst;
    ask.who.projects = Some(vec![mine]);
    ask.who.agents = Some(vec!["researcher".into()]);
    let id = setup.add(ask).await;
    let task = user(mine);

    let other_project = user(ProjectId::new());
    assert!(matches!(
        setup.logins.access(&id, &other_project),
        Access::Refused(_)
    ));
    assert!(setup.logins.usable(&other_project).is_empty());
    let other_agent = LoginUser {
        agent: Some("writer".into()),
        ..task.clone()
    };
    assert!(matches!(
        setup.logins.access(&id, &other_agent),
        Access::Refused(_)
    ));
    let no_agent = LoginUser {
        agent: None,
        ..task.clone()
    };
    assert!(matches!(
        setup.logins.access(&id, &no_agent),
        Access::Refused(_)
    ));

    assert_eq!(setup.logins.access(&id, &task), Access::AskFirst);
    let usable = setup.logins.usable(&task);
    assert_eq!(usable.len(), 1);
    assert!(usable[0].ask_first && usable[0].has_code_key);
    assert!(matches!(
        setup.logins.credentials(&id, &task).await,
        Err(AgentLoginsError::Refused(_))
    ));
    assert!(setup.logins.approve(&id, task.run_id, usable[0].revision));
    assert_eq!(setup.logins.access(&id, &task), Access::Allowed);
    let credentials = setup.logins.credentials(&id, &task).await.unwrap();
    assert_eq!(credentials.username, "agent@example.com");
    assert_eq!(credentials.password.as_str(), PASSWORD_TEXT);
    assert!(!format!("{credentials:?}").contains("horse"));
    // Another task of the same project is asked about again, and so is a
    // later run of this task.
    assert_eq!(setup.logins.access(&id, &user(mine)), Access::AskFirst);
    let next_run = LoginUser {
        run_id: RunId::new(),
        ..task.clone()
    };
    assert_eq!(setup.logins.access(&id, &next_run), Access::AskFirst);

    act(
        &setup.logins,
        AgentLoginAction::Pause {
            login_id: id.clone(),
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        setup.logins.access(&id, &task),
        Access::Refused(_)
    ));
    assert_eq!(setup.login("Forum").await.status, LoginStatus::Paused);
    act(
        &setup.logins,
        AgentLoginAction::Resume {
            login_id: id.clone(),
        },
    )
    .await
    .unwrap();
    assert_eq!(setup.logins.access(&id, &task), Access::Allowed);
    let resumed = setup.logins.usable(&task)[0].revision;
    act(&setup.logins, AgentLoginAction::PauseAll)
        .await
        .unwrap();
    assert!(matches!(
        setup.logins.access(&id, &task),
        Access::Refused(_)
    ));
    assert!(setup.reopened().view().await.paused_all);
    act(&setup.logins, AgentLoginAction::ResumeAll)
        .await
        .unwrap();
    // A request made before Pause all and Resume all allows nothing either.
    assert_ne!(setup.logins.usable(&task)[0].revision, resumed);

    // An approval ends with its run, and with an edit of the rules.
    setup.logins.task_ended(task.task_id);
    assert_eq!(setup.logins.access(&id, &task), Access::Allowed);
    setup.logins.run_ended(task.run_id);
    assert_eq!(setup.logins.access(&id, &task), Access::AskFirst);
    let revision = |setup: &Setup| setup.logins.usable(&task)[0].revision;
    // One asked about before the login was paused and resumed allows nothing.
    assert!(!setup.logins.approve(&id, task.run_id, usable[0].revision));
    assert_eq!(setup.logins.access(&id, &task), Access::AskFirst);
    let before = revision(&setup);
    assert!(setup.logins.approve(&id, task.run_id, before));
    let mut edited = details("Forum", "forum.example");
    edited.how = LoginHow::AskFirst;
    act(
        &setup.logins,
        AgentLoginAction::Edit {
            login_id: id.clone(),
            details: edited,
        },
    )
    .await
    .unwrap();
    assert_eq!(setup.logins.access(&id, &task), Access::AskFirst);
    // The owner's answer to a request about the login before the edit
    // doesn't allow the edited one.
    assert!(!setup.logins.approve(&id, task.run_id, before));
    assert_eq!(setup.logins.access(&id, &task), Access::AskFirst);
    // A sign-out does the same.
    assert!(setup.logins.approve(&id, task.run_id, revision(&setup)));
    act(
        &setup.logins,
        AgentLoginAction::SignOut {
            login_id: id.clone(),
        },
    )
    .await
    .unwrap();
    assert_eq!(setup.logins.access(&id, &task), Access::AskFirst);
}

#[tokio::test]
async fn a_password_read_while_the_login_is_paused_is_not_handed_out() {
    let setup = Setup::new().on().await;
    let id = setup.add(details("Forum", "forum.example")).await;
    let task = user(ProjectId::new());
    setup.vault.hold_get.store(true, Ordering::SeqCst);
    let reading = {
        let logins = setup.logins.clone();
        let (id, task) = (id.clone(), task.clone());
        tokio::spawn(async move { logins.credentials(&id, &task).await })
    };
    setup.vault.reading.notified().await;
    act(
        &setup.logins,
        AgentLoginAction::Pause {
            login_id: id.clone(),
        },
    )
    .await
    .unwrap();
    setup.vault.go.notify_one();
    let read = reading.await.unwrap();
    assert!(
        matches!(read, Err(AgentLoginsError::Refused(_))),
        "{read:?}"
    );
    // Still masked: it was read.
    assert!(masked(&setup.logins, PASSWORD_TEXT).contains(ports::accounts::MASK));
}

#[tokio::test(start_paused = true)]
async fn a_code_about_to_run_out_is_not_typed_and_the_next_one_is() {
    let key = CodeKey::parse(KEY).unwrap();
    let setup = Setup::new().on().await;
    let id = setup.add(details("Forum", "forum.example")).await;
    let task = user(ProjectId::new());
    let code = setup.logins.code(&id, &task).await.unwrap();
    assert_eq!(code.as_str(), key.code_at(NOW).as_str());

    let late_setup = Setup::with_clock(late).on().await;
    let id = late_setup.add(details("Forum", "forum.example")).await;
    let started = tokio::time::Instant::now();
    let code = late_setup.logins.code(&id, &task).await.unwrap();
    assert_eq!(started.elapsed(), Duration::from_secs(2));
    assert_eq!(code.as_str(), key.code_at(LATE + 2).as_str());
    assert_ne!(code.as_str(), key.code_at(LATE).as_str());

    let plain = late_setup
        .add_with(details("Shop", "shop.example"), None)
        .await;
    assert!(matches!(
        late_setup.logins.code(&plain, &task).await,
        Err(AgentLoginsError::NotFound(_))
    ));
}

#[tokio::test]
async fn a_setup_key_can_be_added_replaced_and_removed() {
    let setup = Setup::new().on().await;
    let id = setup
        .add_with(details("Forum", "forum.example"), None)
        .await;
    let task = user(ProjectId::new());
    assert!(!setup.login("Forum").await.has_code_key);
    act(
        &setup.logins,
        AgentLoginAction::SetSecret {
            login_id: id.clone(),
            password: None,
            code_key: Some(Secret::new(format!(
                "otpauth://totp/Forum:me?secret={KEY}&issuer=Forum"
            ))),
        },
    )
    .await
    .unwrap();
    assert!(setup.login("Forum").await.has_code_key);
    assert!(setup.logins.code(&id, &task).await.is_ok());
    act(
        &setup.logins,
        AgentLoginAction::SetSecret {
            login_id: id.clone(),
            password: Some(Secret::new("a new long password")),
            code_key: Some(Secret::new("")),
        },
    )
    .await
    .unwrap();
    assert!(!setup.login("Forum").await.has_code_key);
    assert!(
        setup
            .vault
            .get(&account(&id), Item::CodeKey)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        setup
            .logins
            .credentials(&id, &task)
            .await
            .unwrap()
            .password
            .as_str(),
        "a new long password"
    );
    // The old values stay hidden.
    assert!(masked(&setup.logins, PASSWORD_TEXT).contains(ports::accounts::MASK));
    assert!(masked(&setup.logins, KEY).contains(ports::accounts::MASK));
    let events: Vec<_> = setup
        .logins
        .view()
        .await
        .history
        .iter()
        .map(|h| h.event)
        .collect();
    assert!(
        events.starts_with(&[
            AgentLoginEvent::CodeKeyRemoved,
            AgentLoginEvent::PasswordSet,
            AgentLoginEvent::CodeKeySet
        ]),
        "{events:?}"
    );
    assert!(matches!(
        act(
            &setup.logins,
            AgentLoginAction::SetSecret {
                login_id: id,
                password: None,
                code_key: None
            }
        )
        .await,
        Err(AgentLoginsError::Invalid(_))
    ));
}

#[tokio::test]
async fn a_login_that_cannot_be_kept_leaves_nothing_but_stays_hidden() {
    let setup = Setup::new().on().await;
    setup.vault.fail_set.store(true, Ordering::SeqCst);
    let added = act(
        &setup.logins,
        AgentLoginAction::Add {
            details: details("Forum", "forum.example"),
            password: Some(Secret::new(PASSWORD_TEXT)),
            code_key: Some(Secret::new(KEY)),
        },
    )
    .await;
    assert!(
        matches!(added, Err(AgentLoginsError::Failed(_))),
        "{added:?}"
    );
    assert!(setup.logins.view().await.logins.is_empty());
    assert!(setup.vault.inner.is_empty());
    assert!(masked(&setup.logins, PASSWORD_TEXT).contains(ports::accounts::MASK));
}

#[tokio::test]
async fn deleting_removes_the_secrets_or_leaves_the_login_paused() {
    let setup = Setup::new().on().await;
    let id = setup.add(details("Forum", "forum.example")).await;
    let task = user(ProjectId::new());

    setup.vault.fail_remove.store(true, Ordering::SeqCst);
    let deleted = act(
        &setup.logins,
        AgentLoginAction::Delete {
            login_id: id.clone(),
        },
    )
    .await;
    assert!(
        matches!(deleted, Err(AgentLoginsError::Failed(_))),
        "{deleted:?}"
    );
    let login = setup.login("Forum").await;
    assert!(login.paused);
    assert!(matches!(
        setup.logins.access(&id, &task),
        Access::Refused(_)
    ));

    setup.vault.fail_remove.store(false, Ordering::SeqCst);
    act(
        &setup.logins,
        AgentLoginAction::Delete {
            login_id: id.clone(),
        },
    )
    .await
    .unwrap();
    assert!(setup.logins.view().await.logins.is_empty());
    assert!(setup.vault.inner.is_empty());
    assert!(matches!(
        setup.logins.credentials(&id, &task).await,
        Err(AgentLoginsError::NotFound(_))
    ));
    assert!(masked(&setup.logins, PASSWORD_TEXT).contains(ports::accounts::MASK));
    assert!(
        !setup
            .reopened()
            .view()
            .await
            .logins
            .iter()
            .any(|l| l.login_id == id)
    );
}

#[tokio::test]
async fn a_damaged_file_turns_agent_logins_off() {
    let setup = Setup::new().on().await;
    setup.add(details("Forum", "forum.example")).await;
    std::fs::write(setup.dir.join(FILE), "{ damaged").unwrap();
    let again = setup.reopened();
    let view = again.view().await;
    assert!(!view.enabled && view.paused_all);
    assert!(view.logins.is_empty());
    assert_eq!(view.history[0].event, AgentLoginEvent::PausedAll);
    assert_eq!(view.history[0].by, "Ophio");
    assert!(setup.dir.join("agent-logins.json.damaged").exists());
    // What replaced it is read back as it was saved.
    assert!(!setup.reopened().view().await.enabled);
}

#[tokio::test]
async fn a_use_is_recorded_masked_and_marks_the_task() {
    let setup = Setup::new().on().await;
    let id = setup.add(details("Forum", "forum.example")).await;
    let task = user(ProjectId::new());
    setup.logins.credentials(&id, &task).await.unwrap();
    let changed = setup.logins.changed();
    setup.logins.record(AgentLoginHistoryEntry {
        at: Timestamp::now(),
        login_id: Some(id.clone()),
        name: Some("Forum".into()),
        event: AgentLoginEvent::Filled,
        by: "researcher".into(),
        task_id: Some(task.task_id),
        site: Some("forum.example".into()),
        detail: Some(format!("typed {PASSWORD_TEXT}")),
    });
    assert!(changed.has_changed().unwrap());
    let view = setup.logins.view().await;
    let entry = &view.history[0];
    assert_eq!(entry.event, AgentLoginEvent::Filled);
    assert!(!entry.detail.as_ref().unwrap().contains("horse"));
    let login = setup.login("Forum").await;
    assert_eq!(login.used_by, [task.task_id]);
    assert!(login.last_used.is_some());
    assert!(!setup.saved().contains("horse"));
    setup.logins.task_ended(task.task_id);
    assert!(setup.login("Forum").await.used_by.is_empty());
}

#[tokio::test]
async fn sites_of_logins_a_task_may_not_use_are_blocked_for_it() {
    let setup = Setup::new().on().await;
    let mine = ProjectId::new();
    let mut ours = details("Forum", "forum.example");
    ours.who.projects = Some(vec![mine]);
    setup.add(ours).await;
    let mut theirs = details("Shop", "shop.example");
    theirs.sites.push("forum.example".into());
    theirs.who.projects = Some(vec![ProjectId::new()]);
    setup.add(theirs).await;
    // The site of a login it may use stays open to it.
    assert_eq!(setup.logins.blocked_sites(&user(mine)), ["shop.example"]);
    assert_eq!(
        setup.logins.blocked_sites(&user(ProjectId::new())),
        ["forum.example", "shop.example"]
    );
    act(&setup.logins, AgentLoginAction::PauseAll)
        .await
        .unwrap();
    assert_eq!(setup.logins.blocked_sites(&user(mine)).len(), 2);
}

#[tokio::test]
async fn showing_a_password_is_recorded() {
    let setup = Setup::new().on().await;
    let id = setup.add(details("Forum", "forum.example")).await;
    let shown = setup.logins.reveal(&id, "Pixel").await.unwrap();
    assert_eq!(shown.expose(), PASSWORD_TEXT);
    let view = setup.logins.view().await;
    assert_eq!(view.history[0].event, AgentLoginEvent::Shown);
    assert_eq!(view.history[0].by, "Pixel");
    assert!(matches!(
        setup.logins.reveal("missing", "Pixel").await,
        Err(AgentLoginsError::NotFound(_))
    ));
}

#[tokio::test]
async fn a_password_missing_from_the_vault_needs_the_owner() {
    let setup = Setup::new().on().await;
    let id = setup.add(details("Forum", "forum.example")).await;
    setup.vault.inner.clear().await.unwrap();
    let read = setup.logins.credentials(&id, &user(ProjectId::new())).await;
    assert!(matches!(read, Err(AgentLoginsError::Failed(_))), "{read:?}");
    assert!(matches!(
        setup.login("Forum").await.status,
        LoginStatus::NeedsAttention { .. }
    ));
    act(
        &setup.logins,
        AgentLoginAction::SetSecret {
            login_id: id,
            password: Some(Secret::new(PASSWORD_TEXT)),
            code_key: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(setup.login("Forum").await.status, LoginStatus::Ready);
}
