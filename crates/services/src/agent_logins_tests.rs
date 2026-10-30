// SPDX-License-Identifier: Apache-2.0
use super::*;
use ::accounts::logins::{ComputerAgentLogins, LoginsConfig};
use ::accounts::vault::MemoryVault;
use ports::HarnessEvent;
use ports::accounts::MASK;
use protocol::accounts::Secret;
use protocol::agent_logins::{
    AgentLoginAction, AgentLoginEvent, AgentLoginsView, DISCLAIMER_VERSION, LoginDetails,
};

#[path = "agent_browser_tests.rs"]
mod agent_browser_tests;

const PASSWORD: &str = "synthetic \"login\" pässword";

struct Logins {
    f: Fixture,
    dir: tempfile::TempDir,
    logins: ComputerAgentLogins,
}
async fn with_logins() -> Logins {
    let dir = tempfile::tempdir().unwrap();
    let logins = ComputerAgentLogins::new(LoginsConfig::new(
        dir.path().join("accounts"),
        Arc::new(MemoryVault::default()),
        None,
    ));
    let given = Arc::new(logins.clone());
    let f = Fixture::with(move |b| b.agent_logins(given)).await;
    Logins { f, dir, logins }
}
fn login(action: AgentLoginAction) -> CommandEnvelope {
    CommandEnvelope::new(Command::AgentLogin { action })
}
fn added() -> AgentLoginAction {
    AgentLoginAction::Add {
        details: LoginDetails {
            name: "Forum".into(),
            sites: vec!["forum.example.com".into()],
            username: "agent@example.com".into(),
            notes: String::new(),
            who: Default::default(),
            how: Default::default(),
            notify: false,
        },
        password: Some(Secret::new(PASSWORD)),
        code_key: None,
    }
}
/// Submits and waits for `command`, returning the error it came to.
async fn done(f: &Fixture, caller: &Caller, command: CommandEnvelope) -> Option<ProtocolError> {
    let r = f.hub.submit(caller, command.clone()).await;
    if let Some(error) = r.error {
        return Some(error);
    }
    settle(&f.hub, r.command_id).await;
    let QueryData::CommandStatus { error, .. } = f
        .hub
        .query(
            &Caller::local_owner(),
            Query::CommandStatus {
                command_id: command.command_id,
            },
        )
        .await
        .unwrap()
    else {
        unreachable!()
    };
    error.map(|e| *e)
}
async fn view(f: &Fixture, caller: &Caller) -> AgentLoginsView {
    let QueryData::AgentLogins { view } = f.hub.query(caller, Query::AgentLogins).await.unwrap()
    else {
        panic!("agent logins")
    };
    *view
}
/// A paired phone the computer has let manage accounts.
async fn phone_with_accounts(f: &Fixture) -> Caller {
    let mut phone = pair(&f.hub, 2).await;
    let Principal::Device { device_id } = phone.principal else {
        unreachable!()
    };
    let wider = Grants {
        accounts: true,
        ..phone.grants.clone()
    };
    let r = f
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
    settle(&f.hub, r.command_id).await;
    phone.grants = wider;
    phone
}
/// Every file under `dir`, read whole, for a value that must not be in any.

