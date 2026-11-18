// SPDX-License-Identifier: Apache-2.0
//! Conversation grants are enforced by the host; native tools run within them.
//! A task carried on from a conversation keeps its grant.
use crate::{CompanionHub, error, unsupported};
use ports::{
    HarnessEvent,
    sandbox::{Applied, Blocked, Reach, Sandbox, SandboxSpec},
};
use protocol::{
    access::{AccessBlocked, ConversationAccess},
    event::Event,
    model::*,
    *,
};
use std::{
    borrow::Cow,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

/// Ordinary Work tasks without conversation grants use the native agent's
/// access, rather than Ophio's restricted conversation sandbox. Keep that
/// default shared with standalone checks; their runner narrows it to project
/// writes and allowlisted toolchain reads, never arbitrary host-file reads.
pub(crate) fn launch_access(access: Option<&ConversationAccess>) -> Cow<'_, ConversationAccess> {
    access.map(Cow::Borrowed).unwrap_or_else(|| {
        Cow::Owned(ConversationAccess {
            whole_read: true,
            whole_write: true,
            internet: true,
            local_network: true,
            ..Default::default()
        })
    })
}

impl CompanionHub {
    pub(crate) fn access_lock(&self, task: TaskId) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.inner.access_changes.lock().unwrap();
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(&task).and_then(std::sync::Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(tokio::sync::Mutex::new(()));
        locks.insert(task, Arc::downgrade(&lock));
        lock
    }

    /// Whether `run` is a side turn, a summary say, which takes no writer.
    pub(crate) fn side_turn(&self, run: RunId) -> Result<bool, ProtocolError> {
        self.read(|r| r.get::<TaskState>(crate::runs::SIDE_TURN, run))
            .map(|state| state.is_some())
    }

    pub(crate) fn access_reach(
        &self,
        r: &store::Read<'_>,
        a: &ConversationAccess,
    ) -> Result<Reach, store::Error> {
        let paths = |ids: &[ProjectId]| {
            ids.iter()
                .map(|id| {
                    let project: Project = r.require("project", id)?;
                    Ok(PathBuf::from(project.host_path))
                })
                .collect::<Result<Vec<_>, store::Error>>()
        };
        Ok(Reach {
            read: paths(&a.read)?,
            write: paths(&a.write)?,
            whole_read: a.whole_read || a.whole_write,
            whole_write: a.whole_write,
            internet: a.internet,
            // Unrestricted, nothing holds it in.
            local_network: a.internet && (a.local_network || a.unrestricted()),
        })
    }

    /// The turn's effective grant. A busy project stays read-only for the
    /// whole turn, even if its writer finishes or access changes meanwhile.
    pub(crate) fn conversation_reach(
        &self,
        r: &store::Read<'_>,
        task: &Task,
        run: RunId,
        access: &ConversationAccess,
    ) -> store::Result<Reach> {
        let mut reach = self.access_reach(r, access)?;
        if !access.unrestricted()
            && r.get::<Option<TaskId>>(CHAT_WRITER_HOLD, run)?
                .flatten()
                .is_some()
        {
            let project: Project = r.require("project", task.project_id)?;
            // Whole-computer write with the internet off is still sandboxed.
            // It cannot exclude a subtree, so retain whole-computer read only.
            reach.whole_write = false;
            let mut write = Vec::new();
            for path in reach.write {
                if paths_overlap(&path, Path::new(&project.host_path)) {
                    if !reach.read.contains(&path) {
                        reach.read.push(path);
                    }
                } else {
                    write.push(path);
                }
            }
            reach.write = write;
        }
        Ok(reach)
    }

