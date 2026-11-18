// SPDX-License-Identifier: Apache-2.0
//! The listener on Ophio's side. Shutdown closes connections and unbinds every run.
use crate::{BridgeError, framing};
use ports::bridge::{BridgeBinding, BridgeHandler, BridgeRequest, BridgeResponse, ChecksCall};
use std::{future::Future, path::Path, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncWrite, BufReader},
    sync::watch,
    task::JoinSet,
};

/// The parent directory must be private (0700 on Unix). Existing sockets are
/// never unlinked on startup: the caller must reconcile stale listeners.
/// On Windows `socket_path` is a local `\\.\pipe\...` name with an owner-only DACL.
pub async fn serve_bridge<P, F>(
    socket_path: P,
    handler: Arc<dyn BridgeHandler>,
    shutdown: F,
) -> Result<(), BridgeError>
where
    P: AsRef<Path>,
    F: Future<Output = ()> + Send,
{
    serve_bridge_ready(socket_path, handler, shutdown, None).await
}

/// Like `serve_bridge`, with a notification after the private listener is bound.
/// A dropped notification sender means startup failed; await the listener result.
pub async fn serve_bridge_ready<P, F>(
    socket_path: P,
    handler: Arc<dyn BridgeHandler>,
    shutdown: F,
    ready: Option<tokio::sync::oneshot::Sender<()>>,
) -> Result<(), BridgeError>
where
    P: AsRef<Path>,
    F: Future<Output = ()> + Send,
{
    let path = socket_path.as_ref();
    #[cfg(unix)]
    let (listener, _cleanup) = bind_unix(path)?;
    #[cfg(windows)]
    let listener = crate::socket::listener(path)?;
    if let Some(ready) = ready {
        let _ = ready.send(());
    }
    let (stop, _) = watch::channel(false);
    let mut tasks = JoinSet::new();
    tokio::pin!(shutdown);
    let result = loop {
        tokio::select! {
            biased;
            _ = &mut shutdown => break Ok(()),
            _ = tasks.join_next(), if !tasks.is_empty() => {},
            accepted = async {
                #[cfg(unix)] { listener.accept().await }
                #[cfg(windows)] {
                    listener.accept().await
                }
            }, if tasks.len() < 64 => {
                match accepted {
                    Ok(stream) => { tasks.spawn(connection(stream, handler.clone(), stop.subscribe())); },
                    Err(e) => break Err(e.into()),
                }
            }
        }
    };
    let _ = stop.send(true);
    while tasks.join_next().await.is_some() {}
    result
}

/// The listener, and every connection it accepts, is made and closed so that
/// no process started meanwhile on the Mac keeps it: a run's agent started as
/// another run connects would otherwise hold the other end of that run's
/// bridge, and could read its run token and tool calls or answer them.
#[cfg(unix)]
fn bind_unix(
    path: &Path,
) -> Result<(containment::inherit::UnixListener, SocketCleanup), BridgeError> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    let parent = path.parent().ok_or(BridgeError::PrivateDirectory)?;
    if !parent.exists() {
        std::fs::DirBuilder::new().mode(0o700).create(parent)?;
    }
    let meta = std::fs::symlink_metadata(parent)?;
    if !meta.is_dir()
        || meta.permissions().mode() & 0o077 != 0
        || meta.uid() != rustix::process::geteuid().as_raw()
    {
        return Err(BridgeError::PrivateDirectory);
    }
    let listener = containment::inherit::UnixListener::bind(path)?;
    let cleanup = SocketCleanup(path.to_owned());
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok((listener, cleanup))
}

#[cfg(unix)]
struct SocketCleanup(std::path::PathBuf);
#[cfg(unix)]
impl Drop for SocketCleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

