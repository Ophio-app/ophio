// SPDX-License-Identifier: Apache-2.0
//! GitHub text is classified before it reaches a native session.
use crate::{CompanionHub, error};
use ports::github::{Github, GithubError, Method};
use protocol::{
    ApprovalId, ErrorCode, Event, ProtocolError, RunId,
    access::ConversationAccess,
    capability::Availability,
    command::ApprovalDecision,
    github::GithubLink,
    model::{ApprovalKind, Project, Run, RunState, Task, TaskKind, TaskMode, TaskState},
};
use serde_json::Value;
use std::sync::{Arc, Mutex};

/// Taint stays with the native session, even if a comment is later removed.
pub(crate) const UNTRUSTED: &str = "github_untrusted";
pub(crate) const CLASSIFIED: &str = "github_classified";
pub(crate) const MAX_CONTENT_PAGES: u32 = 3;
/// A companion-owned approval admitted before any agent or tool bridge exists.
pub(crate) const START_APPROVAL: &str = "github_start_approval";

/// Visibility never makes an outside author trusted: private repositories
/// can have outside contributors too. Missing visibility or identity fails closed.
pub(crate) fn untrusted(private: Option<bool>, items: &[Value]) -> bool {
    private.is_none()
        || items.iter().any(|item| {
            let author = item.get("user").or_else(|| item.get("author"));
            let association = item
                .get("author_association")
                .or_else(|| item.get("authorAssociation"))
                .and_then(Value::as_str);
            let kind = author
                .and_then(|a| a.get("type").or_else(|| a.get("__typename")))
                .and_then(Value::as_str);
            let login = author.and_then(|a| a["login"].as_str()).unwrap_or("");
            // Ophio posts through the signed-in person's account. There is
            // no Ophio app identity; a disclosure in a body proves nothing.
            login.is_empty()
                || login.to_ascii_lowercase().ends_with("[bot]")
                || kind.is_some_and(|k| k != "User")
                || item
                    .get("performed_via_github_app")
                    .is_some_and(|a| !a.is_null())
                || !association.is_some_and(crate::github::writes)
        })
}

/// Records the very responses used to build the prompt, avoiding a second
/// trust check against content that might have changed meanwhile.
#[derive(Debug)]
pub(crate) struct Content {
    gh: Arc<dyn Github>,
    items: Mutex<Vec<Value>>,
}
impl Content {
    pub(crate) fn new(gh: Arc<dyn Github>) -> Self {
        Self {
            gh,
            items: Mutex::new(Vec::new()),
        }
    }
    pub(crate) fn untrusted(&self, private: Option<bool>) -> bool {
        untrusted(private, &self.items.lock().unwrap())
    }
}
#[async_trait::async_trait]
impl Github for Content {
    async fn rest(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, GithubError> {
        let value = self.gh.rest(method, path, body).await?;
        let incomplete = path.ends_with(&format!("page={MAX_CONTENT_PAGES}"))
            && value.as_array().is_some_and(|items| items.len() >= 100);
        if incomplete {
            // Unread tails have no provenance. They must not give a turn
            // ordinary access to fetch the rest itself.
            self.items.lock().unwrap().push(Value::Null);
        }
        // A file's patch has the pull request's provenance; it has no author
        // association of its own. All other calls here read authored content.
        if !path.contains("/files?") {
            let mut items = self.items.lock().unwrap();
            match &value {
                Value::Array(values) => items.extend(values.iter().cloned()),
                _ => items.push(value.clone()),
            }
        }
        Ok(value)
    }
    async fn graphql(&self, _: &str, _: Value) -> Result<Value, GithubError> {
        Err(GithubError::Failed("Content snapshots use REST".into()))
    }
}

impl CompanionHub {
    pub(crate) async fn github_start_content(
        &self,
        task: &Task,
        project: &Project,
    ) -> Result<(Option<String>, bool), ProtocolError> {
        let Some(github) = task.github.as_deref() else {
            return Ok((None, false));
        };
        let (owner, name) = self.on_github(task.project_id).await?;
        crate::github::same_repo(github, &owner, &name)?;
        let gh = self.github()?;
        let base = format!("/repos/{}", crate::github::repo_key(&owner, &name));
        // Read visibility now, not from the workflow's cached repository.
        let repo = gh
            .rest(Method::Get, &base, None)
            .await
            .map_err(crate::github::github_error)?;
        let content = Content::new(gh);
        let data = if github.link.read_only() {
            self.github_read_content(task, project, &content, &base)
                .await?
        } else {
            Some(self.github_work_content(task, &content, &base).await?)
        };
        Ok((
            data,
            content.untrusted(repo["private"].as_bool()) || github.from_stranger,
        ))
    }

