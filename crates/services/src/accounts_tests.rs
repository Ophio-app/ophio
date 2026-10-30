// SPDX-License-Identifier: Apache-2.0
use super::*;
use ports::HarnessEvent;
use ports::accounts::{Accounts, AccountsError, CUT_SHOWN, MASK, RENEW_ENV, RunAccounts};
use ports::harness::RunEnv;
use protocol::accounts::{AccountAction, AccountsView, Secret};
use std::sync::Mutex;
use tokio::sync::watch;

const TOKEN: &str = "ghp_synthetic0123456789";
const KEY: &str = "sk-or-v1-synthetic0123456789";
const RENEWED: &str = "ghp_renewed0123456789";

#[derive(Debug)]
struct FakeAccounts {
    acted: Mutex<Vec<(AccountAction, String)>>,
    /// Why the next action fails, as a pause that can't be saved.
    fail: Mutex<Option<String>>,
    /// Why the next run may not start, as a paused agent account refuses it.
    refuse: Mutex<Option<String>>,
    ended: Mutex<Vec<TaskId>>,
    /// Tasks given the account, as `renew` checks.
    given: Mutex<Vec<TaskId>>,
    /// Every value held, as a key set while a run goes adds one.
    seen: Mutex<Arc<Vec<String>>>,
    /// When set, `renew` waits here, as a provider slow to answer.
    slow: Mutex<Option<Arc<tokio::sync::Semaphore>>>,
    renewing: tokio::sync::Notify,
    changed: watch::Sender<u64>,
}
impl Default for FakeAccounts {
    fn default() -> Self {
        Self {
            acted: Mutex::default(),
            fail: Mutex::default(),
            refuse: Mutex::default(),
            ended: Mutex::default(),
            given: Mutex::default(),
            seen: Mutex::new(Arc::new(vec![TOKEN.into()])),
            slow: Mutex::default(),
            renewing: tokio::sync::Notify::new(),
            changed: watch::channel(0).0,
        }
    }
}
#[async_trait::async_trait]
impl Accounts for FakeAccounts {
    async fn view(&self) -> AccountsView {
        AccountsView {
            accounts: Vec::new(),
            sign_in: None,
            history: Vec::new(),
            store: "Synthetic vault".into(),
            store_note: None,
            browser: None,
            password_manager_done: false,
        }
    }
    async fn act(&self, action: AccountAction, by: &str) -> Result<(), AccountsError> {
        self.acted.lock().unwrap().push((action, by.into()));
        self.changed.send_modify(|n| *n += 1);
        match self.fail.lock().unwrap().take() {
            Some(why) => Err(AccountsError::Failed(why)),
            None => Ok(()),
        }
    }
    async fn for_run(
        &self,
        _harness: HarnessKind,
        task: TaskId,
    ) -> Result<RunAccounts, AccountsError> {
        if let Some(why) = self.refuse.lock().unwrap().take() {
            return Err(AccountsError::NotReady(why));
        }
        self.given.lock().unwrap().push(task);
        Ok(RunAccounts {
            env: RunEnv(vec![("GH_TOKEN".into(), TOKEN.into())]),
            secrets: vec![TOKEN.into()],
            files: Vec::new(),
        })
    }
    async fn renew(&self, task: TaskId, account: &str) -> Result<String, AccountsError> {
        if account != "github" || !self.given.lock().unwrap().contains(&task) {
            return Err(AccountsError::NotFound("This task wasn't given it.".into()));
        }
        let slow = self.slow.lock().unwrap().clone();
        if let Some(slow) = slow {
            self.renewing.notify_one();
            drop(slow.acquire().await);
        }
        Ok(RENEWED.into())
    }
    fn secrets(&self) -> Arc<Vec<String>> {
        self.seen.lock().unwrap().clone()
    }
    fn run_ended(&self, task: TaskId) {
        self.ended.lock().unwrap().push(task);
    }
    fn changed(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }
}

async fn with_accounts() -> (Fixture, Arc<FakeAccounts>) {
    let accounts = Arc::new(FakeAccounts::default());
    let given = accounts.clone();
    (Fixture::with(move |b| b.accounts(given)).await, accounts)
}
fn account(action: AccountAction) -> CommandEnvelope {
    CommandEnvelope::new(Command::Account { action })
}

#[tokio::test]
async fn a_phone_reaches_accounts_only_once_the_computer_allows_it() {
    let (f, fake) = with_accounts().await;
    let mut phone = f.device().await;
    assert!(phone.grants.admin && !phone.grants.accounts);
    assert!(
        f.hub
            .host_capabilities()
            .await
            .features
            .contains(&"accounts_v1".to_string())
    );
    let refused = f.hub.query(&phone, Query::Accounts).await.unwrap_err();
    assert_eq!(refused.code, ErrorCode::PermissionDenied);
    let r = f.hub.submit(&phone, account(AccountAction::PauseAll)).await;
    assert_eq!(r.error.unwrap().code, ErrorCode::PermissionDenied);
    // A device that manages the others still cannot give itself Accounts.
    let Principal::Device { device_id } = phone.principal else {
        unreachable!()
    };
    let wider = Grants {
        accounts: true,
        ..phone.grants.clone()
    };
    let give = || {
        CommandEnvelope::new(Command::SetDeviceGrants {
            device_id,
            expected: Some(phone.grants.clone()),
            grants: wider.clone(),
        })
    };
    let r = f.hub.submit(&phone, give()).await;
    let refused = r.error.unwrap();
    assert_eq!(refused.code, ErrorCode::PermissionDenied);
    assert!(refused.message.contains("Only the computer"));
    let r = f.hub.submit(&Caller::local_owner(), give()).await;
    assert_eq!(r.status, ReceiptStatus::Accepted, "{r:?}");
    settle(&f.hub, r.command_id).await;
    phone.grants = wider;
    let QueryData::Accounts { view } = f.hub.query(&phone, Query::Accounts).await.unwrap() else {
        panic!("accounts")
    };
    assert_eq!(view.store, "Synthetic vault");
    let mut live = f.hub.subscribe(&phone, None).await.unwrap().live;
    let r = f.hub.submit(&phone, account(AccountAction::PauseAll)).await;
    assert_eq!(r.status, ReceiptStatus::Accepted, "{r:?}");
    settle(&f.hub, r.command_id).await;
    assert_eq!(
        *fake.acted.lock().unwrap(),
        vec![(AccountAction::PauseAll, "Phone".to_string())]
    );
    // The phone is told to read the view again, not sent it.
    let told = tokio::time::timeout(WAIT, async {
        loop {
            if let Ok(crate::api::Outbound::Event(e)) = live.recv().await
                && matches!(e.event, Event::AccountsChanged {})
            {
                return;
            }
        }
    })
    .await;
    assert!(told.is_ok(), "no AccountsChanged");
    f.close().await;
}