async fn connection<S>(stream: S, handler: Arc<dyn BridgeHandler>, mut stop: watch::Receiver<bool>)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (reader, mut writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(reader);
    let handshake = async {
        let bytes = framing::line(&mut reader)
            .await?
            .ok_or(BridgeError::Disconnected)?;
        let BridgeRequest::Hello { run_token } =
            serde_json::from_slice(&bytes).map_err(|_| BridgeError::Protocol)?
        else {
            return Err(BridgeError::Protocol);
        };
        handler
            .bind(&run_token)
            .await
            .ok_or(BridgeError::Authentication)
    };
    let binding = tokio::select! {
        biased;
        _ = stop.changed() => return,
        result = tokio::time::timeout(Duration::from_secs(5), handshake) => match result {
            Ok(Ok(binding)) => binding,
            _ => { let _ = framing::write(&mut writer, &BridgeResponse::Error { message: "bridge handshake rejected".into() }).await; return; }
        }
    };
    let serve = async {
        framing::write(
            &mut writer,
            &BridgeResponse::Welcome {
                run_id: binding.run_id,
                target_id: binding.target_id,
                computer_use: binding.computer_use,
                browser: binding.browser,
                github: binding.github,
                checklist_answers: binding.checklist_answers,
                read_only: binding.read_only,
                checks: binding.checks,
            },
        )
        .await?;
        loop {
            let Some(bytes) = framing::line(&mut reader).await? else {
                break;
            };
            let request: BridgeRequest =
                serde_json::from_slice(&bytes).map_err(|_| BridgeError::Protocol)?;
            if matches!(request, BridgeRequest::Hello { .. }) {
                return Err(BridgeError::Protocol);
            }
            if let Some(reason) = refusal(&binding, &request) {
                framing::write(
                    &mut writer,
                    &BridgeResponse::Refused {
                        reason: reason.into(),
                    },
                )
                .await?;
                continue;
            }
            // Serial protocol: EOF or another request during a call cancels the
            // connection. Never allow queued input to outlive its binding.
            let response = tokio::select! {
                result = handler.handle(&binding, request) => result,
                _ = framing::line(&mut reader) => return Err(BridgeError::Disconnected),
            };
            framing::write(&mut writer, &response).await?;
        }
        Ok::<_, BridgeError>(())
    };
    tokio::select! { biased; _ = stop.changed() => {}, _ = serve => {} }
    handler.unbound(&binding).await;
}

/// Why the run bound as `binding` may not make `request`, if it may not.
fn refusal(binding: &BridgeBinding, request: &BridgeRequest) -> Option<&'static str> {
    match request {
        BridgeRequest::Checks { .. }
            if !binding.checks || binding.computer_use || binding.checklist_answers =>
        {
            Some("checks are not available for this run")
        }
        BridgeRequest::Checks {
            action: ChecksCall::Propose { .. },
        } if binding.read_only => Some("this turn only reads; drafting checks needs a Work task"),
        BridgeRequest::Checks { .. } => None,
        BridgeRequest::PermissionPrompt { .. } => None,
        BridgeRequest::Browser { .. } if binding.browser && !binding.computer_use => None,
        BridgeRequest::Browser { .. } => Some("the agent browser is not enabled for this task"),
        BridgeRequest::Github { .. } if binding.github && !binding.computer_use => None,
        BridgeRequest::Github { .. } => Some("this task is not linked to GitHub"),
        BridgeRequest::Person { .. } if !binding.computer_use => None,
        BridgeRequest::Person { .. } => {
            Some("showing the person things is not available to the computer specialist")
        }
        BridgeRequest::Work { .. } if !binding.computer_use => None,
        BridgeRequest::Work { .. } => {
            Some("planning work is not available to the computer specialist")
        }
        BridgeRequest::RequestHuman { .. } if binding.browser || binding.computer_use => None,
        // Any run may ask the person for more; Plan and Ask runs may do only that.
        BridgeRequest::RequestAccess { .. } => None,
        _ if binding.computer_use => None,
        _ => Some("computer use is not enabled for this run"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{RunId, model::AccessNeed};

    fn binding(computer_use: bool, read_only: bool) -> BridgeBinding {
        BridgeBinding {
            run_id: RunId::new(),
            target_id: None,
            computer_use,
            browser: false,
            github: false,
            checklist_answers: false,
            read_only,
            checks: false,
        }
    }

    #[test]
    fn checks_are_project_bound_and_drafts_need_a_work_turn() {
        let read = BridgeRequest::Checks {
            action: ChecksCall::Read,
        };
        let propose = BridgeRequest::Checks {
            action: ChecksCall::Propose {
                base_version: 0,
                items: vec![],
            },
        };
        for checks in [false, true] {
            for read_only in [false, true] {
                for special in ["ordinary", "computer", "checklist"] {
                    let mut binding = binding(special == "computer", read_only);
                    binding.checklist_answers = special == "checklist";
                    binding.checks = checks;
                    let allowed = checks && special == "ordinary";
                    assert_eq!(refusal(&binding, &read).is_none(), allowed);
                    assert_eq!(refusal(&binding, &propose).is_none(), allowed && !read_only);
                }
            }
        }
    }

    #[test]
    fn every_run_may_ask_for_more_access() {
        let ask = BridgeRequest::RequestAccess {
            needs: vec![AccessNeed::Internet],
            reason: "fetch the docs".into(),
        };
        for (computer_use, read_only) in [(false, false), (false, true), (true, false)] {
            assert_eq!(refusal(&binding(computer_use, read_only), &ask), None);
        }
        let key = BridgeRequest::Key {
            keys: vec!["a".into()],
        };
        assert!(refusal(&binding(false, false), &key).is_some());
    }
}
