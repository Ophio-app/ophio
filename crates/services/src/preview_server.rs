// SPDX-License-Identifier: Apache-2.0
//! Starting a preview's server again, with the command the agent that showed
//! the page gave. Everything a run starts stops with it, its server too, so
//! the person has Ophio start the server when they open the page later.
use crate::{CompanionHub, api::Caller, error, unsupported};
use ports::TerminalEvent;
use protocol::{model::*, terminal::TerminalState, *};
use std::{path::PathBuf, time::Duration};
use tokio::time::Instant;

/// How long a started server has to answer.
const STARTING: Duration = Duration::from_secs(60);
/// How often a starting server is asked.
const ASKED_EVERY: Duration = Duration::from_millis(500);
/// How long a new shell has to show its prompt before the command is typed.
const PROMPTED: Duration = Duration::from_secs(5);
/// Quiet this long after it has shown something, a shell is taken to wait.
const SETTLED: Duration = Duration::from_millis(300);

impl CompanionHub {
    /// Starts the preview's server with its command in a terminal in the
    /// project's folder, unless the terminal it was started in before is
    /// still open, and waits for the page to answer.
    pub(crate) async fn start_preview_server(
        &self,
        caller: &Caller,
        command: &Command,
        p: &PreviewEndpoint,
        url: &url::Url,
        checker: &dyn crate::preview::PreviewChecker,
    ) -> Result<(), ProtocolError> {
        let start = (p.start.as_deref()).ok_or_else(|| {
            error(
                ErrorCode::InvalidRequest,
                "Ophio wasn't given the command that starts this preview's server",
            )
        })?;
        let terminal = (self.inner.terminal.as_ref()).ok_or_else(|| unsupported("Terminal"))?;
        let title = format!("Preview: {}", p.label);
        let id = {
            let mut servers = self.inner.preview_servers.lock().await;
            let open = match servers.get(&p.preview_id) {
                Some(id) => (terminal.info(*id).await)
                    .is_some_and(|t| t.state == TerminalState::Running)
                    .then_some(*id),
                None => None,
            };
            match open {
                Some(id) => id,
                None => {
                    let id = self
                        .open_project_shell(caller, command, p.project_id, &title)
                        .await?;
                    servers.insert(p.preview_id, id);
                    if let Err(e) = Self::type_line(terminal.as_ref(), id, start).await {
                        servers.remove(&p.preview_id);
                        let _ = terminal.close(id).await;
                        return Err(e);
                    }
                    id
                }
            }
        };
        let until = Instant::now() + STARTING;
        loop {
            let answered = tokio::time::timeout(Duration::from_secs(5), checker.check(url)).await;
            if matches!(answered, Ok(Ok(true))) {
                return Ok(());
            }
            let running =
                (terminal.info(id).await).is_some_and(|t| t.state == TerminalState::Running);
            if !running {
                return Err(error(
                    ErrorCode::TemporarilyUnavailable,
                    format!("The terminal “{title}” closed before {} answered", p.url),
                ));
            }
            if Instant::now() >= until {
                return Err(error(
                    ErrorCode::TemporarilyUnavailable,
                    format!(
                        "Nothing answered at {} within a minute. Its server was started in the terminal “{title}” in Control; look there for what it says, then open the preview again.",
                        p.url
                    ),
                ));
            }
            tokio::time::sleep(ASKED_EVERY).await;
        }
    }

    /// A shell in the project's folder, the caller's as a terminal they open is.
    pub(crate) async fn open_project_shell(
        &self,
        caller: &Caller,
        command: &Command,
        project_id: ProjectId,
        title: &str,
    ) -> Result<TerminalId, ProtocolError> {
        let terminal = (self.inner.terminal.as_ref()).ok_or_else(|| unsupported("Terminal"))?;
        let cwd = PathBuf::from(
            self.read(|r| r.require::<Project>("project", project_id))?
                .host_path,
        );
        if !tokio::fs::metadata(&cwd).await.is_ok_and(|m| m.is_dir()) {
            return Err(error(
                ErrorCode::InvalidRequest,
                "The project's folder is no longer on the computer",
            ));
        }
        let opened = terminal
            .open(ports::TerminalSpec {
                title: title.into(),
                cwd,
                program: None,
                env: Vec::new(),
                project_id: Some(project_id),
                harness: None,
                cols: 120,
                rows: 32,
            })
            .await
            .map_err(crate::plumbing::terminal_error)?;
        let id = opened.terminal_id;
        let terminals = terminal.list().await;
        let recorded = self.write(|tx| {
            // As for a terminal opened from Control: revocation closes only the
            // shells it finds recorded.
            let current = Self::current_caller(&tx.read(), caller)?;
            if !domain::grants::command(&current.grants, command) {
                return Err(error(ErrorCode::PermissionDenied, "Grant was revoked").into());
            }
            tx.put("terminal_owner", id, &caller.principal)?;
            tx.emit(Event::TerminalsChanged { terminals })?;
            Ok(())
        });
        if let Err(e) = recorded {
            let _ = terminal.close(id).await;
            return Err(e);
        }
        Ok(id)
    }

    /// Types `line` into a new shell once it has shown its prompt, or has had
    /// [`PROMPTED`] to, so a shell that drops what is typed early still runs it.
    pub(crate) async fn type_line(
        terminal: &dyn ports::TerminalHost,
        id: TerminalId,
        line: &str,
    ) -> Result<(), ProtocolError> {
        let (replay, mut events) =
            (terminal.attach(id, None).await).map_err(crate::plumbing::terminal_error)?;
        let prompted = Instant::now() + PROMPTED;
        let mut shown = !replay.bytes.is_empty();
        loop {
            let until = match shown {
                true => prompted.min(Instant::now() + SETTLED),
                false => prompted,
            };
            match tokio::time::timeout_at(until, events.recv()).await {
                Ok(Ok(TerminalEvent::Output { .. })) => shown = true,
                Ok(Ok(TerminalEvent::Exited { .. }))
                | Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => {
                    return Err(error(
                        ErrorCode::TemporarilyUnavailable,
                        "The terminal for the server closed as it opened",
                    ));
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        drop(events);
        (terminal.write(id, format!("{line}\r").as_bytes()).await)
            .map_err(crate::plumbing::terminal_error)
    }
}
