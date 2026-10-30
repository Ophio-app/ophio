// SPDX-License-Identifier: Apache-2.0
//! Holding a conversation's agent to the folders and internet the person let
//! it reach, by the computer rather than by asking the agent. The reach can
//! change while the agent works: what it gains it can use at once, and what
//! it loses it loses at once, or the turn is stopped.
use crate::harness::{HarnessError, RunEnv};
use async_trait::async_trait;
use protocol::capability::{Availability, HarnessKind};
use std::ffi::OsString;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// What a run's agent may reach, in folders on the computer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reach {
    /// Folders it may read and not change.
    pub read: Vec<PathBuf>,
    /// Folders it may change, and run what is in them.
    pub write: Vec<PathBuf>,
    /// Everything on the computer may be read.
    pub whole_read: bool,
    /// Everything on the computer may be changed.
    pub whole_write: bool,
    /// Any internet host, rather than its provider's alone.
    pub internet: bool,
    /// With the internet, the person's own networks too, such as their
    /// tailnet: private addresses other than this computer's loopback.
    pub local_network: bool,
}

impl Reach {
    /// Nothing is held back, so there is nothing to hold the agent in.
    pub fn unrestricted(&self) -> bool {
        self.whole_read && self.whole_write && self.internet
    }

    /// Whether the agent may change anything.
    pub fn writes(&self) -> bool {
        self.whole_write || !self.write.is_empty()
    }
}

/// Called with what the agent was kept from.
pub type OnBlocked = Arc<dyn Fn(Blocked) + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Blocked {
    /// An internet host while the internet is off.
    Host(String),
    /// A host on the person's own network, such as their tailnet, while
    /// only the internet is on.
    Local(String),
}

/// The provider whose model endpoints a relay serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayProvider {
    Anthropic,
    OpenAi,
}

/// Limits apply to admitted requests and observed provider usage. A deadline
/// also ends requests already in flight.
#[derive(Debug, Clone)]
pub struct RelayBudget {
    pub max_requests: u64,
    pub max_input_tokens: u64,
    pub max_output_tokens: u64,
    pub deadline: Option<std::time::SystemTime>,
}

#[derive(Debug, Clone, Default)]
pub struct RelayPolicy {
    pub allowed_models: Option<Vec<String>>,
    /// Checks max_tokens (Anthropic) or max_output_tokens (OpenAI).
    pub max_output_tokens: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelaySignal {
    HijackAttempt,
    BudgetSpent,
}
pub type OnRelaySignal = Arc<dyn Fn(RelaySignal) + Send + Sync>;
/// Return true to refuse a reassembled request body before forwarding it.
pub type InspectRelayBody = Arc<dyn Fn(&[u8]) -> bool + Send + Sync>;

#[derive(Clone)]
pub struct RelaySpec {
    pub provider: RelayProvider,
    /// Read only by the host relay. Never mounted in the sandbox.
    pub key_file: PathBuf,
    pub budget: RelayBudget,
    pub policy: RelayPolicy,
    pub log: PathBuf,
    pub on_signal: Option<OnRelaySignal>,
    pub inspect: Option<InspectRelayBody>,
    /// Explicit loopback fake provider for containment tests. Never set by user config.
    pub test_upstream: Option<SocketAddr>,
}
impl std::fmt::Debug for RelaySpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RelaySpec")
            .field("provider", &self.provider)
            .field("budget", &self.budget)
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RelayTotals {
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// The only network service reachable from a relay sandbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelayEndpoint {
    pub provider: RelayProvider,
    pub port: u16,
}
impl RelayEndpoint {
    pub fn base_url(self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

/// One run's sandbox, made before the run starts.
#[derive(Clone)]
pub struct SandboxSpec {
    /// No proxy, relay or outbound socket is mounted, including provider access.
    pub offline: bool,
    pub reach: Reach,
    pub harness: HarnessKind,
    /// Where the run starts, which it may always read.
    pub cwd: PathBuf,
    /// The conversation's own folder in Øphio's data, kept across its
    /// turns: its agent's history, apart from every other conversation's.
    pub state: PathBuf,
    /// Folders and files the run needs whatever its reach, writable: the
    /// task's scratch folder and the tool bridge's socket.
    pub always: Vec<PathBuf>,
    /// Programs the agent starts besides its own, such as the tool
    /// bridge's, which it may run wherever they are installed.
    pub programs: Vec<PathBuf>,
    /// Candidate folders which may be granted during this run. Nested changes
    /// may require restarting the run.
    pub candidates: Vec<PathBuf>,
    /// Host application data that must be masked, even for whole-computer reads.
    pub private: Vec<PathBuf>,
    /// Files and folders the run's accounts name, such as a command-line
    /// tool's sign-in folder, which it may read wherever they are, even
    /// inside `private`.
    pub given: Vec<PathBuf>,
    pub on_blocked: Option<OnBlocked>,
    /// Model-only egress with no provider credential inside the sandbox.
    pub relay: Option<RelaySpec>,
}

impl std::fmt::Debug for SandboxSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SandboxSpec")
            .field("reach", &self.reach)
            .field("harness", &self.harness)
            .field("cwd", &self.cwd)
            .field("state", &self.state)
            .field("always", &self.always)
            .field("programs", &self.programs)
            .field("given", &self.given)
            .finish_non_exhaustive()
    }
}

/// The command that runs an agent inside its sandbox: `program`, then
/// `args`, then the agent's own program and arguments.
#[derive(Debug, Clone)]
pub struct Wrapped {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    /// Set on the command after everything else.
    pub env: Vec<(String, String)>,
    /// Clear the inherited environment and account overrides before applying env.
    pub clear_env: bool,
}

/// What the harness gives [`Sandbox::wrap`].
#[derive(Debug, Clone, Copy)]
pub struct WrapRequest<'a> {
    /// The agent's own program.
    pub program: &'a Path,
    /// Files the harness made for the run, such as its tool configuration
    /// and images, which the agent reads.
    pub visible: &'a [PathBuf],
    /// The run's environment from the computer's accounts, which can name
    /// the agent's own folder.
    pub env: &'a RunEnv,
    /// The native session the run resumes.
    pub resume: Option<&'a str>,
}

