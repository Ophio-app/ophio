// SPDX-License-Identifier: Apache-2.0
//! Finding out before it bites: when Claude Code or Codex isn't signed in on
//! the computer, the person's phones get one Needs-you item, "Claude isn't
//! signed in on Studio", whose Sign in again starts a sign-in finished in
//! the phone's own browser. None of it needs a model or a run.
//!
//! What is seen of an agent's sign-in, and from where:
//!
//! - its status command, at start and with each catalog check (every few
//!   minutes, and again after a run was refused for want of a sign-in or a
//!   sign-in finished in Accounts);
//! - a run the agent refused because its sign-in expired, which the
//!   adapter reports as [`ports::harness::HarnessEvent::SignedOut`];
//! - a sign-in finished in Accounts, or a reply from the model on any run,
//!   which prove it is signed in.
//!
//! One item is raised for each lapse and stays open until the agent is
//! signed in again: a lapse already raised raises nothing more. A status
//! command that still says "signed in" after a run was refused proves
//! nothing (Claude Code keeps an expired login it can't renew), so such a
//! lapse waits for a sign-in or a reply.
use crate::CompanionHub;
use protocol::{
    AttentionId, Event, ProtocolError,
    accounts::{AccountState, SignInStage},
    capability::{AccountReadiness, HarnessInfo, HarnessKind},
    model::{AttentionItem, AttentionKind},
    notifications::AlertTopic,
    time::Timestamp,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// The lapses raised and not yet cleared, by account, under `meta`.
const LAPSES: &str = "sign_in_lapses";
/// When each account's last lapse cleared, under `meta`: a status check
/// begun before then is older news, and raises nothing.
const CLEARED: &str = "sign_in_cleared";
/// The agents this computer has seen signed in, under `meta`. One installed
/// and never signed in here isn't a lapse, so its status raises nothing.
/// Absent until first used, and then seeded from the catalog already kept,
/// so an agent signed in before this was kept still counts.
const KNOWN: &str = "sign_in_known";
/// When the person last signed each agent out in Accounts, under `meta`:
/// its status saying "signed out" then is what they asked for, and a check
/// begun before it doesn't count it as signed in.
const SIGNED_OUT: &str = "sign_in_signed_out";
/// Above a finished task's news, below a question or approval waiting.
const PRIORITY: u8 = 10;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Lapse {
    attention_id: AttentionId,
    since: Timestamp,
    /// Seen only in a run, so far: its status command can't clear it.
    #[serde(default)]
    from_run: bool,
    /// The person put the notice away with Not now: this lapse raises no
    /// other.
    #[serde(default)]
    dismissed: bool,
}

/// What the accounts watcher last saw of the agents' entries.
#[derive(Default)]
pub(crate) struct Agents {
    /// The finished sign-in already acted on.
    done: Option<(String, Timestamp)>,
    states: BTreeMap<String, std::mem::Discriminant<AccountState>>,
}

/// What was seen of an agent's sign-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Seen {
    /// Its status command says it is signed out.
    Out,
    /// It refused a run for want of a sign-in.
    RefusedRun,
    /// Its status command says it is signed in.
    In,
    /// The model answered a run.
    Proven,
    /// A sign-in finished in Accounts.
    SignedIn,
    /// The person signed it out in Accounts.
    SignedOutOnPurpose,
}

/// The Accounts entry for an agent, and its short name for the notice.
pub(crate) fn account_of(harness: HarnessKind) -> Option<(&'static str, &'static str)> {
    match harness {
        HarnessKind::ClaudeCode => Some(("claude", "Claude")),
        HarnessKind::Codex => Some(("codex", "Codex")),
        _ => None,
    }
}

/// The agent an Accounts entry signs in.
pub(crate) fn harness_of(account_id: &str) -> Option<HarnessKind> {
    match account_id {
        "claude" => Some(HarnessKind::ClaudeCode),
        "codex" => Some(HarnessKind::Codex),
        _ => None,
    }
}

/// The notice's words, naming the computer as the person's phones do.
fn words(name: &str, computer: &str) -> (String, String) {
    (
        format!("{name} isn't signed in on {computer}"),
        format!("Tasks that use {name} can't run until it's signed in again."),
    )
}