#[tokio::test]
async fn a_computer_without_accounts_says_so_and_does_not_advertise_them() {
    let f = Fixture::new().await;
    assert!(
        !f.hub
            .host_capabilities()
            .await
            .features
            .contains(&"accounts_v1".to_string())
    );
    let refused = f
        .hub
        .query(&Caller::local_owner(), Query::Accounts)
        .await
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::Unsupported);
    f.close().await;
}

#[tokio::test]
async fn a_key_from_the_phone_reaches_the_vault_and_nothing_else_keeps_it() {
    let (f, fake) = with_accounts().await;
    let owner = Caller::local_owner();
    let set = account(AccountAction::SetKey {
        account_id: "openrouter".into(),
        key: Secret::new(KEY),
    });
    let r = f.hub.submit(&owner, set.clone()).await;
    assert_eq!(r.status, ReceiptStatus::Accepted, "{r:?}");
    settle(&f.hub, r.command_id).await;
    let given = fake.acted.lock().unwrap().clone();
    assert!(matches!(&given[..], [(AccountAction::SetKey { key, .. }, _)] if key.expose() == KEY));
    // The kept command has the key withheld, and a retry still matches it.
    let kept = f.hub.read(|r| r.command(set.command_id)).unwrap().unwrap();
    assert!(!serde_json::to_string(&kept).unwrap().contains(KEY));
    let again = f.hub.submit(&owner, set).await;
    assert_eq!(again.command_id, r.command_id);
    assert!(again.error.is_none(), "{again:?}");
    assert_eq!(fake.acted.lock().unwrap().len(), 1);
    f.hub.shutdown().await.unwrap();
    assert!(!written_anywhere(&f.path, KEY));
    let path = f.path.clone();
    drop(f.hub);
    std::fs::remove_dir_all(path).unwrap();
}

#[tokio::test]
async fn a_run_is_given_its_accounts_and_never_shows_their_values() {
    let (f, fake) = with_accounts().await;
    let owner = Caller::local_owner();
    let project = f.project().await;
    let (task, _) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    let run = run_started(&f, task).await;
    let env = f.harness.specs.lock().unwrap()[0].env.0.clone();
    assert_eq!(env[0], ("GH_TOKEN".into(), TOKEN.into()));
    let [(name, credential), task_env, ..] = &env[1..] else {
        panic!("{env:?}")
    };
    assert_eq!(name, RENEW_ENV);
    assert_eq!(*task_env, (ports::TASK_ENV.into(), task.to_string()));
    for event in [
        HarnessEvent::AgentMessage {
            message_key: "m".into(),
            text: format!("The token is {TOKEN}, renewed with {credential}."),
        },
        HarnessEvent::Activity {
            key: "env".into(),
            tool: "Bash".into(),
            summary: "printenv GH_TOKEN".into(),
            status: ActivityStatus::Succeeded,
            detail: Some(format!("{TOKEN}\n")),
        },
        HarnessEvent::Exited {
            exit: ports::RunExit::Failed {
                reason: format!("exit with {TOKEN}"),
            },
            outcome: None,
        },
    ] {
        f.harness.emit(run, event).await;
    }
    let mut more = waiting();
    while more() && fake.ended.lock().unwrap().is_empty() {
        tokio::task::yield_now().await;
    }
    assert_eq!(*fake.ended.lock().unwrap(), vec![task]);
    let QueryData::TaskTimeline { items, .. } = f
        .hub
        .query(
            &owner,
            Query::TaskTimeline {
                task_id: task,
                before_seq: None,
                limit: 50,
            },
        )
        .await
        .unwrap()
    else {
        panic!("timeline")
    };
    let shown = serde_json::to_string(&items).unwrap();
    assert!(
        !shown.contains(TOKEN) && !shown.contains(credential.as_str()),
        "{shown}"
    );
    assert!(shown.contains(MASK));
    let kept: Run = f.hub.read(|r| r.require("run", run)).unwrap();
    assert!(!serde_json::to_string(&kept).unwrap().contains(TOKEN));
    // Once the run has ended its credential renews nothing.
    let refused = f.hub.renew_for_run(credential, "github").await.unwrap_err();
    assert_eq!(refused.code, ErrorCode::PermissionDenied);
    f.close().await;
}

/// The text a phone was sent live for `run`, piece by piece.
fn live_text(live: &mut tokio::sync::broadcast::Receiver<crate::api::Outbound>) -> String {
    let mut text = String::new();
    while let Ok(out) = live.try_recv() {
        if let crate::api::Outbound::Live(live) = out
            && let protocol::event::LiveEvent::AgentTextDelta { text: piece, .. } = &*live
        {
            text += piece;
        }
    }
    text
}

