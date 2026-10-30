// SPDX-License-Identifier: Apache-2.0
use super::*;

#[tokio::test]
async fn last_manager_demotion_and_revocation_require_a_successor_but_owner_can_recover() {
    let f = Fixture::new().await;
    let manager = f.device().await;
    let other = pair(&f.hub, 2).await;
    let Principal::Device { device_id } = manager.principal else {
        panic!("device")
    };
    let narrow = Grants {
        admin: false,
        ..manager.grants.clone()
    };
    for command in [
        Command::SetDeviceGrants {
            device_id,
            expected: Some(manager.grants.clone()),
            grants: narrow.clone(),
        },
        Command::ChangeDeviceGrants {
            device_id,
            mask: Grants {
                admin: true,
                ..Default::default()
            },
            allowed: false,
        },
        Command::RevokeDevice { device_id },
    ] {
        let receipt = f.hub.submit(&manager, CommandEnvelope::new(command)).await;
        assert_eq!(receipt.status, ReceiptStatus::Rejected);
        assert_eq!(receipt.error.unwrap().code, ErrorCode::LastManager);
        let stored: crate::identity::Device =
            f.hub.read(|r| r.require("device", device_id)).unwrap();
        assert!(stored.record.grants.admin && stored.record.revoked_at.is_none());
    }
    // The computer's owner may deliberately remove all remote managers.
    let receipt = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::SetDeviceGrants {
                device_id,
                expected: Some(manager.grants.clone()),
                grants: narrow.clone(),
            }),
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted);
    assert!(!other.grants.admin);
    // Recovery is based on absence of an active manager, even with paired devices.
    assert!(pair(&f.hub, 3).await.grants.admin);
    let restored = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::SetDeviceGrants {
                device_id,
                expected: Some(narrow),
                grants: manager.grants,
            }),
        )
        .await;
    assert_eq!(restored.status, ReceiptStatus::Accepted);
    f.close().await;
}

#[tokio::test]
async fn stale_grant_replacement_is_refused_and_deltas_preserve_unrelated_revocations() {
    let f = Fixture::new().await;
    let manager = f.device().await;
    let Principal::Device { device_id } = manager.principal else {
        panic!("device")
    };
    let owner = Caller::local_owner();
    let revoke = CommandEnvelope::new(Command::ChangeDeviceGrants {
        device_id,
        mask: Grants {
            view: true,
            ..Default::default()
        },
        allowed: false,
    });
    let id = revoke.command_id;
    assert_eq!(
        f.hub.submit(&owner, revoke).await.status,
        ReceiptStatus::Accepted
    );
    settle(&f.hub, id).await;
    let stale = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::SetDeviceGrants {
                device_id,
                expected: Some(manager.grants.clone()),
                grants: Grants {
                    terminal: false,
                    ..manager.grants
                },
            }),
        )
        .await;
    assert_eq!(stale.error.unwrap().code, ErrorCode::DeviceGrantsChanged);
    let changed = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::ChangeDeviceGrants {
                device_id,
                mask: Grants {
                    terminal: true,
                    ..Default::default()
                },
                allowed: false,
            }),
        )
        .await;
    assert_eq!(changed.status, ReceiptStatus::Accepted);
    let stored: crate::identity::Device = f.hub.read(|r| r.require("device", device_id)).unwrap();
    assert!(
        !stored.record.grants.view
            && !stored.record.grants.control_input
            && !stored.record.grants.terminal
    );
    assert!(stored.record.grants.tasks && stored.record.grants.admin);
    f.close().await;
}

