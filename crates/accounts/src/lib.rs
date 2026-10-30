// SPDX-License-Identifier: Apache-2.0
//! The computer's accounts: signed in once on the computer, checked with each
//! service's own status command, and given to every task it starts.
//!
//! Keys and refresh tokens live in the [`vault`]; tools keep their own logins
//! in their own stores (agents) or in a private folder per account (other
//! command-line tools). The phone sees names, identities and states.
pub mod check;
pub mod desktop;
mod files;
pub mod github;
pub mod logins;
pub mod oauth;
pub mod profiles;
mod signin;
pub mod store;
pub mod tool;
pub mod totp;
pub mod vault;

use check::Checked;
use desktop::Opener;
use ports::accounts::{Accounts, AccountsError, RunAccounts};
use profiles::{GOOGLE_CREDENTIALS, Method, Profile};
use protocol::TaskId;
use protocol::accounts::{
    Account, AccountAction, AccountEvent, AccountHistoryEntry, AccountKind, AccountNeed,
    AccountState, AccountsView, Secret, ServiceProfile, SignIn, SignInStage,
};
use protocol::capability::HarnessKind;
use protocol::time::Timestamp;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use store::{Kept, Saved, Status, Store};
use tokio::sync::watch;
use tool::Tools;
use vault::{Item, Vault};
use zeroize::Zeroizing;

/// The history the phone is shown; more is kept on the computer.
const SHOWN_HISTORY: usize = 50;
const SIGN_OUT_LIMIT: Duration = Duration::from_secs(30);
const TOKEN_LIMIT: Duration = Duration::from_secs(15);
/// An access token this close to expiring is refreshed before a task gets it.
const FRESH_FOR: Duration = Duration::from_secs(5 * 60);
/// How long Sign out waits for a sign-in it cancelled to end.
const CANCEL_LIMIT: Duration = Duration::from_secs(10);
/// Values no longer held that are still masked; expired ones go first, then
/// the oldest.
const REMEMBERED: usize = 1024;
/// What else is held for masking, beside the vault's [`Item`]s.
const ACCESS: &str = "access";
const TOOL: &str = "tool";
/// A value about to be kept, held while it is written.
const INCOMING: &str = "incoming";

pub struct Config {
    /// `<data>/accounts`, private to the owner.
    pub dir: PathBuf,
    pub vault: Arc<dyn Vault>,
    pub opener: Arc<dyn Opener>,
    pub tools: Tools,
    pub http: reqwest::Client,
    pub sign_in_limit: Duration,
}
impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("dir", &self.dir)
            .field("vault", &self.vault)
            .finish_non_exhaustive()
    }
}
impl Config {
    /// The computer's keyring, browser and tools.
    pub async fn computer(data_dir: &Path) -> Self {
        let dir = data_dir.join("accounts");
        Self {
            vault: vault::open(&dir).await,
            dir,
            opener: Arc::new(desktop::Desktop),
            tools: Tools::default(),
            http: http(),
            sign_in_limit: signin::LIMIT,
        }
    }
}

pub fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("Ophio/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("the HTTP client builds")
}

#[derive(Clone)]
pub struct ComputerAccounts {
    shared: Arc<Shared>,
}
impl std::fmt::Debug for ComputerAccounts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ComputerAccounts")
            .field("dir", &self.shared.config.dir)
            .finish_non_exhaustive()
    }
}

struct Shared {
    config: Config,
    store: Store,
    browser: Option<String>,
    state: Mutex<State>,
    /// Held while a key or token is written to or removed from the vault, so
    /// a write begun before a sign-out can't land after its removal.
    writes: tokio::sync::Mutex<()>,
    /// One refresh at a time for each account: a provider that rotates
    /// refresh tokens refuses the second use of one.
    refreshing: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    changed: watch::Sender<u64>,
}

#[derive(Default)]
struct State {
    saved: Saved,
    sign_in: Option<SignIn>,
    running: Option<Running>,
    used_by: BTreeMap<String, BTreeSet<TaskId>>,
    runs: HashMap<TaskId, Vec<String>>,
    /// Tokens fetched for runs and masking: a tool's token, an access token.
    tokens: HashMap<String, Cached>,
    /// Every value read or kept since start, by account and what it is, with
    /// when it stops working if that is known. Masked in every run while held.
    held: HashMap<(String, &'static str), (String, Option<Instant>)>,
    /// Values held before and since replaced or let go, oldest first.
    replaced: VecDeque<(String, Option<Instant>)>,
    /// Both, as every run masks them.
    seen: Arc<Vec<String>>,
    /// Accounts being signed out or deleted: nothing else starts on them.
    stopping: HashSet<String>,
    /// Moved on by each sign-out, at its start and its end. A check, refresh
    /// or sign-in that began under an earlier one keeps none of its results.
    epochs: HashMap<String, u64>,
    /// Moved on when an account's sign-in or OAuth client is replaced. A
    /// refresh that began under an earlier one keeps none of its results.
    generations: HashMap<String, u64>,
    /// A renewed refresh token the vault couldn't keep, used in place of the
    /// one it has until it can be kept.
    unsaved: HashMap<String, Zeroizing<String>>,
}
impl State {
    /// Holds `value` as `id`'s `what`: masked in every run from now on.
    fn hold(&mut self, id: &str, what: &'static str, value: &str, until: Option<Instant>) {
        let held = (value.to_owned(), until);
        match self.held.insert((id.to_owned(), what), held) {
            Some((old, _)) if old == value => return,
            Some(old) => self.replaced.push_back(old),
            None => {}
        }
        self.seen_changed();
    }
    /// Holds `value` about to be kept as `id`'s `what`, before it is written
    /// where a task could read it; the old value stays held until [`State::kept`].
    fn holding(&mut self, id: &str, value: &str) {
        self.held
            .insert((id.to_owned(), INCOMING), (value.to_owned(), None));
        self.seen_changed();
    }
    /// The value from [`State::holding`] was kept as `what`, or wasn't.
    fn kept(&mut self, id: &str, what: &'static str, value: Option<&str>) {
        match value {
            Some(value) => {
                self.held.remove(&(id.to_owned(), INCOMING));
                self.hold(id, what, value, None);
            }
            None => self.let_go(id, &[INCOMING]),
        }
    }
    /// Lets go of `id`'s `what`, each still masked as replaced.
    fn let_go(&mut self, id: &str, what: &[&'static str]) {
        for what in what {
            if let Some(old) = self.held.remove(&(id.to_owned(), *what)) {
                self.replaced.push_back(old);
            }
        }
        self.seen_changed();
    }
    /// What is held is never dropped; of what was replaced, expired values go
    /// first, then the oldest.
    fn seen_changed(&mut self) {
        if self.replaced.len() > REMEMBERED {
            let now = Instant::now();
            self.replaced
                .retain(|(_, until)| until.is_none_or(|until| until > now));
        }
        while self.replaced.len() > REMEMBERED {
            self.replaced.pop_front();
        }
        let mut seen: Vec<String> = self
            .held
            .values()
            .chain(&self.replaced)
            .map(|(value, _)| value.clone())
            .collect();
        // Longest first, the order masking needs; equal values stay together
        // to be dropped.
        seen.sort_unstable_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
        seen.dedup();
        self.seen = Arc::new(seen);
    }
    fn epoch(&self, id: &str) -> u64 {
        self.epochs.get(id).copied().unwrap_or(0)
    }
    fn next_epoch(&mut self, id: &str) {
        *self.epochs.entry(id.to_owned()).or_default() += 1;
    }
    fn generation(&self, id: &str) -> u64 {
        self.generations.get(id).copied().unwrap_or(0)
    }
    /// Both: what a check or refresh began under, kept only if unchanged.
    fn fence(&self, id: &str) -> Fence {
        (self.epoch(id), self.generation(id))
    }
    /// `text` with every held or replaced value hidden, for what is shown,
    /// logged or kept.
    fn masked(&self, text: &str) -> String {
        let mut text = text.to_owned();
        ports::accounts::mask(&mut text, &self.seen);
        text
    }
    /// The account's sign-in, OAuth client or key was replaced.
    fn replaced_sign_in(&mut self, id: &str) {
        *self.generations.entry(id.to_owned()).or_default() += 1;
    }
}

/// Marks an account as being signed out until dropped.
struct Stopping<'a>(&'a ComputerAccounts, String);
impl Drop for Stopping<'_> {
    fn drop(&mut self) {
        self.0.lock().stopping.remove(&self.1);
    }
}

