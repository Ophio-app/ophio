// SPDX-License-Identifier: Apache-2.0
//! Pure admission, queue and input-fencing rules. No I/O or asynchronous work.
pub mod grants;
pub mod lease;
pub mod queue;

use protocol::{ErrorCode, ProtocolError};

/// Check a fixed task intent against the adapter's advertised enforcement.
pub fn task_mode(
    harness: &protocol::capability::HarnessInfo,
    mode: protocol::model::TaskMode,
) -> Result<(), ProtocolError> {
    use protocol::model::TaskMode;
    availability(&harness.capabilities.start_run)?;
    if harness.modes.is_empty() && mode == TaskMode::Work {
        return Ok(());
    }
    let support = harness
        .modes
        .iter()
        .find(|support| support.mode == mode)
        .ok_or_else(|| {
            ProtocolError::new(
                ErrorCode::Unsupported,
                "The harness did not declare enforcement for this task mode",
            )
        })?;
    availability(&support.availability)
}

/// Check an explicit model and effort where the native catalog supplies
/// evidence. Older native tools may still accept their own defaults with no
/// model named.
pub fn model_choice(
    harness: &protocol::capability::HarnessInfo,
    model: Option<&str>,
    effort: Option<&str>,
) -> Result<(), ProtocolError> {
    if harness.catalog.is_none() {
        return Ok(());
    }
    let model = match model {
        Some(id) => harness.models.iter().find(|m| m.id == id),
        None => harness.models.iter().find(|m| m.is_default),
    };
    if effort.is_some_and(|effort| {
        !model
            .and_then(|m| m.details.as_ref())
            .is_some_and(|d| d.efforts.iter().any(|e| e.id == effort))
    }) {
        return Err(ProtocolError::new(
            ErrorCode::InvalidRequest,
            "That effort is not available for the selected model",
        ));
    }
    Ok(())
}

/// Preserve the advertised cause and remedy at admission boundaries.
pub fn availability(value: &protocol::capability::Availability) -> Result<(), ProtocolError> {
    use protocol::capability::Availability;
    let (code, reason) = match value {
        Availability::Available => return Ok(()),
        Availability::TemporarilyUnavailable { reason } => {
            (ErrorCode::TemporarilyUnavailable, reason.as_str())
        }
        Availability::PermissionMissing { remedy } => {
            (ErrorCode::OsPermissionMissing, remedy.as_str())
        }
        Availability::Revoked => (
            ErrorCode::PermissionDenied,
            "The harness permission was revoked; restore permission on the computer",
        ),
        Availability::Incompatible { reason } => (ErrorCode::IncompatibleVersion, reason.as_str()),
        Availability::Unsupported { reason } | Availability::Unknown { reason } => {
            (ErrorCode::Unsupported, reason.as_str())
        }
    };
    Err(ProtocolError::new(code, reason))
}

pub fn revision(expected: Option<u64>, actual: u64) -> Result<(), ProtocolError> {
    if expected != Some(actual) {
        return Err(
            ProtocolError::new(ErrorCode::StaleRevision, "A current revision is required")
                .with_detail(serde_value(actual)),
        );
    }
    Ok(())
}

fn serde_value(actual: u64) -> serde_json::Value {
    serde_json::json!({"current_revision": actual})
}

pub fn recover_run(state: protocol::model::RunState) -> protocol::model::RunState {
    if state.is_terminal() {
        state
    } else {
        protocol::model::RunState::ReconciliationRequired
    }
}

pub mod project_stage;
