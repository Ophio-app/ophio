// SPDX-License-Identifier: Apache-2.0
//! Owner-only newline-delimited control JSON. Pairing is an additional local
//! operation; it is never exposed on the authenticated remote control channel.
use crate::{Error, Result, server::ServerDeps};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum OwnerRequest {
    OpenPairing,
    /// The device waiting for the owner to answer its pairing request.
    WaitingPairing,
    AnswerPairing {
        code: String,
        confirm: bool,
    },
    Jev {
        operation: crate::jev::JevOperation,
    },
    Voice {
        action: crate::frontdoors::VoiceCommand,
    },
    Channels {
        action: crate::frontdoors::ChannelCommand,
    },
    TokenSet {
        connector: crate::frontdoors::Connector,
        token: crate::frontdoors::TokenInput,
    },
    TokenClear {
        connector: crate::frontdoors::Connector,
    },
    FrontdoorStatus,
    Desktop {
        enabled: Option<bool>,
    },
    RootsAdd {
        path: std::path::PathBuf,
        label: Option<String>,
        writable: bool,
    },
    RootsRemove {
        root_id: protocol::LibraryRootId,
    },
    RootsList,
    InboxSet {
        path: std::path::PathBuf,
    },
    RootsSet {
        root_id: protocol::LibraryRootId,
        kind: Option<protocol::library::RootKind>,
        project_id: Option<protocol::ProjectId>,
        #[serde(default)]
        clear_project: bool,
        writable: Option<bool>,
        harness: Option<protocol::capability::HarnessKind>,
        #[serde(default)]
        clear_harness: bool,
    },
    RetentionSet {
        days: u16,
    },
    /// Whether Work runs in a project apply the settings committed in its
    /// folder. Deliberately absent from the phone protocol.
    ProjectSettings {
        project_id: protocol::ProjectId,
        apply: bool,
    },
    /// Shows a project's checklist, or puts items on it or takes them off,
    /// for `ophio checklist`.
    Checklist {
        project_id: protocol::ProjectId,
        #[serde(default)]
        put: Option<services::ChecklistAdditions>,
        #[serde(default)]
        remove: Vec<String>,
    },
    Stop,
    StopTask {
        task_id: protocol::TaskId,
        #[serde(default)]
        force: bool,
    },
    /// Ends Øphio itself, as a terminate signal does on Linux. Windows has no
    /// such signal for a program running without a window.
    Quit,
    /// A key or fresh access token for one account, for `ophio token`.
    /// Deliberately absent from the phone protocol.
    AccountToken {
        account: String,
        /// The credential of the task asking, from inside it: the task gets
        /// only what it was given when it started.
        #[serde(default)]
        run: Option<protocol::accounts::Secret>,
    },
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", content = "offer", rename_all = "snake_case")]
pub enum OwnerResponse {
    PairingOffer(protocol::auth::PairingOffer),
    PairingWaiting(Option<services::api::PairingPrompt>),
    /// The device enrolled on confirmation; none when refused.
    PairingAnswered(Option<protocol::auth::DeviceRecord>),
    Jev(::jev::Access),
    Frontdoors(crate::frontdoors::FrontdoorStatus),
    VoiceProgress(crate::frontdoors::VoiceProgress),
    TaskStop(protocol::command::CommandReceipt),
    ProjectSettings {
        project: protocol::model::Project,
        apply: bool,
    },
    ControlStopped {
        tasks_requested: usize,
    },
    /// Sent before Øphio stops, which closes the connection.
    Quitting,
    Settings {
        config: crate::config::Config,
        applied_live: bool,
        desktop: protocol::capability::Availability,
    },
    /// The secret itself; printed by `ophio token` and nowhere else.
    AccountToken(String),
    /// The checklist as an agent reads it, or what changed on it.
    Checklist(String),
}
impl std::fmt::Debug for OwnerResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OwnerResponse::PairingOffer([redacted])")
    }
}