struct Running {
    account_id: String,
    cancel: watch::Sender<bool>,
}

/// An account's epoch and generation.
type Fence = (u64, u64);

struct Cached {
    value: Zeroizing<String>,
    until: Option<Instant>,
}

fn failed(e: impl std::fmt::Display) -> AccountsError {
    AccountsError::Failed(e.to_string())
}

fn stopping(profile: &Profile) -> AccountsError {
    AccountsError::Busy(format!(
        "{} is being signed out. Try again when it has finished.",
        profile.name
    ))
}

impl ComputerAccounts {
    pub fn new(config: Config) -> Self {
        let store = Store::new(&config.dir);
        let saved = store.load().unwrap_or_else(|| {
            // Which were paused is lost: none is used until the owner says.
            let mut saved = store::Saved::default();
            for profile in Self::profiles(&saved) {
                saved.accounts.entry(profile.id).or_default().paused = true;
            }
            saved.record(AccountHistoryEntry {
                at: Timestamp::now(),
                account_id: String::new(),
                service: "All accounts".into(),
                event: AccountEvent::PausedAll,
                by: "Ophio".into(),
                detail: Some(
                    "What Ophio had saved about accounts couldn't be read, so every account \
                     is paused. Resume the ones tasks should use."
                        .into(),
                ),
            });
            if let Err(e) = store.save(&saved) {
                tracing::warn!("accounts: could not save: {e}");
            }
            saved
        });
        // Whatever a run or a sign-in left behind when the companion stopped.
        let _ = std::fs::remove_dir_all(config.dir.join("runs"));
        let _ = std::fs::remove_dir_all(config.dir.join("signin"));
        let browser = config.opener.browser();
        Self {
            shared: Arc::new(Shared {
                config,
                store,
                browser,
                state: Mutex::new(State {
                    saved,
                    ..State::default()
                }),
                writes: tokio::sync::Mutex::new(()),
                refreshing: Mutex::default(),
                changed: watch::channel(0).0,
            }),
        }
    }

    fn config(&self) -> &Config {
        &self.shared.config
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.shared.state.lock().expect("accounts state poisoned")
    }

    fn bump(&self) {
        self.shared.changed.send_modify(|n| *n = n.wrapping_add(1));
    }

    fn save(&self, state: &State) -> Result<(), AccountsError> {
        self.shared.store.save(&state.saved).map_err(|e| {
            tracing::warn!("accounts: could not save: {e}");
            AccountsError::Failed(e.to_string())
        })
    }

    fn profiles(saved: &Saved) -> Vec<Profile> {
        let mut all = profiles::builtins();
        all.extend(saved.added.iter().cloned());
        all
    }

    fn profile(&self, id: &str) -> Result<Profile, AccountsError> {
        Self::profiles(&self.lock().saved)
            .into_iter()
            .find(|p| p.id == id)
            .ok_or_else(|| AccountsError::NotFound("There is no such account.".into()))
    }

    /// The account named as the owner would type it: its id or its name.
    pub fn find(&self, name: &str) -> Result<Profile, AccountsError> {
        Self::profiles(&self.lock().saved)
            .into_iter()
            .find(|p| p.id == name || p.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| AccountsError::NotFound(format!("There is no account called {name}.")))
    }

    fn config_dir(&self, id: &str) -> PathBuf {
        self.config().dir.join("config").join(id)
    }

    /// Whether what was just fetched for `profile` may still be handed out:
    /// it hasn't been paused, signed out or deleted since `epoch`.
    fn still_on(&self, profile: &Profile, epoch: u64) -> Result<(), AccountsError> {
        Self::on_in(&self.lock(), profile, epoch)
    }
    fn on_in(state: &State, profile: &Profile, epoch: u64) -> Result<(), AccountsError> {
        if state
            .saved
            .accounts
            .get(&profile.id)
            .is_some_and(|k| k.paused)
        {
            return Err(AccountsError::NotReady(format!(
                "{} is paused.",
                profile.name
            )));
        }
        if state.epoch(&profile.id) != epoch || state.stopping.contains(&profile.id) {
            return Err(AccountsError::NotReady(format!(
                "{} was signed out.",
                profile.name
            )));
        }
        Ok(())
    }

    /// A key or refresh token from the vault, held for masking as soon as it
    /// is read.
    async fn fetch(
        &self,
        id: &str,
        item: Item,
    ) -> Result<Option<Zeroizing<String>>, AccountsError> {
        let value = self.config().vault.get(id, item).await.map_err(failed)?;
        if let Some(value) = &value {
            self.lock().hold(id, item.name(), value, None);
        }
        Ok(value)
    }

    /// The account's refresh token: one the vault couldn't keep yet, or the
    /// vault's.
    async fn refresh_token(&self, id: &str) -> Result<Option<Zeroizing<String>>, AccountsError> {
        let unsaved = self.lock().unsaved.get(id).cloned();
        match unsaved {
            Some(refresh) => Ok(Some(refresh)),
            None => self.fetch(id, Item::Refresh).await,
        }
    }

    fn record(
        state: &mut State,
        profile: &Profile,
        event: AccountEvent,
        by: &str,
        detail: Option<String>,
    ) {
        let detail = detail.map(|d| state.masked(&d));
        state.saved.record(AccountHistoryEntry {
            at: Timestamp::now(),
            account_id: profile.id.clone(),
            service: profile.name.clone(),
            event,
            by: by.to_owned(),
            detail,
        });
    }

    fn account(&self, state: &State, profile: &Profile) -> Account {
        let kept = state
            .saved
            .accounts
            .get(&profile.id)
            .cloned()
            .unwrap_or_default();
        // Paused shows first, so it can be resumed; what needs attention
        // shows again once it is.
        let state_now = match &kept.status {
            _ if kept.paused => AccountState::Paused,
            Status::Attention { reason } => AccountState::NeedsAttention {
                reason: reason.clone(),
            },
            Status::SignedIn if state.unsaved.contains_key(&profile.id) => {
                AccountState::NeedsAttention {
                    reason: "Its renewed sign-in couldn't be saved, so it lasts only until Ophio \
                             stops. Unlock the keyring, then tap Check now."
                        .into(),
                }
            }
            Status::SignedIn => AccountState::Active,
            Status::SignedOut | Status::Unchecked => AccountState::SignedOut,
        };
        let needs = match profile.needs() {
            Some(AccountNeed::Client) if kept.client_set => None,
            other => other,
        };
        let note = kept
            .note
            .clone()
            .filter(|_| state_now == AccountState::Active);
        Account {
            account_id: profile.id.clone(),
            service: profile.name.clone(),
            kind: profile.kind(),
            builtin: profile.builtin,
            // Hidden again as shown, for what was held after it was kept.
            identity: kept.identity.as_deref().map(|i| state.masked(i)),
            state: state_now,
            checked_at: kept.checked_at,
            variables: profile.variables(),
            manage_url: profile.manage_url.clone(),
            needs,
            browser_sign_in: profile.browser_sign_in(),
            note,
            used_by: state
                .used_by
                .get(&profile.id)
                .map(|tasks| tasks.iter().copied().collect())
                .unwrap_or_default(),
        }
    }

