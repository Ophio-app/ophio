// SPDX-License-Identifier: Apache-2.0
use super::*;
use rusqlite::{Connection, params};
const OLD_GRANTS: &str =
    include_str!("../../store/tests/fixtures/cacab2d/command-set_device_grants.json");

fn insert_raw(path: &std::path::Path, id: &str, json: &str) {
    Connection::open(path.join("companion.sqlite"))
        .unwrap()
        .execute(
            "INSERT INTO commands(id,json) VALUES(?1,?2)",
            params![id, json],
        )
        .unwrap();
}

#[tokio::test]
async fn old_database_starts_and_preserves_dedupe_without_reapplying_grants() {
    let f = Fixture::new().await;
    let manager = f.device().await;
    let Principal::Device { device_id } = manager.principal else {
        panic!("device")
    };
    f.hub.shutdown().await.unwrap();
    let path = f.path.clone();
    drop(f);
    let mut raw: serde_json::Value = serde_json::from_str(OLD_GRANTS).unwrap();
    raw["envelope"]["body"]["device_id"] = serde_json::to_value(device_id).unwrap();
    let pending: store::StoredCommand = serde_json::from_value(raw.clone()).unwrap();
    insert_raw(
        &path,
        &pending.envelope.command_id.to_string(),
        &raw.to_string(),
    );
    let settled_id = CommandId::new();
    raw["envelope"]["command_id"] = serde_json::to_value(settled_id).unwrap();
    raw["receipt"]["command_id"] = serde_json::to_value(settled_id).unwrap();
    raw["pending"] = false.into();
    raw["stage"] = "observed".into();
    insert_raw(&path, &settled_id.to_string(), &raw.to_string());
    for _ in 0..2 {
        let hub = CompanionHub::builder()
            .data_dir(&path)
            .build()
            .await
            .unwrap();
        let recovered = hub
            .read(|r| Ok(r.command(pending.envelope.command_id)?.unwrap()))
            .unwrap();
        assert!(!recovered.pending);
        assert_eq!(recovered.stage, CommandStage::ReconciliationRequired);
        let settled = hub.read(|r| Ok(r.command(settled_id)?.unwrap())).unwrap();
        assert_eq!(settled.stage, CommandStage::Observed);
        for work in [&recovered, &settled] {
            let receipt = hub
                .submit(&Caller::local_owner(), work.envelope.clone())
                .await;
            assert_eq!(receipt.status, ReceiptStatus::Duplicate);
        }
        let device: crate::identity::Device = hub.read(|r| r.require("device", device_id)).unwrap();
        assert_eq!(
            device.record.grants, manager.grants,
            "legacy commands must never be applied blind"
        );
        hub.shutdown().await.unwrap();
    }
    std::fs::remove_dir_all(path).unwrap();
}

#[tokio::test]
async fn unreadable_commands_are_durably_quarantined_and_never_redispatched() {
    let dir = tempfile::tempdir().unwrap();
    drop(
        store::Store::open(
            dir.path().join("companion.sqlite"),
            store::Retention::default(),
        )
        .unwrap(),
    );
    let legacy: store::StoredCommand = serde_json::from_str(OLD_GRANTS).unwrap();
    insert_raw(
        dir.path(),
        &legacy.envelope.command_id.to_string(),
        OLD_GRANTS,
    );
    let mut bad_variant: serde_json::Value = serde_json::from_str(OLD_GRANTS).unwrap();
    bad_variant["envelope"]["body"]["command"] = "synthetic-secret-token".into();
    let cases = [
        "{truncated".to_owned(),
        bad_variant.to_string(),
        "null".into(),
        OLD_GRANTS.into(),
    ];
    let rows: Vec<_> = cases
        .into_iter()
        .map(|json| (CommandId::new(), json))
        .collect();
    for (id, json) in &rows {
        insert_raw(dir.path(), &id.to_string(), json);
    }
    insert_raw(dir.path(), "invalid-key", "{");
    for attempt in 0..2 {
        let hub = CompanionHub::builder()
            .data_dir(dir.path())
            .build()
            .await
            .unwrap();
        assert_eq!(
            hub.read(|r| Ok(r.command(legacy.envelope.command_id)?.unwrap().stage))
                .unwrap(),
            CommandStage::ReconciliationRequired
        );
        for (id, original) in &rows {
            let mut retry = CommandEnvelope::new(Command::SetAgentUpdates { enabled: false });
            retry.command_id = *id;
            let receipt = hub.submit(&Caller::local_owner(), retry).await;
            assert_eq!(receipt.status, ReceiptStatus::Rejected);
            assert_eq!(receipt.error.unwrap().code, ErrorCode::OutcomeUnknown);
            hub.read(|r| {
                assert!(r.command_quarantined(*id)?);
                let marker: serde_json::Value = r.require("command_quarantine", id)?;
                assert_eq!(marker["stage"], "reconciliation_required");
                assert_eq!(marker["pending"], false);
                assert_eq!(marker["error"]["code"], "outcome_unknown");
                assert!(
                    r.get::<serde_json::Value>("meta", "agent_updates")?
                        .is_none()
                );
                Ok(())
            })
            .unwrap();
            let raw: String = Connection::open(dir.path().join("companion.sqlite"))
                .unwrap()
                .query_row(
                    "SELECT json FROM commands WHERE id=?1",
                    [id.to_string()],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(&raw, original);
        }
        let subscription = hub
            .subscribe(&Caller::local_owner(), Some(0))
            .await
            .unwrap();
        let Backlog::Events(events) = subscription.backlog else {
            panic!("journal backlog")
        };
        for (id, _) in &rows {
            assert_eq!(events.iter().filter(|e| matches!(&e.event,
                Event::CommandProgress { command_id, stage: CommandStage::ReconciliationRequired, error: Some(e), .. }
                if command_id == id && e.code == ErrorCode::OutcomeUnknown)).count(), 1);
        }
        pair(&hub, 10 + attempt * 2).await;
        let limited = pair(&hub, 11 + attempt * 2).await;
        assert!(!limited.grants.admin);
        let subscription = hub.subscribe(&limited, Some(0)).await.unwrap();
        let Backlog::Events(events) = subscription.backlog else {
            panic!("journal backlog")
        };
        assert!(!events.iter().any(|e| matches!(&e.event, Event::CommandProgress { command_id, .. } if rows.iter().any(|(id, _)| id == command_id))));
        hub.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn unreadable_projection_fails_closed_with_a_safe_actionable_cause() {
    let dir = tempfile::tempdir().unwrap();
    let mut db = store::Store::open(
        dir.path().join("companion.sqlite"),
        store::Retention::default(),
    )
    .unwrap();
    db.write(Timestamp(0), |tx| {
        tx.put(
            "approval",
            "synthetic",
            &serde_json::json!({"private": "synthetic-secret"}),
        )
    })
    .unwrap();
    drop(db);
    let error = CompanionHub::builder()
        .data_dir(dir.path())
        .build()
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Internal);
    assert!(
        error.message.contains("missing field `approval_id`"),
        "{error}"
    );
    assert!(!error.message.contains("synthetic-secret"));
}