    /// Gives a working turn its issue, pull request and comments without
    /// opening a network route from the agent to GitHub.
    pub(crate) async fn github_work_content(
        &self,
        task: &Task,
        gh: &dyn Github,
        base: &str,
    ) -> Result<String, ProtocolError> {
        let Some(github) = task.github.as_deref() else {
            return Ok(String::new());
        };
        let mut out = String::new();
        if let Some(number) = github.issue {
            let issue = gh
                .rest(Method::Get, &format!("{base}/issues/{number}"), None)
                .await
                .map_err(crate::github::github_error)?;
            out.push_str(&format!(
                "Issue #{number}, data from GitHub:\n{}",
                crate::github_after::item_entry(&issue, 20_000)
            ));
            out.push_str(&comments(gh, &format!("{base}/issues/{number}/comments")).await?);
        }
        let number = github.pr.as_ref().map(|p| p.number).or(match &github.link {
            GithubLink::Stack { number } => Some(*number),
            _ => None,
        });
        if let Some(number) = number {
            out.push_str(&crate::github_after::pr_changes(gh, base, number).await?);
        }
        Ok(out)
    }

    /// Applies the first hold before account files, app helpers or bridge
    /// credentials are granted. Later person-only access edits stand.
    pub(crate) async fn hold_github_content(
        &self,
        task: &Task,
        run_id: RunId,
        untrusted: bool,
        resumes: bool,
    ) -> Result<Task, ProtocolError> {
        let sandbox = self
            .inner
            .sandboxes
            .as_ref()
            .is_some_and(|s| matches!(s.availability(), Availability::Available));
        let mut task = self.write(|tx| {
            let mut task: Task = tx.read().require("task", task.task_id)?;
            let legacy = task.github.is_some()
                && resumes
                && !tx
                    .read()
                    .get::<bool>(CLASSIFIED, task.task_id)?
                    .unwrap_or(false);
            if task.github.is_some() {
                tx.put(CLASSIFIED, task.task_id, &true)?;
            }
            let already = tx
                .read()
                .get::<bool>(UNTRUSTED, task.task_id)?
                .unwrap_or(false);
            if !(untrusted || already || legacy) {
                return Ok(task);
            }
            tx.put(UNTRUSTED, task.task_id, &true)?;
            if sandbox
                && !tx
                    .read()
                    .get::<bool>("github_access_applied", task.task_id)?
                    .unwrap_or(false)
            {
                tx.put("github_access_applied", task.task_id, &true)?;
                // Intersect an inherited grant; never enlarge it.
                let writable = task.mode == TaskMode::Work
                    && !crate::github::writes_nothing(&task)
                    && task
                        .access
                        .as_ref()
                        .is_none_or(|a| a.whole_write || a.write.contains(&task.project_id));
                task.access = Some(ConversationAccess {
                    read: vec![task.project_id],
                    write: if writable {
                        vec![task.project_id]
                    } else {
                        vec![]
                    },
                    internet: false,
                    ..ConversationAccess::default()
                });
                task.revision += 1;
                task.updated_at = self.inner.clock.now();
                tx.put("task", task.task_id, &task)?;
                tx.emit(Event::TaskUpserted { task: task.clone() })?;
            }
            Ok(task)
        })?;
        let tainted =
            self.read(|r| Ok(r.get::<bool>(UNTRUSTED, task.task_id)?.unwrap_or(false)))?;
        if !tainted {
            return Ok(task);
        }
        if !sandbox && !task.access.as_ref().is_some_and(|a| a.unrestricted()) {
            let id = ApprovalId::new();
            self.write(|tx| tx.put(START_APPROVAL, run_id, &id))?;
            let answer = self.bridge_wait_as(id, run_id, &task, ports::tool_approval::ToolApproval {
                kind: ApprovalKind::Other,
                title: "Start without project limits?".into(),
                detail: Some("The request came from someone outside the project, and this computer can't hold the agent to the project. Starting lets it use the task's ordinary access to this computer and the internet.".into()),
                detail_lines: vec![], questions: vec![], scope: "This turn only".into(), shortened: false,
            }, None, None, None).await?;
            if !matches!(answer, ApprovalDecision::Allow) {
                return Err(error(
                    ErrorCode::PermissionDenied,
                    "The task wasn't started.",
                ));
            }
            // Approval resolution normally returns a running agent to Work;
            // this one has not spawned yet.
            task = self.write(|tx| {
                let mut run: Run = tx.read().require("run", run_id)?;
                if run.ended_at.is_some()
                    || tx.read().get::<bool>("stopped", run_id)?.unwrap_or(false)
                {
                    return Err(error(ErrorCode::Expired, "The task was stopped.").into());
                }
                run.state = RunState::Dispatching;
                tx.put("run", run_id, &run)?;
                tx.emit(Event::RunUpserted { run: Box::new(run) })?;
                let mut task: Task = tx.read().require("task", task.task_id)?;
                task.state = TaskState::Dispatching;
                // A grant cannot be enforced here. The approval is for an
                // ordinary launch, without silently saving a wider grant.
                task.revision += 1;
                tx.put("task", task.task_id, &task)?;
                tx.emit(Event::TaskUpserted { task: task.clone() })?;
                Ok(task)
            })?;
            task.access = None;
        }
        if task.kind == TaskKind::ComputerUse && crate::access::held(&task).is_some() {
            return Err(error(
                ErrorCode::PermissionDenied,
                "This review reads outside content. Review the task and allow the access it needs before using a screen.",
            ));
        }
        Ok(task)
    }

