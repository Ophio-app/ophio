// SPDX-License-Identifier: Apache-2.0
//! What an agent made for the person to look at goes a week after they last
//! looked at it: the pictures and videos it showed, the screenshots it took
//! and the pages it showed. The week counts from the latest of when it was
//! made, when the phone last opened it and when its task finished, and
//! doesn't run while its task is still working or waiting in Next up.
//! What the person kept stays:
//! a picture they drew on or a video they talked over stays with that
//! message, and one they saved to the phone is the phone's. What they sent,
//! the after-evidence a receipt rests on, and a page they added themselves
//! aren't the agent's to expire.

use crate::{CompanionHub, commands::emit_previews, error, scratch::running};
use protocol::{
    ErrorCode, PreviewId, ProtocolError,
    annotation::AnnotationBundle,
    auth::Principal,
    event::Event,
    ids::{ArtifactId, TaskId},
    model::{ArtifactMeta, ArtifactRole, PreviewEndpoint, Task, TaskState},
    queue::QueueView,
    time::Timestamp,
};
use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

/// Kept this long after it was last looked at.
pub(crate) const SHOWN_FOR: Duration = Duration::from_secs(7 * 24 * 60 * 60);

const EXPIRED: &str = "artifact_expired";
/// When the phone last fetched an artifact to show it.
const SEEN: &str = "artifact_seen";
/// When an agent last showed a page, or the person last opened one it
/// showed. A page without one is the person's own, or found running.
const PAGE_SEEN: &str = "preview_seen";

/// The phone fetched it to show the person: what an agent made stays another
/// week.
pub(crate) fn seen(
    tx: &store::Write<'_>,
    artifact_id: ArtifactId,
    now: Timestamp,
) -> store::Result<()> {
    if tx
        .read()
        .get::<ArtifactMeta>("artifact", artifact_id)?
        .is_some()
    {
        tx.put(SEEN, artifact_id, &now)?;
    }
    Ok(())
}

/// An agent showed the page (`shown`), or the person opened it: a page an
/// agent showed stays another week.
pub(crate) fn page_seen(
    tx: &store::Write<'_>,
    preview_id: PreviewId,
    shown: bool,
    now: Timestamp,
) -> store::Result<()> {
    if shown || tx.read().get::<Timestamp>(PAGE_SEEN, preview_id)?.is_some() {
        tx.put(PAGE_SEEN, preview_id, &now)?;
    }
    Ok(())
}

impl CompanionHub {
    /// Forgets the agent's pictures, videos, screenshots and pages nobody
    /// has looked at for [`SHOWN_FOR`]. Their bytes go with the next
    /// `collect_blobs`.
    pub(crate) async fn expire_shown(&self) -> Result<usize, ProtocolError> {
        let now = self.inner.clock.now();
        self.write(|tx| {
            let (gone, pages, kept_since, stale) = {
                let r = tx.read();
                let ended: HashMap<TaskId, Option<i64>> = (r.list::<Task>("task")?.into_iter())
                    .map(|task| {
                        let ended = match task.state {
                            _ if running(&r, &task) => None,
                            TaskState::Completed
                            | TaskState::Failed
                            | TaskState::Interrupted
                            | TaskState::Archived => Some(task.updated_at.millis()),
                            _ => None,
                        };
                        (task.task_id, ended)
                    })
                    .collect();
                let artifacts = r.list::<ArtifactMeta>("artifact")?;
                // Prompts and queues cannot change during this write. Only load
                // their references and pending tasks if a candidate needs them.
                let mut protected = None;
                let mut gone = Vec::new();
                for meta in &artifacts {
                    if expires(&r, meta, &ended, now, &mut protected)? {
                        gone.push(meta.artifact_id);
                    }
                }
                let previews = r.list::<PreviewEndpoint>("preview")?;
                let seen: HashMap<String, Timestamp> = r.entries(PAGE_SEEN)?.into_iter().collect();
                let mut pages = Vec::new();
                let mut kept_since = Vec::new();
                for p in &previews {
                    match seen.get(&p.preview_id.to_string()) {
                        // One found running comes and goes with its server.
                        _ if p.automatic => {}
                        Some(at) if old(*at, now) => pages.push(p.clone()),
                        Some(_) => {}
                        // An agent showed it, with a way to start it again,
                        // before its week was counted: it counts from now.
                        None if p.start.is_some() => kept_since.push(p.preview_id),
                        None => {}
                    }
                }
                // What's gone by other ways, deleted or with its project.
                let known: HashSet<String> = (artifacts.iter().map(|a| a.artifact_id.to_string()))
                    .chain(previews.iter().map(|p| p.preview_id.to_string()))
                    .collect();
                let stale: Vec<(&str, String)> = (r.entries::<Timestamp>(SEEN)?.into_iter())
                    .map(|(id, _)| (SEEN, id))
                    .chain(seen.into_keys().map(|id| (PAGE_SEEN, id)))
                    .filter(|(_, id)| !known.contains(id))
                    .collect();
                (gone, pages, kept_since, stale)
            };
            for &artifact_id in &gone {
                tx.delete("artifact", artifact_id)?;
                tx.delete("artifact_creator", artifact_id)?;
                tx.delete("artifact_provenance", artifact_id)?;
                tx.delete(SEEN, artifact_id)?;
                tx.put(EXPIRED, artifact_id, &now)?;
                tx.emit(Event::ArtifactDeleted { artifact_id })?;
            }
            let projects: HashSet<_> = pages.iter().map(|p| p.project_id).collect();
            for p in &pages {
                tx.delete("preview", p.preview_id)?;
                tx.delete(PAGE_SEEN, p.preview_id)?;
            }
            for project_id in projects {
                emit_previews(tx, project_id)?;
            }
            for preview_id in kept_since {
                tx.put(PAGE_SEEN, preview_id, &now)?;
            }
            for (kind, id) in stale {
                tx.delete(kind, id)?;
            }
            Ok(gone.len() + pages.len())
        })
    }
}

