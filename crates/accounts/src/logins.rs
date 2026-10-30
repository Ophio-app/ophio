// SPDX-License-Identifier: Apache-2.0
//! Agent logins: usernames and passwords agents use on websites. Passwords and
//! setup keys are kept in the vault with the accounts' keys; Øphio types them
//! into pages itself, and no agent is given one. Until the notice has been
//! accepted on this computer, logins can only be paused, signed out and
//! deleted.
use crate::store::Store;
use crate::totp::CodeKey;
use crate::vault::{Item, Vault};
use async_trait::async_trait;
use ports::agent_logins::{
    Access, AgentLogins, AgentLoginsError, COUNTRY_SECOND_LEVELS, Credentials, LoginUser,
    UsableLogin,
};
use protocol::accounts::Secret;
use protocol::agent_logins::{
    AgentLogin, AgentLoginAction, AgentLoginEvent, AgentLoginHistoryEntry, AgentLoginsView,
    DISCLAIMER, DISCLAIMER_CONFIRM, DISCLAIMER_TITLE, DISCLAIMER_VERSION, LoginDetails, LoginHow,
    LoginStatus, LoginWho, REMINDER,
};
use protocol::time::Timestamp;
use protocol::{RunId, TaskId};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::watch;
use zeroize::Zeroizing;

#[cfg(test)]
mod tests;

const FILE: &str = "agent-logins.json";
/// Kept on the computer; the phone is shown [`SHOWN_HISTORY`] of it.
const HISTORY: usize = 500;
const SHOWN_HISTORY: usize = 50;
const MOST_LOGINS: usize = 200;
const MOST_SITES: usize = 10;
/// Values no longer held that are still masked, oldest dropped first.
const REMEMBERED: usize = 1024;
/// A code with less time than this left isn't typed; the next one is.
const CODE_MARGIN: u64 = 3;
const PASSWORD: &str = "password";
const CODE_KEY: &str = "code_key";
/// A value about to be kept, held while it is written.
const INCOMING_PASSWORD: &str = "incoming_password";
const INCOMING_CODE_KEY: &str = "incoming_code_key";

const OFF: &str = "Agent logins is off on this computer. Read its notice and turn it on first.";

/// Shared hosts where anyone can have a site: a login there would be typed
/// into whoever's page the agent was sent to.
const SHARED_HOSTS: &[&str] = &[
    "github.io",
    "gitlab.io",
    "herokuapp.com",
    "vercel.app",
    "netlify.app",
    "pages.dev",
    "workers.dev",
    "web.app",
    "firebaseapp.com",
    "appspot.com",
    "blogspot.com",
    "azurewebsites.net",
    "cloudfront.net",
    "amazonaws.com",
    "onrender.com",
    "fly.dev",
    "glitch.me",
    "ngrok.io",
    "ngrok-free.app",
    "ngrok.app",
    "trycloudflare.com",
    "surge.sh",
    "neocities.org",
    "duckdns.org",
    "ddns.net",
    "readthedocs.io",
    "wixsite.com",
];

