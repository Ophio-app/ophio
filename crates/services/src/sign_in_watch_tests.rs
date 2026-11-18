// SPDX-License-Identifier: Apache-2.0
//! An agent that isn't signed in: one notice per lapse, cleared once it is
//! signed in again, seen only by devices that may manage accounts.
use super::*;
use ports::HarnessEvent;
use protocol::capability::AccountReadiness;
use protocol::model::{AttentionItem, AttentionKind};

fn signed_out() -> AccountReadiness {
    AccountReadiness::SignedOut {
        remedy: "Run claude auth login on the computer".into(),
    }
}
fn signed_in() -> AccountReadiness {
    AccountReadiness::SignedIn {
        account_label: None,
        plan: None,
    }
}

impl Fixture {
    fn says(&self, account: AccountReadiness) {
        *self.harness.account.lock().unwrap() = account;
    }
    /// The notices about the agents' sign-ins, oldest first.
    fn notices(&self) -> Vec<AttentionItem> {
        let mut items: Vec<AttentionItem> = self
            .hub
            .read(|r| r.list::<AttentionItem>("attention"))
            .unwrap()
            .into_iter()
            .filter(|a| a.account_id.is_some())
            .collect();
        items.sort_by_key(|a| (a.created_at, a.attention_id));
        items
    }
    fn open_notices(&self) -> Vec<AttentionItem> {
        let mut open = self.notices();
        open.retain(|a| a.resolved_at.is_none());
        open
    }
    /// Waits for the background work a check started to show.
    async fn until(&self, done: impl Fn(&Self) -> bool) {
        let mut more = waiting();
        while !done(self) {
            assert!(more(), "the computer didn't get there in time");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    /// A device that may manage accounts, as the computer's owner allows.
    async fn accounts_device(&self) -> Caller {
        let mut phone = pair(&self.hub, 2).await;
        let Principal::Device { device_id } = phone.principal else {
            unreachable!()
        };
        let wider = Grants {
            accounts: true,
            ..phone.grants.clone()
        };
        let r = self
            .hub
            .submit(
                &Caller::local_owner(),
                CommandEnvelope::new(Command::SetDeviceGrants {
                    device_id,
                    expected: Some(phone.grants.clone()),
                    grants: wider.clone(),
                }),
            )
            .await;
        assert_eq!(r.status, ReceiptStatus::Accepted, "{r:?}");
        settle(&self.hub, r.command_id).await;
        phone.grants = wider;
        phone
    }
}

async fn snapshot_of(f: &Fixture, caller: &Caller) -> Snapshot {
    match f
        .hub
        .query(
            caller,
            Query::Snapshot {
                reads_withheld: Some(true),
                account_items: true,
            },
        )
        .await
        .unwrap()
    {
        QueryData::Snapshot(snapshot) => *snapshot,
        _ => panic!("snapshot expected"),
    }
}

#[tokio::test]
async fn a_signed_out_agent_raises_one_notice_until_it_is_signed_in() {
    let f = Fixture::new().await;
    let manager = f.accounts_device().await;
    let other = f.device().await;
    assert!(!other.grants.accounts);
    assert!(
        f.hub
            .host_capabilities()
            .await
            .features
            .iter()
            .all(|f| f != protocol::accounts::SIGN_IN_ON_PHONE_FEATURE),
        "a computer without Accounts can't sign in from the phone"
    );

    f.says(signed_out());
    f.hub.refresh_catalog().await.unwrap();
    let raised = f.open_notices();
    let [notice] = raised.as_slice() else {
        panic!("one notice: {raised:?}")
    };
    let computer = f.hub.read(|r| f.hub.computer_name(r)).unwrap();
    assert_eq!(
        notice.title,
        format!("Claude isn't signed in on {computer}")
    );
    assert_eq!(notice.account_id.as_deref(), Some("claude"));
    assert_eq!(notice.kind, AttentionKind::WaitingForYou);
    assert_eq!(notice.task_id, None);

    // Checked again, still signed out: the same lapse, nothing new.
    f.hub.refresh_catalog().await.unwrap();
    f.hub.refresh_catalog().await.unwrap();
    assert_eq!(f.notices().len(), 1);

    // It alerts once, for those who may manage accounts.
    let (alerts, _) = f.hub.open_alerts(&manager, false).await.unwrap();
    let alert = alerts
        .iter()
        .find(|a| a.attention_id == notice.attention_id)
        .expect("the manager is alerted");
    let sign_in = alert.sign_in.as_ref().unwrap();
    assert_eq!(
        (sign_in.account_id.as_str(), sign_in.service.as_str()),
        ("claude", "Claude")
    );
    assert_eq!(alert.headline.as_deref(), Some(notice.title.as_str()));
    assert!(!alert.quiet);
    let (alerts, _) = f.hub.open_alerts(&other, false).await.unwrap();
    assert!(alerts.iter().all(|a| a.sign_in.is_none()));
    // Listed for the one, not the other.
    let listed = |s: &Snapshot| {
        s.attention
            .iter()
            .any(|a| a.attention_id == notice.attention_id)
    };
    assert!(listed(&snapshot_of(&f, &manager).await));
    assert!(!listed(&snapshot_of(&f, &other).await));

    // Signed in again: cleared, and a later lapse is a new notice.
    f.says(signed_in());
    f.hub.refresh_catalog().await.unwrap();
    assert!(f.open_notices().is_empty());
    f.says(signed_out());
    f.hub.refresh_catalog().await.unwrap();
    let all = f.notices();
    assert_eq!(all.len(), 2);
    assert_ne!(all[1].attention_id, notice.attention_id);
    f.close().await;
}

#[tokio::test]
async fn a_lapse_is_remembered_when_the_computer_starts_again() {
    let f = Fixture::new().await;
    f.says(signed_out());
    f.hub.refresh_catalog().await.unwrap();
    assert_eq!(f.open_notices().len(), 1);
    let f = f.reopened(|b| b).await;
    f.hub.refresh_catalog().await.unwrap();
    assert_eq!(f.notices().len(), 1, "no second alert for the same lapse");
    f.close().await;
}

#[tokio::test]
async fn a_run_refused_for_its_sign_in_raises_the_notice_until_the_model_answers() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let (task_id, _) = f
        .task(
            project,
            TaskPlacement::StartNow,
            &Caller::local_owner(),
            false,
        )
        .await;
    let run_id = f
        .hub
        .read(|r| r.require::<Task>("task", task_id))
        .unwrap()
        .current_run
        .unwrap();
    let mut more = waiting();
    while !f.harness.runs().contains(&run_id) {
        assert!(more());
        tokio::task::yield_now().await;
    }
    f.harness
        .emit(
            run_id,
            HarnessEvent::SignedOut {
                reason: "Claude Code's login expired".into(),
            },
        )
        .await;
    f.until(|f| f.open_notices().len() == 1).await;
    // Its status command still says signed in, as an expired login it
    // can't renew does: that proves nothing, and the notice stays.
    f.hub.refresh_catalog().await.unwrap();
    f.hub.refresh_catalog().await.unwrap();
    assert_eq!(f.open_notices().len(), 1);
    // Refused again: the same lapse.
    f.harness
        .emit(
            run_id,
            HarnessEvent::SignedOut {
                reason: "Claude Code's login expired".into(),
            },
        )
        .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(f.notices().len(), 1);
    // The model answering proves the sign-in works again.
    f.harness
        .emit(
            run_id,
            HarnessEvent::AgentMessage {
                message_key: "m1".into(),
                text: "Done.".into(),
            },
        )
        .await;
    f.until(|f| f.open_notices().is_empty()).await;
    f.close().await;
}

#[tokio::test]
async fn a_run_refused_at_the_start_checks_the_sign_in_again() {
    let f = Fixture::new().await;
    let project = f.project().await;
    f.says(signed_out());
    f.hub.refresh_catalog().await.unwrap();
    assert_eq!(f.open_notices().len(), 1);
    // Signed in on the computer since the last check.
    f.says(signed_in());
    let create = || {
        CommandEnvelope::new(Command::CreateTask {
            origin: None,
            from_chat: None,
            github: None,
            agent_chosen: None,
            mode: None,
            project_id: project,
            kind: TaskKind::Coding,
            title: Some("Test task".into()),
            prompt: "test".into(),
            bundle: None,
            harness: HarnessKind::ClaudeCode,
            model: None,
            effort: None,
            launch_policy: None,
            branch: None,
            mission: None,
            target_id: None,
            placement: TaskPlacement::StartNow,
            start: None,
            place: None,
            browser: None,
            conversation: None,
            access: None,
            review: None,
        })
    };
    let refused = f.hub.submit(&Caller::local_owner(), create()).await;
    assert_eq!(
        refused.error.map(|e| e.code),
        Some(ErrorCode::AccountNotReady)
    );
    // The refusal checks again at once: the notice clears and the next
    // try goes ahead.
    f.until(|f| f.open_notices().is_empty()).await;
    let r = f.hub.submit(&Caller::local_owner(), create()).await;
    assert_eq!(r.status, ReceiptStatus::Accepted, "{r:?}");
    f.close().await;
}

#[tokio::test]
async fn a_check_begun_before_a_sign_in_finished_raises_nothing() {
    use crate::sign_in_watch::{Seen, seen};
    let f = Fixture::new().await;
    let at = Timestamp;
    let saw = |what: Seen, asked_at: i64, now: i64| {
        f.hub
            .write(|tx| {
                seen(
                    tx,
                    HarnessKind::ClaudeCode,
                    what,
                    "Studio",
                    at(asked_at),
                    at(now),
                )
            })
            .unwrap()
    };
    saw(Seen::Out, 1_000, 1_000);
    assert_eq!(f.open_notices().len(), 1);
    // Signed in on the phone at 10 s.
    saw(Seen::Proven, 10_000, 10_000);
    assert!(f.open_notices().is_empty());
    // A status check that began at 5 s, before then, and answered after.
    saw(Seen::Out, 5_000, 12_000);
    assert!(f.open_notices().is_empty(), "older news raises nothing");
    assert_eq!(f.notices().len(), 1);
    // One that began after is a new lapse.
    saw(Seen::Out, 11_000, 13_000);
    assert_eq!(f.open_notices().len(), 1);
    assert_eq!(f.notices().len(), 2);
    // Codex was never signed in here: its status saying so is no lapse.
    let codex = |what: Seen, asked_at: i64, now: i64| {
        f.hub
            .write(|tx| {
                seen(
                    tx,
                    HarnessKind::Codex,
                    what,
                    "Studio",
                    at(asked_at),
                    at(now),
                )
            })
            .unwrap()
    };
    let of_codex = |f: &Fixture| {
        f.open_notices()
            .into_iter()
            .filter(|a| a.account_id.as_deref() == Some("codex"))
            .collect::<Vec<_>>()
    };
    codex(Seen::Out, 20_000, 20_000);
    assert!(of_codex(&f).is_empty(), "never signed in here");
    // Once seen signed in, its next "signed out" is a lapse of its own.
    codex(Seen::In, 21_000, 21_000);
    codex(Seen::Out, 22_000, 22_000);
    let raised = of_codex(&f);
    assert_eq!(raised.len(), 1);
    assert_eq!(raised[0].title, "Codex isn't signed in on Studio");
    f.close().await;
}

#[tokio::test]
async fn a_stale_signed_in_answer_or_an_older_run_changes_nothing() {
    use crate::sign_in_watch::{Seen, seen};
    let f = Fixture::new().await;
    let saw = |what: Seen, asked_at: i64, now: i64| {
        f.hub
            .write(|tx| {
                seen(
                    tx,
                    HarnessKind::ClaudeCode,
                    what,
                    "Studio",
                    Timestamp(asked_at),
                    Timestamp(now),
                )
            })
            .unwrap()
    };
    saw(Seen::In, 0, 0);
    // Raised at 10 s; a check that began at 5 s and says "signed in" was
    // asked before the lapse, and doesn't clear it.
    saw(Seen::Out, 10_000, 10_000);
    saw(Seen::In, 5_000, 11_000);
    assert_eq!(f.open_notices().len(), 1);
    // One begun after it does.
    saw(Seen::In, 12_000, 12_000);
    assert!(f.open_notices().is_empty());
    // A run that began before that sign-in had the older one: its refusal
    // raises nothing. One that began after is a lapse.
    saw(Seen::RefusedRun, 11_500, 13_000);
    assert!(f.open_notices().is_empty());
    saw(Seen::RefusedRun, 12_500, 14_000);
    assert_eq!(f.open_notices().len(), 1);
    f.close().await;
}

#[tokio::test]
async fn not_now_puts_one_lapse_away_and_the_next_alerts_again() {
    let f = Fixture::new().await;
    let manager = f.accounts_device().await;
    f.says(signed_out());
    f.hub.refresh_catalog().await.unwrap();
    assert_eq!(f.open_notices().len(), 1);
    let dismiss = || {
        CommandEnvelope::new(Command::Account {
            action: protocol::accounts::AccountAction::DismissSignInNotice {
                account_id: "claude".into(),
            },
        })
    };
    let r = f.hub.submit(&manager, dismiss()).await;
    assert_eq!(r.status, ReceiptStatus::Accepted, "{r:?}");
    settle(&f.hub, r.command_id).await;
    f.until(|f| f.open_notices().is_empty()).await;
    // Still signed out: the same lapse, put away, raises nothing more.
    f.hub.refresh_catalog().await.unwrap();
    assert!(f.open_notices().is_empty());
    assert_eq!(f.notices().len(), 1);
    // Signed in, then out again: a new lapse alerts.
    f.says(signed_in());
    f.hub.refresh_catalog().await.unwrap();
    f.says(signed_out());
    f.hub.refresh_catalog().await.unwrap();
    assert_eq!(f.open_notices().len(), 1);
    assert_eq!(f.notices().len(), 2);
    f.close().await;
}

#[tokio::test]
async fn only_a_phone_that_may_manage_accounts_and_asks_sees_a_sign_in_s_link() {
    use crate::accounts::view_for;
    use protocol::accounts::{AccountsView, Secret, SignIn, SignInStage};
    let f = Fixture::new().await;
    let manager = f.accounts_device().await;
    let other = f.device().await;
    let view = AccountsView {
        accounts: Vec::new(),
        sign_in: Some(SignIn {
            account_id: "codex".into(),
            service: "Codex".into(),
            started_at: Timestamp(1),
            stage: SignInStage::OnPhone {
                link: Secret::new("https://auth.openai.com/codex/device"),
                url_host: "auth.openai.com".into(),
                code: Some("AB12-CDE34".into()),
                paste_code: false,
                code_refused: false,
            },
        }),
        history: Vec::new(),
        store: "Synthetic vault".into(),
        store_note: None,
        browser: None,
        password_manager_done: false,
    };
    let stage = |caller: &Caller, on_phone: bool| {
        view_for(view.clone(), caller, on_phone)
            .sign_in
            .unwrap()
            .stage
    };
    assert!(matches!(stage(&manager, true), SignInStage::OnPhone { .. }));
    let host_only = SignInStage::Browser {
        url_host: "auth.openai.com".into(),
    };
    // An app that didn't ask (an older one), a device without Accounts,
    // the owner's socket (and so a program Ophio started) see the host.
    assert_eq!(stage(&manager, false), host_only);
    assert_eq!(stage(&other, true), host_only);
    assert_eq!(stage(&Caller::local_owner(), true), host_only);
    for caller in [&other, &Caller::local_owner()] {
        let shown = serde_json::to_string(&view_for(view.clone(), caller, true)).unwrap();
        assert!(!shown.contains("codex/device") && !shown.contains("AB12"));
    }
    f.close().await;
}

#[tokio::test]
async fn signing_an_agent_out_on_purpose_raises_nothing_until_it_signs_in_again() {
    use crate::sign_in_watch::{Seen, seen};
    let f = Fixture::new().await;
    let saw = |what: Seen, asked_at: i64, now: i64| {
        f.hub
            .write(|tx| {
                seen(
                    tx,
                    HarnessKind::ClaudeCode,
                    what,
                    "Studio",
                    Timestamp(asked_at),
                    Timestamp(now),
                )
            })
            .unwrap()
    };
    saw(Seen::In, 1_000, 1_000);
    // The person taps Sign out at 5 s; its status then says signed out.
    saw(Seen::SignedOutOnPurpose, 5_000, 5_000);
    saw(Seen::Out, 6_000, 6_000);
    assert!(f.notices().is_empty(), "a sign-out asked for isn't a lapse");
    // A check begun before the sign-out still said "signed in": old news.
    saw(Seen::In, 4_000, 7_000);
    saw(Seen::Out, 8_000, 8_000);
    assert!(f.notices().is_empty());
    // Signed in again, then lost: that is a lapse.
    saw(Seen::In, 9_000, 9_000);
    saw(Seen::Out, 10_000, 10_000);
    assert_eq!(f.open_notices().len(), 1);
    // Signing out on purpose puts it away too.
    saw(Seen::SignedOutOnPurpose, 11_000, 11_000);
    assert!(f.open_notices().is_empty());
    f.close().await;
}

#[tokio::test]
async fn a_sign_out_from_accounts_raises_no_notice() {
    let accounts = Arc::new(super::accounts_tests::FakeAccounts::default());
    let given = accounts.clone();
    let f = Fixture::with(move |b| b.accounts(given)).await;
    let manager = f.accounts_device().await;
    let r = f
        .hub
        .submit(
            &manager,
            CommandEnvelope::new(Command::Account {
                action: protocol::accounts::AccountAction::SignOut {
                    account_id: "claude".into(),
                },
            }),
        )
        .await;
    assert_eq!(r.status, ReceiptStatus::Accepted, "{r:?}");
    settle(&f.hub, r.command_id).await;
    // Its status now says signed out, as the person asked.
    f.says(signed_out());
    f.hub.refresh_catalog().await.unwrap();
    f.hub.refresh_catalog().await.unwrap();
    assert!(f.notices().is_empty(), "a sign-out asked for isn't a lapse");
    // Signed in again later, then lost: alerts as any lapse.
    f.clock.advance(Duration::from_secs(1));
    f.says(signed_in());
    f.hub.refresh_catalog().await.unwrap();
    f.says(signed_out());
    f.hub.refresh_catalog().await.unwrap();
    assert_eq!(f.open_notices().len(), 1);
    f.close().await;
}

#[tokio::test]
async fn a_run_dispatched_before_a_sign_in_with_no_notice_open_raises_nothing() {
    use crate::sign_in_watch::{Seen, seen};
    let f = Fixture::new().await;
    let saw = |what: Seen, asked_at: i64, now: i64| {
        f.hub
            .write(|tx| {
                seen(
                    tx,
                    HarnessKind::ClaudeCode,
                    what,
                    "Studio",
                    Timestamp(asked_at),
                    Timestamp(now),
                )
            })
            .unwrap()
    };
    // No notice open. A run is dispatched at 1 s, the person signs in again
    // in Accounts at 2 s, and the run is refused at 3 s with the older one.
    saw(Seen::SignedIn, 2_000, 2_000);
    saw(Seen::RefusedRun, 1_000, 3_000);
    assert!(f.notices().is_empty());
    // A run dispatched after that sign-in and refused is a lapse.
    saw(Seen::RefusedRun, 2_500, 4_000);
    assert_eq!(f.open_notices().len(), 1);
    f.close().await;
}

#[tokio::test]
async fn an_agent_signed_in_before_the_update_still_alerts() {
    let f = Fixture::new().await;
    // As a database from before: the catalog kept says signed in, and no
    // record of which agents were seen signed in exists yet.
    f.hub
        .write(|tx| tx.delete("meta", "sign_in_known"))
        .unwrap();
    f.says(signed_out());
    f.hub.refresh_catalog().await.unwrap();
    assert_eq!(f.open_notices().len(), 1);
    // Where the kept catalog never found it signed in, nothing.
    let g = Fixture::new().await;
    g.hub
        .write(|tx| {
            tx.delete("meta", "sign_in_known")?;
            let mut kept: Vec<protocol::capability::HarnessInfo> =
                tx.read().get("meta", "harnesses")?.unwrap_or_default();
            for info in &mut kept {
                info.account = signed_out();
            }
            tx.put("meta", "harnesses", &kept)?;
            Ok(())
        })
        .unwrap();
    g.says(signed_out());
    g.hub.refresh_catalog().await.unwrap();
    assert!(g.notices().is_empty());
    f.close().await;
    g.close().await;
}
