// SPDX-License-Identifier: Apache-2.0
#![cfg(target_os = "linux")]

use ::accounts::{
    logins::{ComputerAgentLogins, LoginsConfig},
    vault::MemoryVault,
};
use companion::{identity::Identity, local::LocalServer, server::ServerDeps};
use ports::agent_logins::{Access, AgentLogins, LoginUser};
use protocol::{
    accounts::Secret,
    agent_logins::{AgentLoginAction, DISCLAIMER_VERSION, LoginDetails, LoginHow, LoginWho},
    *,
};
use services::{CompanionHub, Hub};
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn a_started_child_cannot_expand_a_saved_login_but_the_owner_can() {
    const PASSWORD: &str = "synthetic issue 1 password";
    let dir = tempfile::tempdir().unwrap();
    let accounts = dir.path().join("accounts");
    let logins = Arc::new(ComputerAgentLogins::new(LoginsConfig::new(
        accounts.clone(),
        Arc::new(MemoryVault::default()),
        None,
    )));
    let allowed = LoginUser {
        task_id: TaskId::new(),
        run_id: RunId::new(),
        project_id: ProjectId::new(),
        agent: Some("allowed-agent".into()),
    };
    let outside = LoginUser {
        project_id: ProjectId::new(),
        agent: Some("other-agent".into()),
        ..allowed.clone()
    };
    let details = LoginDetails {
        name: "Synthetic login".into(),
        sites: vec!["original.example".into()],
        username: "owner".into(),
        notes: String::new(),
        who: LoginWho {
            projects: Some(vec![allowed.project_id]),
            agents: Some(vec!["allowed-agent".into()]),
        },
        how: LoginHow::AskFirst,
        notify: true,
    };
    for action in [
        AgentLoginAction::Accept {
            version: DISCLAIMER_VERSION,
        },
        AgentLoginAction::Add {
            details: details.clone(),
            password: Some(Secret::new(PASSWORD)),
            code_key: None,
        },
    ] {
        logins.act(action, "Owner").await.unwrap();
    }
    let before = logins.view().await;
    let login_id = before.logins[0].login_id.clone();
    assert!(!before.logins[0].paused);
    assert!(before.logins[0].has_password);
    assert_eq!(logins.access(&login_id, &allowed), Access::AskFirst);
    assert!(matches!(
        logins.access(&login_id, &outside),
        Access::Refused(_)
    ));
    let saved = std::fs::read(accounts.join("agent-logins.json")).unwrap();

    let hub = CompanionHub::builder()
        .data_dir(dir.path().join("data"))
        .agent_logins(logins.clone())
        .build()
        .await
        .unwrap();
    let identity = Arc::new(Identity::generate(hub.host_id(), vec!["test".into()]).unwrap());
    let deps = ServerDeps::new(Arc::new(hub.clone()), identity).unwrap();
    let path = dir.path().join("run/owner.sock");
    let local = LocalServer::bind(&path, companion::started::Started::claim().unwrap())
        .await
        .unwrap();
    let shutdown = CancellationToken::new();
    let server = tokio::spawn(local.run(deps, shutdown.clone()));

    let mut expanded = details.clone();
    expanded.sites.push("elsewhere.example".into());
    expanded.who = LoginWho::default();
    expanded.how = LoginHow::Whenever;
    let edit = || {
        ClientFrame::Command(Box::new(CommandEnvelope::new(Command::AgentLogin {
            action: AgentLoginAction::Edit {
                login_id: login_id.clone(),
                details: expanded.clone(),
            },
        })))
    };
    // The test process stands in for the companion. Its child uses the real
    // owner socket, just as a program started for a coding task can.
    let mut child = tokio::process::Command::new("python3");
    child.kill_on_drop(true).args([
        "-c",
        "import socket, sys\ns = socket.socket(socket.AF_UNIX)\ns.settimeout(5)\ns.connect(sys.argv[1])\ns.sendall((sys.argv[2] + chr(10)).encode())\nprint(s.makefile().readline(), end='')",
    ]);
    let output = tokio::time::timeout(
        Duration::from_secs(10),
        child
            .arg(&path)
            .arg(serde_json::to_string(&edit()).unwrap())
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let ServerFrame::Receipt(receipt) = serde_json::from_slice(&output.stdout).unwrap() else {
        panic!("expected a command receipt");
    };
    assert_eq!(receipt.status, ReceiptStatus::Rejected, "{receipt:?}");
    assert_eq!(receipt.error.unwrap().code, ErrorCode::PermissionDenied);
    assert_eq!(logins.view().await, before);
    assert_eq!(
        std::fs::read(accounts.join("agent-logins.json")).unwrap(),
        saved
    );
    assert_eq!(logins.access(&login_id, &allowed), Access::AskFirst);
    assert!(logins.credentials(&login_id, &allowed).await.is_err());
    assert!(logins.credentials(&login_id, &outside).await.is_err());

    // The owner's fresh command still reaches the real hub and keeps the
    // original password under the new policy.
    let mut changed = logins.changed();
    let mut owner = companion::local::OwnerClient::connect(&path).await.unwrap();
    let reply: ServerFrame = owner.request(&edit()).await.unwrap();
    assert!(matches!(reply, ServerFrame::Receipt(r) if r.status == ReceiptStatus::Accepted));
    tokio::time::timeout(Duration::from_secs(5), changed.changed())
        .await
        .unwrap()
        .unwrap();
    let after = logins.view().await;
    assert_eq!(after.logins[0].sites, expanded.sites);
    assert_eq!(after.logins[0].who, expanded.who);
    assert_eq!(after.logins[0].how, expanded.how);
    assert_eq!(logins.access(&login_id, &outside), Access::Allowed);
    let credentials = logins.credentials(&login_id, &outside).await.unwrap();
    assert_eq!(credentials.password.as_str(), PASSWORD);
    assert_eq!(credentials.sites, expanded.sites);

    drop(owner);
    shutdown.cancel();
    server.await.unwrap().unwrap();
    hub.shutdown().await.unwrap();
}
