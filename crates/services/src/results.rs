// SPDX-License-Identifier: Apache-2.0
//! Durable evidence attribution. Labels never establish task or drawing identity.
use crate::{CompanionHub, error};
use protocol::{annotation::AnnotationBundle, model::*, *};

/// [`validate_link`] for a link a device gives with what it sends or captures:
/// only the agent's show marks a picture as shown.
pub(crate) fn validate_device_link(
    read: &store::Read<'_>,
    link: &ArtifactLink,
) -> store::Result<()> {
    if link.role == ArtifactRole::Shown {
        return Err(error(
            ErrorCode::InvalidRequest,
            "Only the agent shows a picture or video",
        )
        .into());
    }
    validate_link(read, link)
}

pub(crate) fn validate_link(read: &store::Read<'_>, link: &ArtifactLink) -> store::Result<()> {
    let _: Task = read.require("task", link.task_id)?;
    if let Some(id) = link.run_id {
        let run: Run = read.require("run", id)?;
        if run.task_id != link.task_id {
            return Err(error(
                ErrorCode::InvalidRequest,
                "Evidence run belongs to another task",
            )
            .into());
        }
    }
    if let Some(reference) = &link.annotation {
        if reference.mark_ids.len() > 256 {
            return Err(error(ErrorCode::LimitExceeded, "Too many drawing references").into());
        }
        let recorded = if let Some(run_id) = link.run_id {
            read.get::<AnnotationBundle>(
                &format!("run_annotation:{run_id}"),
                format!("{}:{}", reference.bundle_id, reference.revision),
            )?
        } else {
            read.get::<AnnotationBundle>(
                &format!("task_annotation:{}", link.task_id),
                format!("{}:{}", reference.bundle_id, reference.revision),
            )?
        };
        let items: Vec<TimelineItem> = read.list(&format!("timeline:{}", link.task_id))?;
        let bundle = recorded
            .as_ref()
            .or_else(|| {
                items.iter().rev().find_map(|item| {
                    if link.run_id.is_some() && item.run_id != link.run_id {
                        return None;
                    }
                    match &item.item {
                        TimelineBody::UserMessage {
                            bundle: Some(bundle),
                            ..
                        } if bundle.bundle_id == reference.bundle_id
                            && bundle.revision == reference.revision =>
                        {
                            Some(bundle.as_ref())
                        }
                        _ => None,
                    }
                })
            })
            .ok_or_else(|| {
                error(
                    ErrorCode::InvalidRequest,
                    "Reviewed annotation revision is not recorded for this task and run",
                )
            })?;
        if reference
            .mark_ids
            .iter()
            .any(|id| !bundle.marks.iter().any(|m| m.mark_id == *id && !m.deleted))
        {
            return Err(error(
                ErrorCode::InvalidRequest,
                "Drawing is missing from the reviewed annotation",
            )
            .into());
        }
    }
    if link.role == ArtifactRole::After
        && (link.run_id.is_none()
            || link
                .annotation
                .as_ref()
                .is_some_and(|a| a.mark_ids.is_empty()))
    {
        return Err(error(
            ErrorCode::InvalidRequest,
            "After evidence needs a run and any referenced drawings",
        )
        .into());
    }
    Ok(())
}

pub(crate) fn link_bundle(
    tx: &mut store::Write<'_>,
    task_id: TaskId,
    run_id: Option<RunId>,
    bundle: &AnnotationBundle,
) -> store::Result<()> {
    let scope = format!("task_annotation:{task_id}");
    let key = format!("{}:{}", bundle.bundle_id, bundle.revision);
    let previous: Option<AnnotationBundle> = tx.read().get(&scope, &key)?;
    let history: Vec<TimelineItem> = tx.read().list(&format!("timeline:{task_id}"))?;
    let changed = previous.as_ref().is_some_and(|b| b != bundle) || history.iter().any(|item| {
        matches!(&item.item, TimelineBody::UserMessage { bundle: Some(b), .. }
            if b.bundle_id == bundle.bundle_id && b.revision == bundle.revision && b.as_ref() != bundle)
    });
    if changed {
        return Err(error(
            ErrorCode::Conflict,
            "A reviewed annotation revision cannot be changed",
        )
        .into());
    }
    tx.put(&scope, &key, bundle)?;
    if let Some(run_id) = run_id {
        // Queued work and resumed conversations need an explicit run reference even
        // when the original message predates this run.
        tx.put(
            &format!("run_annotation:{run_id}"),
            format!("{}:{}", bundle.bundle_id, bundle.revision),
            bundle,
        )?;
    }
    for source in &bundle.sources {
        let mut artifact: ArtifactMeta = tx.read().require("artifact", source.artifact_id)?;
        let link = ArtifactLink {
            task_id,
            run_id,
            annotation: Some(ArtifactAnnotation {
                bundle_id: bundle.bundle_id,
                revision: bundle.revision,
                mark_ids: bundle
                    .marks
                    .iter()
                    .filter(|m| m.source_key == source.key)
                    .map(|m| m.mark_id)
                    .collect(),
            }),
            role: ArtifactRole::Source,
        };
        if artifact.links.contains(&link) {
            continue;
        }
        if artifact.links.len() >= 256 {
            return Err(error(
                ErrorCode::LimitExceeded,
                "This artifact has reached its task reference limit",
            )
            .into());
        }
        artifact.links.push(link);
        artifact.task_id = artifact.task_id.or(Some(task_id));
        tx.put("artifact", artifact.artifact_id, &artifact)?;
        tx.emit(Event::ArtifactCreated { artifact })?;
    }
    Ok(())
}