/// Keeps what was seen of `harness`'s sign-in, as of `asked_at`, when what
/// saw it began (a status check, or the run the agent refused): raises the
/// notice for a new lapse, clears it once the agent is signed in again.
/// What began before the last change it could undo is older news, and
/// changes nothing.
pub(crate) fn seen(
    tx: &mut store::Write<'_>,
    harness: HarnessKind,
    seen: Seen,
    computer: &str,
    asked_at: Timestamp,
    now: Timestamp,
) -> store::Result<()> {
    let Some((account, name)) = account_of(harness) else {
        return Ok(());
    };
    let quoted = format!("\"{account}\"");
    // A reply from the model comes with nearly every run, and nearly always
    // with nothing to clear and the agent already known: nothing is decoded.
    if matches!(seen, Seen::In | Seen::Proven) {
        let lapsed = (tx.read().text("meta", LAPSES)?).is_some_and(|j| j.contains(&quoted));
        let known = (tx.read().text("meta", KNOWN)?).is_some_and(|j| j.contains(&quoted));
        if !known {
            // Signed in, unless the check began before the person signed
            // it out: then it says nothing of now.
            let out: BTreeMap<String, Timestamp> =
                tx.read().get("meta", SIGNED_OUT)?.unwrap_or_default();
            if out.get(account).is_none_or(|at| asked_at > *at) {
                let mut all = known_agents(tx)?;
                all.insert(account.to_owned());
                tx.put("meta", KNOWN, &all)?;
            }
        }
        if !lapsed {
            return Ok(());
        }
    }
    if seen == Seen::SignedIn {
        // Recorded with or without a lapse open: a run that began before
        // this sign-in had the older one, and its refusal raises nothing.
        let mut all = known_agents(tx)?;
        all.insert(account.to_owned());
        tx.put("meta", KNOWN, &all)?;
        stamp(tx, CLEARED, account, now)?;
    }
    if seen == Seen::SignedOutOnPurpose {
        // Not a lapse: armed again only once it is seen signed in.
        let mut all = known_agents(tx)?;
        all.remove(account);
        tx.put("meta", KNOWN, &all)?;
        stamp(tx, SIGNED_OUT, account, now)?;
    }
    let mut lapses: BTreeMap<String, Lapse> = tx.read().get("meta", LAPSES)?.unwrap_or_default();
    let cleared_since = |tx: &store::Write<'_>| -> store::Result<bool> {
        let cleared: BTreeMap<String, Timestamp> =
            tx.read().get("meta", CLEARED)?.unwrap_or_default();
        Ok(cleared.get(account).is_some_and(|at| *at > asked_at))
    };
    match (seen, lapses.get_mut(account)) {
        (Seen::Out, None) => {
            if !known_agents(tx)?.contains(account) || cleared_since(tx)? {
                return Ok(());
            }
            raise(tx, &mut lapses, account, name, computer, false, now)?;
        }
        // A run that began before the last sign-in had the older one.
        (Seen::RefusedRun, None) => {
            if cleared_since(tx)? {
                return Ok(());
            }
            raise(tx, &mut lapses, account, name, computer, true, now)?;
        }
        // Its status command now agrees, so it can clear it too.
        (Seen::Out, Some(lapse)) if lapse.from_run => lapse.from_run = false,
        // A check begun before the lapse was seen says nothing of it.
        (Seen::In, Some(lapse)) if !lapse.from_run && asked_at >= lapse.since => {
            let id = lapse.attention_id;
            lapses.remove(account);
            clear(tx, account, id, now)?;
        }
        (Seen::Proven | Seen::SignedIn, Some(lapse)) => {
            let id = lapse.attention_id;
            lapses.remove(account);
            clear(tx, account, id, now)?;
        }
        // Signed out on purpose: whatever was open is put away.
        (Seen::SignedOutOnPurpose, Some(lapse)) => {
            let id = lapse.attention_id;
            lapses.remove(account);
            resolve(tx, id, now)?;
        }
        _ => return Ok(()),
    }
    tx.put("meta", LAPSES, &lapses)
}