#[tokio::test]
async fn a_value_split_across_streamed_pieces_is_still_hidden() {
    let (f, fake) = with_accounts().await;
    let owner = Caller::local_owner();
    let project = f.project().await;
    let (task, _) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    let run = run_started(&f, task).await;
    let mut live = f.hub.subscribe(&owner, None).await.unwrap().live;
    // A key set while the run goes is hidden in it too.
    *fake.seen.lock().unwrap() = Arc::new(vec![TOKEN.into(), KEY.into()]);
    // It ends with what no key begins with, not even the one the run is
    // given at random, so none of its end is held back.
    let said = format!("first {TOKEN} then {KEY} end.");
    // Cut inside each value, and between them.
    let mut from = 0;
    for to in [9, 14, 30, 40, 50, said.len()] {
        f.harness
            .emit(
                run,
                HarnessEvent::AgentTextDelta {
                    message_key: "m".into(),
                    text: said[from..to].into(),
                },
            )
            .await;
        from = to;
    }
    let mut more = waiting();
    let mut shown = String::new();
    while more() && !shown.ends_with(" end.") {
        tokio::task::yield_now().await;
        shown += &live_text(&mut live);
    }
    assert_eq!(shown, format!("first {MASK} then {MASK} end."));
    f.close().await;
}

#[tokio::test]
async fn a_held_end_dropped_for_too_many_messages_lets_none_of_its_rest_out() {
    let (f, fake) = with_accounts().await;
    let owner = Caller::local_owner();
    let project = f.project().await;
    let (task, _) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    let run = run_started(&f, task).await;
    let mut live = f.hub.subscribe(&owner, None).await.unwrap().live;
    *fake.seen.lock().unwrap() = Arc::new(vec![TOKEN.into()]);
    let delta = |key: String, text: String| HarnessEvent::AgentTextDelta {
        message_key: key,
        text,
    };
    // `m` stops partway into the token, then enough other messages hold an
    // end back that `m`'s is dropped.
    f.harness
        .emit(run, delta("m".into(), format!("first {}", &TOKEN[..10])))
        .await;
    for i in 0..crate::accounts::HELD_MESSAGES {
        f.harness
            .emit(
                run,
                delta(format!("o{i}"), format!("other {}", &TOKEN[..5])),
            )
            .await;
    }
    f.harness
        .emit(run, delta("m".into(), format!("{} end", &TOKEN[10..])))
        .await;
    f.harness
        .emit(
            run,
            HarnessEvent::AgentMessage {
                message_key: "m".into(),
                text: format!("first {TOKEN} end"),
            },
        )
        .await;
    let mut more = waiting();
    loop {
        assert!(more(), "the whole message never arrived");
        let QueryData::TaskTimeline { items, .. } = f
            .hub
            .query(
                &owner,
                Query::TaskTimeline {
                    task_id: task,
                    before_seq: None,
                    limit: 200,
                },
            )
            .await
            .unwrap()
        else {
            panic!("timeline")
        };
        let kept = serde_json::to_string(&items).unwrap();
        assert!(!kept.contains(&TOKEN[..CUT_SHOWN]), "{kept}");
        if kept.contains(&format!("first {MASK} end")) {
            break;
        }
        tokio::task::yield_now().await;
    }
    let shown = live_text(&mut live);
    assert!(shown.starts_with("first other other"), "{shown}");
    assert!(!shown.contains(&TOKEN[..CUT_SHOWN]), "{shown}");
    assert!(!shown.contains(&TOKEN[10..]), "{shown}");
    assert!(!shown.contains(" end"), "{shown}");
    f.close().await;
}

#[tokio::test]
async fn a_message_holding_back_too_much_turns_live_text_off() {
    let (f, fake) = with_accounts().await;
    let owner = Caller::local_owner();
    let project = f.project().await;
    let (task, _) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    let run = run_started(&f, task).await;
    let mut live = f.hub.subscribe(&owner, None).await.unwrap().live;
    // Every copy overlaps the next, so none of a run of them can be let out.
    let key = "AAAAAAAA";
    *fake.seen.lock().unwrap() = Arc::new(vec![key.into()]);
    let delta = |text: String| HarnessEvent::AgentTextDelta {
        message_key: "m".into(),
        text,
    };
    f.harness.emit(run, delta("said ".into())).await;
    let piece = "A".repeat(1000);
    for _ in 0..crate::accounts::HELD_BYTES / 1000 + 4 {
        f.harness.emit(run, delta(piece.clone())).await;
    }
    f.harness.emit(run, delta(" end".into())).await;
    let said = format!(
        "said {} end",
        piece.repeat(crate::accounts::HELD_BYTES / 1000 + 4)
    );
    f.harness
        .emit(
            run,
            HarnessEvent::AgentMessage {
                message_key: "m".into(),
                text: said,
            },
        )
        .await;
    let mut more = waiting();
    loop {
        assert!(more(), "the whole message never arrived");
        let QueryData::TaskTimeline { items, .. } = f
            .hub
            .query(
                &owner,
                Query::TaskTimeline {
                    task_id: task,
                    before_seq: None,
                    limit: 200,
                },
            )
            .await
            .unwrap()
        else {
            panic!("timeline")
        };
        let kept = serde_json::to_string(&items).unwrap();
        assert!(!kept.contains("AAAA"), "{kept}");
        if kept.contains(&format!("said {MASK} end")) {
            break;
        }
        tokio::task::yield_now().await;
    }
    // What was held was let go, not sent; nothing after it is sent live.
    let shown = live_text(&mut live);
    assert_eq!(shown, "said ");
    f.close().await;
}

#[tokio::test]
async fn a_whole_message_with_many_secrets_is_masked_in_one_pass() {
    let (f, fake) = with_accounts().await;
    let owner = Caller::local_owner();
    let project = f.project().await;
    let (task, _) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    let run = run_started(&f, task).await;
    *fake.seen.lock().unwrap() = Arc::new(vec!["AAAAAAAA".into()]);
    // Nine megabytes, a frame Claude may send, and a million separate keys.
    let mut event = HarnessEvent::AgentMessage {
        message_key: "m".into(),
        text: "AAAAAAAA ".repeat(1_000_000),
    };
    let started = std::time::Instant::now();
    f.hub.mask_run_event(run, &mut event);
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    let HarnessEvent::AgentMessage { text, .. } = event else {
        panic!("message")
    };
    assert_eq!(text, format!("{MASK} ").repeat(1_000_000));
    f.close().await;
}