#[tokio::test]
async fn a_phone_with_accounts_manages_agent_logins_and_sees_a_password_only_when_it_asks() {
    let Logins { f, dir, logins: _ } = with_logins().await;
    assert!(
        f.hub
            .host_capabilities()
            .await
            .features
            .contains(&"agent_logins_v1".to_string())
    );
    let plain = f.device().await;
    let refused = f.hub.query(&plain, Query::AgentLogins).await.unwrap_err();
    assert_eq!(refused.code, ErrorCode::PermissionDenied);
    let phone = phone_with_accounts(&f).await;
    // Nothing but the notice until it is accepted.
    let off = done(&f, &phone, login(added())).await.unwrap();
    assert_eq!(off.code, ErrorCode::PermissionDenied, "{off:?}");
    let mut live = f.hub.subscribe(&phone, None).await.unwrap().live;
    let accept = AgentLoginAction::Accept {
        version: DISCLAIMER_VERSION,
    };
    assert!(done(&f, &phone, login(accept)).await.is_none());
    let add = login(added());
    assert!(done(&f, &phone, add.clone()).await.is_none());
    let told = tokio::time::timeout(WAIT, async {
        loop {
            if let Ok(crate::api::Outbound::Event(e)) = live.recv().await
                && matches!(e.event, Event::AgentLoginsChanged {})
            {
                return;
            }
        }
    })
    .await;
    assert!(told.is_ok(), "no AgentLoginsChanged");
    let seen = view(&f, &phone).await;
    let [kept] = &seen.logins[..] else {
        panic!("{:?}", seen.logins)
    };
    assert!(kept.has_password);
    assert_eq!(seen.history[0].by, "Phone");
    // Only the command's withheld copy is kept.
    let stored = f.hub.read(|r| r.command(add.command_id)).unwrap().unwrap();
    assert!(!serde_json::to_string(&stored).unwrap().contains(PASSWORD));
    for caller in [&phone, &Caller::local_owner()] {
        let QueryData::AgentLoginPassword { password } = f
            .hub
            .query(
                caller,
                Query::AgentLoginPassword {
                    login_id: kept.login_id.clone(),
                },
            )
            .await
            .unwrap()
        else {
            panic!("password")
        };
        assert_eq!(password.expose(), PASSWORD);
    }
    let shown: Vec<_> = (view(&f, &phone).await.history.iter())
        .filter(|e| e.event == AgentLoginEvent::Shown)
        .map(|e| e.by.clone())
        .collect();
    assert_eq!(shown, ["This computer", "Phone"]);
    let missing = f
        .hub
        .query(
            &phone,
            Query::AgentLoginPassword {
                login_id: "0000000000000000".into(),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(missing.code, ErrorCode::NotFound);
    f.hub.shutdown().await.unwrap();
    assert!(!written_anywhere(&f.path, PASSWORD));
    assert!(!written_anywhere(dir.path(), PASSWORD));
    let path = f.path.clone();
    drop(f.hub);
    std::fs::remove_dir_all(path).unwrap();
}

#[tokio::test]
async fn a_chat_never_reaches_agent_logins_or_answers_a_code() {
    let Logins { f, .. } = with_logins().await;
    let id = ChannelIdentityId::new();
    f.hub
        .write(|tx| {
            tx.put(
                "channel",
                id,
                &ChannelIdentity {
                    id,
                    channel: ChannelKind::Telegram,
                    external_user: "1".into(),
                    external_chat: "2".into(),
                    display_name: "Synthetic channel".into(),
                    linked_at: Timestamp(0),
                    revoked_at: None,
                    grants: Grants::owner(),
                    project_id: None,
                },
            )
        })
        .unwrap();
    let chat = Caller {
        principal: Principal::Channel { identity_id: id },
        grants: Grants::owner(),
    };
    let refused = f.hub.query(&chat, Query::AgentLogins).await.unwrap_err();
    assert_eq!(refused.code, ErrorCode::PermissionDenied);
    let reveal = Query::AgentLoginPassword {
        login_id: "0000000000000000".into(),
    };
    let refused = f.hub.query(&chat, reveal).await.unwrap_err();
    assert_eq!(refused.code, ErrorCode::PermissionDenied);
    let refused = done(&f, &chat, login(AgentLoginAction::PauseAll)).await;
    assert_eq!(refused.unwrap().code, ErrorCode::PermissionDenied);
    // A chat may answer approvals, but a code typed there stays in its history.
    let code = AgentLoginAction::AnswerCode {
        approval_id: ApprovalId::new(),
        code: Secret::new("123456"),
    };
    let refused = done(&f, &chat, login(code)).await.unwrap();
    assert_eq!(refused.code, ErrorCode::PermissionDenied);
    assert!(refused.message.contains("not in a chat"), "{refused:?}");
    f.close().await;
}

#[tokio::test]
async fn a_computer_without_agent_logins_says_so_and_does_not_advertise_them() {
    let f = Fixture::new().await;
    assert!(
        !f.hub
            .host_capabilities()
            .await
            .features
            .contains(&"agent_logins_v1".to_string())
    );
    let owner = Caller::local_owner();
    let refused = f.hub.query(&owner, Query::AgentLogins).await.unwrap_err();
    assert_eq!(refused.code, ErrorCode::Unsupported);
    let refused = done(&f, &owner, login(AgentLoginAction::PauseAll)).await;
    assert_eq!(refused.unwrap().code, ErrorCode::Unsupported);
    f.close().await;
}

#[tokio::test]
async fn a_kept_password_is_hidden_in_everything_a_run_says() {
    let Logins { f, logins, .. } = with_logins().await;
    let owner = Caller::local_owner();
    let accept = AgentLoginAction::Accept {
        version: DISCLAIMER_VERSION,
    };
    assert!(done(&f, &owner, login(accept)).await.is_none());
    assert!(done(&f, &owner, login(added())).await.is_none());
    assert!(ports::agent_logins::AgentLogins::secrets(&logins).len() > 1);
    let project = f.project().await;
    let (task, _) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    let run = run_started(&f, task).await;
    let escaped = serde_json::to_string(PASSWORD).unwrap();
    for event in [
        HarnessEvent::AgentMessage {
            message_key: "m".into(),
            text: format!("I read {PASSWORD} from the page."),
        },
        HarnessEvent::Activity {
            key: "page".into(),
            tool: "Bash".into(),
            summary: "curl".into(),
            status: ActivityStatus::Succeeded,
            detail: Some(format!("{{\"password\":{escaped}}}")),
        },
    ] {
        f.harness.emit(run, event).await;
    }
    let timeline = || async {
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
        serde_json::to_string(&items).unwrap()
    };
    let mut more = waiting();
    let mut shown = timeline().await;
    while more() && !shown.contains("\"page\"") {
        tokio::time::sleep(Duration::from_millis(1)).await;
        shown = timeline().await;
    }
    assert!(
        !shown.contains("pässword") && !shown.contains("p\\u00e4ssword"),
        "{shown}"
    );
    assert!(shown.contains(MASK), "{shown}");
    f.close().await;
}

#[tokio::test]
async fn a_found_page_naming_a_kept_password_however_it_writes_it_has_it_hidden() {
    use super::found_previews::{Pages, Servers};
    use crate::preview::{PageAnswer, title};
    let dir = tempfile::tempdir().unwrap();
    let logins = ComputerAgentLogins::new(LoginsConfig::new(
        dir.path().join("accounts"),
        Arc::new(MemoryVault::default()),
        None,
    ));
    let (servers, pages) = (Arc::new(Servers::default()), Arc::new(Pages::default()));
    let f = {
        let (given, servers, pages) = (Arc::new(logins), servers.clone(), pages.clone());
        Fixture::with(move |b| {
            b.agent_logins(given)
                .local_servers(servers)
                .preview_checker(pages)
        })
        .await
    };
    let owner = Caller::local_owner();
    let accept = AgentLoginAction::Accept {
        version: DISCLAIMER_VERSION,
    };
    let kept = |name: &str, password: &str| match added() {
        AgentLoginAction::Add {
            mut details,
            code_key,
            ..
        } => {
            details.name = name.into();
            let password = Some(Secret::new(password));
            login(AgentLoginAction::Add {
                details,
                password,
                code_key,
            })
        }
        _ => unreachable!(),
    };
    assert!(done(&f, &owner, login(accept)).await.is_none());
    assert!(done(&f, &owner, login(added())).await.is_none());
    let spaced = kept("Mail", "synthetic  spaced\u{A0}pässword");
    assert!(done(&f, &owner, spaced).await.is_none());
    assert!(done(&f, &owner, kept("Shop", "ab      cd")).await.is_none());
    assert!(done(&f, &owner, kept("Cut", "abcd    ef")).await.is_none());
    f.project().await;
    let project = ports::library::plain(std::fs::canonicalize(f.project_dir()).unwrap());
    let loopback = std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);
    let serve = |port: u16, written: &str| {
        servers.serve(loopback, port, &project, u64::from(port));
        let named = title(format!("<title>Signed in {written}</title>").as_bytes());
        pages.answer(port, PageAnswer::Page(named));
    };
    // `synthetic "login" pässword` as a page may write it: by name, by
    // number, and by the old names without a `;`, one before letters no
    // name has.
    serve(5173, "synthetic &quot;login&quot; p&auml;ssword");
    serve(5174, "synthetic &#34;login&#x22; p&#228;ssword");
    serve(5175, "synthetic &quotlogin&quot p&aumlssword &notit;");
    // Those with spaces as written, and as a title shows them.
    serve(5176, "synthetic  spaced&nbsp;p&auml;ssword");
    serve(5177, "synthetic\n spaced p&auml;ssword");
    serve(5178, "ab      cd");
    serve(5179, "ab&#9;cd");
    // Cut where its spaces make it shorter than is hidden whole.
    serve(5180, "abcd    e…");
    serve(5181, "abcd&#9;e");
    // And those named before their passwords are kept.
    serve(5182, "later\t held&nbsp;pässword");
    serve(5183, "ef\n gh");
    serve(5184, "ijkl\tm…");
    serve(5185, "ijkl    m");
    // More than are asked at once, so two passes.
    let labels = || async {
        f.hub.find_previews().await.unwrap();
        f.hub.find_previews().await.unwrap();
        let mut previews: Vec<PreviewEndpoint> = f.hub.read(|r| r.list("preview")).unwrap();
        previews.sort_by(|a, b| a.url.cmp(&b.url));
        previews.into_iter().map(|p| p.label).collect::<Vec<_>>()
    };
    let hidden = format!("Signed in {MASK}");
    let mut expected = vec![hidden.clone(); 13];
    expected[2] = format!("{hidden} ¬it;");
    expected[7] = format!("{hidden}…");
    expected[9] = "Signed in later held pässword".into();
    expected[10] = "Signed in ef gh".into();
    expected[11] = "Signed in ijkl m…".into();
    expected[12] = "Signed in ijkl m".into();
    assert_eq!(labels().await, expected);
    // Hidden once, what a title shows has what is held hidden.
    let mut shown = "Signed in synthetic\n spaced pässword".to_string();
    f.hub.found_masking()(&mut shown);
    assert_eq!(shown, hidden);
    let later = kept("Bank", "later  held\u{A0}pässword");
    assert!(done(&f, &owner, later).await.is_none());
    assert!(done(&f, &owner, kept("Post", "ef      gh")).await.is_none());
    assert!(done(&f, &owner, kept("Late", "ijkl    mn")).await.is_none());
    expected[9] = hidden.clone();
    expected[10] = hidden.clone();
    expected[11] = format!("{hidden}…");
    expected[12] = hidden.clone();
    assert_eq!(labels().await, expected);
    // Named before their passwords are kept, which they are once one's
    // program works outside the project, listening still, and the other
    // has stopped.
    serve(5186, "synthetic-password-123");
    serve(5187, "synthetic-password-456");
    expected.push("Signed in synthetic-password-123".into());
    expected.push("Signed in synthetic-password-456".into());
    assert_eq!(labels().await, expected);
    servers.moves(5186, std::path::Path::new("/elsewhere"));
    servers.stop(5187);
    assert_eq!(labels().await, expected);
    let away = kept("Away", "synthetic-password-123");
    assert!(done(&f, &owner, away).await.is_none());
    let gone = kept("Gone", "synthetic-password-456");
    assert!(done(&f, &owner, gone).await.is_none());
    expected[13] = hidden.clone();
    expected[14] = hidden;
    assert_eq!(labels().await, expected);
    f.close().await;
}