/// The agents seen signed in on this computer. The first time, before any
/// were kept, those the kept catalog last found signed in.
fn known_agents(tx: &mut store::Write<'_>) -> store::Result<BTreeSet<String>> {
    if let Some(known) = tx.read().get("meta", KNOWN)? {
        return Ok(known);
    }
    let catalog: Vec<HarnessInfo> = tx.read().get("meta", "harnesses")?.unwrap_or_default();
    let known: BTreeSet<String> = (catalog.iter())
        .filter(|info| matches!(info.account, AccountReadiness::SignedIn { .. }))
        .filter_map(|info| account_of(info.kind).map(|(account, _)| account.to_owned()))
        .collect();
    tx.put("meta", KNOWN, &known)?;
    Ok(known)
}

/// Records `now` for `account` in the map under `meta` `key`.
fn stamp(tx: &mut store::Write<'_>, key: &str, account: &str, now: Timestamp) -> store::Result<()> {
    let mut at: BTreeMap<String, Timestamp> = tx.read().get("meta", key)?.unwrap_or_default();
    at.insert(account.to_owned(), now);
    tx.put("meta", key, &at)
}

fn raise(
    tx: &mut store::Write<'_>,
    lapses: &mut BTreeMap<String, Lapse>,
    account: &str,
    name: &str,
    computer: &str,
    from_run: bool,
    now: Timestamp,
) -> store::Result<()> {
    let (title, body) = words(name, computer);
    let item = AttentionItem {
        child_id: None,
        attention_id: AttentionId::new(),
        kind: AttentionKind::WaitingForYou,
        title,
        body: Some(body),
        task_id: None,
        run_id: None,
        approval_id: None,
        created_at: now,
        resolved_at: None,
        priority: PRIORITY,
        topic: Some(AlertTopic::NeedsYou),
        quiet: false,
        report_id: None,
        account_id: Some(account.to_owned()),
        alert: None,
        alert_handled_at: None,
        agent_update: None,
        new_version: None,
    };
    lapses.insert(
        account.to_owned(),
        Lapse {
            attention_id: item.attention_id,
            since: now,
            from_run,
            dismissed: false,
        },
    );
    tx.put(crate::alerts::HEADLINE, item.attention_id, &item.title)?;
    tx.put("attention", item.attention_id, &item)?;
    tx.emit(Event::AttentionUpserted { item })?;
    Ok(())
}

/// Not now: the notice goes away, and this lapse raises no other. Once the
/// agent is signed in again, the next lapse alerts as any would.
pub(crate) fn dismiss(
    tx: &mut store::Write<'_>,
    account: &str,
    now: Timestamp,
) -> store::Result<bool> {
    let mut lapses: BTreeMap<String, Lapse> = tx.read().get("meta", LAPSES)?.unwrap_or_default();
    let Some(lapse) = lapses.get_mut(account) else {
        return Ok(false);
    };
    if !lapse.dismissed {
        lapse.dismissed = true;
        let id = lapse.attention_id;
        tx.put("meta", LAPSES, &lapses)?;
        resolve(tx, id, now)?;
    }
    Ok(true)
}

fn clear(
    tx: &mut store::Write<'_>,
    account: &str,
    id: AttentionId,
    now: Timestamp,
) -> store::Result<()> {
    stamp(tx, CLEARED, account, now)?;
    resolve(tx, id, now)
}

fn resolve(tx: &mut store::Write<'_>, id: AttentionId, now: Timestamp) -> store::Result<()> {
    let Some(mut item) = tx.read().get::<AttentionItem>("attention", id)? else {
        return Ok(());
    };
    if item.resolved_at.is_some() {
        return Ok(());
    }
    item.resolved_at = Some(now);
    tx.put("attention", id, &item)?;
    tx.emit(Event::AttentionResolved { attention_id: id })?;
    Ok(())
}

/// A sign-in finished, or the model answered: whatever lapse there was is
/// over. Nothing is raised, so no name is needed.
pub(crate) fn proven(
    tx: &mut store::Write<'_>,
    harness: HarnessKind,
    now: Timestamp,
) -> store::Result<()> {
    seen(tx, harness, Seen::Proven, "", now, now)
}