#[tokio::test]
async fn a_task_renews_only_what_it_was_given_and_the_value_stays_hidden() {
    let (f, _fake) = with_accounts().await;
    let owner = Caller::local_owner();
    let project = f.project().await;
    let (task, _) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    let run = run_started(&f, task).await;
    let credential = f.harness.specs.lock().unwrap()[0].env.0[1].1.clone();
    let wrong = f
        .hub
        .renew_for_run("not-the-credential", "github")
        .await
        .unwrap_err();
    assert_eq!(wrong.code, ErrorCode::PermissionDenied);
    let other = f
        .hub
        .renew_for_run(&credential, "openrouter")
        .await
        .unwrap_err();
    assert_eq!(other.code, ErrorCode::NotFound);
    assert_eq!(
        f.hub.renew_for_run(&credential, "github").await.unwrap(),
        RENEWED
    );
    f.harness
        .emit(
            run,
            HarnessEvent::AgentMessage {
                message_key: "m".into(),
                text: format!("renewed: {RENEWED}"),
            },
        )
        .await;
    let QueryData::TaskTimeline { items, .. } = f
        .hub
        .query(
            &owner,
            Query::TaskTimeline {
                task_id: task,
                before_seq: None,
                limit: 50,
            },
        )
        .await
        .unwrap()
    else {
        panic!("timeline")
    };
    let shown = serde_json::to_string(&items).unwrap();
    assert!(!shown.contains(RENEWED), "{shown}");
    f.close().await;
}

#[tokio::test]
async fn a_task_that_ends_while_its_token_is_fetched_gets_nothing() {
    let (f, fake) = with_accounts().await;
    let owner = Caller::local_owner();
    let project = f.project().await;
    let (task, _) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    let run = run_started(&f, task).await;
    let credential = f.harness.specs.lock().unwrap()[0].env.0[1].1.clone();
    let slow = Arc::new(tokio::sync::Semaphore::new(0));
    *fake.slow.lock().unwrap() = Some(slow.clone());
    let renewing = {
        let hub = f.hub.clone();
        tokio::spawn(async move { hub.renew_for_run(&credential, "github").await })
    };
    // Past the hub's own check, waiting on the provider.
    fake.renewing.notified().await;
    f.hub.release_accounts(run);
    slow.add_permits(1);
    let refused = renewing.await.unwrap().unwrap_err();
    assert_eq!(refused.code, ErrorCode::PermissionDenied);
    f.close().await;
}