    pub(crate) fn conversation_sandbox(
        &self,
        task: &Task,
        run: RunId,
        project: &Project,
        scratch: Option<&Path>,
        given: Vec<PathBuf>,
        bridged: bool,
    ) -> Result<Option<Arc<dyn Sandbox>>, ProtocolError> {
        let access = launch_access(task.access.as_ref());
        if access.unrestricted() {
            return Ok(None);
        }
        let backend = self
            .inner
            .sandboxes
            .as_ref()
            .ok_or_else(|| unsupported("Conversation access"))?;
        let (reach, candidates) = self.read(|r| {
            Ok((
                self.conversation_reach(r, task, run, &access)?,
                r.list::<Project>("project")?
                    .into_iter()
                    .map(|p| PathBuf::from(p.host_path))
                    .filter(|p| p.is_dir())
                    .collect(),
            ))
        })?;
        // A turn with Ophio's tools reaches them through the bridge's socket,
        // with its program, wherever the companion's is installed.
        let bridge = self.inner.bridge.as_ref().filter(|_| bridged);
        let mut always: Vec<PathBuf> = scratch.into_iter().map(Path::to_owned).collect();
        always.extend(bridge.map(|b| PathBuf::from(&b.socket)));
        let weak = Arc::downgrade(&self.inner);
        let sandbox = backend
            .create(SandboxSpec {
                offline: false,
                relay: None,
                reach,
                harness: task.harness,
                cwd: PathBuf::from(&project.host_path),
                state: self
                    .inner
                    .data
                    .join("sandboxes")
                    .join(task.task_id.to_string()),
                always,
                programs: bridge.map(|b| b.executable.clone()).into_iter().collect(),
                candidates,
                private: self.inner.private.clone(),
                given,
                on_blocked: Some(Arc::new(move |blocked| {
                    if let Some(inner) = weak.upgrade() {
                        let hub = CompanionHub { inner };
                        let (host, local) = match blocked {
                            Blocked::Host(host) => (host, false),
                            Blocked::Local(host) => (host, true),
                        };
                        let _ = hub.access_blocked(run, None, Some(host), local, false);
                    }
                })),
            })
            .map_err(|e| error(ErrorCode::Unsupported, e.to_string()))?;
        Ok(Some(sandbox))
    }

    pub(crate) async fn apply_conversation_access(
        &self,
        task_id: TaskId,
        command: CommandId,
    ) -> Result<(), ProtocolError> {
        self.apply_access(task_id, command, None).await.map(|_| ())
    }