impl CompanionHub {
    pub(crate) fn task_artifacts(
        &self,
        task_id: TaskId,
        before: Option<ArtifactId>,
        limit: u32,
    ) -> Result<TaskArtifactHistory, ProtocolError> {
        self.read(|read| {
            let _: Task = read.require("task", task_id)?;
            let mut artifacts = read.list::<ArtifactMeta>("artifact")?;
            artifacts.retain(|a| {
                before.is_none_or(|id| a.artifact_id < id)
                    && (a.task_id == Some(task_id) || a.links.iter().any(|l| l.task_id == task_id))
            });
            Ok(page(artifacts, limit))
        })
    }
    /// What agents showed in the project's tasks, newest first, however long
    /// ago their tasks ended.
    pub(crate) fn project_shown(
        &self,
        project_id: ProjectId,
        before: Option<ArtifactId>,
        limit: u32,
    ) -> Result<TaskArtifactHistory, ProtocolError> {
        self.read(|read| {
            let _: Project = read.require("project", project_id)?;
            let tasks: std::collections::HashSet<TaskId> = (read.list::<Task>("task")?.into_iter())
                .filter(|t| t.project_id == project_id)
                .map(|t| t.task_id)
                .collect();
            let mut artifacts = read.list::<ArtifactMeta>("artifact")?;
            artifacts.retain(|a| {
                before.is_none_or(|id| a.artifact_id < id)
                    && (a.links.iter())
                        .any(|l| l.role == ArtifactRole::Shown && tasks.contains(&l.task_id))
            });
            Ok(page(artifacts, limit))
        })
    }
    pub(crate) async fn put_linked_artifact(
        &self,
        bytes: &[u8],
        media_type: &str,
        link: Option<ArtifactLink>,
        creator: protocol::auth::Principal,
        capture: Option<Box<protocol::model::CaptureGeometry>>,
    ) -> Result<ArtifactMeta, ProtocolError> {
        if let Some(link) = &link {
            self.read(|read| validate_link(read, link))?;
        }
        self.put_artifact_with_links(bytes, media_type, link, creator, capture)
            .await
    }
}

impl CompanionHub {
    pub(crate) async fn record_repository_revision(
        &self,
        run_id: RunId,
        before: bool,
    ) -> Result<(), ProtocolError> {
        let project: Project = self.read(|read| {
            let run: Run = read.require("run", run_id)?;
            let task: Task = read.require("task", run.task_id)?;
            read.require("project", task.project_id)
        })?;
        let revision = repository_revision(&project.host_path, self.inner.clock.now()).await;
        self.write(|tx| {
            let mut run: Run = tx.read().require("run", run_id)?;
            let revisions = run.revisions.get_or_insert(RunRevisions {
                before: None,
                after: None,
            });
            if before {
                revisions.before = revision;
            } else {
                revisions.after = revision;
            }
            tx.put("run", run_id, &run)?;
            tx.emit(Event::RunUpserted { run: Box::new(run) })?;
            Ok(())
        })
    }
}

async fn git_read(path: &str, args: &[&str]) -> Option<(bool, Vec<u8>)> {
    use tokio::io::AsyncReadExt;
    let mut command = crate::git::read_only(path);
    command.args(args);
    let mut child = command.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let result = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        let mut bytes = Vec::new();
        (&mut stdout)
            .take(4097)
            .read_to_end(&mut bytes)
            .await
            .ok()?;
        if bytes.len() > 4096 {
            return None;
        }
        let status = child.wait().await.ok()?;
        Some((status.success(), bytes))
    })
    .await
    .ok()
    .flatten();
    // Also reap a process whose output was truncated or whose deadline elapsed.
    if result.is_none() {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    result
}
pub(crate) async fn repository_revision(
    path: &str,
    at: protocol::time::Timestamp,
) -> Option<RepositoryRevision> {
    let (success, bytes) = git_read(path, &["rev-parse", "--verify", "HEAD"]).await?;
    let commit = std::str::from_utf8(&bytes).ok()?.trim();
    if !success
        || !matches!(commit.len(), 40 | 64)
        || !commit.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return None;
    }
    let mut dirty = Some(false);
    for args in [&crate::git::CHANGED[..], &crate::git::UNTRACKED] {
        dirty = match (dirty, git_read(path, args).await) {
            (Some(earlier), Some((true, bytes))) => Some(earlier || !bytes.is_empty()),
            _ => None,
        };
    }
    Some(RepositoryRevision {
        commit: commit.into(),
        observed_at: at,
        dirty,
    })
}

/// Reviewed upload attribution uses the same task validation as other evidence.
pub(crate) fn upload_source(
    read: &store::Read<'_>,
    task_id: Option<TaskId>,
    destination: &protocol::transfer::TransferDestination,
) -> store::Result<Option<ArtifactLink>> {
    let Some(task_id) = task_id else {
        return Ok(None);
    };
    if !matches!(
        destination,
        protocol::transfer::TransferDestination::Artifact
    ) {
        return Err(error(
            ErrorCode::InvalidRequest,
            "Task evidence uploads must use artifact storage",
        )
        .into());
    }
    let link = ArtifactLink {
        task_id,
        run_id: None,
        annotation: None,
        role: ArtifactRole::Source,
    };
    validate_link(read, &link)?;
    Ok(Some(link))
}

/// Newest first, at most `limit` (1 to 100), with where the next page starts.
fn page(mut artifacts: Vec<ArtifactMeta>, limit: u32) -> TaskArtifactHistory {
    artifacts.sort_by_key(|a| std::cmp::Reverse(a.artifact_id));
    let limit = limit.clamp(1, 100) as usize;
    let more = artifacts.len() > limit;
    artifacts.truncate(limit);
    let next_before = if more {
        artifacts.last().map(|a| a.artifact_id)
    } else {
        None
    };
    TaskArtifactHistory {
        artifacts,
        next_before,
    }
}