pub struct LoginsConfig {
    /// `<data>/accounts`, private to the owner.
    pub dir: PathBuf,
    /// The accounts' vault, so that forgetting it forgets these too.
    pub vault: Arc<dyn Vault>,
    /// The browser the agent browser is, as the phone is told.
    pub browser: Option<String>,
    /// Seconds since 1970, for authenticator codes.
    pub clock: fn() -> u64,
}
impl LoginsConfig {
    pub fn new(dir: PathBuf, vault: Arc<dyn Vault>, browser: Option<String>) -> Self {
        Self {
            dir,
            vault,
            browser,
            clock: unix_now,
        }
    }
}
impl std::fmt::Debug for LoginsConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginsConfig")
            .field("dir", &self.dir)
            .field("vault", &self.vault)
            .finish_non_exhaustive()
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// What is kept in `agent-logins.json`: everything but the passwords and
/// setup keys, which are in the vault.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Saved {
    accepted: Option<Accepted>,
    paused_all: bool,
    logins: Vec<Kept>,
    history: VecDeque<AgentLoginHistoryEntry>,
    /// Sites the agent browser is still to be signed out of.
    owed_sign_outs: Vec<String>,
}
impl Saved {
    fn record(&mut self, entry: AgentLoginHistoryEntry) {
        self.history.push_back(entry);
        while self.history.len() > HISTORY {
            self.history.pop_front();
        }
    }
    fn enabled(&self) -> bool {
        self.accepted
            .as_ref()
            .is_some_and(|a| a.version == DISCLAIMER_VERSION)
    }
    fn login(&self, id: &str) -> Option<&Kept> {
        self.logins.iter().find(|k| k.login_id == id)
    }
    fn login_mut(&mut self, id: &str) -> Option<&mut Kept> {
        self.logins.iter_mut().find(|k| k.login_id == id)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Accepted {
    version: u32,
    at: Timestamp,
    by: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Kept {
    login_id: String,
    details: LoginDetails,
    #[serde(default)]
    paused: bool,
    #[serde(default)]
    has_password: bool,
    #[serde(default)]
    has_code_key: bool,
    #[serde(default)]
    last_used: Option<Timestamp>,
    /// Why it needs the owner, when it does.
    #[serde(default)]
    attention: Option<String>,
}
impl Kept {
    fn account(&self) -> String {
        account(&self.login_id)
    }
}

/// The vault account a login's password and setup key are kept under.
fn account(login_id: &str) -> String {
    format!("login-{login_id}")
}

#[derive(Default)]
struct State {
    saved: Saved,
    used_by: BTreeMap<String, BTreeSet<TaskId>>,
    /// "Ask me first" logins the owner let a run use.
    approved: HashSet<(String, RunId)>,
    /// Moved on whenever a login's password, rules or pause change: a read
    /// begun under an earlier one hands nothing out.
    generations: HashMap<String, u64>,
    /// Logins being deleted: nothing else starts on them.
    stopping: HashSet<String>,
    /// Every form of every password and setup key read or kept since start,
    /// by login and what it is. Masked in every run while held.
    held: HashMap<(String, &'static str), Vec<String>>,
    /// Forms held before and since replaced or let go, oldest first.
    replaced: VecDeque<String>,
    /// Both, longest first, as every run masks them.
    seen: Arc<Vec<String>>,
}
impl State {
    fn hold(&mut self, id: &str, what: &'static str, forms: Vec<String>) {
        match self.held.insert((id.to_owned(), what), forms.clone()) {
            Some(old) if old == forms => return,
            Some(old) => self.replaced.extend(old),
            None => {}
        }
        self.seen_changed();
    }
    /// Moves what was held as `from` to `to`, the old `to` still masked.
    fn kept(&mut self, id: &str, from: &'static str, to: &'static str) {
        if let Some(forms) = self.held.remove(&(id.to_owned(), from)) {
            self.hold(id, to, forms);
        }
    }
    /// Lets go of `id`'s `what`, each still masked as replaced.
    fn let_go(&mut self, id: &str, what: &[&'static str]) {
        for what in what {
            if let Some(old) = self.held.remove(&(id.to_owned(), *what)) {
                self.replaced.extend(old);
            }
        }
        self.seen_changed();
    }
    fn seen_changed(&mut self) {
        while self.replaced.len() > REMEMBERED {
            self.replaced.pop_front();
        }
        let mut seen: Vec<String> = self
            .held
            .values()
            .flatten()
            .chain(&self.replaced)
            .cloned()
            .collect();
        seen.sort_unstable_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
        seen.dedup();
        self.seen = Arc::new(seen);
    }
    fn masked(&self, text: &str) -> String {
        let mut text = text.to_owned();
        ports::accounts::mask(&mut text, &self.seen);
        text
    }
    fn generation(&self, id: &str) -> u64 {
        self.generations.get(id).copied().unwrap_or(0)
    }
    fn changed(&mut self, id: &str) {
        *self.generations.entry(id.to_owned()).or_default() += 1;
    }
    fn gate(&self) -> Result<(), AgentLoginsError> {
        if self.saved.enabled() {
            Ok(())
        } else {
            Err(AgentLoginsError::Off(OFF.into()))
        }
    }
    /// The login, unless there is none or it is being deleted.
    fn login(&self, id: &str) -> Result<&Kept, AgentLoginsError> {
        self.saved
            .login(id)
            .filter(|_| !self.stopping.contains(id))
            .ok_or_else(|| AgentLoginsError::NotFound("There is no such login.".into()))
    }
    fn access(&self, login: &Kept, user: &LoginUser) -> Access {
        let name = &login.details.name;
        if !self.saved.enabled() {
            return Access::Refused(OFF.into());
        }
        if self.saved.paused_all {
            return Access::Refused("Agent logins are paused.".into());
        }
        if login.paused || self.stopping.contains(&login.login_id) {
            return Access::Refused(format!("{name} is paused."));
        }
        let who = &login.details.who;
        if who
            .projects
            .as_ref()
            .is_some_and(|projects| !projects.contains(&user.project_id))
        {
            return Access::Refused(format!("{name} isn't for this project."));
        }
        if who.agents.as_ref().is_some_and(|agents| {
            user.agent
                .as_ref()
                .is_none_or(|agent| !agents.contains(agent))
        }) {
            return Access::Refused(format!("{name} isn't for this agent."));
        }
        if login.details.how == LoginHow::AskFirst
            && !self
                .approved
                .contains(&(login.login_id.clone(), user.run_id))
        {
            return Access::AskFirst;
        }
        Access::Allowed
    }
    /// What may be handed out now for `user`: the login's generation, name
    /// and sites.
    fn allowed(&self, id: &str, user: &LoginUser) -> Result<(u64, Kept), AgentLoginsError> {
        let login = self.login(id)?;
        match self.access(login, user) {
            Access::Allowed => Ok((self.generation(id), login.clone())),
            Access::AskFirst => Err(AgentLoginsError::Refused(format!(
                "{} needs your OK for this task first.",
                login.details.name
            ))),
            Access::Refused(why) => Err(AgentLoginsError::Refused(why)),
        }
    }
    fn record(
        &mut self,
        login: Option<&Kept>,
        event: AgentLoginEvent,
        by: &str,
        detail: Option<String>,
    ) {
        let detail = detail.map(|d| self.masked(&d));
        self.saved.record(AgentLoginHistoryEntry {
            at: Timestamp::now(),
            login_id: login.map(|l| l.login_id.clone()),
            name: login.map(|l| l.details.name.clone()),
            event,
            by: by.to_owned(),
            task_id: None,
            site: None,
            detail,
        });
    }
}

#[derive(Clone)]
pub struct ComputerAgentLogins {
    shared: Arc<Shared>,
}
impl std::fmt::Debug for ComputerAgentLogins {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ComputerAgentLogins")
            .field("dir", &self.shared.config.dir)
            .finish_non_exhaustive()
    }
}

struct Shared {
    config: LoginsConfig,
    store: Store<Saved>,
    state: Mutex<State>,
    /// Held while a password or setup key is written to or removed from the
    /// vault, and while what it changes is checked and kept.
    writes: tokio::sync::Mutex<()>,
    changed: watch::Sender<u64>,
}

fn failed(e: impl std::fmt::Display) -> AgentLoginsError {
    AgentLoginsError::Failed(e.to_string())
}

impl ComputerAgentLogins {
    pub fn new(config: LoginsConfig) -> Self {
        let store = Store::<Saved>::named(&config.dir, FILE);
        let saved = store.load().unwrap_or_else(|| {
            // The logins are lost with it; their passwords stay in the vault
            // until it is cleared, and none is used.
            let mut state = State::default();
            state.saved.paused_all = true;
            state.record(
                None,
                AgentLoginEvent::PausedAll,
                "Ophio",
                Some(
                    "What Ophio had saved about agent logins couldn't be read, so Agent \
                     logins is off. Turn it on and add the logins again."
                        .into(),
                ),
            );
            if let Err(e) = store.save(&state.saved) {
                tracing::warn!("agent logins: could not save: {e}");
            }
            state.saved
        });
        Self {
            shared: Arc::new(Shared {
                config,
                store,
                state: Mutex::new(State {
                    saved,
                    ..State::default()
                }),
                writes: tokio::sync::Mutex::new(()),
                changed: watch::channel(0).0,
            }),
        }
    }

    fn vault(&self) -> &dyn Vault {
        self.shared.config.vault.as_ref()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.shared
            .state
            .lock()
            .expect("agent logins state poisoned")
    }

    fn bump(&self) {
        self.shared.changed.send_modify(|n| *n = n.wrapping_add(1));
    }

    fn save(&self, state: &State) -> Result<(), AgentLoginsError> {
        self.shared.store.save(&state.saved).map_err(|e| {
            tracing::warn!("agent logins: could not save: {e}");
            failed(e)
        })
    }

    /// Reads every kept password and setup key, so each is masked from the
    /// first run on.
    pub async fn hold_all(&self) {
        let logins: Vec<Kept> = self.lock().saved.logins.clone();
        for login in logins {
            for (item, what) in [(Item::Password, PASSWORD), (Item::CodeKey, CODE_KEY)] {
                match self.vault().get(&login.account(), item).await {
                    Ok(Some(value)) => {
                        let forms = match item {
                            Item::CodeKey => code_key_forms(&value, None),
                            _ => password_forms(&value),
                        };
                        self.lock().hold(&login.login_id, what, forms);
                    }
                    Ok(None) => {}
                    Err(e) => tracing::warn!(
                        "agent logins: could not read {}'s {what}: {e}",
                        login.details.name
                    ),
                }
            }
        }
    }

    fn accept(&self, version: u32, by: &str) -> Result<(), AgentLoginsError> {
        if version != DISCLAIMER_VERSION {
            return Err(AgentLoginsError::Invalid(
                "The notice has changed since it was shown. Read it again.".into(),
            ));
        }
        let mut state = self.lock();
        if state.saved.enabled() {
            return Ok(());
        }
        let before = state.saved.accepted.replace(Accepted {
            version,
            at: Timestamp::now(),
            by: by.to_owned(),
        });
        state.record(None, AgentLoginEvent::Accepted, by, None);
        if let Err(e) = self.save(&state) {
            state.saved.accepted = before;
            return Err(e);
        }
        Ok(())
    }

    async fn add(
        &self,
        details: LoginDetails,
        password: Option<Secret>,
        code_key: Option<Secret>,
        by: &str,
    ) -> Result<(), AgentLoginsError> {
        let Some(password) = password.filter(|p| !p.expose().is_empty()) else {
            return Err(AgentLoginsError::Invalid(
                "Add the login's password.".into(),
            ));
        };
        check_password(password.expose())?;
        let code_key = code_key
            .filter(|c| !c.expose().trim().is_empty())
            .map(|c| {
                CodeKey::parse(c.expose())
                    .map(|key| (key, c))
                    .map_err(AgentLoginsError::Invalid)
            })
            .transpose()?;
        let _writes = self.shared.writes.lock().await;
        let id = new_id();
        let account = account(&id);
        let details = {
            let mut state = self.lock();
            state.gate()?;
            if state.saved.logins.len() >= MOST_LOGINS {
                return Err(AgentLoginsError::Invalid(format!(
                    "Øphio keeps at most {MOST_LOGINS} logins."
                )));
            }
            let details = checked(details, &state.saved.logins, None)?;
            no_password_in(&details.notes, password.expose())?;
            state.hold(&id, INCOMING_PASSWORD, password_forms(password.expose()));
            if let Some((key, given)) = &code_key {
                state.hold(
                    &id,
                    INCOMING_CODE_KEY,
                    code_key_forms(&key.to_kept(), Some(given.expose())),
                );
            }
            details
        };
        let written = async {
            self.vault()
                .set(&account, Item::Password, password.expose())
                .await?;
            if let Some((key, _)) = &code_key {
                self.vault()
                    .set(&account, Item::CodeKey, &key.to_kept())
                    .await?;
            }
            Ok::<_, crate::vault::VaultError>(())
        }
        .await;
        if let Err(e) = written {
            self.forget(&account).await;
            self.lock()
                .let_go(&id, &[INCOMING_PASSWORD, INCOMING_CODE_KEY]);
            return Err(AgentLoginsError::Failed(format!(
                "The login couldn't be kept in {}: {e}",
                self.vault().name()
            )));
        }
        let saved = {
            let mut state = self.lock();
            state.kept(&id, INCOMING_PASSWORD, PASSWORD);
            state.kept(&id, INCOMING_CODE_KEY, CODE_KEY);
            let login = Kept {
                login_id: id.clone(),
                details,
                paused: false,
                has_password: true,
                has_code_key: code_key.is_some(),
                last_used: None,
                attention: None,
            };
            state.saved.logins.push(login.clone());
            state.record(Some(&login), AgentLoginEvent::Added, by, None);
            let saved = self.save(&state);
            if saved.is_err() {
                state.saved.logins.retain(|k| k.login_id != id);
                state.saved.history.pop_back();
                state.let_go(&id, &[PASSWORD, CODE_KEY]);
            }
            saved
        };
        if let Err(e) = saved {
            self.forget(&account).await;
            return Err(e);
        }
        self.bump();
        Ok(())
    }

    /// Removes what a login that couldn't be added left in the vault.
    async fn forget(&self, account: &str) {
        for item in [Item::Password, Item::CodeKey] {
            if let Err(e) = self.vault().remove(account, item).await {
                tracing::warn!("agent logins: could not remove a login that wasn't added: {e}");
            }
        }
    }

    fn edit(&self, id: &str, details: LoginDetails, by: &str) -> Result<(), AgentLoginsError> {
        let mut state = self.lock();
        state.gate()?;
        state.login(id)?;
        let details = checked(details, &state.saved.logins, Some(id))?;
        if let Some(forms) = state.held.get(&(id.to_owned(), PASSWORD)) {
            for password in forms {
                no_password_in(&details.notes, password)?;
            }
        }
        let login = state.saved.login_mut(id).expect("found above");
        let before = std::mem::replace(&mut login.details, details);
        let login = login.clone();
        state.changed(id);
        // Approvals were given under the old rules.
        state.approved.retain(|(login_id, _)| login_id != id);
        state.record(Some(&login), AgentLoginEvent::Edited, by, None);
        if let Err(e) = self.save(&state) {
            state.saved.login_mut(id).expect("found above").details = before;
            state.saved.history.pop_back();
            return Err(e);
        }
        drop(state);
        self.bump();
        Ok(())
    }

    async fn set_secret(
        &self,
        id: &str,
        password: Option<Secret>,
        code_key: Option<Secret>,
        by: &str,
    ) -> Result<(), AgentLoginsError> {
        if password.is_none() && code_key.is_none() {
            return Err(AgentLoginsError::Invalid(
                "There is nothing to change.".into(),
            ));
        }
        if let Some(password) = &password {
            check_password(password.expose())?;
        }
        // An empty setup key removes the one kept.
        let code_key = code_key
            .map(|c| {
                if c.expose().trim().is_empty() {
                    Ok(None)
                } else {
                    CodeKey::parse(c.expose())
                        .map(|key| Some((key, c)))
                        .map_err(AgentLoginsError::Invalid)
                }
            })
            .transpose()?;
        let _writes = self.shared.writes.lock().await;
        let login = {
            let mut state = self.lock();
            state.gate()?;
            let login = state.login(id)?.clone();
            if let Some(password) = &password {
                no_password_in(&login.details.notes, password.expose())?;
                state.hold(id, INCOMING_PASSWORD, password_forms(password.expose()));
            }
            if let Some(Some((key, given))) = &code_key {
                state.hold(
                    id,
                    INCOMING_CODE_KEY,
                    code_key_forms(&key.to_kept(), Some(given.expose())),
                );
            }
            // Whatever was being read is no longer what is kept.
            state.changed(id);
            login
        };
        let account = login.account();
        let mut done = Vec::new();
        let mut trouble = None;
        if let Some(password) = &password {
            match self
                .vault()
                .set(&account, Item::Password, password.expose())
                .await
            {
                Ok(()) => done.push(AgentLoginEvent::PasswordSet),
                Err(e) => trouble = Some(format!("The password couldn't be kept: {e}")),
            }
        }
        if trouble.is_none() {
            let result = match &code_key {
                Some(Some((key, _))) => Some(
                    self.vault()
                        .set(&account, Item::CodeKey, &key.to_kept())
                        .await
                        .map(|()| AgentLoginEvent::CodeKeySet),
                ),
                Some(None) => Some(
                    self.vault()
                        .remove(&account, Item::CodeKey)
                        .await
                        .map(|()| AgentLoginEvent::CodeKeyRemoved),
                ),
                None => None,
            };
            match result {
                Some(Ok(event)) => done.push(event),
                Some(Err(e)) => trouble = Some(format!("The setup key couldn't be kept: {e}")),
                None => {}
            }
        }
        let mut state = self.lock();
        let Some(kept) = state.saved.login_mut(id) else {
            // Deleted meanwhile: what was written goes with it.
            state.let_go(id, &[INCOMING_PASSWORD, INCOMING_CODE_KEY]);
            return Err(AgentLoginsError::NotFound("There is no such login.".into()));
        };
        for event in &done {
            match event {
                AgentLoginEvent::PasswordSet => {
                    kept.has_password = true;
                    kept.attention = None;
                }
                AgentLoginEvent::CodeKeySet => kept.has_code_key = true,
                _ => kept.has_code_key = false,
            }
        }
        let login = kept.clone();
        if done.contains(&AgentLoginEvent::PasswordSet) {
            state.kept(id, INCOMING_PASSWORD, PASSWORD);
        }
        match done.last() {
            Some(AgentLoginEvent::CodeKeySet) => state.kept(id, INCOMING_CODE_KEY, CODE_KEY),
            Some(AgentLoginEvent::CodeKeyRemoved) => state.let_go(id, &[CODE_KEY]),
            _ => {}
        }
        state.let_go(id, &[INCOMING_PASSWORD, INCOMING_CODE_KEY]);
        // A read begun while it was being written may have had the old one.
        state.changed(id);
        for event in done {
            state.record(Some(&login), event, by, None);
        }
        let saved = self.save(&state);
        drop(state);
        self.bump();
        match (trouble, saved) {
            (Some(trouble), _) => Err(AgentLoginsError::Failed(trouble)),
            (None, Err(e)) => Err(AgentLoginsError::Failed(format!(
                "The change was made, but couldn't be saved: {e}"
            ))),
            (None, Ok(())) => Ok(()),
        }
    }

    fn set_paused(&self, id: &str, paused: bool, by: &str) -> Result<(), AgentLoginsError> {
        let mut state = self.lock();
        if !paused {
            state.gate()?;
        }
        let login = state.login(id)?;
        if login.paused == paused {
            return Ok(());
        }
        let login = {
            let login = state.saved.login_mut(id).expect("found above");
            login.paused = paused;
            login.clone()
        };
        state.changed(id);
        let event = if paused {
            AgentLoginEvent::Paused
        } else {
            AgentLoginEvent::Resumed
        };
        state.record(Some(&login), event, by, None);
        let saved = self.save(&state);
        if let Err(e) = saved {
            if paused {
                // Paused here all the same: only resuming waits to be saved.
                drop(state);
                self.bump();
                return Err(AgentLoginsError::Failed(format!(
                    "{} is paused until Ophio stops, but that couldn't be saved: {e}",
                    login.details.name
                )));
            }
            state.saved.login_mut(id).expect("found above").paused = true;
            state.saved.history.pop_back();
            return Err(e);
        }
        drop(state);
        self.bump();
        Ok(())
    }

    fn set_paused_all(&self, paused: bool, by: &str) -> Result<(), AgentLoginsError> {
        let mut state = self.lock();
        if !paused {
            state.gate()?;
        }
        if state.saved.paused_all == paused {
            return Ok(());
        }
        state.saved.paused_all = paused;
        // Requests made before stay unanswerable, as when one login pauses.
        let ids: Vec<String> = (state.saved.logins.iter())
            .map(|l| l.login_id.clone())
            .collect();
        for id in &ids {
            state.changed(id);
        }
        let event = if paused {
            AgentLoginEvent::PausedAll
        } else {
            AgentLoginEvent::ResumedAll
        };
        state.record(None, event, by, None);
        if let Err(e) = self.save(&state) {
            if paused {
                drop(state);
                self.bump();
                return Err(AgentLoginsError::Failed(format!(
                    "Agent logins are paused until Ophio stops, but that couldn't be saved: {e}"
                )));
            }
            state.saved.paused_all = true;
            state.saved.history.pop_back();
            return Err(e);
        }
        drop(state);
        self.bump();
        Ok(())
    }

    /// Services sign the agent browser out of the login's sites first; this
    /// records it, and ends what tasks were let use it.
    fn signed_out(&self, id: &str, by: &str) -> Result<(), AgentLoginsError> {
        let mut state = self.lock();
        let login = state.login(id)?.clone();
        state.changed(id);
        state.approved.retain(|(login_id, _)| login_id != id);
        state.record(Some(&login), AgentLoginEvent::SignedOut, by, None);
        let saved = self.save(&state);
        drop(state);
        self.bump();
        saved
    }

    async fn delete(&self, id: &str, by: &str) -> Result<(), AgentLoginsError> {
        let _writes = self.shared.writes.lock().await;
        let login = {
            let mut state = self.lock();
            let login = state.login(id)?.clone();
            state.stopping.insert(id.to_owned());
            state.changed(id);
            login
        };
        let account = login.account();
        let mut trouble = None;
        for item in [Item::Password, Item::CodeKey] {
            if let Err(e) = self.vault().remove(&account, item).await {
                trouble = Some(e);
            }
        }
        let mut state = self.lock();
        state.stopping.remove(id);
        if let Some(e) = trouble {
            let store = self.vault().name();
            let reason = format!("It couldn't be removed from {store}: {e}. It stays paused.");
            let kept = state.saved.login_mut(id).expect("only deleted here");
            kept.paused = true;
            kept.attention = Some(reason.clone());
            let login = kept.clone();
            state.record(Some(&login), AgentLoginEvent::Paused, "Ophio", Some(reason));
            let _ = self.save(&state);
            drop(state);
            self.bump();
            return Err(AgentLoginsError::Failed(format!(
                "{} couldn't be deleted: {e}. It is paused until it can be.",
                login.details.name
            )));
        }
        state.saved.logins.retain(|k| k.login_id != id);
        state.let_go(id, &[PASSWORD, CODE_KEY]);
        state.approved.retain(|(login_id, _)| login_id != id);
        state.used_by.remove(id);
        state.generations.remove(id);
        state.record(Some(&login), AgentLoginEvent::Deleted, by, None);
        let saved = self.save(&state);
        drop(state);
        self.bump();
        saved.map_err(|e| {
            AgentLoginsError::Failed(format!(
                "{} was deleted, but that couldn't be saved: {e}",
                login.details.name
            ))
        })
    }

    fn agent_login(state: &State, login: &Kept) -> AgentLogin {
        let status = if login.paused {
            LoginStatus::Paused
        } else if let Some(reason) = &login.attention {
            LoginStatus::NeedsAttention {
                reason: reason.clone(),
            }
        } else {
            LoginStatus::Ready
        };
        AgentLogin {
            login_id: login.login_id.clone(),
            name: login.details.name.clone(),
            sites: login.details.sites.clone(),
            username: login.details.username.clone(),
            has_password: login.has_password,
            has_code_key: login.has_code_key,
            notes: state.masked(&login.details.notes),
            who: login.details.who.clone(),
            how: login.details.how,
            notify: login.details.notify,
            paused: login.paused,
            status,
            last_used: login.last_used,
            used_by: state
                .used_by
                .get(&login.login_id)
                .map(|tasks| tasks.iter().copied().collect())
                .unwrap_or_default(),
        }
    }

    /// The login's secret from the vault, held for masking as soon as it is
    /// read. A password the vault no longer has needs the owner.
    async fn read(&self, login: &Kept, item: Item) -> Result<Zeroizing<String>, AgentLoginsError> {
        let value = self
            .vault()
            .get(&login.account(), item)
            .await
            .map_err(|e| {
                AgentLoginsError::Failed(format!("{} can't be read: {e}", login.details.name))
            })?;
        let mut state = self.lock();
        let Some(value) = value else {
            let what = match item {
                Item::CodeKey => "setup key",
                _ => "password",
            };
            if let Some(kept) = state.saved.login_mut(&login.login_id) {
                kept.attention = Some(format!(
                    "Its {what} is missing from {}. Set it again.",
                    self.vault().name()
                ));
            }
            let _ = self.save(&state);
            drop(state);
            self.bump();
            return Err(AgentLoginsError::Failed(format!(
                "{}'s {what} is missing. Set it again on the phone.",
                login.details.name
            )));
        };
        match item {
            Item::CodeKey => state.hold(&login.login_id, CODE_KEY, code_key_forms(&value, None)),
            _ => state.hold(&login.login_id, PASSWORD, password_forms(&value)),
        }
        Ok(value)
    }

    /// Whether what was read for `user` under `generation` may still be
    /// handed out.
    fn still_allowed(
        &self,
        id: &str,
        user: &LoginUser,
        generation: u64,
    ) -> Result<(), AgentLoginsError> {
        let state = self.lock();
        let (now, login) = state.allowed(id, user)?;
        if now != generation {
            return Err(AgentLoginsError::Refused(format!(
                "{} changed while it was being read. Try again.",
                login.details.name
            )));
        }
        Ok(())
    }
}

#[async_trait]
impl AgentLogins for ComputerAgentLogins {
    async fn view(&self) -> AgentLoginsView {
        let state = self.lock();
        let mut logins: Vec<AgentLogin> = state
            .saved
            .logins
            .iter()
            .filter(|login| !state.stopping.contains(&login.login_id))
            .map(|login| Self::agent_login(&state, login))
            .collect();
        logins.sort_by_cached_key(|login| login.name.to_lowercase());
        AgentLoginsView {
            enabled: state.saved.enabled(),
            accepted_version: state.saved.accepted.as_ref().map(|a| a.version),
            disclaimer_version: DISCLAIMER_VERSION,
            disclaimer_title: DISCLAIMER_TITLE.into(),
            disclaimer: DISCLAIMER.iter().map(|&p| p.to_owned()).collect(),
            disclaimer_confirm: DISCLAIMER_CONFIRM.into(),
            reminder: REMINDER.into(),
            paused_all: state.saved.paused_all,
            logins,
            history: state
                .saved
                .history
                .iter()
                .rev()
                .take(SHOWN_HISTORY)
                .cloned()
                .collect(),
            store: self.vault().name(),
            store_note: self.vault().note(),
            browser: self.shared.config.browser.clone(),
        }
    }

    fn enabled(&self) -> bool {
        self.lock().saved.enabled()
    }

    async fn act(&self, action: AgentLoginAction, by: &str) -> Result<(), AgentLoginsError> {
        match action {
            AgentLoginAction::Accept { version } => {
                self.accept(version, by)?;
                self.bump();
                Ok(())
            }
            AgentLoginAction::Add {
                details,
                password,
                code_key,
            } => self.add(details, password, code_key, by).await,
            AgentLoginAction::Edit { login_id, details } => self.edit(&login_id, details, by),
            AgentLoginAction::SetSecret {
                login_id,
                password,
                code_key,
            } => self.set_secret(&login_id, password, code_key, by).await,
            AgentLoginAction::Pause { login_id } => self.set_paused(&login_id, true, by),
            AgentLoginAction::Resume { login_id } => self.set_paused(&login_id, false, by),
            AgentLoginAction::PauseAll => self.set_paused_all(true, by),
            AgentLoginAction::ResumeAll => self.set_paused_all(false, by),
            AgentLoginAction::SignOut { login_id } => self.signed_out(&login_id, by),
            AgentLoginAction::Delete { login_id } => self.delete(&login_id, by).await,
            // A code request is the services' to answer.
            AgentLoginAction::AnswerCode { .. } => Err(AgentLoginsError::NotFound(
                "There is no such code request.".into(),
            )),
        }
    }

    async fn reveal(&self, login_id: &str, by: &str) -> Result<Secret, AgentLoginsError> {
        let login = {
            let state = self.lock();
            state.gate()?;
            state.login(login_id)?.clone()
        };
        let password = self.read(&login, Item::Password).await?;
        let mut state = self.lock();
        state.record(Some(&login), AgentLoginEvent::Shown, by, None);
        let _ = self.save(&state);
        drop(state);
        self.bump();
        Ok(Secret::new(password.as_str()))
    }

    fn usable(&self, user: &LoginUser) -> Vec<UsableLogin> {
        let state = self.lock();
        state
            .saved
            .logins
            .iter()
            .filter(|login| login.has_password)
            .filter_map(|login| {
                let ask_first = match state.access(login, user) {
                    Access::Allowed => false,
                    Access::AskFirst => true,
                    Access::Refused(_) => return None,
                };
                Some(UsableLogin {
                    login_id: login.login_id.clone(),
                    name: login.details.name.clone(),
                    sites: login.details.sites.clone(),
                    username: login.details.username.clone(),
                    has_code_key: login.has_code_key,
                    notes: state.masked(&login.details.notes),
                    ask_first,
                    revision: state.generation(&login.login_id),
                })
            })
            .collect()
    }

    fn access(&self, login_id: &str, user: &LoginUser) -> Access {
        let state = self.lock();
        match state.login(login_id) {
            Ok(login) => state.access(login, user),
            Err(e) => Access::Refused(e.to_string()),
        }
    }

    fn approve(&self, login_id: &str, run_id: RunId, revision: u64) -> bool {
        let mut state = self.lock();
        if state.login(login_id).is_err() || state.generation(login_id) != revision {
            return false;
        }
        state.approved.insert((login_id.to_owned(), run_id));
        true
    }

    async fn credentials(
        &self,
        login_id: &str,
        user: &LoginUser,
    ) -> Result<Credentials, AgentLoginsError> {
        let (generation, login) = self.lock().allowed(login_id, user)?;
        let password = self.read(&login, Item::Password).await?;
        self.still_allowed(login_id, user, generation)?;
        Ok(Credentials {
            name: login.details.name,
            sites: login.details.sites,
            username: login.details.username,
            password,
        })
    }

    async fn code(
        &self,
        login_id: &str,
        user: &LoginUser,
    ) -> Result<Zeroizing<String>, AgentLoginsError> {
        let (generation, login) = self.lock().allowed(login_id, user)?;
        if !login.has_code_key {
            return Err(AgentLoginsError::NotFound(format!(
                "{} has no setup key. Add one on the phone, or answer the code yourself.",
                login.details.name
            )));
        }
        let kept = self.read(&login, Item::CodeKey).await?;
        let key = CodeKey::parse(&kept).map_err(|e| {
            AgentLoginsError::Failed(format!(
                "{}'s setup key can't be used: {e}",
                login.details.name
            ))
        })?;
        self.still_allowed(login_id, user, generation)?;
        let now = (self.shared.config.clock)();
        let left = key.lasts(now);
        if left >= CODE_MARGIN {
            return Ok(key.code_at(now));
        }
        // Too late to type this one: the next, once it is current.
        tokio::time::sleep(Duration::from_secs(left)).await;
        self.still_allowed(login_id, user, generation)?;
        Ok(key.code_at(now + left))
    }

    fn blocked_sites(&self, user: &LoginUser) -> Vec<String> {
        let state = self.lock();
        let mut blocked = BTreeSet::new();
        let mut allowed = BTreeSet::new();
        for login in &state.saved.logins {
            let sites = login.details.sites.iter().cloned();
            match state.access(login, user) {
                Access::Allowed => allowed.extend(sites),
                _ => blocked.extend(sites),
            }
        }
        blocked.difference(&allowed).cloned().collect()
    }

    fn record(&self, mut entry: AgentLoginHistoryEntry) {
        let mut guard = self.lock();
        let state = &mut *guard;
        entry.detail = entry.detail.map(|d| state.masked(&d));
        if matches!(
            entry.event,
            AgentLoginEvent::Filled | AgentLoginEvent::CodeFilled | AgentLoginEvent::CodeGiven
        ) && let (Some(id), Some(task)) = (&entry.login_id, entry.task_id)
            && let Some(login) = state.saved.login_mut(id)
        {
            login.last_used = Some(entry.at);
            state.used_by.entry(id.clone()).or_default().insert(task);
        }
        state.saved.record(entry);
        let _ = self.save(state);
        drop(guard);
        self.bump();
    }

    fn owed_sign_outs(&self) -> Vec<String> {
        self.lock().saved.owed_sign_outs.clone()
    }

    fn owe_sign_out(&self, sites: &[String]) -> Result<(), AgentLoginsError> {
        let mut state = self.lock();
        for site in sites {
            if !state.saved.owed_sign_outs.contains(site) {
                state.saved.owed_sign_outs.push(site.clone());
            }
        }
        self.save(&state)
    }

    fn settled_sign_outs(&self, sites: &[String]) {
        let mut state = self.lock();
        state.saved.owed_sign_outs.retain(|s| !sites.contains(s));
        let _ = self.save(&state);
    }

    fn task_ended(&self, task_id: TaskId) {
        let mut state = self.lock();
        let before = state.used_by.values().map(BTreeSet::len).sum::<usize>();
        for tasks in state.used_by.values_mut() {
            tasks.remove(&task_id);
        }
        state.used_by.retain(|_, tasks| !tasks.is_empty());
        let after = state.used_by.values().map(BTreeSet::len).sum::<usize>();
        drop(state);
        if before != after {
            self.bump();
        }
    }

    fn run_ended(&self, run_id: RunId) {
        self.lock().approved.retain(|(_, run)| *run != run_id);
    }

    fn secrets(&self) -> Arc<Vec<String>> {
        self.lock().seen.clone()
    }

    fn changed(&self) -> watch::Receiver<u64> {
        self.shared.changed.subscribe()
    }
}

fn new_id() -> String {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).expect("the system's random source failed");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn check_password(password: &str) -> Result<(), AgentLoginsError> {
    let invalid = |why: &str| Err(AgentLoginsError::Invalid(why.into()));
    if password.chars().count() < ports::accounts::MASKED_FROM {
        return invalid("Øphio takes passwords of 8 characters or more.");
    }
    if password.len() > 1024 {
        return invalid("That password is too long.");
    }
    if password.chars().any(char::is_control) {
        return invalid("A password can't have line breaks or other control characters.");
    }
    if password.trim().is_empty() {
        return invalid("A password can't be only spaces.");
    }
    Ok(())
}

fn no_password_in(notes: &str, password: &str) -> Result<(), AgentLoginsError> {
    if !password.is_empty() && notes.contains(password) {
        return Err(AgentLoginsError::Invalid(
            "The notes contain the password. Agents read the notes, so take it out.".into(),
        ));
    }
    Ok(())
}

/// `details` tidied, or why it can't be kept.
fn checked(
    details: LoginDetails,
    logins: &[Kept],
    except: Option<&str>,
) -> Result<LoginDetails, AgentLoginsError> {
    let invalid = |why: String| Err(AgentLoginsError::Invalid(why));
    let name = details.name.trim().to_owned();
    if name.is_empty() || name.chars().count() > 60 || name.chars().any(char::is_control) {
        return invalid("A login's name is 1 to 60 characters on one line.".into());
    }
    if logins.iter().any(|login| {
        Some(login.login_id.as_str()) != except
            && login.details.name.to_lowercase() == name.to_lowercase()
    }) {
        return invalid(format!("There is already a login called {name}."));
    }
    let username = details.username.trim().to_owned();
    if username.is_empty()
        || username.chars().count() > 200
        || username.chars().any(char::is_control)
    {
        return invalid("A username is 1 to 200 characters on one line.".into());
    }
    let notes = details.notes.trim().to_owned();
    if notes.chars().count() > 2000 {
        return invalid("Notes are at most 2,000 characters.".into());
    }
    if notes
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return invalid("Notes can't have control characters.".into());
    }
    if details.sites.is_empty() || details.sites.len() > MOST_SITES {
        return invalid(format!("A login is for 1 to {MOST_SITES} sites."));
    }
    let mut sites = Vec::new();
    for text in &details.sites {
        let host = site(text).map_err(AgentLoginsError::Invalid)?;
        if !sites.contains(&host) {
            sites.push(host);
        }
    }
    let who = LoginWho {
        projects: match details.who.projects {
            Some(projects) if projects.is_empty() => {
                return invalid("Choose at least one project, or let every project use it.".into());
            }
            Some(mut projects) if projects.len() <= 200 => {
                let mut seen = HashSet::new();
                projects.retain(|project| seen.insert(*project));
                Some(projects)
            }
            Some(_) => return invalid("Choose at most 200 projects.".into()),
            None => None,
        },
        agents: match details.who.agents {
            Some(agents) if agents.is_empty() => {
                return invalid("Choose at least one agent, or let every agent use it.".into());
            }
            Some(agents) if agents.len() <= 50 => {
                let mut kept = Vec::new();
                for agent in agents {
                    let agent = agent.trim().to_owned();
                    if agent.is_empty()
                        || agent.chars().count() > 100
                        || agent.chars().any(char::is_control)
                    {
                        return invalid("An agent is named by its catalog id.".into());
                    }
                    if !kept.contains(&agent) {
                        kept.push(agent);
                    }
                }
                Some(kept)
            }
            Some(_) => return invalid("Choose at most 50 agents.".into()),
            None => None,
        },
    };
    Ok(LoginDetails {
        name,
        sites,
        username,
        notes,
        who,
        how: details.how,
        notify: details.notify,
    })
}

/// The site `text` names, as the host its pages are on: `https://www.Example.com/login`
/// is `example.com`. Addresses, shared hosts and bare country domains are
/// refused: a login for one of them could be typed into a stranger's page.
pub fn site(text: &str) -> Result<String, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("A site can't be empty.".into());
    }
    let address = if text.contains("://") {
        text.to_owned()
    } else {
        format!("https://{text}")
    };
    // Messages name no more than the host read from the address: what was
    // typed may hold a password.
    const NOT_AN_ADDRESS: &str = "That isn't a web address, such as example.com.";
    let url = url::Url::parse(&address).map_err(|_| NOT_AN_ADDRESS.to_owned())?;
    if !matches!(url.scheme(), "https" | "http") {
        return Err("That isn't a web address (https://…).".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("Leave the username and password out of the site's address.".into());
    }
    let host = match url.host() {
        Some(url::Host::Domain(host)) => host.trim_end_matches('.'),
        Some(_) => return Err("That is an IP address. Use the site's name.".into()),
        None => return Err(NOT_AN_ADDRESS.into()),
    };
    let shown: String = host.chars().take(80).collect();
    let host = host.strip_prefix("www.").unwrap_or(host);
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2 || labels.iter().any(|label| label.is_empty()) {
        return Err(format!(
            "{shown} isn't a whole site name, such as example.com."
        ));
    }
    if let Some(shared) = SHARED_HOSTS
        .iter()
        .find(|shared| host == **shared || host.ends_with(&format!(".{shared}")))
    {
        return Err(format!(
            "{shown} is on {shared}, where anyone can have a site. Øphio can't tell whose page \
             is whose there."
        ));
    }
    if let [second, top] = labels.as_slice()
        && top.len() == 2
        && COUNTRY_SECOND_LEVELS.contains(second)
    {
        return Err(format!(
            "{shown} is a country's domain, not a site. Use the site's own name, such as \
             example.{host}."
        ));
    }
    Ok(host.to_owned())
}

/// Every way a password is likely to be written where a run could show it:
/// as typed, inside JSON (also with only ASCII), in HTML and in a form.
fn password_forms(password: &str) -> Vec<String> {
    let mut forms = vec![password.to_owned()];
    let json = serde_json::to_string(password).expect("a string serializes");
    let body = json[1..json.len() - 1].to_owned();
    let ascii = |upper: bool| -> String {
        let mut out = String::new();
        for c in body.chars() {
            if c.is_ascii() {
                out.push(c);
            } else {
                for unit in c.encode_utf16(&mut [0; 2]) {
                    if upper {
                        out.push_str(&format!("\\u{unit:04X}"));
                    } else {
                        out.push_str(&format!("\\u{unit:04x}"));
                    }
                }
            }
        }
        out
    };
    forms.push(ascii(false));
    forms.push(ascii(true));
    forms.push(body);
    let html = |apostrophe: &str| {
        password
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', apostrophe)
    };
    forms.push(html("&#39;"));
    forms.push(html("&#x27;"));
    let form: String = url::form_urlencoded::byte_serialize(password.as_bytes()).collect();
    forms.push(form.replace('+', "%20"));
    forms.push(form);
    forms.sort();
    forms.dedup();
    forms
}

/// The setup key as kept, its letters either case and in groups of four,
/// and as the owner gave it.
fn code_key_forms(kept: &str, given: Option<&str>) -> Vec<String> {
    let mut forms = vec![kept.to_owned()];
    if let Ok(key) = CodeKey::parse(kept) {
        let letters = key.letters();
        let grouped: Vec<String> = letters
            .as_bytes()
            .chunks(4)
            .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
            .collect();
        let grouped = grouped.join(" ");
        forms.push(letters.to_lowercase());
        forms.push(grouped.to_lowercase());
        forms.push(grouped);
        forms.push(letters.as_str().to_owned());
    }
    if let Some(given) = given.map(str::trim)
        && given.len() >= ports::accounts::MASKED_FROM
    {
        forms.push(given.to_owned());
    }
    forms.sort();
    forms.dedup();
    forms
}