    /// Applies `task_id`'s access to its running turn: within the turn when
    /// its sandbox can change as it runs, otherwise by ending the turn and
    /// carrying on in a fresh one of the same session. `allowed`: the
    /// access request the person allowed, and what the fresh turn tells the
    /// agent; it is settled as allowed before that turn ends.
    pub(crate) async fn apply_access(
        &self,
        task_id: TaskId,
        command: CommandId,
        allowed: Option<(&crate::api::Caller, ApprovalId, String)>,
    ) -> Result<AccessApplied, ProtocolError> {
        let lock = self.access_lock(task_id);
        let guard = lock.lock().await;
        let task: Task = self.read(|r| r.require("task", task_id))?;
        let Some(run_id) = task.current_run else {
            return Ok(AccessApplied::Live);
        };
        let control = self.inner.runs.lock().await.get(&run_id).cloned();
        let Some(control) = control else {
            return Ok(AccessApplied::Live);
        };
        let sandbox = self
            .inner
            .sandbox_runs
            .lock()
            .unwrap()
            .get(&run_id)
            .cloned();
        let side = self.side_turn(run_id)?;
        let access = held_to(task.access.as_ref(), side)
            .ok_or_else(|| error(ErrorCode::InvalidRequest, "Missing conversation access"))?;
        let (reach, needs_writer, was_unrestricted) = self.read(|r| {
            let run: Run = r.require("run", run_id)?;
            let blocked = r
                .get::<Option<TaskId>>(CHAT_WRITER_HOLD, run_id)?
                .flatten()
                .is_some();
            let was_unrestricted = r
                .get::<Option<ConversationAccess>>("run_access", run_id)?
                .flatten()
                .is_some_and(|a| a.unrestricted());
            Ok((
                self.conversation_reach(r, &task, run_id, &access)?,
                !side
                    && !run.holds_writer
                    && conversation_writes_project(r, &task)?
                    && (!blocked || (access.unrestricted() && !was_unrestricted)),
                was_unrestricted,
            ))
        })?;
        let result = if needs_writer {
            // Never grant live write access without reserving the project.
            // The next turn either claims it or snapshots its current holder.
            Applied::NeedsRestart(
                "The conversation needs to check who may change this project.".into(),
            )
        } else {
            match sandbox {
                Some(sandbox) => sandbox.update(reach).await,
                None if access.unrestricted() && was_unrestricted => Applied::Applied,
                None => Applied::NeedsRestart("The conversation now needs a sandbox.".into()),
            }
        };
        let Applied::NeedsRestart(reason) = result else {
            return Ok(AccessApplied::Live);
        };
        // Terminate, not SIGSTOP or a native interrupt: open file descriptors
        // must close before the smaller grant can be considered enforced.
        self.write(|tx| {
            if let Some((caller, id, _)) = &allowed {
                crate::switch_work::settle_allowed(
                    tx,
                    *id,
                    caller.principal,
                    self.inner.clock.now(),
                )?;
            }
            tx.put("stopped", run_id, &true)
        })?;
        control
            .terminate()
            .await
            .map_err(|e| error(ErrorCode::NativeFailure, e.to_string()))?;
        tokio::time::timeout(Duration::from_secs(30), async {
            while self.inner.runs.lock().await.contains_key(&run_id) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| {
            error(
                ErrorCode::OutcomeUnknown,
                "Waiting for the previous sandbox to end",
            )
        })?;
        if let Some((_, id, _)) = &allowed {
            // The turn that waited on it has ended; nothing answers it there.
            self.inner.waits.lock().await.remove(id);
        }
        let previous: Run = self.read(|r| r.require("run", run_id))?;
        if previous.state == RunState::Completed {
            return Ok(AccessApplied::Restarted);
        }
        tracing::debug!(%reason, "conversation access needs a new turn");
        // The turn hears the new access ahead of this (see [`note`]).
        let text = match allowed {
            Some((_, _, text)) => text,
            None => "Ophio stopped your previous turn to apply this. Commands you started in it have ended: check what finished before starting any again, then carry on with the same request.".to_owned(),
        };
        let next = self.write(|tx| {
            let mut task: Task = tx.read().require("task", task_id)?;
            if task.current_run != Some(run_id) { return Ok(None); }
            let previous: Run = tx.read().require("run", run_id)?;
            if previous.native_session.is_none() {
                return Err(error(ErrorCode::OutcomeUnknown,"The agent ended before reporting a native session; send the request again").into());
            }
            let next = self.new_run(tx, &mut task, command, Some(text.clone()), None, side)?;
            // The same request carries on: the turn is the person's if the one it replaces was.
            if crate::proposals::is_person_turn(&tx.read(), run_id)? {
                crate::proposals::person_turn(tx, next)?;
            }
            tx.put("task", task_id, &task)?;
            tx.emit(Event::TaskUpserted { task })?;
            tx.emit(Event::TimelineAppended { task_id, item: TimelineItem { seq:0, at:self.inner.clock.now(), run_id:Some(next),
                item:TimelineBody::Note { text:"Access changed. Continuing the same agent session with the new permissions.".into() } } })?;
            Ok(Some(next))
        })?;
        drop(guard);
        if let Some(next) = next {
            self.dispatch_task(task_id, next).await?;
        }
        Ok(AccessApplied::Restarted)
    }

    /// What a conversation's turn tells its agent about its access, put
    /// ahead of the person's words, and the access to record as told once
    /// the turn starts; see [`note`].
    pub(crate) fn access_note(
        &self,
        task: &Task,
        resumes: bool,
        reach: &Reach,
        scratch: bool,
    ) -> Result<Option<(String, ConversationAccess)>, ProtocolError> {
        let Some(access) = task.access.as_ref() else {
            return Ok(None);
        };
        let told: Option<ConversationAccess> = match resumes {
            true => self.read(|r| r.get(TOLD, task.task_id))?,
            false => None,
        };
        let project: Project = self.read(|r| r.require("project", task.project_id))?;
        let home = std::env::var_os("HOME").and_then(|home| std::fs::canonicalize(home).ok());
        let at = Folders {
            cwd: Path::new(&project.host_path),
            home: home.as_deref(),
            scratch,
        };
        let what = match task.conversation {
            true => "conversation",
            false => "task",
        };
        Ok(note(told.as_ref(), access, reach, &at, what).map(|text| (text, access.clone())))
    }

    /// A writer hold is scheduling, not a change to the person's access.
    /// Record the returned hold only after the agent has heard this note.
    pub(crate) fn writer_note(
        &self,
        task: &Task,
        run: RunId,
        resumes: bool,
    ) -> Result<Option<(String, Option<TaskId>)>, ProtocolError> {
        let (hold, told, holder) = self.read(|r| {
            let hold = r.get::<Option<TaskId>>(CHAT_WRITER_HOLD, run)?.flatten();
            let told = if resumes {
                r.get::<Option<TaskId>>(WRITER_TOLD, task.task_id)?
                    .flatten()
            } else {
                None
            };
            let holder = hold
                .map(|id| r.get::<Task>("task", id))
                .transpose()?
                .flatten();
            Ok((hold, told, holder))
        })?;
        if hold == told {
            return Ok(None);
        }
        let text = match hold {
            Some(_) => {
                let title = holder.as_ref().map(|t| t.title.as_str()).unwrap_or("another task");
                let mut text = format!(
                    "Ophio: “{title}” is changing this project’s files. Read them, but do not change them this turn. If the person asks for changes to this project, suggest them as work using Ophio’s tools, or tell the person you can make them in a later reply once “{title}” finishes. This temporary pause does not change the person’s access choices."
                );
                if let Some(access) = &task.access {
                    if access.unrestricted() {
                        text.push_str(" With unrestricted access, the computer cannot enforce this instruction.");
                    } else if access.whole_write {
                        text.push_str(" For this reply, whole-computer access is read-only. Only separately allowed folders that do not overlap this project, and your scratch folder if available, remain writable.");
                    }
                }
                text
            }
            None if self.read(|r| Ok(r.require::<Run>("run", run)?.holds_writer))? =>
                "Ophio: the pause on changing this project has ended. This reply may change this project’s files within the person’s existing access permissions.".into(),
            None => "Ophio: the pause on changing this project has ended. Follow this reply’s access permissions; this does not grant any additional access.".into(),
        };
        Ok(Some((text, hold)))
    }

    fn access_blocked(
        &self,
        run_id: RunId,
        path: Option<String>,
        host: Option<String>,
        local: bool,
        write: bool,
    ) -> Result<(), ProtocolError> {
        self.write(|tx| {
            let run: Run = tx.read().require("run", run_id)?;
            let mut task: Task = tx.read().require("task", run.task_id)?;
            if task.access.is_none() || task.current_run != Some(run_id) {
                return Ok(());
            }
            #[cfg(windows)]
            let project_id = path.as_deref().and_then(|path| {
                let path = ports::library::comparison(Path::new(path)).ok()?;
                tx.read()
                    .list::<Project>("project")
                    .ok()?
                    .into_iter()
                    .filter_map(|project| {
                        let root =
                            ports::library::comparison(Path::new(&project.host_path)).ok()?;
                        path.starts_with(&root)
                            .then_some((root.components().count(), project.project_id))
                    })
                    .max_by_key(|(depth, _)| *depth)
                    .map(|(_, id)| id)
            });
            #[cfg(not(windows))]
            let project_id = path.as_deref().and_then(|path| {
                tx.read()
                    .list::<Project>("project")
                    .ok()
                    .and_then(|projects| {
                        projects
                            .into_iter()
                            .filter(|p| {
                                lexical(Path::new(path))
                                    .starts_with(lexical(Path::new(&p.host_path)))
                            })
                            .max_by_key(|p| p.host_path.len())
                            .map(|p| p.project_id)
                    })
            });
            task.access_blocked = Some(Box::new(AccessBlocked {
                path,
                host,
                local,
                write,
                project_id,
                at: self.inner.clock.now(),
            }));
            task.updated_at = self.inner.clock.now();
            task.revision += 1;
            tx.put("task", task.task_id, &task)?;
            tx.emit(Event::TaskUpserted { task })?;
            Ok(())
        })
    }

    pub(crate) fn conversation_block_event(
        &self,
        run_id: RunId,
        event: &HarnessEvent,
    ) -> Result<(), ProtocolError> {
        if !self
            .inner
            .sandbox_runs
            .lock()
            .unwrap()
            .contains_key(&run_id)
        {
            return Ok(());
        }
        if let HarnessEvent::Activity {
            detail: Some(detail),
            ..
        } = event
            && let Some(path) = blocked_result(detail)
        {
            self.access_blocked(run_id, Some(path), None, false, true)?;
        }
        Ok(())
    }
}

/// How a change of access reached a running turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AccessApplied {
    /// Within the turn, or there was no turn to change.
    Live,
    /// The turn ended, and a fresh one of the same session carries on.
    Restarted,
}