/// Whether a change of reach reached the running agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Applied {
    /// The agent works with the new reach from now on.
    Applied,
    /// The agent can't be moved to it while it runs; stop the turn, and the
    /// next one starts with it. Says why.
    NeedsRestart(String),
}

#[async_trait]
pub trait Sandboxes: Send + Sync + std::fmt::Debug {
    /// Whether this computer can hold agents in, and if not, what the
    /// person can do about it.
    fn availability(&self) -> Availability;
    fn create(&self, spec: SandboxSpec) -> Result<Arc<dyn Sandbox>, HarnessError>;
}

#[async_trait]
pub trait Sandbox: Send + Sync + std::fmt::Debug {
    fn relay(&self) -> Option<RelayEndpoint> {
        None
    }
    fn relay_totals(&self) -> Option<RelayTotals> {
        None
    }
    /// The command line that starts the agent's program held in.
    fn wrap(&self, request: WrapRequest<'_>) -> Result<Wrapped, HarnessError>;
    /// Lets the wrapped command keep what the sandbox hands it. Call before
    /// spawning, and [`Sandbox::started`] once it has.
    fn prepare(&self, command: &mut std::process::Command) -> std::io::Result<()>;
    /// Finishes setting the sandbox up and lets the agent's program run.
    /// On an error, end the process the command started.
    async fn started(&self) -> Result<(), HarnessError>;
    /// Moves the running agent to `reach`.
    async fn update(&self, reach: Reach) -> Applied;
    /// Record the native identity, so only this conversation's transcript is
    /// persisted after its processes end.
    fn session_started(&self, _id: &str) -> Result<(), HarnessError> {
        Ok(())
    }
    /// Called after containment confirms all run processes have ended.
    /// Persists the resumed native history, if any.
    async fn finished(&self) -> Result<(), HarnessError> {
        Ok(())
    }
    /// A connection to a server the agent runs in the sandbox, such as the
    /// page it shows the person, at the first of `addresses` on the
    /// sandbox's own loopback that answers. The sandbox's network is its
    /// own, so the computer's loopback doesn't reach that server.
    async fn connect(&self, _addresses: &[SocketAddr]) -> Result<TcpStream, HarnessError> {
        Err(HarnessError::Unsupported(
            "This sandbox can't be reached from the computer".into(),
        ))
    }
}

/// For a run whose spec has no sandbox: what [`Sandbox::wrap`] would leave.
pub fn unwrapped(program: &Path) -> Wrapped {
    Wrapped {
        program: program.to_path_buf(),
        args: Vec::new(),
        env: Vec::new(),
        clear_env: false,
    }
}
