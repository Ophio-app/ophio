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
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

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

    pub(crate) fn conversation_sandbox(
        &self,
        task: &Task,
        run: RunId,
        project: &Project,
        scratch: Option<&Path>,
        given: Vec<PathBuf>,
        bridged: bool,
    ) -> Result<Option<Arc<dyn Sandbox>>, ProtocolError> {
        let Some(access) = task.access.as_ref().filter(|a| !a.unrestricted()) else {
            return Ok(None);
        };
        let backend = self
            .inner
            .sandboxes
            .as_ref()
            .ok_or_else(|| unsupported("Conversation access"))?;
        let (reach, candidates) = self.read(|r| {
            Ok((
                self.access_reach(r, access)?,
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
        let lock = self.access_lock(task_id);
        let guard = lock.lock().await;
        let task: Task = self.read(|r| r.require("task", task_id))?;
        let Some(run_id) = task.current_run else {
            return Ok(());
        };
        let control = self.inner.runs.lock().await.get(&run_id).cloned();
        let Some(control) = control else {
            return Ok(());
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
        let reach = self.read(|r| self.access_reach(r, &access))?;
        let result = match sandbox {
            Some(sandbox) => sandbox.update(reach).await,
            None if access.unrestricted()
                && self.read(|r| {
                    Ok(r.get::<Option<ConversationAccess>>("run_access", run_id)?
                        .flatten()
                        .is_some_and(|a| a.unrestricted()))
                })? =>
            {
                Applied::Applied
            }
            None => Applied::NeedsRestart("The conversation now needs a sandbox.".into()),
        };
        let Applied::NeedsRestart(reason) = result else {
            return Ok(());
        };
        // Terminate, not SIGSTOP or a native interrupt: open file descriptors
        // must close before the smaller grant can be considered enforced.
        self.write(|tx| tx.put("stopped", run_id, &true))?;
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
        let previous: Run = self.read(|r| r.require("run", run_id))?;
        if previous.state == RunState::Completed {
            return Ok(());
        }
        tracing::debug!(%reason, "conversation access needs a new turn");
        // The turn hears the new access ahead of this (see [`note`]).
        let text = "Ophio stopped your previous turn to apply this. Commands you started in it have ended: check what finished before starting any again, then carry on with the same request.".to_owned();
        let next = self.write(|tx| {
            let mut task: Task = tx.read().require("task", task_id)?;
            if task.current_run != Some(run_id) { return Ok(None); }
            let previous: Run = tx.read().require("run", run_id)?;
            if previous.native_session.is_none() {
                return Err(error(ErrorCode::OutcomeUnknown,"The agent ended before reporting a native session; send the request again").into());
            }
            let next = self.new_run(tx, &mut task, command, Some(text.clone()), None, side)?;
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
        Ok(())
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

/// The access a conversation's agent was last told, by task: what its
/// native session has heard.
pub(crate) const TOLD: &str = "access_told";

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
            "Ophio holds this {what} to what the person allows in its access bar, and the computer enforces it. {reaches} They can change it at any time, even during your turn; your next turn starts with what changed. Something refused, like a \"Read-only file system\" error, a folder that looks empty or a 403 from the proxy, is their choice, not a fault: don't work around it with another path, tool or copy. Say what you need and why, finish what you can, then end your turn so they can allow it from their phone."
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
    #[test]
    fn finds_actual_denied_paths_in_shell_and_python_results() {
        assert_eq!(
            blocked_write("/bin/sh: line 1: /repo/out: Read-only file system"),
            Some("/repo/out".into())
        );
        assert_eq!(
            blocked_write("OSError: [Errno 30] Read-only file system: '/repo/a b'"),
            Some("/repo/a b".into())
        );
        assert_eq!(blocked_write("wrote /repo/out"), None);
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