/// The access a conversation's agent was last told, by task: what its
/// native session has heard.
pub(crate) const TOLD: &str = "access_told";

/// Per-run snapshot: another task owns this conversation's project writer.
pub(crate) const CHAT_WRITER_HOLD: &str = "chat_writer_hold";
/// The writer hold the native session last heard, keyed by task.
pub(crate) const WRITER_TOLD: &str = "chat_writer_told";

/// Whether this conversation's grant can change anything inside its project.
/// A parent or child project's write grant overlaps just as its own does.
pub(crate) fn conversation_writes_project(r: &store::Read<'_>, task: &Task) -> store::Result<bool> {
    let Some(access) = task.access.as_ref().filter(|_| task.conversation) else {
        return Ok(false);
    };
    if access.whole_write {
        return Ok(true);
    }
    let project: Project = r.require("project", task.project_id)?;
    for id in &access.write {
        let writable: Project = r.require("project", id)?;
        if paths_overlap(
            Path::new(&project.host_path),
            Path::new(&writable.host_path),
        ) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn paths_overlap(a: &Path, b: &Path) -> bool {
    #[cfg(windows)]
    {
        match (ports::library::comparison(a), ports::library::comparison(b)) {
            (Ok(a), Ok(b)) => a.starts_with(&b) || b.starts_with(&a),
            // This predicate controls writer exclusion. Unverified paths must
            // not leave a potentially overlapping grant writable.
            _ => true,
        }
    }
    #[cfg(not(windows))]
    {
        let a = std::fs::canonicalize(a).unwrap_or_else(|_| lexical(a));
        let b = std::fs::canonicalize(b).unwrap_or_else(|_| lexical(b));
        a.starts_with(&b) || b.starts_with(&a)
    }
}

/// What `task`'s runs are held to now, if anything: an unrestricted
/// conversation reaches what the computer does.
pub(crate) fn held(task: &Task) -> Option<&ConversationAccess> {
    task.access.as_ref().filter(|a| !a.unrestricted())
}

/// What a run is held to: `access`, or for a side turn, which takes no
/// writer, only what `access` reads.
pub(crate) fn held_to(
    access: Option<&ConversationAccess>,
    side: bool,
) -> Option<ConversationAccess> {
    access.map(|a| match side {
        true => ConversationAccess {
            write: vec![],
            whole_write: false,
            ..a.clone()
        },
        false => a.clone(),
    })
}

/// What a conversation's agent hears about its access ahead of the
/// person's words, about `what` it is: a conversation, or the task one was
/// carried on as. A session that hasn't heard it gets all of it: what it
/// may reach, that the computer enforces it, and that a refusal is the
/// person's choice to ask about rather than work around. A session that
/// has gets what it may reach now, and only when that changed: what it
/// heard stays in its session, so saying it every turn would only spend
/// the person's tokens. Nothing for a session told nothing yet that may
/// reach everything, as there's nothing to hold it to.
fn note(
    told: Option<&ConversationAccess>,
    now: &ConversationAccess,
    reach: &Reach,
    at: &Folders<'_>,
    what: &str,
) -> Option<String> {
    let reaches = reached(reach, at);
    match told {
        Some(told) if told.clone().normalized() == now.clone().normalized() => None,
        Some(_) => Some(format!(
            "Ophio: the person changed this {what}'s access. {reaches}"
        )),
        None if now.unrestricted() => None,
        None => Some(format!(
            "Ophio holds this {what} to what the person allows in its access bar, and the computer enforces it. {reaches} They can change it at any time, even during your turn; your next turn starts with what changed. Something refused, like a \"Read-only file system\" error, a folder that looks empty or a 403 from the proxy, is their choice, not a fault: don't work around it with another path, tool or copy. Ask for what you need with request_access, saying why: they allow it from their phone in one tap. If they decline, finish what you can, then say what's left."
        )),
    }
}

/// Where a conversation's agent works: its project's folder, the home
/// folder, and whether it has a scratch folder.
struct Folders<'a> {
    cwd: &'a Path,
    home: Option<&'a Path>,
    scratch: bool,
}

/// What `reach` lets the agent read, change and connect to. The project's
/// folder is always readable, and the scratch folder always writable.
/// Ophio's own data and the other agent's settings stay covered even when
/// the home folder is shown, so an agent that may read it hears that
/// rather than taking the empty folders for the person's choice.
fn reached(reach: &Reach, at: &Folders<'_>) -> String {
    let Folders { cwd, home, scratch } = *at;
    let shown = |path: &Path| {
        reach.whole_read
            || (std::iter::once(cwd)
                .chain(reach.read.iter().chain(&reach.write).map(PathBuf::as_path)))
            .any(|r| path.starts_with(r))
    };
    let home_shown = home.is_some_and(shown);
    let folders = |paths: &[&Path]| {
        let quoted: Vec<_> = paths.iter().map(|p| format!("`{}`", p.display())).collect();
        quoted.join(", ")
    };
    let mut text = String::new();
    if reach.whole_write {
        text.push_str("You may read and change anything on this computer.");
    } else {
        let write: Vec<_> = reach.write.iter().map(PathBuf::as_path).collect();
        text.push_str(&match (write.is_empty(), scratch) {
            (true, true) => "You may change nothing but your scratch folder.".to_owned(),
            (true, false) => "You may change nothing.".to_owned(),
            (false, _) => format!("You may change {}.", folders(&write)),
        });
        if reach.whole_read {
            text.push_str(" You may read anything else.");
        } else {
            let mut read: Vec<_> = std::iter::once(cwd)
                .chain(reach.read.iter().map(PathBuf::as_path))
                .filter(|p| !write.iter().any(|w| p.starts_with(w)))
                .collect();
            read.sort();
            read.dedup();
            if !read.is_empty() {
                text.push_str(&format!(" You may read {}.", folders(&read)));
            }
            text.push_str(match home_shown {
                true => " The rest of the computer looks empty or missing to you.",
                false => " The rest of the computer, your home folder included, looks empty or missing to you.",
            });
        }
    }
    if home_shown {
        text.push_str(" Ophio's own data and other agents' settings and history stay hidden whatever they allow.");
    }
    text.push_str(match (reach.internet, reach.local_network) {
        (true, true) => " You may reach the internet and the person's own network, such as their LAN or tailnet.",
        (true, false) => " You may reach the internet. The person's own network, such as their LAN or tailnet, stays off unless they allow it.",
        (false, _) => " The internet is off: only your own provider answers.",
    });
    text
}

#[cfg(not(windows))]
fn lexical(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            part => normalized.push(part.as_os_str()),
        }
    }
    normalized
}