    // ---- Checks -----------------------------------------------------------

    /// Runs the status checks of one account or all of them, now.
    pub async fn check(&self, account_id: Option<&str>) {
        let profiles: Vec<Profile> = Self::profiles(&self.lock().saved)
            .into_iter()
            .filter(|p| account_id.is_none_or(|id| p.id == id))
            .collect();
        let mut checks = tokio::task::JoinSet::new();
        for profile in profiles {
            let this = self.clone();
            checks.spawn(async move { this.check_one(&profile).await });
        }
        while checks.join_next().await.is_some() {}
    }

    /// Checks again every `every`, starting now.
    pub fn spawn_checks(&self, every: Duration) -> tokio::task::JoinHandle<()> {
        let this = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(every);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                this.check(None).await;
            }
        })
    }

    async fn check_one(&self, profile: &Profile) -> Checked {
        let fence = self.lock().fence(&profile.id);
        self.check_at(profile, fence).await
    }

    /// Checks `profile`, keeping what is found only if it hasn't been signed
    /// out or in again since `fence`.
    async fn check_at(&self, profile: &Profile, fence: Fence) -> Checked {
        let checked = match &profile.method {
            Method::Agent { .. } => check::tool(profile, &self.config().tools, Path::new("")).await,
            Method::Cli { .. } => {
                let dir = self.config_dir(&profile.id);
                if dir.is_dir() {
                    check::tool(profile, &self.config().tools, &dir).await
                } else {
                    Checked::Out
                }
            }
            Method::ApiKey { check_url, .. } => match self.fetch(&profile.id, Item::Key).await {
                Ok(key) => {
                    check::key(
                        &self.config().http,
                        check_url.as_deref(),
                        key.as_deref().map(|k| k.as_str()),
                    )
                    .await
                }
                Err(e) => Checked::Unknown(e.to_string()),
            },
            Method::Oauth { token_url, .. } => self.oauth_check(profile, token_url).await,
        };
        if let (Checked::In { .. }, Method::Cli { token: Some(_), .. }) =
            (&checked, &profile.method)
        {
            // Read again: a sign-in since may have replaced the token.
            self.lock().tokens.remove(&profile.id);
            let _ = self.tool_token(profile).await;
        }
        self.apply(profile, &checked, fence);
        checked
    }

    /// Keeps what a check found, unless the account was signed out, or
    /// signed in again, since `fence`, when the check began.
    fn apply(&self, profile: &Profile, checked: &Checked, fence: Fence) {
        let mut state = self.lock();
        if state.fence(&profile.id) != fence {
            return;
        }
        let previous = state
            .saved
            .accounts
            .get(&profile.id)
            .cloned()
            .unwrap_or_default();
        let mut kept = previous.clone();
        match checked {
            Checked::In { identity, note } => {
                kept.status = Status::SignedIn;
                kept.identity = identity.as_deref().map(|i| state.masked(i));
                kept.note = note.clone();
            }
            Checked::Attention(reason) => {
                kept.status = Status::Attention {
                    reason: state.masked(reason),
                }
            }
            Checked::Missing(reason) if previous.status == Status::SignedIn => {
                kept.status = Status::Attention {
                    reason: state.masked(reason),
                }
            }
            Checked::Out | Checked::Missing(_) => {
                kept.status = Status::SignedOut;
                kept.identity = None;
            }
            Checked::Unknown(reason) => {
                let reason = state.masked(reason);
                tracing::debug!("accounts: {} could not be checked: {reason}", profile.id);
                return;
            }
        }
        if kept.status != Status::SignedIn {
            state.tokens.remove(&profile.id);
            kept.note = None;
        }
        kept.checked_at = Some(Timestamp::now());
        state.saved.accounts.insert(profile.id.clone(), kept);
        let _ = self.save(&state);
        drop(state);
        self.bump();
    }

    async fn oauth_client(&self, id: &str) -> Result<Option<oauth::Client>, AccountsError> {
        let Some(json) = self
            .config()
            .vault
            .get(id, Item::Client)
            .await
            .map_err(failed)?
        else {
            return Ok(None);
        };
        let client = oauth::Client::parse(&json).map_err(AccountsError::Failed)?;
        if let Some(secret) = &client.secret {
            self.lock().hold(id, Item::Client.name(), secret, None);
        }
        Ok(Some(client))
    }

    async fn oauth_check(&self, profile: &Profile, token_url: &str) -> Checked {
        match self.access_token(profile, token_url, true).await {
            Ok(_) => Checked::In {
                identity: self
                    .lock()
                    .saved
                    .accounts
                    .get(&profile.id)
                    .and_then(|k| k.identity.clone()),
                note: None,
            },
            Err(Access::Out) => Checked::Out,
            Err(Access::Refused(reason)) => Checked::Attention(reason),
            Err(Access::Failed(reason)) => Checked::Unknown(reason),
        }
    }

    /// A fresh access token, refreshed if it is close to expiring (or
    /// `force`), with the refresh token rotated if the provider sent a new one.
    async fn access_token(
        &self,
        profile: &Profile,
        token_url: &str,
        force: bool,
    ) -> Result<Zeroizing<String>, Access> {
        let fresh = |state: &State| {
            state
                .tokens
                .get(&profile.id)
                .filter(|cached| cached.until.is_some_and(|u| u > Instant::now() + FRESH_FOR))
                .map(|cached| cached.value.clone())
        };
        let before = {
            let state = self.lock();
            match fresh(&state) {
                Some(cached) if !force => return Ok(cached),
                cached => cached,
            }
        };
        let one = self
            .shared
            .refreshing
            .lock()
            .expect("accounts refreshes poisoned")
            .entry(profile.id.clone())
            .or_default()
            .clone();
        let _refreshing = one.lock().await;
        let epoch = {
            let state = self.lock();
            // Refreshed while this waited: that token is used.
            if let Some(cached) = fresh(&state)
                && (!force || before.as_ref() != Some(&cached))
            {
                return Ok(cached);
            }
            state.fence(&profile.id)
        };
        let (epoch, generation) = epoch;
        // What the provider answered stands only for the sign-in it was asked
        // about: signed out since, nothing; signed in again, that sign-in's.
        let fenced = |answer: Result<Zeroizing<String>, Access>| {
            let state = self.lock();
            if state.epoch(&profile.id) != epoch {
                Err(Access::Out)
            } else if state.generation(&profile.id) != generation {
                fresh(&state).ok_or_else(|| {
                    Access::Failed("The sign-in changed while it was renewed. Try again.".into())
                })
            } else {
                answer
            }
        };
        let vault = &self.config().vault;
        let refresh = match self.refresh_token(&profile.id).await {
            Ok(Some(r)) => r,
            Ok(None) => return fenced(Err(Access::Out)),
            Err(e) => return fenced(Err(Access::Failed(e.to_string()))),
        };
        let client = match self.oauth_client(&profile.id).await {
            Ok(Some(c)) => c,
            Ok(None) => {
                return fenced(Err(Access::Refused(
                    "Set the OAuth client again, then sign in.".into(),
                )));
            }
            Err(e) => return fenced(Err(Access::Failed(e.to_string()))),
        };
        let tokens = match oauth::refresh(&self.config().http, token_url, &client, &refresh).await {
            Ok(t) => t,
            Err(oauth::OauthError::Refused(r)) => return fenced(Err(Access::Refused(r))),
            Err(oauth::OauthError::Failed(r)) => return fenced(Err(Access::Failed(r))),
        };
        let _writing = self.shared.writes.lock().await;
        // Signed out or in again while the provider answered: keep nothing.
        if self.lock().fence(&profile.id) != (epoch, generation) {
            return fenced(Err(Access::Out));
        }
        // A renewed refresh token, or one the vault couldn't keep before.
        let keep =
            (tokens.refresh.clone()).or_else(|| self.lock().unsaved.get(&profile.id).cloned());
        if let Some(refresh) = keep {
            self.lock()
                .hold(&profile.id, Item::Refresh.name(), &refresh, None);
            let kept = vault.set(&profile.id, Item::Refresh, &refresh).await;
            let mut state = self.lock();
            match kept {
                Ok(()) => {
                    state.unsaved.remove(&profile.id);
                }
                Err(e) => {
                    tracing::warn!(
                        "accounts: could not keep {}'s new refresh token: {e}",
                        profile.id
                    );
                    state.unsaved.insert(profile.id.clone(), refresh);
                }
            }
        }
        let mut state = self.lock();
        state.hold(&profile.id, ACCESS, &tokens.access, Some(tokens.expires_at));
        if let Some(identity) = tokens.identity.as_deref() {
            let identity = state.masked(identity);
            state
                .saved
                .accounts
                .entry(profile.id.clone())
                .or_default()
                .identity = Some(identity);
        }
        state.tokens.insert(
            profile.id.clone(),
            Cached {
                value: tokens.access.clone(),
                until: Some(tokens.expires_at),
            },
        );
        Ok(tokens.access)
    }

    /// The token a command-line tool prints, kept so runs can mask it.
    async fn tool_token(&self, profile: &Profile) -> Option<Zeroizing<String>> {
        let Method::Cli {
            token: Some(token),
            config_variable,
            ..
        } = &profile.method
        else {
            return None;
        };
        let fence = {
            let state = self.lock();
            if let Some(cached) = state.tokens.get(&profile.id) {
                return Some(cached.value.clone());
            }
            state.fence(&profile.id)
        };
        let extra = [(
            config_variable.clone(),
            OsString::from(self.config_dir(&profile.id)),
        )];
        let out = self
            .config()
            .tools
            .run(token, &extra, TOKEN_LIMIT)
            .await
            .ok()?;
        let value = Zeroizing::new(out.stdout.trim().to_owned());
        if !out.success || !ports::accounts::maskable(&value) {
            // A token masking could miss is given to no task.
            return None;
        }
        let mut state = self.lock();
        state.hold(&profile.id, TOOL, &value, None);
        let now = state.fence(&profile.id);
        if now != fence {
            // Signed out meanwhile, nothing is given; signed in again, only
            // what that sign-in's check read.
            return (now.0 == fence.0)
                .then(|| state.tokens.get(&profile.id).map(|c| c.value.clone()))
                .flatten();
        }
        state.tokens.insert(
            profile.id.clone(),
            Cached {
                value: value.clone(),
                until: None,
            },
        );
        Some(value)
    }

    // ---- Sign-in ----------------------------------------------------------

    async fn sign_in(&self, id: &str, by: &str) -> Result<(), AccountsError> {
        let profile = self.profile(id)?;
        match &profile.method {
            Method::ApiKey { .. } => {
                return Err(AccountsError::Invalid(format!(
                    "{} uses a key. Set its key instead.",
                    profile.name
                )));
            }
            Method::Oauth { .. } if self.oauth_client(id).await?.is_none() => {
                return Err(AccountsError::Invalid(format!(
                    "Set {}'s OAuth client first.",
                    profile.name
                )));
            }
            _ => {}
        }
        let (cancel, epoch) = {
            let mut state = self.lock();
            if state.running.is_some() {
                return Err(AccountsError::Busy(
                    "Another sign-in is running. Finish or cancel it first.".into(),
                ));
            }
            if state.stopping.contains(&profile.id) {
                return Err(stopping(&profile));
            }
            let (cancel, watching) = watch::channel(false);
            let epoch = state.epoch(&profile.id);
            state.running = Some(Running {
                account_id: profile.id.clone(),
                cancel,
            });
            state.sign_in = Some(SignIn {
                account_id: profile.id.clone(),
                service: profile.name.clone(),
                started_at: Timestamp::now(),
                stage: SignInStage::Starting,
            });
            (watching, epoch)
        };
        self.bump();
        let this = self.clone();
        let by = by.to_owned();
        tokio::spawn(async move {
            let ended = this.signing(&profile, cancel, epoch).await;
            this.finish_sign_in(&profile, ended, &by, epoch);
        });
        Ok(())
    }

    fn stage(&self, id: &str, stage: SignInStage) {
        let mut state = self.lock();
        if state.running.as_ref().is_some_and(|r| r.account_id == id)
            && let Some(sign_in) = state.sign_in.as_mut()
        {
            sign_in.stage = stage;
            drop(state);
            self.bump();
        }
    }

    async fn signing(
        &self,
        profile: &Profile,
        cancel: watch::Receiver<bool>,
        epoch: u64,
    ) -> SignInStage {
        let stage = |s: SignInStage| self.stage(&profile.id, s);
        let config = self.config();
        let (argv, pty, extra) = match &profile.method {
            Method::Agent { sign_in, pty, .. } => (sign_in, *pty, Vec::new()),
            Method::Cli {
                sign_in,
                config_variable,
                ..
            } => {
                let dir = self.config_dir(&profile.id);
                if let Err(e) = files::private_dir(&dir) {
                    return SignInStage::Failed {
                        reason: format!("Ophio could not prepare the sign-in: {e}"),
                    };
                }
                (
                    sign_in,
                    false,
                    vec![(config_variable.clone(), dir.into_os_string())],
                )
            }
            Method::Oauth {
                authorize_url,
                token_url,
                scopes,
                ..
            } => {
                return self
                    .oauth_sign_in(
                        profile,
                        (authorize_url, token_url, scopes),
                        &stage,
                        cancel,
                        epoch,
                    )
                    .await;
            }
            Method::ApiKey { .. } => unreachable!("refused before starting"),
        };
        let ended = signin::run(
            signin::Run {
                argv,
                extra,
                pty,
                tools: &config.tools,
                opener: config.opener.as_ref(),
                scratch: config.dir.join("signin"),
                limit: config.sign_in_limit,
            },
            &stage,
            cancel,
        )
        .await;
        let success = match ended {
            signin::Ended::Exited { success } => success,
            signin::Ended::Cancelled => return SignInStage::Cancelled,
            signin::Ended::TimedOut => return SignInStage::TimedOut,
            signin::Ended::Failed(reason) => return SignInStage::Failed { reason },
        };
        stage(SignInStage::Checking);
        // Checked either way, so Accounts shows the login there is now; kept
        // only if no sign-out began since this sign-in did. A check that
        // began before the login changed keeps nothing.
        let fence = {
            let mut state = self.lock();
            state.replaced_sign_in(&profile.id);
            (epoch, state.generation(&profile.id))
        };
        let checked = self.check_at(profile, fence).await;
        if !success {
            // A login from before isn't this sign-in succeeding.
            return SignInStage::Failed {
                reason: format!(
                    "{}'s sign-in stopped before it finished. Try again.",
                    profile.name
                ),
            };
        }
        match checked {
            Checked::In { identity, .. } => SignInStage::Done { identity },
            Checked::Attention(reason) | Checked::Missing(reason) => SignInStage::Failed { reason },
            Checked::Out | Checked::Unknown(_) => SignInStage::Failed {
                reason: format!("{} did not report a sign-in. Try again.", profile.name),
            },
        }
    }

    async fn oauth_sign_in(
        &self,
        profile: &Profile,
        (authorize_url, token_url, scopes): (&str, &str, &[String]),
        stage: &(dyn Fn(SignInStage) + Send + Sync),
        mut cancel: watch::Receiver<bool>,
        epoch: u64,
    ) -> SignInStage {
        let fail = |reason: String| SignInStage::Failed { reason };
        // Replacing the client moves this on: tokens from the old one are
        // no use with the new.
        let generation = self.lock().generation(&profile.id);
        let client = match self.oauth_client(&profile.id).await {
            Ok(Some(c)) => c,
            Ok(None) => return fail(format!("Set {}'s OAuth client first.", profile.name)),
            Err(e) => return fail(e.to_string()),
        };
        let pending = match oauth::begin(authorize_url, scopes, &client).await {
            Ok(p) => p,
            Err(e) => return fail(e),
        };
        if let Err(e) = self.config().opener.open(&pending.url) {
            return fail(e);
        }
        stage(SignInStage::Browser {
            url_host: url::Url::parse(authorize_url)
                .ok()
                .and_then(|u| u.host_str().map(str::to_owned))
                .unwrap_or_default(),
        });
        let http = &self.config().http;
        let tokens = tokio::select! {
            r = pending.finish(http, token_url, &client) => match r {
                Ok(t) => t,
                Err(e) => return fail(e),
            },
            () = tokio::time::sleep(self.config().sign_in_limit) => return SignInStage::TimedOut,
            () = async { while cancel.changed().await.is_ok() && !*cancel.borrow() {} } => return SignInStage::Cancelled,
        };
        stage(SignInStage::Checking);
        let Some(refresh) = tokens.refresh.as_ref() else {
            return fail("The provider did not allow Ophio to stay signed in.".into());
        };
        let _writing = self.shared.writes.lock().await;
        if self.lock().epoch(&profile.id) != epoch {
            return SignInStage::Cancelled;
        }
        if self.lock().generation(&profile.id) != generation {
            return fail(format!(
                "{}'s OAuth client was replaced during the sign-in. Sign in again.",
                profile.name
            ));
        }
        // Held before it is kept, so a task can't read it unmasked. A refresh
        // or check still going keeps nothing it gets.
        let fence = {
            let mut state = self.lock();
            state.replaced_sign_in(&profile.id);
            state.unsaved.remove(&profile.id);
            state.holding(&profile.id, refresh);
            (epoch, state.generation(&profile.id))
        };
        let kept = self
            .config()
            .vault
            .set(&profile.id, Item::Refresh, refresh)
            .await;
        let held = kept.is_ok().then_some(refresh.as_str());
        self.lock().kept(&profile.id, Item::Refresh.name(), held);
        if let Err(e) = kept {
            return fail(format!("The sign-in could not be kept: {e}"));
        }
        {
            let mut state = self.lock();
            state.hold(&profile.id, ACCESS, &tokens.access, Some(tokens.expires_at));
            state.tokens.insert(
                profile.id.clone(),
                Cached {
                    value: tokens.access.clone(),
                    until: Some(tokens.expires_at),
                },
            );
        }
        self.apply(
            profile,
            &Checked::In {
                identity: tokens.identity.clone(),
                note: None,
            },
            fence,
        );
        SignInStage::Done {
            identity: tokens.identity,
        }
    }

    fn finish_sign_in(&self, profile: &Profile, ended: SignInStage, by: &str, epoch: u64) {
        let mut state = self.lock();
        state.running = None;
        // Signed out meanwhile: whatever the sign-in got is gone with it.
        let ended = if state.epoch(&profile.id) == epoch {
            ended
        } else {
            SignInStage::Cancelled
        };
        let ended = match ended {
            SignInStage::Failed { reason } => SignInStage::Failed {
                reason: state.masked(&reason),
            },
            SignInStage::Done { identity } => SignInStage::Done {
                identity: identity.as_deref().map(|i| state.masked(i)),
            },
            ended => ended,
        };
        let (event, detail) = match &ended {
            SignInStage::Done { identity } => (AccountEvent::SignedIn, identity.clone()),
            SignInStage::Failed { reason } => (AccountEvent::SignInFailed, Some(reason.clone())),
            SignInStage::Cancelled => (AccountEvent::SignInFailed, Some("Cancelled".into())),
            SignInStage::TimedOut => (
                AccountEvent::SignInFailed,
                Some("Not finished in time".into()),
            ),
            _ => (AccountEvent::SignInFailed, None),
        };
        if let Some(sign_in) = state.sign_in.as_mut() {
            sign_in.stage = ended;
        }
        Self::record(&mut state, profile, event, by, detail);
        let _ = self.save(&state);
        drop(state);
        self.bump();
    }

    // ---- Other actions ----------------------------------------------------

    fn set_paused(&self, ids: &[String], paused: bool, by: &str) -> Result<(), AccountsError> {
        let mut state = self.lock();
        let all = Self::profiles(&state.saved);
        let chosen: Vec<&Profile> = all.iter().filter(|p| ids.contains(&p.id)).collect();
        if chosen.is_empty() {
            return Err(AccountsError::NotFound("There is no such account.".into()));
        }
        for profile in &chosen {
            state
                .saved
                .accounts
                .entry(profile.id.clone())
                .or_default()
                .paused = paused;
        }
        if let [profile] = chosen.as_slice() {
            let event = if paused {
                AccountEvent::Paused
            } else {
                AccountEvent::Resumed
            };
            Self::record(&mut state, profile, event, by, None);
        } else {
            state.saved.record(AccountHistoryEntry {
                at: Timestamp::now(),
                account_id: String::new(),
                service: "All accounts".into(),
                event: AccountEvent::PausedAll,
                by: by.to_owned(),
                detail: None,
            });
        }
        let saved = self.save(&state);
        drop(state);
        self.bump();
        // It holds until Ophio stops either way; said so it isn't trusted
        // past a restart.
        saved.map_err(|e| {
            AccountsError::Failed(format!(
                "{} until Ophio stops, but it couldn't be saved ({e}), so a restart would \
                 undo it. Make room on the computer, then try again.",
                if paused { "Paused" } else { "Resumed" }
            ))
        })
    }

    /// Signs out; `delete` also forgets the account's keys (and an added
    /// service itself) and opens the provider's page where the session or
    /// key has to be ended by hand. Anything that stops part way is an error,
    /// and an added service whose sign-in may remain is kept.
    async fn sign_out(&self, id: &str, by: &str, delete: bool) -> Result<(), AccountsError> {
        let profile = self.profile(id)?;
        let _stopping = {
            let mut state = self.lock();
            if !state.stopping.insert(id.to_owned()) {
                return Err(stopping(&profile));
            }
            // A check, refresh or sign-in already going keeps nothing it gets.
            state.next_epoch(id);
            Stopping(self, id.to_owned())
        };
        self.end_sign_in(&profile).await?;
        let vault = &self.config().vault;
        let tools = &self.config().tools;
        // Whether the provider has to be visited to end what Ophio cannot.
        let mut still_open = true;
        let mut trouble = None;
        match &profile.method {
            Method::Agent { sign_out, .. } => {
                match tools.run(sign_out, &[], SIGN_OUT_LIMIT).await {
                    Ok(out) if out.success => {}
                    Ok(_) | Err(_) => trouble = Some(format!("{} did not sign out.", profile.name)),
                }
            }
            Method::Cli {
                sign_out,
                config_variable,
                ..
            } => {
                let dir = self.config_dir(id);
                if let Some(sign_out) = sign_out.as_ref().filter(|_| dir.is_dir()) {
                    let extra = [(config_variable.clone(), dir.clone().into_os_string())];
                    if !tools
                        .run(sign_out, &extra, SIGN_OUT_LIMIT)
                        .await
                        .is_ok_and(|o| o.success)
                    {
                        tracing::info!(
                            "accounts: {id}'s own sign-out did not finish; removing its folder"
                        );
                    }
                }
            }
            Method::ApiKey { .. } | Method::Oauth { .. } => {}
        }
        // What is kept goes under the write lock, so nothing begun earlier
        // writes it back after.
        let writing = self.shared.writes.lock().await;
        match &profile.method {
            Method::Agent { .. } => {}
            Method::Cli { .. } => {
                if let Err(e) = std::fs::remove_dir_all(self.config_dir(id))
                    && e.kind() != std::io::ErrorKind::NotFound
                {
                    trouble = Some(format!(
                        "Ophio could not remove {}'s sign-in: {e}",
                        profile.name
                    ));
                }
            }
            Method::ApiKey { .. } => vault.remove(id, Item::Key).await.map_err(failed)?,
            Method::Oauth { revoke_url, .. } => {
                if let (Some(url), Some(refresh)) = (revoke_url, self.refresh_token(id).await?) {
                    match oauth::revoke(&self.config().http, url, &refresh).await {
                        Ok(()) => still_open = false,
                        Err(e) => tracing::info!("accounts: {id} was not revoked: {e}"),
                    }
                }
                vault.remove(id, Item::Refresh).await.map_err(failed)?;
                if delete {
                    vault.remove(id, Item::Client).await.map_err(failed)?;
                }
            }
        }
        {
            let mut state = self.lock();
            state.next_epoch(id);
            state.tokens.remove(id);
            if trouble.is_none() {
                state.unsaved.remove(id);
                // Still masked, as replaced.
                let mut gone = vec![Item::Refresh.name(), ACCESS, TOOL, Item::Key.name()];
                if delete {
                    gone.push(Item::Client.name());
                }
                state.let_go(id, &gone);
            }
            let kept = state.saved.accounts.entry(id.to_owned()).or_default();
            if trouble.is_none() {
                kept.status = Status::SignedOut;
                kept.identity = None;
                kept.checked_at = Some(Timestamp::now());
            }
            if delete {
                kept.client_set = false;
                if !profile.builtin && trouble.is_none() {
                    state.saved.added.retain(|p| p.id != id);
                    state.saved.accounts.remove(id);
                }
            }
            let event = if delete {
                AccountEvent::Deleted
            } else {
                AccountEvent::SignedOut
            };
            Self::record(&mut state, &profile, event, by, trouble.clone());
            let _ = self.save(&state);
        }
        drop(writing);
        self.bump();
        if delete
            && still_open
            && trouble.is_none()
            && let Some(url) = &profile.manage_url
            && let Err(e) = self.config().opener.open(url)
        {
            tracing::warn!("accounts: {e}");
        }
        if matches!(profile.method, Method::Agent { .. }) && (!delete || profile.builtin) {
            // The tool's own word on whether it is signed out.
            self.check_one(&profile).await;
        }
        match trouble {
            Some(trouble) => Err(AccountsError::Failed(trouble)),
            None => Ok(()),
        }
    }

    /// Cancels the account's sign-in, if one is running, and waits for it to
    /// end so it can't write a login after the sign-out removes it.
    async fn end_sign_in(&self, profile: &Profile) -> Result<(), AccountsError> {
        let mut changed = self.shared.changed.subscribe();
        let deadline = tokio::time::Instant::now() + CANCEL_LIMIT;
        loop {
            match self
                .lock()
                .running
                .as_ref()
                .filter(|r| r.account_id == profile.id)
            {
                Some(running) => {
                    let _ = running.cancel.send(true);
                }
                None => return Ok(()),
            }
            if tokio::time::timeout_at(deadline, changed.changed())
                .await
                .is_err()
            {
                return Err(AccountsError::Busy(format!(
                    "{}'s sign-in is still stopping. Try again in a moment.",
                    profile.name
                )));
            }
        }
    }

    async fn set_key(&self, id: &str, key: &Secret, by: &str) -> Result<(), AccountsError> {
        let writing = self.shared.writes.lock().await;
        // Looked up under the lock: a delete just before leaves nothing to set.
        let profile = self.profile(id)?;
        if self.lock().stopping.contains(id) {
            return Err(stopping(&profile));
        }
        let value = key.expose().trim();
        let detail = match &profile.method {
            Method::ApiKey { .. } => {
                // A key masking could miss might show in a task's output.
                if !ports::accounts::maskable(value) || value.len() > 4096 {
                    return Err(AccountsError::Invalid(
                        "That does not look like a key. A key is at least 8 letters, \
                         digits or punctuation, without spaces or quotes."
                            .into(),
                    ));
                }
                {
                    let mut state = self.lock();
                    // Held before it is kept, so a task can't read it unmasked.
                    state.holding(id, value);
                    // A check of the key being replaced keeps nothing.
                    state.replaced_sign_in(id);
                }
                let kept = self.config().vault.set(id, Item::Key, value).await;
                let held = kept.is_ok().then_some(value);
                let mut state = self.lock();
                // Nor does one that read the old key while this was kept.
                state.replaced_sign_in(id);
                state.kept(id, Item::Key.name(), held);
                drop(state);
                kept.map_err(failed)?;
                None
            }
            Method::Oauth { .. } => {
                let client = oauth::Client::parse(value).map_err(AccountsError::Invalid)?;
                {
                    let mut state = self.lock();
                    // Its tokens would belong to the client being replaced.
                    if state.running.as_ref().is_some_and(|r| r.account_id == id) {
                        return Err(AccountsError::Busy(format!(
                            "{} is signing in. Finish or cancel the sign-in first.",
                            profile.name
                        )));
                    }
                    // A refresh or sign-in still going, with the client
                    // replaced, keeps nothing.
                    state.replaced_sign_in(id);
                }
                if let Some(secret) = &client.secret {
                    self.lock().holding(id, secret);
                }
                let kept = self
                    .config()
                    .vault
                    .set(id, Item::Client, &client.to_json())
                    .await;
                let mut state = self.lock();
                // A sign-in that read the old client while this was kept
                // keeps nothing either.
                state.replaced_sign_in(id);
                match (&client.secret, &kept) {
                    (Some(secret), Ok(())) => {
                        state.kept(id, Item::Client.name(), Some(secret.as_str()));
                    }
                    (Some(_), Err(_)) => state.kept(id, Item::Client.name(), None),
                    (None, Ok(())) => state.let_go(id, &[Item::Client.name()]),
                    (None, Err(_)) => {}
                }
                drop(state);
                kept.map_err(failed)?;
                Some("OAuth client".to_owned())
            }
            _ => {
                return Err(AccountsError::Invalid(format!(
                    "{} has no key to set. Sign in instead.",
                    profile.name
                )));
            }
        };
        let mut state = self.lock();
        state.tokens.remove(id);
        let kept = state.saved.accounts.entry(id.to_owned()).or_default();
        if detail.is_some() {
            kept.client_set = true;
        }
        Self::record(&mut state, &profile, AccountEvent::KeySet, by, detail);
        let _ = self.save(&state);
        drop(state);
        drop(writing);
        self.bump();
        if matches!(profile.method, Method::ApiKey { .. }) {
            let this = self.clone();
            tokio::spawn(async move { this.check_one(&profile).await });
        }
        Ok(())
    }

    fn add_service(&self, service: &ServiceProfile, by: &str) -> Result<(), AccountsError> {
        let mut state = self.lock();
        let all = Self::profiles(&state.saved);
        let profile = profiles::added(service, &all).map_err(AccountsError::Invalid)?;
        for variable in profile.variables() {
            if let Some(other) = all.iter().find(|p| p.variables().contains(&variable)) {
                return Err(AccountsError::Invalid(format!(
                    "{} already gives tasks {variable}.",
                    other.name
                )));
            }
        }
        state.saved.added.push(profile.clone());
        state
            .saved
            .accounts
            .insert(profile.id.clone(), Kept::default());
        Self::record(&mut state, &profile, AccountEvent::ServiceAdded, by, None);
        let _ = self.save(&state);
        drop(state);
        self.bump();
        Ok(())
    }

    // ---- What runs receive ------------------------------------------------

    /// One account's variables, the secrets to mask and the files the
    /// variables name.
    async fn give(
        &self,
        profile: &Profile,
        task: TaskId,
    ) -> Result<(Vec<(String, String)>, Vec<String>, Vec<PathBuf>), AccountsError> {
        match &profile.method {
            Method::Agent { .. } => Ok((Vec::new(), Vec::new(), Vec::new())),
            Method::ApiKey { variable, .. } => {
                let key = self
                    .fetch(&profile.id, Item::Key)
                    .await?
                    .ok_or_else(|| failed("no key is set"))?;
                Ok((
                    vec![(variable.clone(), key.to_string())],
                    vec![key.to_string()],
                    Vec::new(),
                ))
            }
            Method::Cli {
                config_variable, ..
            } => {
                let dir = self.config_dir(&profile.id);
                let secrets = self
                    .tool_token(profile)
                    .await
                    .map(|t| t.to_string())
                    .into_iter()
                    .collect();
                Ok((
                    vec![(config_variable.clone(), dir.to_string_lossy().into_owned())],
                    secrets,
                    vec![dir],
                ))
            }
            Method::Oauth {
                variable,
                token_url,
                google,
                ..
            } => {
                let access = self
                    .access_token(profile, token_url, false)
                    .await
                    .map_err(|e| failed(e.reason()))?;
                let mut env = vec![(variable.clone(), access.to_string())];
                let mut secrets = vec![access.to_string()];
                let mut files = Vec::new();
                if *google {
                    let refresh = self
                        .refresh_token(&profile.id)
                        .await?
                        .ok_or_else(|| failed("signed out"))?;
                    let client = self
                        .oauth_client(&profile.id)
                        .await?
                        .ok_or_else(|| failed("no client"))?;
                    let dir = self.config().dir.join("runs").join(task.to_string());
                    files::private_dir(&dir).map_err(failed)?;
                    let path = dir.join(format!("{}.json", profile.id));
                    files::write_private(
                        &path,
                        oauth::google_authorized_user(&client, &refresh).as_bytes(),
                    )
                    .map_err(failed)?;
                    env.push((
                        GOOGLE_CREDENTIALS.into(),
                        path.to_string_lossy().into_owned(),
                    ));
                    secrets.push(refresh.to_string());
                    secrets.extend(client.secret);
                    files.push(path);
                }
                Ok((env, secrets, files))
            }
        }
    }

    /// Before Ophio's data is deleted: ends what it can of each session it
    /// keeps, revoking refresh tokens and running each command-line tool's
    /// own sign-out, then removes everything it keeps in the vault. Agents'
    /// logins are the owner's own and are kept. Returns the name of each
    /// account whose sign-in or key may still work, with the page where it
    /// is ended by hand, and whether the vault was emptied.
    pub async fn forget_all(&self) -> (Vec<(String, String)>, Result<(), AccountsError>) {
        let vault = &self.config().vault;
        let profiles = Self::profiles(&self.lock().saved);
        let mut open = Vec::new();
        for profile in profiles {
            let id = &profile.id;
            let still_open = match &profile.method {
                Method::Agent { .. } => false,
                Method::Cli {
                    sign_out,
                    config_variable,
                    ..
                } => {
                    let dir = self.config_dir(id);
                    let signed_in = dir.is_dir();
                    if let Some(sign_out) = sign_out.as_ref().filter(|_| signed_in) {
                        let extra = [(config_variable.clone(), dir.into_os_string())];
                        let _ = self
                            .config()
                            .tools
                            .run(sign_out, &extra, SIGN_OUT_LIMIT)
                            .await;
                    }
                    // A tool's own sign-out forgets its token but leaves it
                    // working at the provider.
                    signed_in
                }
                // A key the vault cannot answer for may be there.
                Method::ApiKey { .. } => !matches!(vault.get(id, Item::Key).await, Ok(None)),
                Method::Oauth { revoke_url, .. } => match vault.get(id, Item::Refresh).await {
                    Ok(None) => false,
                    Ok(Some(refresh)) => match revoke_url {
                        Some(url) => oauth::revoke(&self.config().http, url, &refresh)
                            .await
                            .inspect_err(|e| tracing::info!("accounts: {id} was not revoked: {e}"))
                            .is_err(),
                        None => true,
                    },
                    Err(_) => true,
                },
            };
            if still_open && let Some(url) = &profile.manage_url {
                open.push((profile.name.clone(), url.clone()));
            }
        }
        self.lock().tokens.clear();
        (open, vault.clear().await.map_err(failed))
    }

    /// A key or fresh token for `ophio token`, so a task can renew what it
    /// was given without seeing anything else.
    pub async fn token(&self, name: &str) -> Result<Zeroizing<String>, AccountsError> {
        let profile = self.find(name)?;
        let epoch = self.lock().epoch(&profile.id);
        self.still_on(&profile, epoch)?;
        let token = self.fetch_token(&profile).await?;
        // Paused or signed out while it was fetched: nothing is given.
        self.still_on(&profile, epoch)?;
        Ok(token)
    }

    async fn fetch_token(&self, profile: &Profile) -> Result<Zeroizing<String>, AccountsError> {
        match &profile.method {
            Method::ApiKey { .. } => self
                .fetch(&profile.id, Item::Key)
                .await?
                .ok_or_else(|| AccountsError::NotReady(format!("{} has no key.", profile.name))),
            Method::Cli { token: Some(_), .. } => {
                self.lock().tokens.remove(&profile.id);
                self.tool_token(profile).await.ok_or_else(|| {
                    AccountsError::NotReady(format!("{} is not signed in.", profile.name))
                })
            }
            Method::Oauth { token_url, .. } => self
                .access_token(profile, token_url, false)
                .await
                .map_err(|e| AccountsError::NotReady(e.reason())),
            _ => Err(AccountsError::Invalid(format!(
                "{} has no token Ophio can give.",
                profile.name
            ))),
        }
    }
}

