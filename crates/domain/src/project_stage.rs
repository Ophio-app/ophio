// SPDX-License-Identifier: Apache-2.0
//! Validation only: stage decisions never invoke task or authority rules.
use protocol::{
    ErrorCode, ProtocolError,
    project::{ProjectStage, ProjectStageTransition, STAGE_NOTE_LIMIT},
};

pub fn validate(
    current: Option<&ProjectStageTransition>,
    expected: Option<u64>,
    target: ProjectStage,
    note: Option<&str>,
) -> Result<(u64, Option<String>), ProtocolError> {
    let revision = current.map_or(0, |t| t.revision);
    if expected != Some(revision) {
        return Err(ProtocolError::new(ErrorCode::StaleRevision, "The project stage changed. Review the current stage before trying again.")
            .with_detail(serde_json::json!({"current_revision": revision, "current_stage": current.map(|t| t.to), "current": current})));
    }
    if current.is_some_and(|t| t.to == target) {
        return Err(ProtocolError::new(
            ErrorCode::Conflict,
            "The project is already in this stage",
        ));
    }
    if revision >= protocol::MAX_SAFE_INTEGER {
        return Err(ProtocolError::new(
            ErrorCode::LimitExceeded,
            "Project stage revision exhausted",
        ));
    }
    if note.is_some_and(|n| {
        n.chars().count() > STAGE_NOTE_LIMIT as usize
            || n.chars().any(|c| c.is_control() && c != '\n' && c != '\t')
    }) {
        return Err(ProtocolError::new(
            ErrorCode::InvalidRequest,
            "The stage note must be at most 500 characters, without control characters",
        ));
    }
    let note = note
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(str::to_owned);
    Ok((revision + 1, note))
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{auth::Principal, time::Timestamp};

    #[test]
    fn revisions_notes_and_all_reverse_transitions_are_checked() {
        assert_eq!(
            validate(None, Some(0), ProjectStage::Planning, Some("  note  ")).unwrap(),
            (1, Some("note".into()))
        );
        assert_eq!(
            validate(None, None, ProjectStage::Planning, None)
                .unwrap_err()
                .code,
            ErrorCode::StaleRevision
        );
        assert!(
            validate(
                None,
                Some(0),
                ProjectStage::Planning,
                Some(&"é".repeat(500))
            )
            .is_ok()
        );
        for invalid in ["x".repeat(501), "hidden\0note".into()] {
            assert_eq!(
                validate(None, Some(0), ProjectStage::Planning, Some(&invalid))
                    .unwrap_err()
                    .code,
                ErrorCode::InvalidRequest
            );
        }
        for from in ProjectStage::ALL {
            let current = ProjectStageTransition {
                revision: 4,
                from: None,
                to: from,
                note: None,
                at: Timestamp(5),
                principal: Principal::LocalOwner,
                actor_name: "Local owner".into(),
            };
            for to in ProjectStage::ALL {
                assert_eq!(
                    validate(Some(&current), Some(4), to, None).is_ok(),
                    from != to
                );
            }
            let stale =
                validate(Some(&current), Some(3), ProjectStage::Planning, None).unwrap_err();
            assert_eq!(
                stale.detail.unwrap()["current_stage"],
                serde_json::to_value(from).unwrap()
            );
            let exhausted = ProjectStageTransition {
                revision: protocol::MAX_SAFE_INTEGER,
                ..current
            };
            let to = if from == ProjectStage::Planning {
                ProjectStage::Design
            } else {
                ProjectStage::Planning
            };
            assert_eq!(
                validate(Some(&exhausted), Some(protocol::MAX_SAFE_INTEGER), to, None)
                    .unwrap_err()
                    .code,
                ErrorCode::LimitExceeded
            );
        }
    }
}