/// What a fresh catalog, checked from `asked_at`, says of each agent's
/// sign-in.
pub(crate) fn catalog_seen(
    tx: &mut store::Write<'_>,
    harnesses: &[HarnessInfo],
    computer: &str,
    asked_at: Timestamp,
    now: Timestamp,
) -> store::Result<()> {
    for info in harnesses {
        let seen_now = match info.account {
            AccountReadiness::SignedIn { .. } => Seen::In,
            AccountReadiness::SignedOut { .. } => Seen::Out,
            // Not installed, or the check couldn't tell: the last answer stands.
            AccountReadiness::Unknown { .. } => continue,
        };
        seen(tx, info.kind, seen_now, computer, asked_at, now)?;
    }
    Ok(())
}

impl CompanionHub {
    pub(crate) fn computer_name(&self, r: &store::Read<'_>) -> store::Result<String> {
        Ok(crate::computer_name::effective(r, &self.inner.hostname)?.0)
    }

    /// Keeps what was seen of `harness`'s sign-in, outside any other write.
    pub(crate) fn sign_in_seen(
        &self,
        harness: HarnessKind,
        what: Seen,
    ) -> Result<(), ProtocolError> {
        let now = self.inner.clock.now();
        self.write(|tx| {
            let computer = self.computer_name(&tx.read())?;
            seen(tx, harness, what, &computer, now, now)
        })
    }

    /// A run that began at `started` was refused because `harness`'s
    /// sign-in expired: the notice goes up, unless the agent was signed in
    /// again since the run began, and the status commands are asked again
    /// at once, so a sign-in gone for good shows as signed out everywhere.
    pub(crate) fn run_signed_out(
        &self,
        harness: HarnessKind,
        started: Timestamp,
    ) -> Result<(), ProtocolError> {
        let now = self.inner.clock.now();
        self.write(|tx| {
            let computer = self.computer_name(&tx.read())?;
            seen(tx, harness, Seen::RefusedRun, &computer, started, now)
        })?;
        self.check_sign_ins_soon();
        Ok(())
    }

    /// Follows the agents' entries in Accounts: a sign-in finished there
    /// clears the notice and checks again at once, so runs aren't refused
    /// on an older check; a sign-in Accounts finds gone checks again too.
    pub(crate) async fn agents_changed(&self, agents: &mut Agents) {
        let Some(accounts) = self.inner.accounts.clone() else {
            return;
        };
        let view = accounts.view().await;
        let mut again = false;
        if let Some(sign_in) = &view.sign_in
            && let SignInStage::Done { .. } = sign_in.stage
            && let Some(harness) = harness_of(&sign_in.account_id)
            && agents.done.as_ref() != Some(&(sign_in.account_id.clone(), sign_in.started_at))
        {
            agents.done = Some((sign_in.account_id.clone(), sign_in.started_at));
            if let Err(e) = self.sign_in_seen(harness, Seen::SignedIn) {
                tracing::debug!(code = ?e.code, "clearing a sign-in notice failed");
            }
            again = true;
        }
        for account in &view.accounts {
            if harness_of(&account.account_id).is_none() {
                continue;
            }
            let state = std::mem::discriminant(&account.state);
            let before = agents.states.insert(account.account_id.clone(), state);
            again |= before.is_some_and(|before| before != state);
        }
        if again {
            self.check_sign_ins_soon();
        }
    }

    /// Asks the agents' status commands again, in the background: one
    /// check at a time, and one more after it for whatever asked meanwhile.
    pub(crate) fn check_sign_ins_soon(&self) {
        use std::sync::atomic::Ordering;
        if self.inner.sign_in_checks.fetch_add(1, Ordering::AcqRel) > 0 {
            return;
        }
        let hub = self.clone();
        self.spawn(async move {
            loop {
                let asked = hub.inner.sign_in_checks.load(Ordering::Acquire);
                if let Err(e) = hub.refresh_catalog().await {
                    tracing::debug!(code = ?e.code, "checking the agents' sign-ins again failed");
                }
                if hub
                    .inner
                    .sign_in_checks
                    .compare_exchange(asked, 0, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    break;
                }
            }
        });
    }
}