#[tokio::test]
async fn a_tool_asking_approval_shows_no_value_but_gets_its_own_input_back() {
    use ports::bridge::{BridgeBinding, BridgeHandler, BridgeRequest, BridgeResponse};
    let (f, _fake) = with_accounts().await;
    let owner = Caller::local_owner();
    let project = f.project().await;
    let (task, _) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    let run = run_started(&f, task).await;
    // The token straddles where the 640-character preview is cut.
    let filler = "x".repeat(630);
    let input = serde_json::json!({"command": format!("{filler} -H 'Authorization: {TOKEN}' x")});
    let asked = {
        let hub = f.hub.clone();
        let request = BridgeRequest::PermissionPrompt {
            tool_name: "Bash".into(),
            input: input.clone(),
            tool_use_id: Some("toolu_1".into()),
            native_request_id: Some("1".into()),
        };
        tokio::spawn(async move {
            let binding = BridgeBinding {
                run_id: run,
                target_id: None,
                computer_use: false,
                browser: false,
                github: false,
                checklist_answers: false,
            };
            hub.handle(&binding, request).await
        })
    };
    let approval = tokio::time::timeout(WAIT, async {
        loop {
            if let Some(a) = f
                .hub
                .read(|r| r.list::<Approval>("approval"))
                .unwrap()
                .into_iter()
                .find(|a| a.state == ApprovalState::Pending)
            {
                break a;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let shown = serde_json::to_string(&approval).unwrap();
    assert!(approval.shortened, "{shown}");
    assert!(!shown.contains(&TOKEN[..CUT_SHOWN]), "{shown}");
    assert!(shown.contains(MASK), "{shown}");
    f.hub
        .resolve_approval(&owner, approval.approval_id, &ApprovalDecision::Allow)
        .await
        .unwrap();
    let BridgeResponse::Permission { decision } = asked.await.unwrap() else {
        panic!("permission")
    };
    assert_eq!(decision["updatedInput"], input);
    f.close().await;
}

#[tokio::test]
async fn a_paused_agent_account_keeps_the_task_from_starting() {
    let (f, fake) = with_accounts().await;
    *fake.refuse.lock().unwrap() = Some("Claude Code is paused in Accounts.".into());
    let owner = Caller::local_owner();
    let project = f.project().await;
    let (task, _) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    let current = || f.hub.read(|r| r.require::<Task>("task", task)).unwrap();
    let run_id = current().current_run.unwrap();
    let failed = || {
        f.hub
            .read(|r| r.require::<Run>("run", run_id))
            .unwrap()
            .state
            == RunState::Failed
    };
    let mut more = waiting();
    while more() && !failed() {
        tokio::task::yield_now().await;
    }
    assert!(failed());
    assert_eq!(f.harness.starts.load(Ordering::SeqCst), 0);
    let QueryData::TaskTimeline { items, .. } = f
        .hub
        .query(
            &owner,
            Query::TaskTimeline {
                task_id: task,
                before_seq: None,
                limit: 50,
            },
        )
        .await
        .unwrap()
    else {
        panic!("timeline")
    };
    assert!(items.iter().any(|i| matches!(&i.item,
        TimelineBody::Note { text } if text == "The agent didn’t start. Claude Code is paused in Accounts.")));
    f.close().await;
}

#[tokio::test]
async fn an_account_action_that_fails_later_says_so_in_its_status() {
    let (f, fake) = with_accounts().await;
    let owner = Caller::local_owner();
    *fake.fail.lock().unwrap() = Some("The disk is full.".into());
    let pause = account(AccountAction::PauseAll);
    let r = f.hub.submit(&owner, pause.clone()).await;
    // Taken first: only its status says what it came to.
    assert!(r.error.is_none(), "{r:?}");
    settle(&f.hub, r.command_id).await;
    let QueryData::CommandStatus { stage, error, .. } = f
        .hub
        .query(
            &owner,
            Query::CommandStatus {
                command_id: pause.command_id,
            },
        )
        .await
        .unwrap()
    else {
        unreachable!()
    };
    assert_eq!(stage, Some(protocol::command::CommandStage::Failed));
    let error = error.unwrap();
    assert!(error.message.contains("The disk is full."), "{error:?}");
    f.close().await;
}

#[tokio::test]
async fn what_an_agent_shows_the_person_hides_its_keys_and_codes() {
    use super::shown::{PNG, bound, person};
    use ports::bridge::PersonCall;
    const PHRASE: &str = "synthetic horse battery";
    let (f, fake) = with_accounts().await;
    let (task, run, binding) = bound(&f).await;
    assert!(f.hub.add_run_code(run, "482913"));
    std::fs::write(f.project_dir().join(format!("{TOKEN}.png")), PNG).unwrap();
    let shown = [
        PersonCall::Show {
            path: format!("{TOKEN}.png"),
            caption: Some(format!("signed in with {TOKEN}")),
            invite: None,
        },
        PersonCall::Preview {
            url: "http://127.0.0.1:5173/".into(),
            label: Some(format!("Site {TOKEN}")),
            note: Some("the code was 482913".into()),
            start: None,
            questions: vec![protocol::model::ApprovalQuestion {
                id: None,
                header: None,
                question: format!("Did {TOKEN} sign you in?"),
                options: vec![protocol::model::ApprovalQuestionOption {
                    label: "With 482913".into(),
                    description: None,
                }],
                multi_select: false,
                secret: false,
            }],
        },
        PersonCall::Invite {
            action: PersonAction::SendFiles,
            note: format!("paste {TOKEN} back"),
            goal: None,
        },
        PersonCall::Invite {
            action: PersonAction::ComputerUse,
            note: "Try signing in".into(),
            goal: Some(format!("Sign in with {TOKEN}")),
        },
    ];
    for call in shown {
        person(&f, &binding, call).await.unwrap();
    }
    // An address is opened as given: one holding a token or code isn't
    // shown, however it is spelled.
    *fake.seen.lock().unwrap() = Arc::new(vec![TOKEN.into(), PHRASE.into()]);
    let escaped: String = TOKEN.bytes().map(|b| format!("%{b:02X}")).collect();
    for query in [
        format!("key={TOKEN}"),
        format!("key={escaped}"),
        "code=%34%38%32%39%31%33".into(),
        "code=%2534%2538%2532%2539%2531%2533".into(),
        format!("pass={}", PHRASE.replace(' ', "+")),
    ] {
        let refused = person(
            &f,
            &binding,
            PersonCall::Preview {
                url: format!("http://127.0.0.1:5173/?{query}"),
                label: None,
                note: None,
                start: None,
                questions: Vec::new(),
            },
        )
        .await
        .unwrap_err();
        assert!(refused.contains("key or token"), "{query}: {refused}");
    }
    let items: Vec<TimelineItem> = f.hub.read(|r| r.list(&format!("timeline:{task}"))).unwrap();
    let kept = serde_json::to_string(&items).unwrap();
    assert!(!kept.contains(TOKEN) && !kept.contains("482913"), "{kept}");
    assert_eq!(kept.matches(MASK).count(), 8, "{kept}");
    let previews: Vec<PreviewEndpoint> = f.hub.read(|r| r.list("preview")).unwrap();
    assert_eq!(previews.len(), 1);
    assert_eq!(previews[0].label, format!("Site {MASK}"));
    assert!(!written_anywhere(&f.path, TOKEN));
    f.close().await;
}

#[tokio::test]
async fn a_page_found_in_a_project_is_named_with_its_keys_and_codes_hidden() {
    use super::found_previews::{Pages, Servers};
    use super::shown::{bound, person};
    use crate::preview::PageAnswer;
    use ports::bridge::PersonCall;
    let (servers, pages) = (Arc::new(Servers::default()), Arc::new(Pages::default()));
    let fake = Arc::new(FakeAccounts::default());
    let f = {
        let (servers, pages, fake) = (servers.clone(), pages.clone(), fake.clone());
        Fixture::with(move |b| {
            b.accounts(fake)
                .local_servers(servers)
                .preview_checker(pages)
        })
        .await
    };
    let (task, run, binding) = bound(&f).await;
    assert!(f.hub.add_run_code(run, "482913"));
    let env = f.harness.specs.lock().unwrap()[0].env.0.clone();
    let (_, credential) = (env.iter()).find(|(name, _)| name == RENEW_ENV).unwrap();
    let dir = ports::library::plain(std::fs::canonicalize(f.project_dir()).unwrap());
    let loopback = std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);
    servers.serve(loopback, 5173, &dir, 1);
    servers.serve(loopback, 8080, &dir, 2);
    servers.serve(loopback, 4000, &dir, 3);
    pages.answer(5173, PageAnswer::Page(Some(format!("{TOKEN} 482913"))));
    pages.answer(8080, PageAnswer::Page(Some(KEY.into())));
    pages.answer(4000, PageAnswer::Page(Some(credential.clone())));
    f.hub.find_previews().await.unwrap();
    let mut previews: Vec<PreviewEndpoint> = f.hub.read(|r| r.list("preview")).unwrap();
    previews.sort_by(|a, b| a.url.cmp(&b.url));
    let labels: Vec<&str> = previews.iter().map(|p| p.label.as_str()).collect();
    assert_eq!(labels, [MASK, format!("{MASK} {MASK}").as_str(), KEY]);
    // A key set since is hidden where the agent shows the page it named.
    *fake.seen.lock().unwrap() = Arc::new(vec![TOKEN.into(), KEY.into()]);
    for url in ["http://127.0.0.1:5173/", "http://127.0.0.1:8080/"] {
        let call = PersonCall::Preview {
            url: url.into(),
            label: Some("Mine".into()),
            note: None,
            start: None,
            questions: Vec::new(),
        };
        let reply = person(&f, &binding, call).await.unwrap();
        assert!(reply.contains(MASK) && !reply.contains(KEY), "{reply}");
    }
    let items: Vec<TimelineItem> = f.hub.read(|r| r.list(&format!("timeline:{task}"))).unwrap();
    let kept = serde_json::to_string(&items).unwrap();
    assert!(!kept.contains(TOKEN) && !kept.contains(KEY), "{kept}");
    assert!(!kept.contains("482913"), "{kept}");
    assert_eq!(kept.matches(MASK).count(), 3, "{kept}");
    f.close().await;
}

#[tokio::test]
async fn a_found_page_named_past_what_is_shown_shows_no_part_of_a_code() {
    use super::found_previews::{Pages, Servers};
    use super::shown::bound;
    use crate::preview::PageAnswer;
    let (servers, pages) = (Arc::new(Servers::default()), Arc::new(Pages::default()));
    let f = {
        let (servers, pages) = (servers.clone(), pages.clone());
        Fixture::with(move |b| {
            b.accounts(Arc::new(FakeAccounts::default()))
                .local_servers(servers)
                .preview_checker(pages)
        })
        .await
    };
    let (_, run, _) = bound(&f).await;
    assert!(f.hub.add_run_code(run, "482913"));
    let dir = ports::library::plain(std::fs::canonicalize(f.project_dir()).unwrap());
    let loopback = std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);
    // The code runs past where the name is cut.
    let named = format!("{}482913", "x".repeat(55));
    servers.serve(loopback, 5173, &dir, 1);
    pages.answer(5173, PageAnswer::Page(Some(named)));
    f.hub.find_previews().await.unwrap();
    let previews: Vec<PreviewEndpoint> = f.hub.read(|r| r.list("preview")).unwrap();
    let [found] = &previews[..] else {
        panic!("{previews:?}")
    };
    let shown = crate::preview::short(&format!("{}{MASK}", "x".repeat(55)));
    assert_eq!(found.label, shown);
    assert!(!found.label.contains('4'));
    f.close().await;
}

#[tokio::test]
async fn a_found_page_stays_named_with_its_codes_hidden_after_its_run_ends() {
    use super::found_previews::{Pages, Servers};
    use super::shown::bound;
    use crate::preview::PageAnswer;
    let (servers, pages) = (Arc::new(Servers::default()), Arc::new(Pages::default()));
    let f = {
        let (servers, pages) = (servers.clone(), pages.clone());
        Fixture::with(move |b| {
            b.accounts(Arc::new(FakeAccounts::default()))
                .local_servers(servers)
                .preview_checker(pages)
        })
        .await
    };
    let (_, run, _) = bound(&f).await;
    let dir = ports::library::plain(std::fs::canonicalize(f.project_dir()).unwrap());
    let loopback = std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);
    let labels = || async {
        f.hub.find_previews().await.unwrap();
        let mut previews: Vec<PreviewEndpoint> = f.hub.read(|r| r.list("preview")).unwrap();
        previews.sort_by(|a, b| a.url.cmp(&b.url));
        previews.into_iter().map(|p| p.label).collect::<Vec<_>>()
    };
    // Its run is given the code while it is asked, after what is hidden
    // is first taken.
    servers.serve(loopback, 5174, &dir, 2);
    pages.answer(5174, PageAnswer::Page(Some("Code 731905".into())));
    let given = f.hub.clone();
    pages
        .meanwhile
        .set(move || assert!(given.add_run_code(run, "731905")));
    assert_eq!(labels().await, [format!("Code {MASK}")]);
    // Its run ends while it is asked, and it is asked no more after.
    assert!(f.hub.add_run_code(run, "482913"));
    servers.serve(loopback, 5173, &dir, 1);
    pages.answer(5173, PageAnswer::Page(Some("Code 482913".into())));
    let ending = f.hub.clone();
    pages.meanwhile.set(move || ending.release_accounts(run));
    let both = [format!("Code {MASK}"), format!("Code {MASK}")];
    assert_eq!(labels().await, both);
    assert_eq!(labels().await, both);
    assert_eq!(pages.asks("http://127.0.0.1:5173/"), 1);
    assert_eq!(pages.asks("http://127.0.0.1:5174/"), 1);
    f.close().await;
}

#[tokio::test]
async fn a_found_page_naming_a_code_in_character_references_has_it_hidden_after_its_run_ends() {
    use super::found_previews::{Pages, Servers};
    use super::shown::bound;
    use crate::preview::{PageAnswer, title};
    let (servers, pages) = (Arc::new(Servers::default()), Arc::new(Pages::default()));
    let f = {
        let (servers, pages) = (servers.clone(), pages.clone());
        Fixture::with(move |b| {
            b.accounts(Arc::new(FakeAccounts::default()))
                .local_servers(servers)
                .preview_checker(pages)
        })
        .await
    };
    let (_, run, _) = bound(&f).await;
    assert!(f.hub.add_run_code(run, "482913"));
    let dir = ports::library::plain(std::fs::canonicalize(f.project_dir()).unwrap());
    let loopback = std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);
    servers.serve(loopback, 5173, &dir, 1);
    // As the page itself writes it.
    let named = title(b"<title>Code &#52;&#x38;2&#X39;&#0049;3</title>");
    pages.answer(5173, PageAnswer::Page(named));
    let label = || async {
        f.hub.find_previews().await.unwrap();
        let previews: Vec<PreviewEndpoint> = f.hub.read(|r| r.list("preview")).unwrap();
        previews.into_iter().map(|p| p.label).collect::<Vec<_>>()
    };
    assert_eq!(label().await, [format!("Code {MASK}")]);
    f.hub.release_accounts(run);
    assert_eq!(label().await, [format!("Code {MASK}")]);
    assert_eq!(pages.asks("http://127.0.0.1:5173/"), 1);
    f.close().await;
}