#[tokio::test]
async fn a_new_device_name_is_held_to_the_rule_its_pairing_name_met() {
    let f = Fixture::new().await;
    let manager = f.device().await;
    let Principal::Device { device_id } = manager.principal else {
        panic!("device")
    };
    let rename = |name: &str| {
        CommandEnvelope::new(Command::RenameDevice {
            device_id,
            name: name.into(),
        })
    };
    // `ophio devices` prints the name in the owner's terminal, where these
    // would hide the device's entry or forge another.
    for name in [
        "Phone\u{1b}[8m",
        "Phone\u{1b}[1A\u{1b}[2K",
        "Phone\n  It may watch the desktop.",
        "Phone\r",
    ] {
        let receipt = f.hub.submit(&manager, rename(name)).await;
        assert_eq!(receipt.status, ReceiptStatus::Rejected, "{name:?}");
        assert_eq!(receipt.error.unwrap().code, ErrorCode::InvalidRequest);
    }
    let stored: crate::identity::Device = f.hub.read(|r| r.require("device", device_id)).unwrap();
    assert!(!stored.record.name.chars().any(char::is_control));
    let receipt = f.hub.submit(&manager, rename("Work phone")).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted);
    let stored: crate::identity::Device = f.hub.read(|r| r.require("device", device_id)).unwrap();
    assert_eq!(stored.record.name, "Work phone");
    f.close().await;
}

#[tokio::test]
async fn simultaneous_manager_demotions_cannot_remove_both_managers() {
    let f = Fixture::new().await;
    let first = f.device().await;
    let mut second = pair(&f.hub, 2).await;
    let Principal::Device { device_id } = second.principal else {
        panic!("device")
    };
    let added = f
        .hub
        .submit(
            &first,
            CommandEnvelope::new(Command::SetDeviceGrants {
                device_id,
                expected: Some(second.grants.clone()),
                grants: domain::grants::manager_defaults(),
            }),
        )
        .await;
    assert_eq!(added.status, ReceiptStatus::Accepted);
    second.grants = domain::grants::manager_defaults();
    let demote = |caller: &Caller| {
        let Principal::Device { device_id } = caller.principal else {
            panic!("device")
        };
        CommandEnvelope::new(Command::SetDeviceGrants {
            device_id,
            expected: Some(caller.grants.clone()),
            grants: Grants {
                admin: false,
                ..caller.grants.clone()
            },
        })
    };
    let (a, b) = tokio::join!(
        f.hub.submit(&first, demote(&first)),
        f.hub.submit(&second, demote(&second))
    );
    let receipts = [a, b];
    assert_eq!(
        receipts
            .iter()
            .filter(|r| r.status == ReceiptStatus::Accepted)
            .count(),
        1
    );
    assert_eq!(
        receipts
            .iter()
            .filter(|r| r
                .error
                .as_ref()
                .is_some_and(|e| e.code == ErrorCode::LastManager))
            .count(),
        1
    );
    f.close().await;
}

#[tokio::test]
async fn legacy_live_grant_replacement_requires_an_app_update() {
    let f = Fixture::new().await;
    let manager = f.device().await;
    let Principal::Device { device_id } = manager.principal else {
        panic!("device")
    };
    for expected in [None, Some(serde_json::Value::Null)] {
        let mut body = serde_json::json!({
            "command": "set_device_grants", "device_id": device_id,
            "grants": Grants::default(),
        });
        if let Some(expected) = expected {
            body["expected"] = expected;
        }
        let command = serde_json::from_value(body).unwrap();
        let receipt = f.hub.submit(&manager, CommandEnvelope::new(command)).await;
        assert_eq!(receipt.status, ReceiptStatus::Rejected);
        let error = receipt.error.unwrap();
        assert_eq!(error.code, ErrorCode::IncompatibleVersion);
        assert!(error.message.contains("Update the app"));
        let device: crate::identity::Device =
            f.hub.read(|r| r.require("device", device_id)).unwrap();
        assert_eq!(device.record.grants, manager.grants);
    }
    f.close().await;
}

#[derive(Default)]
struct RecordedChatProjects(std::sync::Mutex<Vec<(ChannelKind, String, Option<ProjectId>)>>);
#[async_trait::async_trait]
impl crate::api::ChatProjects for RecordedChatProjects {
    async fn set(
        &self,
        channel: ChannelKind,
        chat: &str,
        project: Option<ProjectId>,
    ) -> Result<(), ProtocolError> {
        self.0.lock().unwrap().push((channel, chat.into(), project));
        Ok(())
    }
}

