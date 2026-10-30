// SPDX-License-Identifier: Apache-2.0
use protocol::{
    ErrorCode,
    capability::{Availability, HarnessInfo},
    model::{RunState, TaskMode},
};
use serde_json::json;

fn harness(mode_availability: Availability) -> HarnessInfo {
    let available = json!({"state": "available"});
    serde_json::from_value(json!({
        "kind": "codex", "display_name": "Fixture", "native_version": null,
        "integration": "managed", "installed": available,
        "account": {"state": "signed_in", "account_label": null, "plan": null},
        "models": [], "efforts": [],
        "capabilities": {
            "start_run": available, "steer_running": available, "interrupt": available,
            "native_approvals": available, "resume_session": available,
            "child_agents": available, "computer_use_delegation": available,
            "model_selection": available, "effort_selection": available
        },
        "modes": [{"mode": "ask", "availability": mode_availability,
                   "enforcement": "Synthetic read-only policy"}]
    }))
    .unwrap()
}

#[test]
fn missing_read_only_enforcement_and_stale_revisions_are_rejected() {
    let mut info = harness(Availability::Available);
    assert!(domain::task_mode(&info, TaskMode::Ask).is_ok());
    info.modes.clear();
    assert!(domain::task_mode(&info, TaskMode::Work).is_ok());
    for mode in [TaskMode::Ask, TaskMode::Plan] {
        assert_eq!(
            domain::task_mode(&info, mode).unwrap_err().code,
            ErrorCode::Unsupported
        );
    }
    assert!(domain::revision(Some(7), 7).is_ok());
    for revision in [None, Some(6), Some(8)] {
        assert_eq!(
            domain::revision(revision, 7).unwrap_err().code,
            ErrorCode::StaleRevision
        );
    }
    for state in [
        RunState::Admitted,
        RunState::Dispatching,
        RunState::Working,
        RunState::AwaitingApproval,
        RunState::AwaitingInput,
        RunState::Stopping,
        RunState::ReconciliationRequired,
    ] {
        assert_eq!(domain::recover_run(state), RunState::ReconciliationRequired);
    }
    for state in [RunState::Completed, RunState::Failed, RunState::Interrupted] {
        assert_eq!(domain::recover_run(state), state);
    }
}

#[test]
fn temporary_mode_unavailability_keeps_its_actionable_error_code() {
    let info = harness(Availability::TemporarilyUnavailable {
        reason: "Fixture backend is busy; retry later".into(),
    });
    let error = domain::task_mode(&info, TaskMode::Ask).unwrap_err();
    assert_eq!(error.code, ErrorCode::TemporarilyUnavailable);
}

#[test]
fn all_mode_and_start_availability_causes_are_preserved() {
    let message = "Actionable reason";
    for (availability, code) in [
        (
            Availability::TemporarilyUnavailable {
                reason: message.into(),
            },
            ErrorCode::TemporarilyUnavailable,
        ),
        (
            Availability::PermissionMissing {
                remedy: message.into(),
            },
            ErrorCode::OsPermissionMissing,
        ),
        (
            Availability::Incompatible {
                reason: message.into(),
            },
            ErrorCode::IncompatibleVersion,
        ),
        (
            Availability::Unsupported {
                reason: message.into(),
            },
            ErrorCode::Unsupported,
        ),
        (
            Availability::Unknown {
                reason: message.into(),
            },
            ErrorCode::Unsupported,
        ),
        (Availability::Revoked, ErrorCode::PermissionDenied),
    ] {
        for at_start in [false, true] {
            let mut info = harness(Availability::Available);
            if at_start {
                info.capabilities.start_run = availability.clone();
            } else {
                info.modes[0].availability = availability.clone();
            }
            let error = domain::task_mode(&info, TaskMode::Ask).unwrap_err();
            assert_eq!(error.code, code);
            if availability != Availability::Revoked {
                assert_eq!(error.message, message);
            }
        }
    }
}