#[tokio::test]
async fn a_found_title_hides_a_value_whole_with_another_inside_it() {
    use super::shown::bound;
    let accounts = Arc::new(FakeAccounts::default());
    *accounts.seen.lock().unwrap() = Arc::new(vec![
        "my-private-token    token-suffix".into(),
        "token-suffix".into(),
    ]);
    let f = Fixture::with({
        let accounts = accounts.clone();
        move |b: HubBuilder| b.accounts(accounts)
    })
    .await;
    let (_, run, _) = bound(&f).await;
    // A code inside it too, and one spaced as a title isn't.
    assert!(f.hub.add_run_code(run, "private"));
    assert!(f.hub.add_run_code(run, "48\t29"));
    let hide = f.hub.found_masking();
    let hidden = format!("Signed in {MASK}");
    for written in [
        "my-private-token\ttoken-suffix",
        "my-private-token    token-suffix",
        " my-private-token\u{A0}token-suffix\n",
        "48\n 29",
    ] {
        let mut title = format!("Signed in {written}");
        hide(&mut title);
        assert_eq!(title, hidden, "{written:?}");
        // And so as it is kept, hidden again.
        hide(&mut title);
        assert_eq!(title, hidden, "{written:?}");
    }
    // Cut short as it shows.
    let mut title = "Signed in my-private-token\ttok…".to_string();
    hide(&mut title);
    assert_eq!(title, format!("{hidden}…"));
    f.close().await;
}

