// SPDX-License-Identifier: Apache-2.0
//! Release compatibility fixtures, generated with the serializer at cacab2d.
//!
//! Refresh at a release point (keep older fixture directories):
//! 1. `git worktree add --detach /tmp/companion-release-fixtures <release>`.
//! 2. In that TEMPORARY checkout, add a protocol example which exports
//!    `schemars::schema_for!(protocol::Command)` and the document types changed
//!    since the preceding device release (`git diff <old> <release> -- crates/protocol`).
//!    Enumerate every Command schema `oneOf` branch and construct a synthetic
//!    instance: fixed UUIDs/timestamps, dummy strings, no real installation data.
//! 3. Add a temporary store example: deserialize those instances with the RELEASE
//!    Command type, wrap each in that release's StoredCommand (accepted, pending),
//!    and serialize with serde_json::to_string_pretty. Serialize changed document
//!    types and the journal events which embed them the same way. Run the examples
//!    with an isolated CARGO_TARGET_DIR. Never hand-edit an old wire shape to make
//!    it resemble today's serializer. Existing fixtures can seed unchanged variants.
//! 4. Commit only the generated synthetic JSON under fixtures/<release>/; add the
//!    release's exact variant count and document/event types to the test below.
//!    Remove the temporary checkout. Never regenerate historical fixtures using
//!    current types: the old serializer is the compatibility oracle.
//!
//! Changes since cacab2d: SetDeviceGrants.expected was the required-field break;
//! Approval.delivery defaults to None (also embedded in ApprovalUpserted journal
//! entries). ChangeDeviceGrants, ErrorCode::{DeviceGrantsChanged, LastManager},
//! and ApprovalDelivery are new variants/types, so cannot invalidate old data.
//! Grants::apply_delta changes behavior only, not the stored Grants shape.
use protocol::{Command, Event, EventEnvelope, model::Approval};
use std::{collections::BTreeSet, path::Path};
use store::StoredCommand;

#[test]
fn cacab2d_stored_commands_documents_and_journal_still_decode() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cacab2d");
    let mut variants = BTreeSet::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_str().unwrap();
        if !name.starts_with("command-") {
            continue;
        }
        let json = std::fs::read_to_string(&path).unwrap();
        let command: StoredCommand =
            serde_json::from_str(&json).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        let variant = value["envelope"]["body"]["command"].as_str().unwrap();
        assert_eq!(name, format!("command-{variant}.json"));
        assert!(variants.insert(variant.to_owned()));
        assert_eq!(command.envelope.command_id, command.receipt.command_id);
        if let Command::SetDeviceGrants { expected, .. } = command.envelope.body {
            assert!(expected.is_none());
            assert!(value["envelope"]["body"].get("expected").is_none());
        }
    }
    // Of cacab2d's 62 variants, two have since been retired. A stored one
    // no longer decodes and is quarantined like any unreadable command.
    assert_eq!(
        variants.len(),
        60,
        "every cacab2d Command variant still accepted is covered"
    );
    let approval: Approval =
        serde_json::from_str(include_str!("fixtures/cacab2d/approval.json")).unwrap();
    assert!(approval.delivery.is_none());
    let event: EventEnvelope =
        serde_json::from_str(include_str!("fixtures/cacab2d/journal-approval.json")).unwrap();
    let Event::ApprovalUpserted {
        approval: journal_approval,
    } = event.event
    else {
        panic!("approval event")
    };
    assert_eq!(approval, journal_approval);
}

#[test]
fn quarantine_is_atomic_and_reserves_the_original_row() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("companion.sqlite");
    let mut db = store::Store::open(&path, store::Retention::default()).unwrap();
    let old: StoredCommand = serde_json::from_str(include_str!(
        "fixtures/cacab2d/command-set_device_grants.json"
    ))
    .unwrap();
    let id = old.envelope.command_id;
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute(
            "INSERT INTO commands(id,json) VALUES(?1,'{broken')",
            [id.to_string()],
        )
        .unwrap();
    let failed: store::Result<store::Commit<()>> = db.write(protocol::time::Timestamp(0), |tx| {
        tx.quarantine_command(&id.to_string())?;
        Err(store::Error::IntegerExhausted)
    });
    assert!(failed.is_err());
    db.read(|r| {
        assert!(!r.command_quarantined(id)?);
        assert!(matches!(
            r.commands()?.as_slice(),
            [store::CommandRead::Unreadable { .. }]
        ));
        assert!(r.journal(0)?.events.is_empty());
        Ok(())
    })
    .unwrap();
    db.write(protocol::time::Timestamp(0), |tx| {
        tx.quarantine_command(&id.to_string())
    })
    .unwrap();
    assert!(
        matches!(db.write(protocol::time::Timestamp(0), |tx| tx.command(&old)), Err(store::Error::Rule(e)) if e.code == protocol::ErrorCode::OutcomeUnknown)
    );
    db.read(|r| {
        assert!(r.command_quarantined(id)?);
        assert!(r.commands()?.is_empty());
        assert_eq!(r.journal(0)?.events.len(), 1);
        Ok(())
    })
    .unwrap();
}
