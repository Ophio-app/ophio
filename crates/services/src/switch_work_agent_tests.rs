// SPDX-License-Identifier: Apache-2.0
// Kept alongside switch_work.rs: no native agent is started in these tests.
use super::*;
use protocol::capability::HarnessKind;

fn run(task_id: TaskId, harness: HarnessKind) -> Run {
    Run {
        chat_handover: None,
        helpers_checked: None,
        human_resolution: None,
        carried_on: None,
        force_stoppable: false,
        cleanup_pending: false,
        nothing_running: false,
        run_id: RunId::new(),
        task_id,
        harness,
        requested_model: None,
        actual_model: None,
        requested_effort: None,
        launch_policy: None,
        actual_effort: None,
        account_label: None,
        native_session: None,
        state: RunState::Completed,
        admitted_at: Timestamp(1),
        dispatched_at: None,
        acknowledged_at: None,
        ended_at: None,
        children: Vec::new(),
        earlier_children: None,
        outcome: None,
        holds_writer: false,
        mode: None,
        revisions: None,
        follow_up: None,
    }
}

fn approval(run: &Run) -> Approval {
    Approval {
        approval_id: ApprovalId::new(),
        run_id: run.run_id,
        task_id: run.task_id,
        project_id: ProjectId::new(),
        native_request_id: "same request".into(),
        kind: ApprovalKind::Command,
        title: "run this command".into(),
        headline: None,
        facts: Vec::new(),
        detail: None,
        detail_lines: Vec::new(),
        questions: Vec::new(),
        raw_request: Some("same bytes".into()),
        scope: "once".into(),
        shortened: false,
        revision: 1,
        state: ApprovalState::Pending,
        access_request: None,
        actions: None,
        delivery: None,
        outcome: None,
        requested_at: Timestamp(1),
        expires_at: None,
        resolved_by: None,
        resolved_at: None,
    }
}

#[test]
fn a_previous_agents_late_allow_does_not_approve_the_new_agents_identical_request() {
    let dir = tempfile::tempdir().unwrap();
    let mut db = store::Store::open(
        dir.path().join("passes.sqlite"),
        store::Retention::default(),
    )
    .unwrap();
    let task = TaskId::new();
    let old = run(task, HarnessKind::ClaudeCode);
    let another_old = run(task, HarnessKind::ClaudeCode);
    let new = run(task, HarnessKind::Codex);
    let asked = approval(&old);
    let new_request = Approval {
        run_id: new.run_id,
        ..asked.clone()
    };
    db.write(Timestamp(2), |tx| {
        for run in [&old, &another_old, &new] {
            tx.put("run", run.run_id, run)?;
        }
        preapprove(tx, &asked, Principal::LocalOwner, true, Timestamp(2))?;
        assert!(!same_agent(&tx.read(), old.run_id, new.run_id)?);
        assert!(same_agent(&tx.read(), old.run_id, another_old.run_id)?);
        assert_eq!(take_preapproved(tx, &new_request, Timestamp(3))?, None);
        assert_eq!(
            passes(&tx.read(), task)?.len(),
            1,
            "old pass is not spent by another agent"
        );
        Ok(())
    })
    .unwrap();
}