fn old(at: Timestamp, now: Timestamp) -> bool {
    now.millis().saturating_sub(at.millis()) >= SHOWN_FOR.as_millis() as i64
}

/// An agent's picture, video or screenshot, unsent, that nobody has looked
/// at for [`SHOWN_FOR`]: since it was made, since the phone last fetched it,
/// and since its last task finished. Never while one of its tasks works or
/// waits in Next up. A task no longer known ended when the artifact was made.
fn expires(
    r: &store::Read<'_>,
    meta: &ArtifactMeta,
    ended: &HashMap<TaskId, Option<i64>>,
    now: Timestamp,
    protected: &mut Option<Protection>,
) -> store::Result<bool> {
    let agents = !meta.links.is_empty()
        && (meta.links.iter())
            .all(|l| matches!(l.role, ArtifactRole::Shown | ArtifactRole::Observation));
    if !agents
        || r.get::<Principal>("artifact_creator", meta.artifact_id)? != Some(Principal::System)
    {
        return Ok(false);
    }
    let mut tasks = (meta.links.iter().map(|l| l.task_id)).chain(meta.task_id);
    let mut last = meta.created_at;
    if let Some(at) = r.get::<Timestamp>(SEEN, meta.artifact_id)? {
        last = last.max(at);
    }
    for task in tasks.clone() {
        match ended.get(&task) {
            Some(Some(at)) => last = last.max(Timestamp(*at)),
            Some(None) => return Ok(false),
            None => {}
        }
    }
    if !old(last, now) {
        return Ok(false);
    }
    let protected = match protected {
        Some(protected) => protected,
        None => protected.insert(protection(r)?),
    };
    Ok(!protected.sent.contains(&meta.artifact_id)
        && !tasks.any(|task| protected.queued.contains(&task)))
}

struct Protection {
    sent: HashSet<ArtifactId>,
    queued: HashSet<TaskId>,
}

/// Initial prompts and queued requests protect their reviewed sources even
/// without Source links. Source links themselves are excluded by `expires`.
/// Pending continuations protect all their task's artifacts: while waiting
/// in Next up, the task still carries its previous terminal state.
fn protection(r: &store::Read<'_>) -> store::Result<Protection> {
    let mut sent = HashSet::new();
    let mut queued = HashSet::new();
    let mut add = |bundle: Option<Box<AnnotationBundle>>| {
        if let Some(bundle) = bundle {
            sent.extend(bundle.sources.into_iter().map(|source| source.artifact_id));
        }
    };
    for (_, bundle) in r.list::<(String, Option<Box<AnnotationBundle>>)>("prompt")? {
        add(bundle);
    }
    for queue in r.list::<QueueView>("queue")? {
        for item in queue.items {
            if item.is_pending() {
                queued.insert(item.task_id);
            }
            if let Some(request) = item.request {
                add(request.bundle);
            }
        }
    }
    Ok(Protection { sent, queued })
}

/// The artifact, or why it's gone if it expired.
pub(crate) fn require_artifact(
    r: &store::Read<'_>,
    artifact_id: ArtifactId,
) -> store::Result<ArtifactMeta> {
    if let Some(meta) = r.get("artifact", artifact_id)? {
        return Ok(meta);
    }
    if r.get::<Timestamp>(EXPIRED, artifact_id)?.is_some() {
        return Err(error(
            ErrorCode::Expired,
            "Removed from the computer after a week unopened",
        )
        .into());
    }
    r.require("artifact", artifact_id)
}