#[tokio::test]
async fn a_linked_chat_is_pointed_at_a_project_only_by_a_manager() {
    let f = Fixture::new().await;
    let manager = f.device().await;
    let member = pair(&f.hub, 2).await;
    let first = f.project().await;
    let second_dir = f.project_dir().join("second");
    std::fs::create_dir_all(&second_dir).unwrap();
    let r = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::AddProject {
                host_path: second_dir.to_string_lossy().into_owned(),
                name: None,
            }),
        )
        .await;
    let Some(CommandResult::ProjectAdded { project_id: second }) = r.result else {
        panic!("project")
    };
    let code = f
        .hub
        .request_channel_link(ChannelLinkRequest {
            channel: ChannelKind::Telegram,
            external_user: "7".into(),
            external_chat: "42".into(),
            display_name: "Sam".into(),
        })
        .await
        .unwrap();
    let r = f
        .hub
        .submit(
            &manager,
            CommandEnvelope::new(Command::ApproveChannelLink { link_code: code }),
        )
        .await;
    assert_eq!(r.status, ReceiptStatus::Accepted, "{r:?}");
    let identity: ChannelIdentity = f.hub.read(|r| r.list("channel")).unwrap().pop().unwrap();
    let choose = |project_id| {
        CommandEnvelope::new(Command::SetChannelProject {
            identity_id: identity.id,
            project_id,
        })
    };
    let settle_error = |envelope: CommandEnvelope, caller: Caller| {
        let hub = &f.hub;
        async move {
            let id = envelope.command_id;
            let r = hub.submit(&caller, envelope).await;
            if r.status != ReceiptStatus::Accepted {
                return r.error;
            }
            settle(hub, id).await;
            hub.read(|r| r.command(id)).unwrap().unwrap().error
        }
    };

    // Without the companion's chat settings there is nothing to change.
    let error = settle_error(choose(Some(first)), manager.clone()).await;
    assert_eq!(error.map(|e| e.code), Some(ErrorCode::Unsupported));
    let settings = Arc::new(RecordedChatProjects::default());
    f.hub.set_chat_project_settings(settings.clone());

    // Choosing where a chat works is the same grant as linking it.
    let error = settle_error(choose(Some(first)), member).await;
    assert_eq!(error.map(|e| e.code), Some(ErrorCode::PermissionDenied));
    let error = settle_error(choose(Some(ProjectId::new())), manager.clone()).await;
    assert_eq!(error.map(|e| e.code), Some(ErrorCode::NotFound));
    assert!(settings.0.lock().unwrap().is_empty());

    // Choose, change and stop, each handed to the chat settings in turn.
    for project in [Some(first), Some(second), None] {
        assert_eq!(settle_error(choose(project), manager.clone()).await, None);
    }
    let chat = |project| (ChannelKind::Telegram, "42".to_owned(), project);
    assert_eq!(
        *settings.0.lock().unwrap(),
        [chat(Some(first)), chat(Some(second)), chat(None)]
    );

    // The listing shows what the companion reports, including CLI changes.
    let mut events = f.hub.subscribe(&manager, None).await.unwrap();
    f.hub
        .set_chat_projects(ChannelKind::Telegram, [("42".into(), second)].into())
        .unwrap();
    let QueryData::Devices { identities, .. } =
        f.hub.query(&manager, Query::Devices).await.unwrap()
    else {
        panic!("devices")
    };
    assert_eq!(identities[0].project_id, Some(second));
    let changed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let crate::api::Outbound::Event(e) = events.live.recv().await.unwrap()
                && let Event::ChannelIdentitiesChanged { identities } = &e.event
            {
                return identities.clone();
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(changed[0].project_id, Some(second));

    // A revoked chat keeps no settings the app can change.
    let r = f
        .hub
        .submit(
            &manager,
            CommandEnvelope::new(Command::RevokeChannelIdentity {
                identity_id: identity.id,
            }),
        )
        .await;
    settle(&f.hub, r.command_id).await;
    let error = settle_error(choose(Some(first)), manager).await;
    assert_eq!(error.map(|e| e.code), Some(ErrorCode::Conflict));
    f.close().await;
}