enum Access {
    Out,
    Refused(String),
    Failed(String),
}
impl Access {
    fn reason(self) -> String {
        match self {
            Self::Out => "It is not signed in.".into(),
            Self::Refused(r) | Self::Failed(r) => r,
        }
    }
}

#[async_trait::async_trait]
impl Accounts for ComputerAccounts {
    async fn view(&self) -> AccountsView {
        let state = self.lock();
        let accounts = Self::profiles(&state.saved)
            .iter()
            .map(|p| self.account(&state, p))
            .collect();
        AccountsView {
            accounts,
            sign_in: state.sign_in.clone(),
            history: state
                .saved
                .history
                .iter()
                .rev()
                .take(SHOWN_HISTORY)
                .cloned()
                .collect(),
            store: self.config().vault.name(),
            store_note: self.config().vault.note(),
            browser: self.shared.browser.clone(),
            password_manager_done: state.saved.password_manager_done,
        }
    }

    async fn act(&self, action: AccountAction, by: &str) -> Result<(), AccountsError> {
        match action {
            AccountAction::SignIn { account_id } => self.sign_in(&account_id, by).await,
            AccountAction::CancelSignIn => {
                if let Some(running) = self.lock().running.as_ref() {
                    let _ = running.cancel.send(true);
                }
                Ok(())
            }
            AccountAction::Pause { account_id } => self.set_paused(&[account_id], true, by),
            AccountAction::Resume { account_id } => self.set_paused(&[account_id], false, by),
            AccountAction::PauseAll => {
                let ids: Vec<String> = Self::profiles(&self.lock().saved)
                    .into_iter()
                    .map(|p| p.id)
                    .collect();
                self.set_paused(&ids, true, by)
            }
            AccountAction::SignOut { account_id } => self.sign_out(&account_id, by, false).await,
            AccountAction::Delete { account_id } => self.sign_out(&account_id, by, true).await,
            AccountAction::SetKey { account_id, key } => self.set_key(&account_id, &key, by).await,
            AccountAction::AddService { service } => self.add_service(&service, by),
            AccountAction::Check { account_id } => {
                if let Some(id) = &account_id {
                    self.profile(id)?;
                }
                let this = self.clone();
                tokio::spawn(async move { this.check(account_id.as_deref()).await });
                Ok(())
            }
            AccountAction::SetUpPasswordManager { manager } => self
                .config()
                .opener
                .open(manager.extension_page())
                .map_err(AccountsError::Failed),
            AccountAction::PasswordManagerDone => {
                let mut state = self.lock();
                state.saved.password_manager_done = true;
                let _ = self.save(&state);
                drop(state);
                self.bump();
                Ok(())
            }
        }
    }