/// Local owner controls are deliberately outside the phone protocol.
#[async_trait::async_trait]
pub trait OwnerRuntime: Send + Sync {
    async fn request(&self, request: OwnerRequest) -> Result<OwnerResponse>;
    async fn request_progress(
        &self,
        request: OwnerRequest,
        _progress: tokio::sync::mpsc::Sender<OwnerResponse>,
        _cancel: CancellationToken,
    ) -> Result<OwnerResponse> {
        self.request(request).await
    }
    async fn shutdown(&self) -> Result<()>;
    /// Unexpected runtime failure requires the host's listeners to stop as well.
    async fn failed(&self);
}

/// No owner endpoint, or nothing listening on it, means Ophio is not running.
fn not_running(error: std::io::Error) -> Error {
    match error.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => Error::NotRunning,
        _ => error.into(),
    }
}

/// Kept separate from the OS lookup so a mismatched peer is deterministic to test.
pub fn check_uid(owner: u32, peer: u32) -> Result<()> {
    if owner != peer {
        return Err(protocol::ProtocolError::new(
            protocol::ErrorCode::Unauthenticated,
            "local peer is not the owner",
        )
        .into());
    }
    Ok(())
}

#[cfg(unix)]
mod unix {
    use super::*;
    use crate::started::Started;
    use crate::wire::{self, Wire};
    use std::{
        fs,
        os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt},
        path::{Path, PathBuf},
    };
    use tokio::{
        net::{UnixListener, UnixStream},
        task::JoinSet,
    };

    #[derive(Debug)]
    pub struct LocalServer {
        listener: UnixListener,
        path: PathBuf,
        started: Started,
        _lock: store::InstanceLock,
    }
    impl LocalServer {
        pub async fn bind(path: &Path, started: Started) -> Result<Self> {
            let parent = path.parent().ok_or(Error::Identity)?;
            crate::identity::private_dir(parent)?;
            let lock = crate::identity::private_open(&parent.join("owner.lock"))?;
            let lock = store::InstanceLock::acquire(lock).map_err(|_| Error::Running)?;
            if let Ok(m) = fs::symlink_metadata(path) {
                check_uid(rustix::process::geteuid().as_raw(), m.uid())?;
                if !m.file_type().is_socket() {
                    return Err(Error::Identity);
                }
                if UnixStream::connect(path).await.is_ok() {
                    return Err(Error::Running);
                }
                fs::remove_file(path)?;
            }
            let listener = UnixListener::bind(path)?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
            Ok(Self {
                listener,
                path: path.to_owned(),
                started,
                _lock: lock,
            })
        }
        pub async fn run(self, deps: ServerDeps, shutdown: CancellationToken) -> Result<()> {
            let mut tasks = JoinSet::new();
            let result = loop {
                tokio::select! {
                    biased;
                    _ = shutdown.cancelled() => break Ok(()),
                    Some(_) = tasks.join_next(), if !tasks.is_empty() => {},
                    accepted = crate::server::accept_with_retry(|| self.listener.accept(), &shutdown) => {
                        let (stream, _) = match accepted {
                            Ok(Some(stream)) => stream,
                            Ok(None) => break Ok(()),
                            Err(error) => { shutdown.cancel(); break Err(Error::Io(error)); }
                        };
                        let Ok(credential) = stream.peer_cred() else { continue; };
                        if tasks.len() >= 32 || check_uid(rustix::process::geteuid().as_raw(), credential.uid()).is_err() { continue; }
                        let deps = deps.clone(); let shutdown = shutdown.clone(); let started = self.started.clone();
                        tasks.spawn(async move {
                            let mut wire = Wire::local(stream);
                            let started = credential.pid().and_then(|pid| u32::try_from(pid).ok()).is_none_or(|pid| started.includes(pid));
                            let caller = services::api::Caller::local_owner();
                            let result = crate::control::run(&mut wire, &deps, caller.clone(), &shutdown, true, started).await;
                            deps.hub.control_disconnected(&caller).await;
                            if let Err(error) = result { let _ = wire::send_json(&mut wire.writer, &protocol::ServerFrame::Fatal(error.protocol()), deps.limits.write).await; }
                        });
                    }
                }
            };
            while tasks.join_next().await.is_some() {}
            result
        }
    }
    impl Drop for LocalServer {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
        }
    }

    pub struct OwnerClient {
        wire: Wire,
    }
    impl std::fmt::Debug for OwnerClient {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("OwnerClient").finish_non_exhaustive()
        }
    }
    impl OwnerClient {
        pub async fn connect(path: &Path) -> Result<Self> {
            let metadata = fs::symlink_metadata(path).map_err(not_running)?;
            if !metadata.file_type().is_socket() || metadata.mode() & 0o077 != 0 {
                return Err(Error::Identity);
            }
            check_uid(rustix::process::geteuid().as_raw(), metadata.uid())?;
            let stream = UnixStream::connect(path).await.map_err(not_running)?;
            check_uid(
                rustix::process::geteuid().as_raw(),
                stream.peer_cred()?.uid(),
            )?;
            Ok(Self {
                wire: Wire::local(stream),
            })
        }
        pub async fn request<T: Serialize, R: serde::de::DeserializeOwned>(
            &mut self,
            request: &T,
        ) -> Result<R> {
            self.request_with_progress(request, |_| {}).await
        }
        pub async fn request_with_progress<T: Serialize, R: serde::de::DeserializeOwned>(
            &mut self,
            request: &T,
            mut progress: impl FnMut(crate::frontdoors::VoiceProgress),
        ) -> Result<R> {
            let timeout = std::time::Duration::from_secs(180);
            wire::send_owner_json(&mut self.wire.writer, request, timeout).await?;
            loop {
                let response: serde_json::Value =
                    tokio::time::timeout(timeout, wire::receive(&mut self.wire.reader))
                        .await
                        .map_err(|_| Error::Timeout)??;
                if let Ok(OwnerResponse::VoiceProgress(value)) =
                    serde_json::from_value(response.clone())
                {
                    progress(value);
                    continue;
                }
                if let Ok(protocol::ServerFrame::Fatal(error)) =
                    serde_json::from_value(response.clone())
                {
                    return Err(error.into());
                }
                return serde_json::from_value(response)
                    .map_err(|_| crate::error::invalid("unexpected local response"));
            }
        }
    }
}
#[cfg(unix)]
pub use unix::{LocalServer, OwnerClient};

