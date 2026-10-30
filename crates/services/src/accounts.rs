// SPDX-License-Identifier: Apache-2.0
//! The computer's accounts, as the phone and the CLI reach them.
//!
//! Secrets never pass through here on their way out: the port keeps them,
//! gives them to the runs it is asked about, and answers everything else with
//! names and states.
use crate::identity::{hash, random_secret};
use crate::{Caller, CompanionHub, error, unsupported};
use ports::accounts::{
    Accounts, AccountsError, MASKED_FROM, RENEW_ENV, mask_again, mask_cut_codes, mask_streamed,
    mask_strings, mask_value,
};
use ports::agent_browser::Observation;
use ports::harness::{HarnessEvent, RunEnv};
use protocol::{
    ErrorCode, ProtocolError, RunId, TaskId, accounts::AccountAction, auth::Principal,
    capability::HarnessKind, command::CommandResult, model::Approval,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// Messages whose ends one run can have held back at once. Past this the run
/// sends no more live text: what was held can't be checked against what comes
/// next, so only whole messages, masked, arrive from then on.
pub(crate) const HELD_MESSAGES: usize = 64;

/// The most one message can have held back, twice the longest key Accounts
/// takes. Only a run of secrets overlapping each other, like a repeated
/// character, holds more; past this the run sends no more live text either,
/// so what is held and searched again with each piece stays this small.
pub(crate) const HELD_BYTES: usize = 16 * 1024;

/// What is hidden in what one run says.
struct Hide {
    secrets: Vec<String>,
    codes: Vec<String>,
}
impl Hide {
    fn is_empty(&self) -> bool {
        self.secrets.is_empty() && self.codes.is_empty()
    }
    /// Whole text that may have been cut to fit.
    fn cut(&self, text: &mut String) {
        if !self.is_empty() {
            mask_cut_codes(text, &self.secrets, &self.codes);
        }
    }
    /// [`Self::cut`] for text hidden in before, its secrets as it shows
    /// those long enough to hide, hidden however short.
    fn again(&self, text: &mut String) {
        if !self.is_empty() {
            mask_again(text, &self.secrets, &self.codes);
        }
    }
}

/// What masking keeps for one run.
pub(crate) struct RunSecrets {
    task_id: TaskId,
    /// The keys and tokens it was given or has renewed, and its credential.
    secrets: Vec<String>,
    /// Sign-in codes typed into its pages, hidden however short they are.
    codes: Vec<String>,
    /// The hash of the credential `ophio token` sends from inside the run,
    /// when Accounts gave it one.
    renew: Option<[u8; 32]>,
    /// For each message streaming in pieces, the end held back because it
    /// could be the start of a secret.
    held: HashMap<String, String>,
    /// More messages were held back than [`HELD_MESSAGES`], or more of one
    /// than [`HELD_BYTES`]: live text is no longer sent for this run.
    live_off: bool,
}

impl RunSecrets {
    fn new(task_id: TaskId, secrets: Vec<String>, renew: Option<[u8; 32]>) -> Self {
        Self {
            task_id,
            secrets,
            codes: Vec::new(),
            renew,
            held: HashMap::new(),
            live_off: false,
        }
    }
}

impl CompanionHub {
    pub(crate) fn accounts(&self) -> Result<Arc<dyn Accounts>, ProtocolError> {
        self.inner
            .accounts
            .clone()
            .ok_or_else(|| unsupported("Accounts"))
    }

    pub(crate) async fn account_action(
        &self,
        caller: &Caller,
        action: &AccountAction,
    ) -> Result<Option<CommandResult>, ProtocolError> {
        let accounts = self.accounts()?;
        let by = self.caller_name(caller)?;
        accounts
            .act(action.clone(), &by)
            .await
            .map_err(account_error)?;
        Ok(None)
    }

    /// The accounts a run about to start on `harness` receives. Their values
    /// are masked in everything the run says until [`Self::release_accounts`].
    pub(crate) async fn accounts_for_run(
        &self,
        run_id: RunId,
        task_id: TaskId,
        harness: HarnessKind,
    ) -> Result<(RunEnv, Vec<PathBuf>), ProtocolError> {
        let Some(accounts) = self.inner.accounts.clone() else {
            // Agent logins alone still need the run masked, for the codes
            // typed into its pages.
            if self.inner.agent_logins.is_some() {
                self.inner
                    .run_secrets
                    .lock()
                    .expect("run secrets poisoned")
                    .insert(run_id, RunSecrets::new(task_id, Vec::new(), None));
            }
            return Ok(Default::default());
        };
        let mut given = accounts
            .for_run(harness, task_id)
            .await
            .map_err(account_error)?;
        // Lets `ophio token` inside the run renew what the run was given,
        // and nothing else.
        let renew = match random_secret() {
            Ok(renew) => renew,
            Err(e) => {
                accounts.run_ended(task_id);
                return Err(e);
            }
        };
        given.env.0.push((RENEW_ENV.into(), renew.clone()));
        let renew_hash = hash(&renew);
        given.secrets.push(renew);
        self.inner
            .run_secrets
            .lock()
            .expect("run secrets poisoned")
            .insert(
                run_id,
                RunSecrets::new(task_id, given.secrets, Some(renew_hash)),
            );
        Ok((given.env, given.files))
    }

    /// A key or fresh token for `account`, asked for by `ophio token` inside
    /// the run whose credential is `credential`. The run must have been given
    /// the account; the value is masked in the run from then on.
    pub async fn renew_for_run(
        &self,
        credential: &str,
        account: &str,
    ) -> Result<String, ProtocolError> {
        let wanted = hash(credential);
        let found = self
            .inner
            .run_secrets
            .lock()
            .expect("run secrets poisoned")
            .iter()
            .find(|(_, run)| {
                run.renew.is_some_and(|renew| {
                    bool::from(subtle::ConstantTimeEq::ct_eq(&renew[..], &wanted[..]))
                })
            })
            .map(|(run_id, run)| (*run_id, run.task_id));
        let Some((run_id, task_id)) = found else {
            return Err(error(
                ErrorCode::PermissionDenied,
                "This task has ended, so it can't renew its accounts.",
            ));
        };
        let token = self
            .accounts()?
            .renew(task_id, account)
            .await
            .map_err(account_error)?;
        // The run may have ended while the token was fetched: it gets
        // nothing then.
        let mut runs = self.inner.run_secrets.lock().expect("run secrets poisoned");
        let Some(run) = runs.get_mut(&run_id) else {
            return Err(error(
                ErrorCode::PermissionDenied,
                "This task has ended, so it can't renew its accounts.",
            ));
        };
        if !run.secrets.contains(&token) {
            run.secrets.push(token.clone());
        }
        Ok(token)
    }

    /// Hides `code` in everything the run says from now on, before it is
    /// typed into one of its pages. False if the run has no masking, and the
    /// code must not be typed.
    pub(crate) fn add_run_code(&self, run_id: RunId, code: &str) -> bool {
        /// Enough for every code a run's sign-ins need; past it, none is
        /// typed, as forgetting one would show it.
        const CODES: usize = 64;
        let mut runs = self.inner.run_secrets.lock().expect("run secrets poisoned");
        let Some(run) = runs.get_mut(&run_id) else {
            return false;
        };
        if !run.codes.iter().any(|c| c == code) {
            if run.codes.len() >= CODES {
                return false;
            }
            run.codes.push(code.to_owned());
        }
        true
    }

    /// The run no longer reaches what it was given.
    pub(crate) fn release_accounts(&self, run_id: RunId) {
        let released = self
            .inner
            .run_secrets
            .lock()
            .expect("run secrets poisoned")
            .remove(&run_id);
        if let (Some(run), Some(accounts)) = (released, &self.inner.accounts) {
            accounts.run_ended(run.task_id);
        }
    }

    /// The run's own secrets, every key and token Accounts has held, and every
    /// password and setup key Agent logins keeps, so a value replaced or
    /// renewed while the run goes is hidden too.
    fn secrets_for(&self, run: Option<&RunSecrets>) -> Vec<String> {
        let mut secrets: Vec<String> = run.map(|r| r.secrets.clone()).unwrap_or_default();
        if let Some(accounts) = &self.inner.accounts {
            secrets.extend(accounts.secrets().iter().cloned());
        }
        if let Some(logins) = &self.inner.agent_logins {
            secrets.extend(logins.secrets().iter().cloned());
        }
        // Longest first, so a value inside a longer one doesn't leave the
        // rest of the longer one showing.
        secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
        secrets
    }

    /// What to hide in what the run says: [`Self::secrets_for`] and the
    /// sign-in codes typed into its pages.
    fn hide_for(&self, run_id: RunId) -> Hide {
        let runs = self.inner.run_secrets.lock().expect("run secrets poisoned");
        let run = runs.get(&run_id);
        Hide {
            secrets: self.secrets_for(run),
            codes: run.map(|r| r.codes.clone()).unwrap_or_default(),
        }
    }

    /// [`Self::hide_for`], for what the agent browser answers a run: none
    /// once the run's masking has been let go, as the codes typed into its
    /// pages went with it, so a request that finishes late is refused.
    fn hide_browsing(&self, run_id: RunId) -> Option<Hide> {
        let runs = self.inner.run_secrets.lock().expect("run secrets poisoned");
        let run = runs.get(&run_id);
        // A run is masked from its start when there is anything to hide.
        let masked = self.inner.accounts.is_some() || self.inner.agent_logins.is_some();
        if masked && run.is_none() {
            return None;
        }
        Some(Hide {
            secrets: self.secrets_for(run),
            codes: run.map(|r| r.codes.clone()).unwrap_or_default(),
        })
    }

    /// Hides keys and tokens in what the run says, before it is kept or sent.
    /// Text arriving in pieces keeps back an end that could begin a secret
    /// until the next piece, or the whole message, shows what it is.
    pub(crate) fn mask_run_event(&self, run_id: RunId, event: &mut HarnessEvent) {
        let mut runs = self.inner.run_secrets.lock().expect("run secrets poisoned");
        let run = runs.get_mut(&run_id);
        let secrets = self.secrets_for(run.as_deref());
        let codes = run.as_ref().map(|r| r.codes.clone()).unwrap_or_default();
        if secrets.is_empty() && codes.is_empty() {
            return;
        }
        match (event, run) {
            (HarnessEvent::AgentTextDelta { text, .. }, Some(run)) if run.live_off => {
                text.clear();
            }
            (HarnessEvent::AgentTextDelta { message_key, text }, Some(run)) => {
                let mut whole = run.held.remove(message_key).unwrap_or_default();
                whole.push_str(text);
                let held = mask_streamed(&mut whole, &secrets);
                // A code split between pieces isn't caught here; the agent
                // never sees one, so only a page's echo of it could be split.
                mask_cut_codes(&mut whole, &[], &codes);
                if !held.is_empty() {
                    if run.held.len() >= HELD_MESSAGES || held.len() > HELD_BYTES {
                        // A dropped end would let the rest of its secret out
                        // in the next piece.
                        run.held.clear();
                        run.live_off = true;
                    } else {
                        run.held.insert(message_key.clone(), held);
                    }
                }
                *text = whole;
            }
            (event, run) => {
                if let (HarnessEvent::AgentMessage { message_key, .. }, Some(run)) = (&*event, run)
                {
                    run.held.remove(message_key);
                }
                // Whole texts: an adapter may have cut one to fit.
                event.visit_text(&mut |text| mask_cut_codes(text, &secrets, &codes));
            }
        }
    }

    /// Hides keys and tokens in an approval a run's tool asks for. What the
    /// tool is answered with is kept apart, unmasked.
    pub(crate) fn mask_approval(&self, run_id: RunId, approval: &mut Approval) {
        let hide = self.hide_for(run_id);
        if hide.is_empty() {
            return;
        }
        let f = &mut |text: &mut String| hide.cut(text);
        f(&mut approval.title);
        approval.detail.iter_mut().for_each(&mut *f);
        approval.raw_request.iter_mut().for_each(&mut *f);
        ports::harness::visit_serialized(&mut approval.detail_lines, f);
        ports::harness::visit_question_text(&mut approval.questions, f);
    }

    /// A copy of a tool's input with the run's keys and tokens hidden, for
    /// drawing what an approval shows: a preview cut from it can't keep part
    /// of one.
    pub(crate) fn masked_input(
        &self,
        run_id: RunId,
        input: &serde_json::Value,
    ) -> serde_json::Value {
        let hide = self.hide_for(run_id);
        let mut shown = input.clone();
        if !hide.secrets.is_empty() {
            mask_value(&mut shown, &hide.secrets);
        }
        if !hide.codes.is_empty() {
            mask_strings(&mut shown, &mut |text| {
                mask_cut_codes(text, &[], &hide.codes)
            });
        }
        shown
    }

    /// What the agent browser answers a run's agent, every text in it masked
    /// as whole text that may have been cut to fit.
    pub(crate) fn masked_result(
        &self,
        run_id: RunId,
        result: &mut serde_json::Value,
    ) -> Result<(), ProtocolError> {
        let hide = self.hide_browsing(run_id).ok_or_else(run_ended)?;
        if !hide.is_empty() {
            mask_strings(result, &mut |text| hide.cut(text));
        }
        Ok(())
    }

    /// What the agent browser saw, masked, then tidied to what the agent is
    /// shown, then masked again for the start of a secret left at a cut.
    pub(crate) fn masked_observation(
        &self,
        run_id: RunId,
        seen: &mut Observation,
    ) -> Result<(), ProtocolError> {
        let hide = self.hide_browsing(run_id).ok_or_else(run_ended)?;
        seen.visit_text(&mut |text| hide.cut(text));
        seen.tidy();
        seen.visit_text(&mut |text| hide.cut(text));
        Ok(())
    }

    /// A message from the agent browser for a run's agent, with the run's
    /// keys and tokens hidden.
    pub(crate) fn masked_message(&self, run_id: RunId, text: &str) -> String {
        let Some(hide) = self.hide_browsing(run_id) else {
            return run_ended().message;
        };
        let mut text = text.to_owned();
        hide.cut(&mut text);
        text
    }

    /// Hides the run's keys, tokens and sign-in codes in what its agent puts
    /// in front of the person, as in everything else it says. Refused once
    /// the run's masking has been let go: the run has ended.
    pub(crate) fn person_masking(
        &self,
        run_id: RunId,
    ) -> Result<impl Fn(&mut String) + use<>, ProtocolError> {
        let hide = self
            .hide_browsing(run_id)
            .ok_or_else(|| error(ErrorCode::Expired, "Run is no longer active"))?;
        Ok(move |text: &mut String| hide.cut(text))
    }

    /// Hides every key, token and sign-in code Ophio holds in what a program
    /// working in a project's folder shows the person, its page's title:
    /// any run there, or none, may have started it. A title shows with its
    /// spaces collapsed, so it is made so and everything held is looked for
    /// in it at once, as it shows: a value long enough to hide is hidden,
    /// whole or cut, however short that makes it. It may have been hidden in
    /// before.
    pub(crate) fn found_masking(&self) -> impl Fn(&mut String) + use<> {
        use crate::preview::spaced;
        let runs = self.inner.run_secrets.lock().expect("run secrets poisoned");
        let mut held = self.secrets_for(None);
        held.extend(runs.values().flat_map(|r| r.secrets.iter().cloned()));
        let mut secrets: Vec<String> = (held.iter())
            .filter(|s| s.len() >= MASKED_FROM)
            .map(|s| spaced(s))
            .collect();
        let mut codes: Vec<String> = (runs.values())
            .flat_map(|r| r.codes.iter().map(|c| spaced(c)))
            .collect();
        for values in [&mut secrets, &mut codes] {
            values.sort();
            values.dedup();
        }
        let hide = Hide { secrets, codes };
        move |title: &mut String| {
            *title = spaced(title);
            hide.again(title);
        }
    }

    /// What a run's agent asks to post on GitHub, with the run's keys and
    /// tokens hidden, and anything shaped like a GitHub token besides: a
    /// post is public, and stays.
    pub(crate) fn redacted(&self, run_id: RunId, text: &str) -> Result<String, ProtocolError> {
        let hide = self
            .hide_browsing(run_id)
            .ok_or_else(|| error(ErrorCode::Expired, "This run has ended."))?;
        let mut text = text.to_owned();
        hide.cut(&mut text);
        Ok(hide_github_tokens(&text))
    }

    /// [`Self::redacted`] for what a run said once it is over and its own
    /// secrets have gone: what it said was masked with them as it said it.
    pub(crate) fn redacted_after(&self, text: &str) -> String {
        let hide = Hide {
            secrets: self.secrets_for(None),
            codes: Vec::new(),
        };
        let mut text = text.to_owned();
        hide.cut(&mut text);
        hide_github_tokens(&text)
    }

    /// Who asked, as the account history shows it.
    pub(crate) fn caller_name(&self, caller: &Caller) -> Result<String, ProtocolError> {
        Ok(match caller.principal {
            Principal::Device { device_id } => self
                .read(|r| r.get::<crate::identity::Device>("device", device_id))?
                .map(|d| d.record.name)
                .unwrap_or_else(|| "A paired device".into()),
            Principal::LocalOwner => "This computer".into(),
            Principal::Channel { .. } => "A chat".into(),
            Principal::System => "Ophio".into(),
        })
    }
}

/// `text` with GitHub's token forms hidden: a known prefix and at least
/// twenty more of the characters tokens are made of.
pub(crate) fn hide_github_tokens(text: &str) -> String {
    const PREFIXES: [&str; 6] = ["github_pat_", "ghp_", "gho_", "ghu_", "ghs_", "ghr_"];
    let token_char = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while !rest.is_empty() {
        // A token starts where a word does.
        let at_word = out.chars().next_back().is_none_or(|c| !token_char(c));
        let token = PREFIXES.iter().filter(|_| at_word).find_map(|prefix| {
            let tail = rest.strip_prefix(prefix)?;
            let len = tail.find(|c| !token_char(c)).unwrap_or(tail.len());
            (len >= 20).then_some(prefix.len() + len)
        });
        match token {
            Some(len) => {
                out.push_str(ports::accounts::MASK);
                rest = &rest[len..];
            }
            None => {
                let c = rest.chars().next().expect("not empty");
                out.push(c);
                rest = &rest[c.len_utf8()..];
            }
        }
    }
    out
}

/// What a request of a run that is no longer browsing is refused with.
pub(crate) fn run_ended() -> ProtocolError {
    error(
        ErrorCode::PermissionDenied,
        "This run has ended; its tabs are closed.",
    )
}

pub(crate) fn account_error(e: AccountsError) -> ProtocolError {
    match e {
        AccountsError::NotFound(m) => error(ErrorCode::NotFound, m),
        AccountsError::Invalid(m) => error(ErrorCode::InvalidRequest, m),
        AccountsError::Busy(m) => error(ErrorCode::Conflict, m),
        AccountsError::NotReady(m) => error(ErrorCode::AccountNotReady, m),
        AccountsError::Failed(m) => error(ErrorCode::TemporarilyUnavailable, m),
    }
}
