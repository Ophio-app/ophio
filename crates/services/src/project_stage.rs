// SPDX-License-Identifier: Apache-2.0
use crate::{Caller, CompanionHub};
use protocol::{
    ProjectId,
    auth::Principal,
    command::CommandResult,
    model::Project,
    project::{ProjectStage, ProjectStageTransition},
};

impl CompanionHub {
    pub(crate) fn set_project_stage(
        &self,
        tx: &mut store::Write<'_>,
        caller: &Caller,
        project_id: ProjectId,
        target: ProjectStage,
        note: Option<&str>,
        expected: Option<u64>,
    ) -> store::Result<CommandResult> {
        let project: Project = tx.read().require("project", project_id)?;
        let (revision, note) =
            domain::project_stage::validate(project.stage.as_ref(), expected, target, note)?;
        let actor_name = match caller.principal {
            Principal::LocalOwner => "Local owner".to_owned(),
            Principal::Device { device_id } => {
                tx.read()
                    .require::<crate::identity::Device>("device", device_id)?
                    .record
                    .name
            }
            // Channel authority is stripped by admission, including admin.
            Principal::Channel { .. } | Principal::System => {
                return Err(protocol::ProtocolError::new(
                    protocol::ErrorCode::PermissionDenied,
                    "Project settings require a paired device or the local owner",
                )
                .into());
            }
        };
        let transition = ProjectStageTransition {
            revision,
            from: project.stage.map(|t| t.to),
            to: target,
            note,
            at: self.inner.clock.now(),
            principal: caller.principal,
            actor_name,
        };
        tx.append_project_stage(project_id, &transition)?;
        Ok(CommandResult::ProjectStageSet {
            project_id,
            transition,
        })
    }
}
