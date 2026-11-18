// SPDX-License-Identifier: Apache-2.0
//! Project-bound check tools. Reading uses approved definitions; proposals only
//! replace the authenticated task's draft and never approve anything.
use crate::{CompanionHub, error};
use ports::bridge::{BridgeBinding, ChecksCall};
use protocol::{ErrorCode, ProtocolError, model::*};

pub(crate) fn available(task: &Task, answers: bool) -> bool {
    task.kind == TaskKind::Coding && !answers
}

/// Revalidate inside the same database read or write as the operation. A Stop
/// or credential withdrawal that commits first must prevent a later proposal.
fn authorize(
    hub: &CompanionHub,
    read: &store::Read<'_>,
    binding: &BridgeBinding,
    proposing: bool,
) -> store::Result<(Task, Project)> {
    let token: crate::bridge::TokenRecord = read.require("run_token", binding.run_id)?;
    if token.run_id != binding.run_id || token.epoch != hub.inner.epoch {
        return Err(error(ErrorCode::Expired, "This run's bridge is no longer active").into());
    }
    if crate::runs::stopped(read, binding.run_id)? {
        return Err(error(ErrorCode::Expired, "Run is no longer active").into());
    }
    let run: Run = read.require("run", binding.run_id)?;
    let task: Task = read.require("task", run.task_id)?;
    let answers = crate::checklist::answers(read, task.task_id)?;
    if !binding.checks
        || binding.computer_use
        || binding.checklist_answers
        || !available(&task, answers)
    {
        return Err(error(
            ErrorCode::PermissionDenied,
            "Checks are not available for this run",
        )
        .into());
    }
    if proposing
        && (binding.read_only
            || crate::runs::run_mode(&run, &task) != TaskMode::Work
            || crate::runs::launched_mode(read, &run, &task)? != TaskMode::Work)
    {
        return Err(error(
            ErrorCode::PermissionDenied,
            "This turn only reads; drafting checks needs a Work task",
        )
        .into());
    }
    let project: Project = read.require("project", task.project_id)?;
    Ok((task, project))
}

impl CompanionHub {
    pub(crate) fn checks_request(
        &self,
        binding: &BridgeBinding,
        action: ChecksCall,
    ) -> Result<String, ProtocolError> {
        match action {
            ChecksCall::Read => self.read(|read| {
                let (_, project) = authorize(self, read, binding, false)?;
                let approved = read
                    .project_checks(project.project_id)?
                    .unwrap_or_else(|| store::checks::empty_checks(project.project_id));
                Ok(serde_json::to_string(&approved)?)
            }),
            ChecksCall::Propose {
                base_version,
                items,
            } => self.write(|tx| {
                let (task, project) = authorize(self, &tx.read(), binding, true)?;
                let request = tx.read().get::<String>("checks_request", task.task_id)?;
                let draft = tx.propose_checks(
                    project.project_id,
                    base_version,
                    Some(task.task_id),
                    request,
                    items,
                    std::path::Path::new(&project.host_path),
                )?;
                Ok(serde_json::to_string(&draft)?)
            }),
        }
    }
}