// Protected local pipes verify both peers before JSON framing. No TCP fallback.
#[cfg(windows)]
mod windows {
    use super::*;
    use crate::wire::{self, Wire};
    use axum::extract::ws::Message;
    use futures_util::{SinkExt, StreamExt};
    use std::path::Path;
    use tokio::{
        io::{AsyncRead, AsyncWrite},
        task::JoinSet,
    };
    use tokio_util::codec::{Framed, LinesCodec};

    fn local<T: AsyncRead + AsyncWrite + Unpin + Send + 'static>(stream: T) -> Wire {
        let (sink, stream) = Framed::new(
            stream,
            LinesCodec::new_with_max_length(wire::MAX_JSON_BYTES),
        )
        .split();
        Wire {
            reader: Box::pin(stream.map(|r| {
                r.map(|s| Message::Text(s.into()))
                    .map_err(|_| crate::error::invalid("invalid local frame"))
            })),
            writer: Box::pin(wire::closed_on_error(sink).with(|msg: Message| async move {
                match msg {
                    Message::Text(s) => Ok(s.to_string()),
                    _ => Err(crate::error::invalid("local connection expects JSON")),
                }
            })),
        }
    }
    #[derive(Debug)]
    pub struct LocalServer {
        listener: platform_windows::pipe::OwnerListener,
        started: crate::started::Started,
    }
    impl LocalServer {
        pub async fn bind(_: &Path, started: crate::started::Started) -> Result<Self> {
            Ok(Self {
                listener: platform_windows::pipe::OwnerListener::bind()?,
                started,
            })
        }
        pub async fn run(mut self, deps: ServerDeps, shutdown: CancellationToken) -> Result<()> {
            let mut tasks = JoinSet::new();
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown.cancelled() => break,
                    Some(_) = tasks.join_next(), if !tasks.is_empty() => {},
                    accepted = self.listener.accept() => {
                        let stream = match accepted {
                            Ok(stream) => stream,
                            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => continue,
                            Err(error) => return Err(error.into()),
                        };
                        if tasks.len() >= 32 { continue; }
                        let started = platform_windows::pipe::client_process_id(&stream).map_or(true, |pid| self.started.includes(pid));
                        let deps = deps.clone(); let shutdown = shutdown.clone();
                        tasks.spawn(async move {
                            let mut wire = local(stream);
                            let caller = services::api::Caller::local_owner();
                            let result = crate::control::run(&mut wire, &deps, caller.clone(), &shutdown, true, started).await;
                            deps.hub.control_disconnected(&caller).await;
                            if let Err(error) = result { let _ = wire::send_json(&mut wire.writer, &protocol::ServerFrame::Fatal(error.protocol()), deps.limits.write).await; }
                        });
                    }
                }
            }
            while tasks.join_next().await.is_some() {}
            Ok(())
        }
    }
    pub struct OwnerClient {
        wire: Wire,
        server: u32,
    }
    impl std::fmt::Debug for OwnerClient {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("OwnerClient").finish_non_exhaustive()
        }
    }
    impl OwnerClient {
        pub async fn connect(_: &Path) -> Result<Self> {
            let pipe = platform_windows::pipe::connect()
                .await
                .map_err(not_running)?;
            Ok(Self {
                server: platform_windows::pipe::server_process_id(&pipe)?,
                wire: local(pipe),
            })
        }
        /// The process id of the Øphio this client is connected to.
        pub fn server(&self) -> u32 {
            self.server
        }
        pub async fn request<T: Serialize, R: serde::de::DeserializeOwned>(
            &mut self,
            request: &T,
        ) -> Result<R> {
            let timeout = std::time::Duration::from_secs(10);
            wire::send_owner_json(&mut self.wire.writer, request, timeout).await?;
            let response: serde_json::Value =
                tokio::time::timeout(timeout, wire::receive(&mut self.wire.reader))
                    .await
                    .map_err(|_| Error::Timeout)??;
            if let Ok(protocol::ServerFrame::Fatal(error)) =
                serde_json::from_value(response.clone())
            {
                return Err(error.into());
            }
            serde_json::from_value(response)
                .map_err(|_| crate::error::invalid("unexpected local response"))
        }
        pub async fn request_with_progress<T: Serialize, R: serde::de::DeserializeOwned>(
            &mut self,
            request: &T,
            mut progress: impl FnMut(crate::frontdoors::VoiceProgress),
        ) -> Result<R> {
            let timeout = std::time::Duration::from_secs(180);
            wire::send_owner_json(&mut self.wire.writer, request, timeout).await?;
            loop {
                let response: serde_json::Value =
                    tokio::time::timeout(timeout, wire::receive(&mut self.wire.reader))
                        .await
                        .map_err(|_| Error::Timeout)??;
                if let Ok(protocol::ServerFrame::Fatal(error)) =
                    serde_json::from_value(response.clone())
                {
                    return Err(error.into());
                }
                if let Ok(OwnerResponse::VoiceProgress(update)) =
                    serde_json::from_value(response.clone())
                {
                    progress(update);
                    continue;
                }
                return serde_json::from_value(response)
                    .map_err(|_| crate::error::invalid("unexpected local response"));
            }
        }
    }
}
#[cfg(windows)]
pub use windows::{LocalServer, OwnerClient};

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[tokio::test]
    async fn a_missing_or_silent_endpoint_means_ophio_is_not_running() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("owner.sock");
        let missing = super::OwnerClient::connect(&path).await.unwrap_err();
        assert!(matches!(missing, crate::Error::NotRunning));
        // A socket left behind by an Ophio that has since stopped. It is bound but
        // never listens: a listener dropped here could still be accepting in a
        // child that another test is starting, until that child executes.
        let stale_socket =
            socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None).unwrap();
        stale_socket
            .bind(&socket2::SockAddr::unix(&path).unwrap())
            .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let stale = super::OwnerClient::connect(&path).await.unwrap_err();
        assert!(matches!(stale, crate::Error::NotRunning));
        assert!(
            stale
                .to_string()
                .starts_with("Øphio is not running. Run `ophio run`")
        );
    }

    #[test]
    fn different_uid_is_rejected() {
        assert!(super::check_uid(1000, 1000).is_ok());
        assert!(super::check_uid(1000, 1001).is_err());
        assert!(super::check_uid(1000, 0).is_err());
    }
}