    pub(crate) fn github_tools_held(&self, task: &Task) -> Result<bool, ProtocolError> {
        Ok(crate::access::held(task).is_some()
            || self.read(|r| Ok(r.get::<bool>(UNTRUSTED, task.task_id)?.unwrap_or(false)))?)
    }
}

pub(crate) async fn comments(gh: &dyn Github, path: &str) -> Result<String, ProtocolError> {
    let mut out = String::new();
    for page in 1..=MAX_CONTENT_PAGES {
        let values = gh
            .rest(
                Method::Get,
                &format!("{path}?per_page=100&page={page}"),
                None,
            )
            .await
            .map_err(crate::github::github_error)?;
        let values = values.as_array().ok_or_else(|| {
            error(
                ErrorCode::NativeFailure,
                "GitHub didn't return a comment list.",
            )
        })?;
        for value in values {
            out.push_str(&crate::github_after::comment_entry(value));
        }
        if values.len() < 100 {
            break;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn associations_visibility_and_bots() {
        for private in [Some(true), Some(false), None] {
            for association in [
                "OWNER",
                "MEMBER",
                "COLLABORATOR",
                "CONTRIBUTOR",
                "FIRST_TIME_CONTRIBUTOR",
                "FIRST_TIMER",
                "NONE",
                "MANNEQUIN",
                "",
                "FUTURE_ASSOCIATION",
            ] {
                for kind in ["User", "Bot", "Organization", "Mannequin", "FutureActor"] {
                    for app in [Value::Null, json!({"slug":"some-app"})] {
                        let content = json!({"user":{"login":"someone", "type":kind}, "author_association":association, "performed_via_github_app":app});
                        let expected = private.is_none()
                            || !crate::github::writes(association)
                            || kind != "User"
                            || !app.is_null();
                        assert_eq!(
                            untrusted(private, &[content]),
                            expected,
                            "{private:?} {association} {kind} {app}"
                        );
                    }
                }
            }
        }
        for item in [
            json!({}),
            json!({"user":null,"author_association":"OWNER"}),
            json!({"user":{"login":"x[bot]"},"author_association":"OWNER"}),
        ] {
            assert!(untrusted(Some(false), &[item]));
        }
    }

    #[test]
    fn one_outside_comment_taints_a_mixed_thread() {
        let trusted =
            json!({"author":{"login":"member","__typename":"User"},"authorAssociation":"MEMBER"});
        let stranger =
            json!({"author":{"login":"outside","__typename":"User"},"authorAssociation":"NONE"});
        assert!(!untrusted(Some(false), &[trusted.clone(), trusted.clone()]));
        assert!(untrusted(Some(true), &[trusted.clone(), stranger, trusted]));
    }
}