    async fn for_run(
        &self,
        harness: HarnessKind,
        task: TaskId,
    ) -> Result<RunAccounts, AccountsError> {
        let (profiles, kept) = {
            let state = self.lock();
            (Self::profiles(&state.saved), state.saved.accounts.clone())
        };
        let kept_of = |id: &str| kept.get(id).cloned().unwrap_or_default();
        let agent = profiles
            .iter()
            .find(|p| matches!(&p.method, Method::Agent { harness: h, .. } if *h == harness));
        if let Some(agent) = agent
            && kept_of(&agent.id).paused
        {
            return Err(AccountsError::NotReady(format!(
                "{} is paused in Accounts. Resume it to start tasks on it.",
                agent.name
            )));
        }
        let mut given = RunAccounts::default();
        let mut gifts = Vec::new();
        for profile in profiles.iter().filter(|p| p.kind() != AccountKind::Agent) {
            let kept = kept_of(&profile.id);
            if kept.paused || kept.status != Status::SignedIn {
                continue;
            }
            let epoch = self.lock().epoch(&profile.id);
            match self.give(profile, task).await {
                Ok((env, secrets, files)) => {
                    // Masked even if it isn't given after all.
                    given.secrets.extend(secrets);
                    gifts.push((profile, epoch, env, files));
                }
                Err(e) => tracing::warn!("accounts: {} was not given to a task: {e}", profile.id),
            }
        }
        let mut state = self.lock();
        // Paused while the task's accounts were fetched.
        if let Some(agent) = agent
            && state
                .saved
                .accounts
                .get(&agent.id)
                .is_some_and(|k| k.paused)
        {
            return Err(AccountsError::NotReady(format!(
                "{} is paused in Accounts. Resume it to start tasks on it.",
                agent.name
            )));
        }
        let mut names = HashSet::new();
        // The task runs on its agent's account too.
        let mut ids: Vec<String> = agent.map(|a| a.id.clone()).into_iter().collect();
        for (profile, epoch, env, files) in gifts {
            // Paused or signed out while this or a later one was fetched.
            if let Err(e) = Self::on_in(&state, profile, epoch) {
                tracing::warn!("accounts: {} was not given to a task: {e}", profile.id);
                continue;
            }
            for (name, value) in env {
                if names.insert(name.clone()) {
                    given.env.0.push((name, value));
                }
            }
            given.files.extend(files);
            ids.push(profile.id.clone());
        }
        for id in &ids {
            state.used_by.entry(id.clone()).or_default().insert(task);
        }
        state.runs.insert(task, ids);
        drop(state);
        self.bump();
        Ok(given)
    }

