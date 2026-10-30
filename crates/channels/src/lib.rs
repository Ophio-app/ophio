// SPDX-License-Identifier: Apache-2.0
//! Optional chat transports using the companion's existing command authority.
mod config;
pub mod context_links;
mod core;
mod discord;
mod http;
mod recovery;
mod state;
mod telegram;

pub use config::{ChannelConfig, ChannelState, ChannelStatus, Config, Endpoints, Exposure};
pub use core::{command_id, escape_html, split_message};

use async_trait::async_trait;
use protocol::{ErrorCode, auth::ChannelKind};
use services::api::Hub;
use std::{fmt, path::PathBuf, sync::Arc};
use tokio::{sync::watch, task::JoinHandle};
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

pub type Result<T> = std::result::Result<T, Error>;

/// Deliberately excludes URLs, upstream response bodies and secret values.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid channel configuration: {0}")]
    Configuration(&'static str),
    #[error("channel state is unavailable; check the local state directory")]
    State,
    #[error("another connector owns this state directory")]
    StateLocked,
    #[error("platform connection failed")]
    Connection,
    #[error("invalid platform response")]
    PlatformProtocol,
    #[error("platform returned HTTP {0}")]
    Http(u16),
    /// The platform is limiting sends to one chat for this long. Other chats
    /// go ahead meanwhile; this one is tried again once the limit ends.
    #[error("platform is limiting this chat")]
    RateLimited(std::time::Duration),
    #[error("another program or webhook owns this bot")]
    Conflict,
    #[error("channel stopped")]
    Shutdown,
    #[error("the computer refused the request: {0:?}")]
    Hub(ErrorCode),
    #[error("token store is unavailable")]
    TokenStore,
}

/// Tokens are injected by the owner-only companion secret store. Never serialized.
pub struct BotToken(Zeroizing<String>);
impl BotToken {
    pub fn new(value: String) -> Result<Self> {
        let value = Zeroizing::new(value);
        if value.is_empty()
            || value.len() > 512
            || !value
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b":._-".contains(&c))
        {
            return Err(Error::Configuration("invalid bot token format"));
        }
        Ok(Self(value))
    }
    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}
impl fmt::Debug for BotToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BotToken([redacted])")
    }
}
impl fmt::Display for BotToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}
#[async_trait]
pub trait TokenSource: Send + Sync + 'static {
    async fn token(&self, channel: ChannelKind) -> Result<Option<BotToken>>;
}

