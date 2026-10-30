// SPDX-License-Identifier: Apache-2.0
use crate::{Error, Result};
use protocol::{ProjectId, auth::ChannelKind};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub telegram: ChannelConfig,
    pub discord: ChannelConfig,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ChannelConfig {
    pub enabled: bool,
    /// Exact external chat/channel id to project id. No implicit group inheritance.
    pub projects: BTreeMap<String, ProjectId>,
    pub exposure: Exposure,
    pub endpoints: Endpoints,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Exposure {
    #[default]
    Minimal,
    Titles,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Endpoints {
    pub telegram_api: String,
    pub discord_api: String,
    pub discord_gateway: String,
}
impl Default for Endpoints {
    fn default() -> Self {
        Self {
            telegram_api: "https://api.telegram.org".into(),
            discord_api: "https://discord.com/api/v10".into(),
            discord_gateway: "wss://gateway.discord.gg".into(),
        }
    }
}
impl Config {
    pub fn validate(&self) -> Result<()> {
        for cfg in [&self.telegram, &self.discord] {
            for chat in cfg.projects.keys() {
                if chat.is_empty()
                    || chat.len() > 32
                    || !chat
                        .bytes()
                        .enumerate()
                        .all(|(i, b)| b.is_ascii_digit() || (i == 0 && b == b'-'))
                {
                    return Err(Error::Configuration(
                        "chat ids must be numeric platform ids",
                    ));
                }
            }
            validate_endpoint(&cfg.endpoints.telegram_api, false)?;
            validate_endpoint(&cfg.endpoints.discord_api, false)?;
            validate_endpoint(&cfg.endpoints.discord_gateway, true)?;
        }
        Ok(())
    }
}
pub(crate) fn validate_endpoint(value: &str, websocket: bool) -> Result<()> {
    let u = reqwest::Url::parse(value).map_err(|_| Error::Configuration("invalid endpoint"))?;
    let secure = if websocket { "wss" } else { "https" };
    let insecure = if websocket { "ws" } else { "http" };
    let loopback = u
        .host_str()
        .and_then(|h| h.trim_matches(['[', ']']).parse::<std::net::IpAddr>().ok())
        .is_some_and(|ip| ip.is_loopback());
    if (u.scheme() != secure && !(u.scheme() == insecure && loopback))
        || !u.username().is_empty()
        || u.password().is_some()
        || u.query().is_some()
        || u.fragment().is_some()
        || u.host_str().is_none()
    {
        return Err(Error::Configuration(
            "endpoints require TLS (numeric loopback is permitted for tests)",
        ));
    }
    Ok(())
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelState {
    Disabled,
    Connecting,
    Connected,
    TokenMissing,
    Conflict,
    PlatformError,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelStatus {
    pub channel: ChannelKind,
    pub state: ChannelState,
    pub remedy: Option<String>,
}
impl ChannelStatus {
    pub(crate) fn new(channel: ChannelKind, state: ChannelState) -> Self {
        let remedy = match state {
            ChannelState::Disabled => Some("Enable this chat front door in Øphio's settings on the computer."),
            ChannelState::TokenMissing => Some("Create your own bot and paste its token into Øphio's secret store on the computer, never into chat."),
            ChannelState::Conflict => Some("Use a dedicated bot. Review the other program or webhook yourself, then restart this connector. Øphio does not remove webhooks or stop other programs."),
            ChannelState::PlatformError => Some("Check the network, bot token, bot permissions and local state directory, then restart the connector."),
            _ => None,
        }.map(str::to_owned);
        Self {
            channel,
            state,
            remedy,
        }
    }
}