    async fn renew(&self, task: TaskId, account: &str) -> Result<String, AccountsError> {
        let profile = self.find(account)?;
        let given = self
            .lock()
            .runs
            .get(&task)
            .is_some_and(|ids| ids.contains(&profile.id));
        if !given {
            return Err(AccountsError::NotFound(format!(
                "This task wasn't given {}.",
                profile.name
            )));
        }
        let token = self.token(&profile.id).await?;
        // The task ended while it was fetched.
        if !self.lock().runs.contains_key(&task) {
            return Err(AccountsError::NotFound(
                "This task has ended, so it can't renew its accounts.".into(),
            ));
        }
        Ok(token.to_string())
    }

    fn secrets(&self) -> Arc<Vec<String>> {
        self.lock().seen.clone()
    }

    fn run_ended(&self, task: TaskId) {
        let mut state = self.lock();
        let Some(ids) = state.runs.remove(&task) else {
            return;
        };
        for id in ids {
            if let Some(tasks) = state.used_by.get_mut(&id) {
                tasks.remove(&task);
                if tasks.is_empty() {
                    state.used_by.remove(&id);
                }
            }
        }
        drop(state);
        let _ = std::fs::remove_dir_all(self.config().dir.join("runs").join(task.to_string()));
        self.bump();
    }

    fn changed(&self) -> watch::Receiver<u64> {
        self.shared.changed.subscribe()
    }
}

#[cfg(test)]
mod tests;
