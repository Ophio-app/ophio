// SPDX-License-Identifier: Apache-2.0
//! Local-owner voice installation and bot secret storage.
use crate::{
    Error, Result,
    config::{Config, Paths},
    local::{OwnerRequest, OwnerResponse},
};
use async_trait::async_trait;
use clap::{Subcommand, ValueEnum};
use ports::{COMMAND, PRODUCT_NAME};
use protocol::{auth::ChannelKind, capability::Availability};
use serde::{Deserialize, Serialize};
use std::{path::Path, sync::Arc};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Connector {
    /// Your Telegram bot.
    Telegram,
    /// Your Discord bot.
    Discord,
}
impl Connector {
    pub fn name(self) -> &'static str {
        match self {
            Self::Telegram => "telegram",
            Self::Discord => "discord",
        }
    }
    pub fn kind(self) -> ChannelKind {
        match self {
            Self::Telegram => ChannelKind::Telegram,
            Self::Discord => ChannelKind::Discord,
        }
    }
    pub fn config(self, config: &mut channels::Config) -> &mut channels::ChannelConfig {
        match self {
            Self::Telegram => &mut config.telegram,
            Self::Discord => &mut config.discord,
        }
    }
}
impl From<ChannelKind> for Connector {
    fn from(value: ChannelKind) -> Self {
        match value {
            ChannelKind::Telegram => Self::Telegram,
            ChannelKind::Discord => Self::Discord,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, Subcommand)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum VoiceCommand {
    /// Download, check and select a speech model.
    ///
    /// Shows download progress. Ctrl-C cancels and removes the partial
    /// download. New voice sessions use the model; current ones finish with
    /// the model they started with.
    Install {
        /// tiny.en, base.en or small.en.
        model: String,
    },
    /// Delete an installed speech model.
    ///
    /// The selection stays the same. If you remove the selected model, voice
    /// input needs setup until you install it again or use another.
    Remove {
        /// tiny.en, base.en or small.en.
        model: String,
    },
    /// Choose the speech model voice input uses. Install it first.
    Use {
        /// tiny.en, base.en or small.en.
        model: String,
    },
    /// Show the selected speech model and whether voice input is ready.
    Status,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Exposure {
    /// Post task states only. Approval details stay in the app.
    Minimal,
    /// Also post task titles, summaries and approval details, which the chat
    /// platform and everyone in the chat can read.
    Titles,
}
#[derive(Debug, Clone, Serialize, Deserialize, Subcommand)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ChannelCommand {
    /// Turn a chat connector on.
    ///
    /// It connects once its bot token is stored with
    /// `ophio channels token set`.
    #[command(after_help = "Example:
  ophio channels enable telegram")]
    Enable {
        /// Which bot.
        connector: Connector,
    },
    /// Turn a chat connector off. Its token and chat mappings are kept.
    Disable {
        /// Which bot.
        connector: Connector,
    },
    /// Send a chat's messages to a project.
    ///
    /// Each group, channel and Discord thread needs its own mapping. You can
    /// also choose the project in the app, on the chat's card in
    /// Connections › Chat.
    #[command(after_help = "Examples:
  ophio channels map telegram 123456789 <PROJECT-ID>          A private chat
  ophio channels map telegram -1001234567890 <PROJECT-ID>     A group; its ID starts with a minus sign
  ophio channels map discord 112233445566778899 <PROJECT-ID>  A Discord channel or thread")]
    Map {
        /// Which bot.
        connector: Connector,
        /// The chat, group or channel ID from the chat platform.
        #[arg(allow_hyphen_values = true)]
        chat_id: String,
        /// The project's ID, from `ophio projects list`.
        project_id: protocol::ProjectId,
    },
    /// Stop sending a chat's messages to a project.
    Unmap {
        /// Which bot.
        connector: Connector,
        /// The chat, group or channel ID from the chat platform.
        #[arg(allow_hyphen_values = true)]
        chat_id: String,
    },
    /// Choose how much task detail the bot posts in chats.
    Exposure {
        /// Which bot.
        connector: Connector,
        /// How much the bot posts. The default is minimal.
        exposure: Exposure,
    },
    /// Show each connector's state and what to do next.
    Status,
}
#[derive(Debug, Subcommand)]
pub enum TokenCommand {
    /// Store a bot's access token on this computer.
    ///
    /// Prompts without showing what you type, or reads one line from standard
    /// input. The token is never a command-line argument. If the connector is
    /// on, it reconnects with the new token.
    Set {
        /// Which bot.
        connector: Connector,
    },
    /// Delete a bot's stored access token. The connector stops until a new
    /// token is stored.
    Clear {
        /// Which bot.
        connector: Connector,
    },
}
#[derive(Debug, Subcommand)]
pub enum ChannelsCli {
    #[command(flatten)]
    Settings(ChannelCommand),
    /// Store or delete a bot's access token.
    #[command(after_help = "Examples:
  ophio channels token set telegram                   Type or paste the token when asked
  ophio channels token set discord < discord-token    Read the token from a file
  ophio channels token clear telegram                 Delete the stored token")]
    Token {
        #[command(subcommand)]
        action: TokenCommand,
    },
}
#[derive(Serialize, Deserialize, zeroize::Zeroize, zeroize::ZeroizeOnDrop)]
#[serde(transparent)]
pub struct TokenInput(String);
impl std::fmt::Debug for TokenInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TokenInput([redacted])")
    }
}
impl TokenInput {
    pub(crate) fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }
    pub fn new(value: String) -> Result<Self> {
        let token = Self(value);
        Self::validate(&token.0)?;
        Ok(token)
    }
    fn validate(value: &str) -> Result<()> {
        // Validate by reference; no temporary plaintext String escapes zeroization.
        if value.is_empty()
            || value.len() > 512
            || !value
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b":._-".contains(&c))
        {
            return Err(Error::Config("Invalid bot token format".into()));
        }
        Ok(())
    }
    pub fn read_stdin() -> Result<Self> {
        use std::io::IsTerminal;
        if std::io::stdin().is_terminal() {
            Self::new(rpassword::prompt_password("Access token (hidden): ")?)
        } else {
            Self::read_pipe(std::io::stdin().lock())
        }
    }
    fn read_pipe(reader: impl std::io::BufRead) -> Result<Self> {
        use std::io::BufRead;
        // No reallocation of partially read token bytes within the size limit.
        let mut bytes = zeroize::Zeroizing::new(Vec::with_capacity(514));
        reader.take(514).read_until(b'\n', &mut bytes)?;
        if bytes.len() > 513 {
            return Err(Error::Config("Bot token is too long".into()));
        }
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| Error::Config("Invalid bot token format".into()))?;
        Self::new(text.trim_end_matches(['\r', '\n']).to_owned())
    }
}
#[derive(Debug, Clone)]
pub struct FileTokens {
    paths: Paths,
}
impl FileTokens {
    pub fn new(paths: &Paths) -> Self {
        Self {
            paths: paths.clone(),
        }
    }
    fn path(&self, connector: Connector) -> std::path::PathBuf {
        self.paths
            .data
            .join("secrets")
            .join(format!("{}.token", connector.name()))
    }
    pub fn set(&self, connector: Connector, token: &TokenInput) -> Result<()> {
        use crate::identity::SecretStore;
        TokenInput::validate(&token.0)?;
        crate::identity::FileSecretStore::new(self.path(connector)).save(token.0.as_bytes(), true)
    }
    pub fn clear(&self, connector: Connector) -> Result<()> {
        let path = self.path(connector);
        crate::identity::private_dir(path.parent().ok_or(Error::Identity)?)?;
        match std::fs::remove_file(&path) {
            Ok(()) => {
                #[cfg(unix)]
                std::fs::File::open(path.parent().ok_or(Error::Identity)?)?.sync_all()?;
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}
#[async_trait]
impl channels::TokenSource for FileTokens {
    async fn token(&self, channel: ChannelKind) -> channels::Result<Option<channels::BotToken>> {
        use crate::identity::SecretStore;
        let path = self.path(channel.into());
        tokio::task::spawn_blocking(move || {
            match crate::identity::FileSecretStore::new(path).load() {
                Ok(Some(bytes)) => {
                    let text =
                        std::str::from_utf8(&bytes).map_err(|_| channels::Error::TokenStore)?;
                    TokenInput::validate(text).map_err(|_| channels::Error::TokenStore)?;
                    channels::BotToken::new(text.to_owned()).map(Some)
                }
                Ok(None) => Ok(None),
                Err(_) => Err(channels::Error::TokenStore),
            }
        })
        .await
        .map_err(|_| channels::Error::TokenStore)?
    }
}

pub fn install_remedy() -> String {
    format!("{COMMAND} voice install {}", speech::DEFAULT_MODEL)
}
pub fn missing_voice() -> Availability {
    Availability::PermissionMissing {
        remedy: install_remedy(),
    }
}
pub fn model(name: &str) -> Result<&'static speech::CatalogEntry> {
    speech::catalog()
        .iter()
        .find(|entry| entry.name == name)
        .ok_or_else(|| {
            Error::Config("Choose a reviewed voice model: tiny.en, base.en or small.en".into())
        })
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceProgress {
    pub model: String,
    pub file: String,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
}
#[async_trait]
pub trait SpeechFactory: Send + Sync {
    /// Called on a blocking worker. Implementations must verify the installation.
    fn open(&self, directory: &Path, name: &str) -> Result<Arc<dyn ports::SpeechEngine>>;
    async fn install(
        &self,
        directory: &Path,
        name: &str,
        progress: mpsc::Sender<OwnerResponse>,
    ) -> Result<()>;
}
#[derive(Debug)]
pub struct LocalSpeech;
#[async_trait]
impl SpeechFactory for LocalSpeech {
    fn open(&self, directory: &Path, name: &str) -> Result<Arc<dyn ports::SpeechEngine>> {
        speech::Installed::open_model(directory, name).map_err(|e| Error::Config(e.to_string()))
    }
    async fn install(
        &self,
        directory: &Path,
        name: &str,
        progress: mpsc::Sender<OwnerResponse>,
    ) -> Result<()> {
        speech::install(model(name)?, directory, |p| {
            // Progress is informational. A slow terminal cannot block installer cleanup.
            let _ = progress.try_send(OwnerResponse::VoiceProgress(VoiceProgress {
                model: p.model.into(),
                file: p.file.into(),
                downloaded_bytes: p.downloaded_bytes,
                total_bytes: p.total_bytes,
            }));
        })
        .await
        .map_err(|e| Error::Config(e.to_string()))?;
        Ok(())
    }
}
pub async fn open_speech(
    paths: &Paths,
    config: &Config,
    factory: Arc<dyn SpeechFactory>,
) -> Result<(Option<Arc<dyn ports::SpeechEngine>>, Availability)> {
    let directory = paths.data.join("voice");
    crate::identity::private_dir(&directory)?;
    let name = config.voice_model.clone();
    let engine =
        tokio::task::spawn_blocking(move || match directory.join(&name).symlink_metadata() {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            _ => factory.open(&directory, &name).map(Some),
        })
        .await
        .map_err(|_| Error::Config("Voice loader stopped unexpectedly".into()))??;
    let availability = match &engine {
        Some(engine) => engine.availability().await,
        None => missing_voice(),
    };
    Ok((engine, availability))
}
pub async fn apply_files(
    paths: &Paths,
    request: &OwnerRequest,
    factory: &dyn SpeechFactory,
    progress: mpsc::Sender<OwnerResponse>,
    cancel: CancellationToken,
) -> Result<()> {
    match request {
        OwnerRequest::Voice { action } => {
            let directory = paths.data.join("voice");
            crate::identity::private_dir(&directory)?;
            match action {
                VoiceCommand::Install { model: name } => {
                    model(name)?;
                    tokio::select! { biased; _ = cancel.cancelled() => return Err(Error::Config("Voice installation cancelled; staging files removed".into())), result = factory.install(&directory, name, progress) => result? }
                }
                VoiceCommand::Remove { model: name } => {
                    let entry = model(name)?;
                    tokio::task::spawn_blocking(move || speech::remove(entry, &directory))
                        .await
                        .map_err(|_| Error::Config("Voice removal worker failed".into()))?
                        .map_err(|e| Error::Config(e.to_string()))?;
                }
                VoiceCommand::Use { model: name } => {
                    model(name)?;
                }
                VoiceCommand::Status => {}
            }
        }
        OwnerRequest::TokenSet { connector, token } => {
            FileTokens::new(paths).set(*connector, token)?
        }
        OwnerRequest::TokenClear { connector } => FileTokens::new(paths).clear(*connector)?,
        _ => {}
    }
    Ok(())
}
pub fn apply_channel_config(config: &mut channels::Config, action: ChannelCommand) -> Result<()> {
    match action {
        ChannelCommand::Enable { connector } => connector.config(config).enabled = true,
        ChannelCommand::Disable { connector } => connector.config(config).enabled = false,
        ChannelCommand::Map {
            connector,
            chat_id,
            project_id,
        } => {
            connector
                .config(config)
                .projects
                .insert(chat_id, project_id);
        }
        ChannelCommand::Unmap { connector, chat_id } => {
            connector.config(config).projects.remove(&chat_id);
        }
        ChannelCommand::Exposure {
            connector,
            exposure,
        } => {
            connector.config(config).exposure = match exposure {
                Exposure::Minimal => channels::Exposure::Minimal,
                Exposure::Titles => channels::Exposure::Titles,
            }
        }
        ChannelCommand::Status => {}
    }
    config.validate().map_err(|e| Error::Config(e.to_string()))
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceStatus {
    pub model: String,
    pub availability: Availability,
    pub remedy: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrontdoorStatus {
    pub voice: VoiceStatus,
    pub channels: Vec<channels::ChannelStatus>,
    pub applied_live: bool,
    pub note: String,
}
pub fn status(
    config: &Config,
    availability: Availability,
    channels: Vec<channels::ChannelStatus>,
    live: bool,
) -> FrontdoorStatus {
    let remedy = match &availability {
        Availability::PermissionMissing { remedy } => Some(remedy.clone()),
        Availability::Unsupported { reason } => Some(reason.clone()),
        _ => None,
    };
    FrontdoorStatus {
        voice: VoiceStatus {
            model: config.voice_model.clone(),
            availability,
            remedy,
        },
        channels,
        applied_live: live,
        note: if live {
            "Voice changes apply to new sessions; current sessions finish with their loaded model."
                .into()
        } else {
            format!("{PRODUCT_NAME} applies the changes the next time it starts.")
        },
    }
}
pub fn channel_availability(status: &channels::ChannelStatus) -> Availability {
    let name = Connector::from(status.channel).name();
    match status.state {
        channels::ChannelState::Disabled => Availability::PermissionMissing {
            remedy: format!("{COMMAND} channels enable {name}"),
        },
        channels::ChannelState::TokenMissing => Availability::PermissionMissing {
            remedy: format!("{COMMAND} channels token set {name}"),
        },
        channels::ChannelState::Connected => Availability::Available,
        channels::ChannelState::Connecting => Availability::Unknown {
            reason: "Connecting to the chat platform".into(),
        },
        channels::ChannelState::Conflict | channels::ChannelState::PlatformError => {
            Availability::Unknown {
                reason: status
                    .remedy
                    .clone()
                    .unwrap_or_else(|| "Check the connector on the computer".into()),
            }
        }
    }
}
pub fn decorate_status(mut statuses: Vec<channels::ChannelStatus>) -> Vec<channels::ChannelStatus> {
    for s in &mut statuses {
        if let Availability::PermissionMissing { remedy } = channel_availability(s) {
            s.remedy = Some(remedy);
        }
    }
    statuses
}

pub async fn offline_channels(
    paths: &Paths,
    config: &Config,
) -> Result<Vec<channels::ChannelStatus>> {
    use channels::TokenSource;
    let source = FileTokens::new(paths);
    let mut statuses = Vec::new();
    for (connector, cfg) in [
        (Connector::Telegram, &config.channels.telegram),
        (Connector::Discord, &config.channels.discord),
    ] {
        let (state, remedy) = if !cfg.enabled {
            (
                channels::ChannelState::Disabled,
                format!("{COMMAND} channels enable {}", connector.name()),
            )
        } else if source
            .token(connector.kind())
            .await
            .map_err(|_| Error::Config("Cannot read the owner-only bot token file".into()))?
            .is_none()
        {
            (
                channels::ChannelState::TokenMissing,
                format!("{COMMAND} channels token set {}", connector.name()),
            )
        } else {
            (
                channels::ChannelState::PlatformError,
                format!("{COMMAND} run"),
            )
        };
        statuses.push(channels::ChannelStatus {
            channel: connector.kind(),
            state,
            remedy: Some(remedy),
        });
    }
    Ok(statuses)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::APP_NAME;
    #[tokio::test]
    async fn stored_tokens_round_trip_replace_clear_and_reject_invalid_contents() {
        use channels::TokenSource;
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths {
            data: dir.path().join("data"),
            config: dir.path().join("config"),
            runtime: dir.path().join("run"),
        };
        paths.initialize().unwrap();
        let store = FileTokens::new(&paths);
        assert!(store.token(ChannelKind::Discord).await.unwrap().is_none());
        for fixture in ["synthetic-first-token", "synthetic-replacement-token"] {
            let token = TokenInput::new(fixture.into()).unwrap();
            store.set(Connector::Discord, &token).unwrap();
            assert!(store.token(ChannelKind::Discord).await.unwrap().is_some());
            let bytes = crate::identity::protected_read(&store.path(Connector::Discord)).unwrap();
            #[cfg(windows)]
            {
                assert!(
                    !bytes
                        .windows(fixture.len())
                        .any(|b| b == fixture.as_bytes())
                );
                assert_eq!(
                    platform_windows::dpapi::unprotect(&bytes)
                        .unwrap()
                        .as_slice(),
                    fixture.as_bytes()
                );
            }
            #[cfg(unix)]
            assert_eq!(bytes.as_slice(), fixture.as_bytes());
        }
        crate::identity::atomic_write(
            &store.path(Connector::Discord),
            b"invalid token bytes",
            true,
        )
        .unwrap();
        assert!(store.token(ChannelKind::Discord).await.is_err());
        store.clear(Connector::Discord).unwrap();
        store.clear(Connector::Discord).unwrap();
        assert!(store.token(ChannelKind::Discord).await.unwrap().is_none());
    }
    #[test]
    fn piped_tokens_are_bounded_trimmed_and_validated_without_echoing_input() {
        for bytes in [
            b"synthetic-token".as_slice(),
            b"synthetic-token\n",
            b"synthetic-token\r\n",
        ] {
            assert_eq!(TokenInput::read_pipe(bytes).unwrap().0, "synthetic-token");
        }
        for bytes in [
            b"synthetic invalid".to_vec(),
            vec![0xff],
            vec![b'a'; 515],
            vec![],
        ] {
            let error = TokenInput::read_pipe(bytes.as_slice()).unwrap_err();
            assert!(!format!("{error:?}").contains("synthetic"));
        }
    }

    #[test]
    fn token_input_redacts_debug_and_zeroizes_with_a_drop_contract() {
        use zeroize::Zeroize;
        fn require_drop<T: zeroize::ZeroizeOnDrop>() {}
        require_drop::<TokenInput>();
        let mut token = TokenInput::new("synthetic-token".into()).unwrap();
        assert_eq!(format!("{token:?}"), "TokenInput([redacted])");
        token.zeroize();
        assert!(token.0.is_empty());
        let request = OwnerRequest::TokenSet {
            connector: Connector::Discord,
            token: TokenInput::new("synthetic-token".into()).unwrap(),
        };
        assert!(!format!("{request:?}").contains("synthetic-token"));
    }

    #[test]
    fn token_validation_matches_channel_boundary_without_copying_plaintext() {
        for fixture in [
            "",
            "synthetic:token._-12",
            "white space",
            "\n",
            "unicode-\u{00e9}",
        ] {
            assert_eq!(
                TokenInput::new(fixture.into()).is_ok(),
                channels::BotToken::new(fixture.into()).is_ok()
            );
        }
        assert!(TokenInput::new("a".repeat(512)).is_ok());
        assert!(TokenInput::new("a".repeat(513)).is_err());
    }
    #[test]
    fn cli_accepts_negative_group_ids_and_never_accepts_token_arguments() {
        use clap::Parser;
        let project = protocol::ProjectId::new().to_string();
        assert!(
            crate::cli::Cli::try_parse_from([
                APP_NAME, "channels", "map", "telegram", "-10042", &project
            ])
            .is_ok()
        );
        assert!(
            crate::cli::Cli::try_parse_from([APP_NAME, "channels", "unmap", "telegram", "-10042"])
                .is_ok()
        );
        assert!(
            crate::cli::Cli::try_parse_from([
                APP_NAME,
                "channels",
                "token",
                "set",
                "telegram",
                "synthetic-token"
            ])
            .is_err()
        );
    }
    #[test]
    fn config_rejects_unknown_channel_fields_and_untrusted_plaintext_endpoints() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        for text in [
            "[channels.telegram]\ntoken = 'must-not-be-config'\n",
            "[channels.telegram]\nenabeld = true\n",
            "[channels.telegram.endpoints]\ntelegram_api = 'http://example.invalid'\n",
            "voice_model = '../other-directory'\n",
        ] {
            std::fs::write(&path, text).unwrap();
            assert!(Config::load(&path).is_err());
        }
        std::fs::write(
            &path,
            "[channels.telegram.endpoints]\ntelegram_api = 'http://127.0.0.1:5555'\n",
        )
        .unwrap();
        assert!(Config::load(&path).is_ok());
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn token_source_rejects_unprotected_files_and_links_without_disclosing_token() {
        use channels::TokenSource;
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths {
            data: dir.path().join("data"),
            config: dir.path().join("config"),
            runtime: dir.path().join("run"),
        };
        paths.initialize().unwrap();
        let tokens = FileTokens::new(&paths);
        assert!(tokens.token(ChannelKind::Discord).await.unwrap().is_none());
        let value = TokenInput::new("synthetic_secret".into()).unwrap();
        assert!(!format!("{value:?}").contains("synthetic_secret"));
        tokens.set(Connector::Discord, &value).unwrap();
        let path = tokens.path(Connector::Discord);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let error = tokens.token(ChannelKind::Discord).await.unwrap_err();
        assert!(!error.to_string().contains("synthetic_secret"));
        assert!(matches!(error, channels::Error::TokenStore));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let other = paths.data.join("other");
        std::fs::rename(&path, &other).unwrap();
        std::os::unix::fs::symlink(&other, &path).unwrap();
        assert!(tokens.token(ChannelKind::Discord).await.is_err());
        std::fs::remove_file(&path).unwrap();
        std::fs::hard_link(&other, &path).unwrap();
        assert!(tokens.token(ChannelKind::Discord).await.is_err());
    }
}