#[tokio::test]
async fn a_found_title_shows_a_value_too_short_to_hide_however_it_is_spaced() {
    let accounts = Arc::new(FakeAccounts::default());
    // Hidden nowhere, so not as a title spaces it either.
    *accounts.seen.lock().unwrap() = Arc::new(vec!["pw  ok".into(), "pw\u{A0}ok".into()]);
    let f = Fixture::with({
        let accounts = accounts.clone();
        move |b: HubBuilder| b.accounts(accounts)
    })
    .await;
    let mut title = "Signed in pw ok".to_string();
    f.hub.found_masking()(&mut title);
    assert_eq!(title, "Signed in pw ok");
    f.close().await;
}

#[tokio::test]
async fn a_found_title_hides_the_start_of_a_value_its_spaces_make_short() {
    let accounts = Arc::new(FakeAccounts::default());
    // Ten characters held, seven as a title shows them.
    *accounts.seen.lock().unwrap() = Arc::new(vec!["abcd    ef".into()]);
    let f = Fixture::with({
        let accounts = accounts.clone();
        move |b: HubBuilder| b.accounts(accounts)
    })
    .await;
    let hide = f.hub.found_masking();
    let hidden = format!("Signed in {MASK}");
    for (written, shown) in [
        ("abcd    e…", format!("{hidden}…")),
        ("abcd\te… more", format!("{hidden}… more")),
        ("abcd    e", hidden.clone()),
        ("abcd \u{A0}ef", hidden.clone()),
    ] {
        let mut title = format!("Signed in {written}");
        hide(&mut title);
        assert_eq!(title, shown, "{written:?}");
        hide(&mut title);
        assert_eq!(title, shown, "{written:?}");
    }
    f.close().await;
}

#[tokio::test]
async fn a_preview_not_found_now_has_what_ophio_holds_now_hidden_in_its_name() {
    use super::found_previews::{Pages, Servers};
    use super::shown::bound;
    use crate::preview::PageAnswer;
    let (servers, pages) = (Arc::new(Servers::default()), Arc::new(Pages::default()));
    let given = {
        let (servers, pages) = (servers.clone(), pages.clone());
        move |b: HubBuilder| {
            b.accounts(Arc::new(FakeAccounts::default()))
                .local_servers(servers)
                .preview_checker(pages)
        }
    };
    let f = Fixture::with(given.clone()).await;
    let (_, run, _) = bound(&f).await;
    let dir = ports::library::plain(std::fs::canonicalize(f.project_dir()).unwrap());
    let loopback = std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);
    let x = "x".repeat(55);
    // Their names are cut short inside what they hold.
    let codes = [(5173, "482913"), (5174, "271828"), (5175, "314159")];
    for (port, code) in codes {
        servers.serve(loopback, port, &dir, u64::from(port));
        let title = format!("{x} {code} more");
        pages.answer(port, PageAnswer::Page(Some(title)));
    }
    async fn previews(f: &Fixture) -> Vec<PreviewEndpoint> {
        f.hub.find_previews().await.unwrap();
        let mut previews: Vec<PreviewEndpoint> = f.hub.read(|r| r.list("preview")).unwrap();
        previews.sort_by(|a, b| a.url.cmp(&b.url));
        previews
    }
    let labels = |previews: &[PreviewEndpoint]| -> Vec<String> {
        previews.iter().map(|p| p.label.clone()).collect()
    };
    let titles = |f: &Fixture| -> Vec<String> {
        let kept = f
            .hub
            .read(|r| r.entries::<String>(crate::found_previews::TITLES));
        kept.unwrap().into_iter().map(|(_, title)| title).collect()
    };
    assert_eq!(
        labels(&previews(&f).await),
        codes.map(|(_, code)| format!("{x} {}…", &code[..4]))
    );
    // One server's program works outside every project, listening still,
    // and another stops.
    servers.moves(5173, std::path::Path::new("/elsewhere"));
    servers.stop(5174);
    let stopped = previews(&f).await;
    assert!(stopped[..2].iter().all(|p| p.reachable == Some(false)));
    // Then Ophio is given what they show, and the third stops meanwhile.
    f.clock.advance(Duration::from_secs(20));
    for (_, code) in codes {
        assert!(f.hub.add_run_code(run, code));
    }
    servers.stop(5175);
    let hidden = [
        format!("{x} [hid…"),
        format!("{x} [hid…"),
        format!("{x} [hid…"),
    ];
    let now = previews(&f).await;
    assert_eq!(labels(&now), hidden);
    assert_eq!(now[2].reachable, Some(false));
    // Still stopped since they stopped.
    for (now, then) in now[..2].iter().zip(&stopped) {
        assert_eq!(
            (now.reachable, now.checked_at),
            (then.reachable, then.checked_at)
        );
    }
    assert_eq!(labels(&previews(&f).await), hidden);
    // What they are named from is kept only hidden, and so as the
    // companion starts again.
    let named_from = vec![format!("{x} {MASK} more"); 3];
    assert_eq!(titles(&f), named_from);
    f.hub.release_accounts(run);
    let f = f.reopened(given).await;
    assert_eq!(labels(&previews(&f).await), hidden);
    assert_eq!(titles(&f), named_from);
    // Until they are no longer kept.
    f.clock.advance(Duration::from_secs(11 * 60));
    assert!(previews(&f).await.is_empty());
    assert!(titles(&f).is_empty());
    f.close().await;
}