fn blocked_result(detail: &str) -> Option<String> {
    match serde_json::from_str::<serde_json::Value>(detail) {
        Ok(serde_json::Value::String(text)) => blocked_write(&text),
        Ok(serde_json::Value::Array(blocks)) => blocks
            .iter()
            .rev()
            .find_map(|b| b["text"].as_str().and_then(blocked_write)),
        _ => blocked_write(detail),
    }
}

/// A tool result is evidence of a denial, not authority to change a grant.
fn blocked_write(detail: &str) -> Option<String> {
    for line in detail.lines().rev() {
        let lower = line.to_ascii_lowercase();
        if !(lower.contains("read-only file system") || lower.contains("erofs")) {
            continue;
        }
        let path = if let Some((_, path)) = line.split_once("Read-only file system: '") {
            path.split('\'').next()?
        } else if let Some((prefix, _)) = line.split_once(": Read-only file system") {
            prefix.rsplit(": ").next()?.trim_matches(['\'', '"'])
        } else {
            let start = line.find('/')?;
            line[start..].split(['\'', '"', ':']).next()?
        };
        if Path::new(path).is_absolute() {
            return Some(path.into());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn windows_overlap_resolves_missing_children_and_extended_aliases() {
        let temp = tempfile::tempdir().unwrap();
        let ordinary = ports::library::canonical(temp.path()).unwrap();
        let extended = std::fs::canonicalize(temp.path()).unwrap();
        let missing = ordinary.join("not-created-yet").join("child");
        assert!(!missing.exists());
        assert!(paths_overlap(&extended, &missing));
        assert!(paths_overlap(&missing, &extended));
        assert!(!paths_overlap(
            &ordinary.join("first-missing"),
            &extended.join("different-missing"),
        ));
        let compared = ports::library::comparison(&missing).unwrap();
        assert_eq!(compared, extended.join("not-created-yet").join("child"));
        assert!(paths_overlap(Path::new("\0"), &ordinary));
    }

    #[cfg(windows)]
    #[test]
    fn windows_comparison_refuses_a_dangling_junction_instead_of_trusting_its_name() {
        use std::os::windows::process::CommandExt;
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target");
        let link = temp.path().join("link");
        std::fs::create_dir(&target).unwrap();
        let system = PathBuf::from(std::env::var_os("SystemRoot").unwrap());
        // cmd's built-in mklink has legacy path parsing. Keep its arguments
        // relative to our owned fixture instead of passing extended paths.
        let fixture = ports::library::canonical(temp.path()).unwrap();
        let output = std::process::Command::new(system.join("System32").join("cmd.exe"))
            .current_dir(&fixture)
            .raw_arg("/D /C mklink /J link target")
            .creation_flags(0x08000000)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{output:?}; fixture={fixture:?}; link={link:?}; target={target:?}"
        );
        std::fs::remove_dir(&target).unwrap();
        assert!(ports::library::comparison(&link.join("missing")).is_err());
        std::fs::remove_dir(&link).unwrap();
    }
    #[test]
    fn finds_actual_denied_paths_in_shell_and_python_results() {
        // A rooted Unix path has no drive on Windows, so it is not absolute.
        let root = if cfg!(windows) { r"C:\repo" } else { "/repo" };
        let out = Path::new(root).join("out").to_string_lossy().into_owned();
        let spaced = Path::new(root).join("a b").to_string_lossy().into_owned();
        assert_eq!(
            blocked_write(&format!("/bin/sh: line 1: {out}: Read-only file system")),
            Some(out.clone())
        );
        assert_eq!(
            blocked_write(&format!(
                "OSError: [Errno 30] Read-only file system: '{spaced}'"
            )),
            Some(spaced)
        );
        assert_eq!(blocked_write(&format!("wrote {out}")), None);
        assert_eq!(blocked_write("relative/out: Read-only file system"), None);
    }

    fn reach(access: &ConversationAccess, write: &[&str]) -> Reach {
        Reach {
            read: Vec::new(),
            write: write.iter().map(PathBuf::from).collect(),
            whole_read: access.whole_read || access.whole_write,
            whole_write: access.whole_write,
            internet: access.internet,
            local_network: access.internet && (access.local_network || access.unrestricted()),
        }
    }

    fn at(cwd: &str, scratch: bool) -> Folders<'_> {
        Folders {
            cwd: Path::new(cwd),
            home: Some(Path::new("/home/sam")),
            scratch,
        }
    }

    #[test]
    fn a_session_hears_its_access_once_then_only_what_changed() {
        let app = at("/home/sam/app", true);
        let default = ConversationAccess::default();
        let first = note(None, &default, &reach(&default, &[]), &app, "conversation").unwrap();
        assert!(first.starts_with("Ophio holds this conversation"));
        assert!(first.contains("You may change nothing but your scratch folder. You may read `/home/sam/app`. The rest of the computer, your home folder included, looks empty or missing to you. You may reach the internet."));
        assert!(first.contains("don't work around it"));
        // Unchanged: nothing, however the lists are ordered.
        assert_eq!(
            note(
                Some(&default),
                &default,
                &reach(&default, &[]),
                &app,
                "conversation"
            ),
            None
        );
        let offline = ConversationAccess {
            internet: false,
            ..ConversationAccess::default()
        };
        assert_eq!(
            note(
                Some(&default),
                &offline,
                &reach(&offline, &[]),
                &at("/home/sam/app", false),
                "conversation"
            )
            .unwrap(),
            "Ophio: the person changed this conversation's access. You may change nothing. You may read `/home/sam/app`. The rest of the computer, your home folder included, looks empty or missing to you. The internet is off: only your own provider answers."
        );
        let everything = ConversationAccess {
            whole_read: true,
            whole_write: true,
            ..ConversationAccess::default()
        };
        // Nothing to hold a new session to, so nothing to say.
        assert_eq!(
            note(
                None,
                &everything,
                &reach(&everything, &[]),
                &app,
                "conversation"
            ),
            None
        );
        assert_eq!(
            note(
                Some(&offline),
                &everything,
                &reach(&everything, &[]),
                &app,
                "conversation"
            )
            .unwrap(),
            "Ophio: the person changed this conversation's access. You may read and change anything on this computer. Ophio's own data and other agents' settings and history stay hidden whatever they allow. You may reach the internet and the person's own network, such as their LAN or tailnet."
        );
    }

    #[test]
    fn folders_it_may_change_are_not_listed_again_as_read_only() {
        let access = ConversationAccess {
            whole_read: true,
            ..ConversationAccess::default()
        };
        let text = reached(
            &reach(&access, &["/home/sam/app"]),
            &at("/home/sam/app", true),
        );
        assert_eq!(
            text,
            "You may change `/home/sam/app`. You may read anything else. Ophio's own data and other agents' settings and history stay hidden whatever they allow. You may reach the internet. The person's own network, such as their LAN or tailnet, stays off unless they allow it."
        );
        let text = reached(
            &Reach {
                read: vec!["/home/sam/lib".into(), "/home/sam/app/docs".into()],
                ..reach(&ConversationAccess::default(), &["/home/sam/app"])
            },
            &at("/home/sam/app", true),
        );
        assert!(
            text.starts_with("You may change `/home/sam/app`. You may read `/home/sam/lib`. The rest of the computer, your home folder included,"),
            "{text}"
        );
    }

    /// A conversation in the home folder's own project may read all of it,
    /// except what Ophio always covers.
    #[test]
    fn an_agent_that_may_read_home_hears_what_stays_hidden_there() {
        let default = ConversationAccess::default();
        let text = reached(&reach(&default, &[]), &at("/home/sam", true));
        assert_eq!(
            text,
            "You may change nothing but your scratch folder. You may read `/home/sam`. The rest of the computer looks empty or missing to you. Ophio's own data and other agents' settings and history stay hidden whatever they allow. You may reach the internet. The person's own network, such as their LAN or tailnet, stays off unless they allow it."
        );
    }
}