#[derive(Debug)]
pub struct ChannelsHandle {
    status: watch::Sender<Vec<ChannelStatus>>,
    shutdown: CancellationToken,
    workers: Vec<JoinHandle<()>>,
    released: watch::Receiver<()>,
}
impl ChannelsHandle {
    pub fn status(&self) -> Vec<ChannelStatus> {
        self.status.borrow().clone()
    }
    /// Subscribe before reading the initial state to avoid missed transitions.
    pub fn subscribe_status(&self) -> watch::Receiver<Vec<ChannelStatus>> {
        self.status.subscribe()
    }
    /// Cancel and join all connector work before closing the companion. Returns
    /// only once the state directory is free, so a replacement can open it.
    pub async fn shutdown(mut self) {
        self.shutdown.cancel();
        for worker in self.workers.drain(..) {
            let _ = worker.await;
        }
        while self.released.changed().await.is_ok() {}
    }
}
impl Drop for ChannelsHandle {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

pub async fn spawn(
    hub: Arc<dyn Hub>,
    config: Config,
    tokens: Arc<dyn TokenSource>,
    state_dir: PathBuf,
    shutdown: CancellationToken,
) -> Result<ChannelsHandle> {
    spawn_with_clock(
        hub,
        config,
        tokens,
        state_dir,
        shutdown,
        Arc::new(ports::SystemClock),
    )
    .await
}

/// Injectable monotonic clock for admission limits and reference expiry.
pub async fn spawn_with_clock(
    hub: Arc<dyn Hub>,
    config: Config,
    tokens: Arc<dyn TokenSource>,
    state_dir: PathBuf,
    shutdown: CancellationToken,
    clock: Arc<dyn ports::Clock>,
) -> Result<ChannelsHandle> {
    config.validate()?;
    let state = state::Store::open(state_dir).await?;
    let released = state.released();
    let shutdown = shutdown.child_token();
    let (status, _) = watch::channel(Vec::new());
    let mut workers = Vec::new();
    for (kind, cfg) in [
        (ChannelKind::Telegram, config.telegram),
        (ChannelKind::Discord, config.discord),
    ] {
        let index = status.borrow().len();
        status.send_modify(|states| states.push(ChannelStatus::new(kind, ChannelState::Disabled)));
        if !cfg.enabled {
            continue;
        }
        let token = match tokens.token(kind).await {
            Ok(Some(token)) => token,
            Ok(None) => {
                set_status(&status, index, kind, ChannelState::TokenMissing);
                continue;
            }
            Err(_) => {
                set_status(&status, index, kind, ChannelState::PlatformError);
                continue;
            }
        };
        set_status(&status, index, kind, ChannelState::Connecting);
        let engine = Arc::new(core::Core::new(
            hub.clone(),
            cfg.clone(),
            state.clone(),
            clock.clone(),
            kind,
        ));
        let statuses = status.clone();
        let stop = shutdown.child_token();
        workers.push(tokio::spawn(async move {
            let update = move |state| set_status(&statuses, index, kind, state);
            if engine.withdraw_approval_controls().await.is_err() {
                update(ChannelState::PlatformError);
                return;
            }
            let token = Arc::new(token);
            let mut delay = 1;
            loop {
                let recovered = std::sync::atomic::AtomicBool::new(false);
                let progress = |state| {
                    if state == ChannelState::Connected {
                        recovered.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                    update(state);
                };
                let result = match kind {
                    ChannelKind::Telegram => {
                        telegram::run(
                            engine.clone(),
                            cfg.endpoints.clone(),
                            token.clone(),
                            stop.clone(),
                            &progress,
                        )
                        .await
                    }
                    ChannelKind::Discord => {
                        discord::run(
                            engine.clone(),
                            cfg.endpoints.clone(),
                            token.clone(),
                            stop.clone(),
                            &progress,
                        )
                        .await
                    }
                };
                let Err(error) = result else {
                    break;
                };
                let failure = error.failure(recovery::Scope::Channel);
                if failure == recovery::Failure::Shutdown {
                    break;
                }
                update(if matches!(error, Error::Conflict | Error::StateLocked) {
                    ChannelState::Conflict
                } else {
                    ChannelState::PlatformError
                });
                if failure == recovery::Failure::Fatal {
                    break;
                }
                if recovered.load(std::sync::atomic::Ordering::Relaxed) {
                    delay = 1;
                }
                if http::backoff(&stop, delay).await.is_err() {
                    break;
                }
                delay = (delay * 2).min(60);
            }
        }));
    }
    Ok(ChannelsHandle {
        status,
        shutdown,
        workers,
        released,
    })
}
fn set_status(
    status: &watch::Sender<Vec<ChannelStatus>>,
    index: usize,
    kind: ChannelKind,
    state: ChannelState,
) {
    status.send_modify(|states| states[index] = ChannelStatus::new(kind, state));
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod token_tests {
    use super::*;
    use zeroize::Zeroize;

    #[test]
    fn token_is_redacted_and_uses_zeroizing_storage() {
        let mut token = BotToken::new("synthetic-secret".into()).unwrap();
        assert_eq!(format!("{token:?}"), "BotToken([redacted])");
        assert_eq!(format!("{token}"), "[redacted]");
        let storage: &mut Zeroizing<String> = &mut token.0;
        storage.zeroize();
        assert_eq!(token.expose(), "");
        assert!(std::mem::needs_drop::<BotToken>());
        assert_eq!(
            BotToken::new("invalid synthetic token".into())
                .unwrap_err()
                .to_string(),
            "invalid channel configuration: invalid bot token format"
        );
    }

    #[test]
    fn zeroizing_wrapper_wipes_on_drop_without_inspecting_freed_memory() {
        struct Witness<'a>(&'a mut [u8]);
        impl Zeroize for Witness<'_> {
            fn zeroize(&mut self) {
                self.0.zeroize();
            }
        }
        let mut bytes = *b"synthetic-secret";
        {
            let _secret = Zeroizing::new(Witness(&mut bytes));
        }
        assert_eq!(bytes, [0; 16]);
    }
}
