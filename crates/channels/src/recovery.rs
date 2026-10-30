// SPDX-License-Identifier: Apache-2.0
use crate::{ChannelState, Error, Result};
use std::{collections::HashSet, sync::Mutex};

#[derive(Clone, Copy)]
pub(crate) enum Scope {
    Channel,
    Item,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Failure {
    Fatal,
    Transient,
    Refused,
    Shutdown,
}

impl Error {
    pub(crate) fn failure(&self, scope: Scope) -> Failure {
        match self {
            Self::Shutdown => Failure::Shutdown,
            Self::Conflict
            | Self::StateLocked
            | Self::State
            | Self::Configuration(_)
            | Self::TokenStore => Failure::Fatal,
            Self::Http(401 | 403) if matches!(scope, Scope::Channel) => Failure::Fatal,
            Self::Connection | Self::Http(429 | 500..=599) | Self::RateLimited(_) => {
                Failure::Transient
            }
            Self::Http(_) | Self::PlatformProtocol | Self::Hub(_) => Failure::Refused,
        }
    }
}

/// Refusal discards only this item; callers can durably advance its cursor.
pub(crate) fn skip_refused(result: Result<()>) -> Result<()> {
    match result {
        Err(error) if error.failure(Scope::Item) == Failure::Refused => {
            tracing::warn!(error = %error, "chat item refused; skipping");
            Ok(())
        }
        result => result,
    }
}

/// A healthy poll/gateway must not hide another route's pending retry.
pub(crate) struct Status<'a> {
    update: &'a (dyn Fn(ChannelState) + Send + Sync),
    state: Mutex<(bool, HashSet<String>)>,
}
impl<'a> Status<'a> {
    pub fn new(update: &'a (dyn Fn(ChannelState) + Send + Sync)) -> Self {
        Self {
            update,
            state: Mutex::new((false, HashSet::new())),
        }
    }
    pub fn connected(&self) {
        self.change("transport", false, true);
    }
    pub fn retrying(&self, key: &str) {
        self.change(key, true, false);
    }
    pub fn recovered(&self, key: &str) {
        self.change(key, false, false);
    }
    fn change(&self, key: &str, failed: bool, connected: bool) {
        let mut state = self.state.lock().expect("channel status lock");
        state.0 |= connected;
        if failed {
            state.1.insert(key.into());
        } else {
            state.1.remove(key);
        }
        (self.update)(if !state.1.is_empty() {
            ChannelState::PlatformError
        } else if state.0 {
            ChannelState::Connected
        } else {
            ChannelState::Connecting
        });
    }
}