#[tokio::test]
async fn a_preview_not_found_whose_title_outgrows_a_name_once_hidden_is_named_by_its_address() {
    use super::found_previews::{Pages, Servers};
    use super::shown::bound;
    use crate::found_previews::{NAME_BYTES, TITLES};
    use crate::preview::PageAnswer;
    let (servers, pages) = (Arc::new(Servers::default()), Arc::new(Pages::default()));
    let given = {
        let (servers, pages) = (servers.clone(), pages.clone());
        move |b: HubBuilder| {
            b.accounts(Arc::new(FakeAccounts::default()))
                .local_servers(servers)
                .preview_checker(pages)
        }
    };
    let f = Fixture::with(given.clone()).await;
    let (_, run, _) = bound(&f).await;
    let dir = ports::library::plain(std::fs::canonicalize(f.project_dir()).unwrap());
    let loopback = std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);
    // A code many times over is a name until the code is hidden in it.
    let title = vec!["482913"; 140].join(" ");
    assert!(title.len() <= NAME_BYTES);
    for port in [5173, 5174] {
        servers.serve(loopback, port, &dir, u64::from(port));
        pages.answer(port, PageAnswer::Page(Some(title.clone())));
    }
    async fn labels(f: &Fixture) -> Vec<String> {
        f.hub.find_previews().await.unwrap();
        let mut previews: Vec<PreviewEndpoint> = f.hub.read(|r| r.list("preview")).unwrap();
        previews.sort_by(|a, b| a.url.cmp(&b.url));
        previews.into_iter().map(|p| p.label).collect()
    }
    let titles = |f: &Fixture| f.hub.read(|r| r.list::<String>(TITLES)).unwrap().len();
    assert_eq!(labels(&f).await, vec![crate::preview::short(&title); 2]);
    // One server's program works outside every project, and the other
    // stops, before Ophio is given the code.
    servers.moves(5173, std::path::Path::new("/elsewhere"));
    servers.stop(5174);
    labels(&f).await;
    assert!(f.hub.add_run_code(run, "482913"));
    let addresses = ["127.0.0.1:5173", "127.0.0.1:5174"];
    assert_eq!(labels(&f).await, addresses);
    assert_eq!(titles(&f), 0);
    // And so as the companion starts again.
    f.hub.release_accounts(run);
    let f = f.reopened(given).await;
    assert_eq!(labels(&f).await, addresses);
    assert_eq!(titles(&f), 0);
    f.close().await;
}

#[tokio::test]
async fn a_found_page_keeps_its_code_hidden_while_its_server_runs_wherever_it_works() {
    use super::found_previews::{Pages, Servers};
    use super::shown::bound;
    use crate::preview::PageAnswer;
    let (servers, pages) = (Arc::new(Servers::default()), Arc::new(Pages::default()));
    let given = {
        let (servers, pages) = (servers.clone(), pages.clone());
        move |b: HubBuilder| {
            b.accounts(Arc::new(FakeAccounts::default()))
                .local_servers(servers)
                .preview_checker(pages)
        }
    };
    let f = Fixture::with(given.clone()).await;
    let (_, run, _) = bound(&f).await;
    assert!(f.hub.add_run_code(run, "482913"));
    // A code can be a word of what hides it.
    assert!(f.hub.add_run_code(run, "Ophio"));
    let dir = ports::library::plain(std::fs::canonicalize(f.project_dir()).unwrap());
    let loopback = std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);
    servers.serve(loopback, 5173, &dir, 1);
    pages.answer(5173, PageAnswer::Page(Some("Code 482913 for Ophio".into())));
    async fn label(f: &Fixture) -> Vec<String> {
        f.hub.find_previews().await.unwrap();
        let previews: Vec<PreviewEndpoint> = f.hub.read(|r| r.list("preview")).unwrap();
        previews.into_iter().map(|p| p.label).collect()
    }
    let hidden = [format!("Code {MASK} for {MASK}")];
    assert_eq!(label(&f).await, hidden);
    assert_eq!(label(&f).await, hidden);
    f.hub.release_accounts(run);
    // Its program works outside every project for a pass, listening still,
    // and comes back.
    servers.moves(5173, std::path::Path::new("/elsewhere"));
    assert_eq!(label(&f).await, hidden);
    servers.moves(5173, &dir);
    assert_eq!(label(&f).await, hidden);
    // And the companion starts again, with the server running on.
    let f = f.reopened(given).await;
    assert_eq!(label(&f).await, hidden);
    assert_eq!(pages.asks("http://127.0.0.1:5173/"), 1);
    f.close().await;
}
