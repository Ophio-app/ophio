// SPDX-License-Identifier: Apache-2.0
//! The owner's inbox of problem reports other people send. Each new
//! problem raises one Needs-you item on the owner's devices and repeats
//! fold into it. What a reporter wrote is shown as data only: nothing here
//! starts work or hands it to an agent. The owner's verdict on a problem
//! reaches everyone who reported it.
use crate::{CompanionHub, api::Caller, error};
use intake::maintainer::{
    AgentBlock, CheckOutcome, CheckRefusal, NotRunReason, ReportCursor, ReportFilter, ReportPage,
    ReportRecord, TestCheck,
};
use protocol::{
    auth::Principal,
    model::{AttentionItem, AttentionKind},
    notifications::AlertTopic,
    reports::{
        DeclineReason, InboxReportCode, ReportBundle, ReportOrigin, ReportPlatform, ReportProgress,
        ReportTestKind, ReportVerdict, report_reference,
    },
    time::Timestamp,
    *,
};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, path::PathBuf, sync::Arc, time::Duration};

/// Each group of reports the inbox raised an item for, by its first report.
pub(crate) const INBOX: &str = "report_inbox";
/// How far the inbox has read the intake.
const CURSOR: &str = "report_inbox_cursor";
/// Reports read from the intake at a time.
const PAGE: usize = 50;
/// Groups whose status is checked in one round.
const REFRESH: usize = 200;
/// The longest stretch of a reporter's words an item shows.
const WORDS: usize = 280;
/// The longest screen or build an item shows.
const FIELD: usize = 80;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InboxConfig {
    /// The intake's maintainer address, such as `http://127.0.0.1:8071`.
    pub intake_url: String,
    /// The maintainer token, read once at start.
    pub intake_token_file: PathBuf,
    #[serde(default = "default_poll_seconds")]
    pub poll_seconds: u64,
}

fn default_poll_seconds() -> u64 {
    30
}

/// The intake as the inbox reads it.
#[async_trait::async_trait]
pub(crate) trait Source: Send + Sync {
    async fn page(&self, after: Option<ReportCursor>) -> Option<ReportPage>;
    /// One report with what was sent.
    async fn report(&self, report_id: ReportId) -> Option<ReportRecord>;
    /// Gives every open report in `group` this status. False if any could
    /// not be told; telling one twice changes nothing.
    async fn tell(&self, group: ReportId, status: &ReportProgress) -> bool;
    /// Lets the fixer take the group's first report. None if the intake
    /// couldn't be asked; false if it won't, because the report closed or
    /// the fixer is done with it.
    async fn admit(&self, group: ReportId) -> Option<bool>;
    /// Distinct reporter keys with open reports at this failure site,
    /// excluding this item's whole group.
    async fn sharing(&self, group: ReportId, failure_site: &str) -> Option<u64>;
}

#[async_trait::async_trait]
impl Source for intake::client::Client {
    async fn page(&self, after: Option<ReportCursor>) -> Option<ReportPage> {
        let filter = ReportFilter {
            after,
            limit: Some(PAGE),
            ..Default::default()
        };
        self.list(&filter).await.ok()
    }
    async fn report(&self, report_id: ReportId) -> Option<ReportRecord> {
        intake::client::Client::report(self, report_id).await.ok()
    }
    async fn tell(&self, group: ReportId, status: &ReportProgress) -> bool {
        let Some(first) = Source::report(self, group).await else {
            return false;
        };
        // A group shares its first report's signature.
        let mut filter = ReportFilter {
            signature: Some(first.signature.key()),
            limit: Some(PAGE),
            ..Default::default()
        };
        let mut open = Vec::new();
        loop {
            let Ok(page) = self.list(&filter).await else {
                return false;
            };
            open.extend(
                page.reports
                    .iter()
                    .filter(|r| r.group_id == group && !self::status(r).closed())
                    .map(|r| r.report_id),
            );
            match page.next {
                Some(next) => filter.after = Some(next),
                None => break,
            }
        }
        for report_id in open {
            if self.set_status(report_id, status.clone()).await.is_err() {
                return false;
            }
        }
        true
    }
    async fn sharing(&self, group: ReportId, failure_site: &str) -> Option<u64> {
        let mut filter = ReportFilter {
            failure_site: Some(failure_site.to_owned()),
            limit: Some(PAGE),
            ..Default::default()
        };
        let mut reporters = HashSet::new();
        loop {
            let page = self.list(&filter).await.ok()?;
            reporters.extend(
                page.reports
                    .iter()
                    .filter(|r| r.group_id != group && !status(r).closed())
                    .map(|r| r.key_fingerprint.clone()),
            );
            match page.next {
                Some(next) => filter.after = Some(next),
                None => return Some(reporters.len() as u64),
            }
        }
    }
    async fn admit(&self, group: ReportId) -> Option<bool> {
        match intake::client::Client::admit(self, group).await {
            Ok(()) => Some(true),
            Err(intake::client::Error::Status(403 | 404)) => Some(false),
            Err(_) => None,
        }
    }
}

/// Whether the owner chose a report for the fixer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Look {
    #[default]
    No,
    /// Chosen, until the intake has it.
    Asked,
    Chosen,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Cursor {
    after: Option<ReportCursor>,
    /// Whether the reports from before the inbox started have been read.
    /// Those raise quiet items.
    synced: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    /// The group's first report.
    group: ReportId,
    attention_id: AttentionId,
    reference: String,
    /// What the first report said, as the item shows it.
    detail: String,
    count: u64,
    status: ReportProgress,
    /// The owner's verdict, until the intake has it.
    #[serde(default)]
    telling: Option<ReportProgress>,
    #[serde(default)]
    look: Look,
    /// Kept privately; full code is fetched only when the person asks.
    #[serde(default)]
    code: Option<InboxReportCode>,
    #[serde(default)]
    check: Option<TestCheck>,
    #[serde(default)]
    sharing: Option<u64>,
}

impl Entry {
    fn body(&self) -> String {
        let times = match self.count {
            0 | 1 => "Reported once".to_owned(),
            n => format!("Reported {n} times"),
        };
        let telling = if self.telling.is_some() {
            ", not yet sent to reporters"
        } else {
            ""
        };
        let look = match self.look {
            Look::No => "",
            Look::Asked => "\nYou chose it for the fixer; not yet sent",
            Look::Chosen => "\nOphio is looking into it",
        };
        let mut detail = self.detail.clone();
        if self.code.is_some() {
            let check = self.check.as_ref();
            detail.push_str(&format!(
                "\nAt their build: {}\nOn main: {}",
                outcome_text(check.map(|c| &c.at_build)),
                outcome_text(check.map(|c| &c.on_main)),
            ));
            if let Some(check) = check {
                if check.at_build == CheckOutcome::Fails && check.on_main == CheckOutcome::Passes {
                    detail.push_str(
                        "\nFails at their build and passes on main: it may already be fixed there.",
                    );
                }
                if let Some(block) = check.agent_block {
                    detail.push('\n');
                    detail.push_str(agent_block_text(block));
                }
                if let Some(site) = &check.failure_site {
                    detail.push_str(&format!("\nFailure site (a hint): {}", field(site)));
                    match self.sharing {
                        Some(1) => detail.push_str("\n1 other reporter's open report fails here."),
                        Some(n) => detail.push_str(&format!(
                            "\n{n} other reporters have open reports that fail here."
                        )),
                        None => detail.push_str(
                            "\nOther reporters with open reports at this site couldn't be counted.",
                        ),
                    }
                }
            }
        }
        format!(
            "{detail}\n{times} · status: {}{telling}{look}",
            status_text(&self.status)
        )
    }
}

/// Whether `item` is one the inbox raised: a report's, about no task, for
/// the person to read.
pub(crate) fn is_inbox(item: &AttentionItem) -> bool {
    item.report_id.is_some() && item.task_id.is_none() && item.kind == AttentionKind::WaitingForYou
}

impl CompanionHub {
    /// Called only by companion composition, with the owner's maintainer
    /// token configured.
    pub fn start_report_inbox(&self, config: InboxConfig) {
        let token = match std::fs::read_to_string(&config.intake_token_file) {
            Ok(s) => s,
            Err(_) => {
                tracing::warn!("Report inbox stays off: intake token is unreadable");
                return;
            }
        };
        let intake = match intake::client::Client::new(&config.intake_url, token.trim()) {
            Ok(c) => c,
            Err(_) => {
                tracing::warn!("Report inbox stays off: intake configuration is invalid");
                return;
            }
        };
        let source: Arc<dyn Source> = Arc::new(intake);
        let hub = self.clone();
        let mut shutdown = self.inner.shutdown.subscribe();
        self.spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(config.poll_seconds.max(5)));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown.changed() => break,
                    _ = hub.inner.reports.inbox_wake.notified() => {}
                    _ = tick.tick() => {}
                }
                tokio::select! {
                    biased;
                    _ = shutdown.changed() => break,
                    result = hub.sync_report_inbox(&*source) => {
                        if result.is_err() {
                            tracing::warn!("Report inbox could not be updated");
                        }
                    }
                }
            }
        });
    }

    /// Reads what reached the intake since last time, then the status of
    /// the groups already raised.
    pub(crate) async fn sync_report_inbox(&self, source: &dyn Source) -> Result<(), ProtocolError> {
        self.send_verdicts(source).await?;
        self.send_looks(source).await?;
        let mut cursor: Cursor = self.read(|r| r.get(CURSOR, "intake"))?.unwrap_or_default();
        loop {
            let Some(page) = source.page(cursor.after.clone()).await else {
                return Ok(());
            };
            let more = page.next.is_some();
            for record in page.reports {
                cursor.after = Some(ReportCursor {
                    received_at: record.received_at,
                    report_id: record.report_id,
                });
                self.receive(source, record, !cursor.synced).await?;
                self.write(|tx| tx.put(CURSOR, "intake", &cursor))?;
            }
            if !more {
                break;
            }
        }
        if !cursor.synced {
            cursor.synced = true;
            self.write(|tx| tx.put(CURSOR, "intake", &cursor))?;
        }
        self.refresh(source).await
    }

    async fn receive(
        &self,
        source: &dyn Source,
        record: ReportRecord,
        backlog: bool,
    ) -> Result<(), ProtocolError> {
        let group = record.group_id;
        let entry: Option<Entry> = self.read(|r| r.get(INBOX, group))?;
        let now = self.inner.clock.now();
        if let Some(entry) = entry {
            // Counted already when the group was first read.
            if record.group_count <= entry.count {
                return Ok(());
            }
            return self.write(|tx| {
                let Some(mut entry) = tx.read().get::<Entry>(INBOX, group)? else {
                    return Ok(());
                };
                entry.count = entry.count.max(record.group_count);
                let open = tx
                    .read()
                    .get::<AttentionItem>("attention", entry.attention_id)?
                    .filter(|i| i.resolved_at.is_none());
                match open {
                    Some(mut item) => {
                        item.body = Some(entry.body());
                        put(tx, item)?;
                    }
                    // Put away, and someone else has met it since: it comes
                    // back, quietly.
                    None => {
                        let item = raise(&entry, group, now, true);
                        entry.attention_id = item.attention_id;
                        put(tx, item)?;
                    }
                }
                tx.put(INBOX, group, &entry)
            });
        }
        let status = status(&record);
        if status.closed() {
            return Ok(());
        }
        // A repeat of a group whose first report closed reads as its own.
        let sent = match record.bundle {
            Some(_) => Some(record.clone()),
            None => source.report(record.report_id).await,
        };
        let detail = match sent.as_ref().and_then(|r| r.bundle.as_ref()) {
            Some(bundle) => detail(bundle),
            None => "What was sent couldn't be read.".to_owned(),
        };
        let code = sent
            .as_ref()
            .and_then(|r| r.bundle.as_ref())
            .and_then(report_code);
        let check = sent.as_ref().and_then(|r| r.check.clone()).or(record.check);
        let sharing = match check.as_ref().and_then(|c| c.failure_site.as_deref()) {
            Some(site) if code.is_some() => source.sharing(group, site).await,
            _ => None,
        };
        let entry = Entry {
            group,
            attention_id: AttentionId::new(),
            reference: report_reference(&group),
            detail,
            count: record.group_count.max(1),
            status,
            telling: None,
            look: Look::No,
            code,
            check,
            sharing,
        };
        let item = raise(&entry, group, now, backlog);
        let entry = Entry {
            attention_id: item.attention_id,
            ..entry
        };
        self.write(|tx| {
            put(tx, item)?;
            tx.put(INBOX, group, &entry)
        })
    }

    /// Follows each raised group's status: an item says where its report
    /// is, and goes once nothing more will happen to it.
    async fn refresh(&self, source: &dyn Source) -> Result<(), ProtocolError> {
        let groups: Vec<Entry> = self.read(|r| r.list(INBOX))?;
        for entry in groups.into_iter().take(REFRESH) {
            let group = entry.group;
            let Some(record) = source.report(group).await else {
                continue;
            };
            let status = status(&record);
            // Also hydrate older inbox entries, which predate submitted code.
            let detail = record
                .bundle
                .as_ref()
                .map(detail)
                .unwrap_or_else(|| entry.detail.clone());
            let code = record
                .bundle
                .as_ref()
                .map(report_code)
                .unwrap_or_else(|| entry.code.clone());
            let check = record.check;
            let site = check.as_ref().and_then(|c| c.failure_site.as_deref());
            let old_site = entry.check.as_ref().and_then(|c| c.failure_site.as_deref());
            let sharing = match site {
                Some(site) if code.is_some() && !status.closed() => source
                    .sharing(group, site)
                    .await
                    .or_else(|| (Some(site) == old_site).then_some(entry.sharing).flatten()),
                _ => None,
            };
            // A verdict that closed the report still has its item to put away.
            if status == entry.status
                && !status.closed()
                && record.group_count <= entry.count
                && check == entry.check
                && sharing == entry.sharing
                && detail == entry.detail
                && code == entry.code
            {
                continue;
            }
            let now = self.inner.clock.now();
            self.write(|tx| {
                // The owner's verdict comes first; the next round reads it back.
                let Some(entry) = tx
                    .read()
                    .get::<Entry>(INBOX, group)?
                    .filter(|e| e.telling.is_none())
                else {
                    return Ok(());
                };
                let open = tx
                    .read()
                    .get::<AttentionItem>("attention", entry.attention_id)?
                    .filter(|i| i.resolved_at.is_none());
                if status.closed() {
                    tx.delete(INBOX, group)?;
                    if let Some(mut item) = open {
                        item.resolved_at = Some(now);
                        tx.put("attention", item.attention_id, &item)?;
                        tx.emit(Event::AttentionResolved {
                            attention_id: item.attention_id,
                        })?;
                    }
                    return Ok(());
                }
                let entry = Entry {
                    count: entry.count.max(record.group_count),
                    status,
                    detail,
                    code,
                    check,
                    sharing,
                    ..entry
                };
                if let Some(mut item) = open {
                    item.body = Some(entry.body());
                    put(tx, item)?;
                }
                tx.put(INBOX, group, &entry)
            })?;
        }
        Ok(())
    }
}

impl CompanionHub {
    /// Submitted code is only data for the person, never a prompt or a notification.
    pub(crate) fn inbox_report_code(
        &self,
        caller: &Caller,
        report_id: ReportId,
    ) -> Result<Option<InboxReportCode>, ProtocolError> {
        let entry: Option<Entry> = self.read(|r| {
            person(caller, "Only the person can read a report's submitted code")?;
            r.get(INBOX, report_id)
        })?;
        Ok(entry.and_then(|e| e.code).map(|mut code| {
            code.path = plain(&code.path);
            code.source = plain_code(&code.source);
            code.patch = code.patch.map(|p| plain_code(&p));
            code
        }))
    }

    /// Keeps the owner's verdict on a report in their inbox for the intake,
    /// and shows it on the item at once.
    pub(crate) fn set_report_verdict(
        &self,
        tx: &mut store::Write<'_>,
        caller: &Caller,
        report_id: ReportId,
        verdict: &ReportVerdict,
    ) -> store::Result<()> {
        person(caller, "Only the person can answer a problem report")?;
        let Some(mut entry) = tx.read().get::<Entry>(INBOX, report_id)? else {
            return Err(error(ErrorCode::NotFound, "That report isn't in your inbox").into());
        };
        let status = match verdict {
            ReportVerdict::Reproduced => ReportProgress::Reproduced,
            ReportVerdict::NotReproduced => ReportProgress::NotReproduced,
            ReportVerdict::Fixed => ReportProgress::Fixed {
                build: self.inner.reports.config.build.clone(),
            },
            ReportVerdict::Declined { reason } => ReportProgress::Declined { reason: *reason },
        };
        entry.status = status.clone();
        entry.telling = Some(status);
        if let Some(mut item) = tx
            .read()
            .get::<AttentionItem>("attention", entry.attention_id)?
            .filter(|i| i.resolved_at.is_none())
        {
            item.body = Some(entry.body());
            put(tx, item)?;
        }
        tx.put(INBOX, report_id, &entry)
    }

    /// Chooses a report in the owner's inbox for the fixer, which takes
    /// nothing the owner hasn't chosen.
    pub(crate) fn look_into_report(
        &self,
        tx: &mut store::Write<'_>,
        caller: &Caller,
        report_id: ReportId,
    ) -> store::Result<()> {
        person(caller, "Only the person can choose a report for the fixer")?;
        if !self
            .inner
            .reports
            .fixer
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return Err(error(
                ErrorCode::Unsupported,
                "The fixer isn't on for this computer",
            )
            .into());
        }
        let Some(mut entry) = tx.read().get::<Entry>(INBOX, report_id)? else {
            return Err(error(ErrorCode::NotFound, "That report isn't in your inbox").into());
        };
        if entry
            .check
            .as_ref()
            .is_some_and(|c| c.agent_block.is_some())
        {
            return Err(error(
                ErrorCode::PermissionDenied,
                "The fixer can't use this report's test: no agent may be shown its code",
            )
            .into());
        }
        if entry.look != Look::No {
            return Ok(());
        }
        entry.look = Look::Asked;
        if let Some(mut item) = tx
            .read()
            .get::<AttentionItem>("attention", entry.attention_id)?
            .filter(|i| i.resolved_at.is_none())
        {
            item.body = Some(entry.body());
            put(tx, item)?;
        }
        tx.put(INBOX, report_id, &entry)
    }

    /// Sends the owner's choices to the intake, which hands the fixer only
    /// those.
    async fn send_looks(&self, source: &dyn Source) -> Result<(), ProtocolError> {
        let entries: Vec<Entry> = self.read(|r| r.list(INBOX))?;
        for entry in entries.into_iter().filter(|e| e.look == Look::Asked) {
            let Some(admitted) = source.admit(entry.group).await else {
                continue;
            };
            self.write(|tx| {
                let Some(mut entry) = tx
                    .read()
                    .get::<Entry>(INBOX, entry.group)?
                    .filter(|e| e.look == Look::Asked)
                else {
                    return Ok(());
                };
                // Refused, it closed or the fixer is done; the status says which.
                entry.look = if admitted { Look::Chosen } else { Look::No };
                if let Some(mut item) = tx
                    .read()
                    .get::<AttentionItem>("attention", entry.attention_id)?
                    .filter(|i| i.resolved_at.is_none())
                {
                    item.body = Some(entry.body());
                    put(tx, item)?;
                }
                tx.put(INBOX, entry.group, &entry)
            })?;
        }
        Ok(())
    }

    /// Sends the verdicts the intake doesn't have yet.
    async fn send_verdicts(&self, source: &dyn Source) -> Result<(), ProtocolError> {
        let entries: Vec<Entry> = self.read(|r| r.list(INBOX))?;
        for entry in entries {
            let Some(status) = entry.telling else {
                continue;
            };
            if !source.tell(entry.group, &status).await {
                continue;
            }
            self.write(|tx| {
                // Unless the owner changed their mind meanwhile.
                let Some(mut entry) = tx
                    .read()
                    .get::<Entry>(INBOX, entry.group)?
                    .filter(|e| e.telling.as_ref() == Some(&status))
                else {
                    return Ok(());
                };
                entry.telling = None;
                if let Some(mut item) = tx
                    .read()
                    .get::<AttentionItem>("attention", entry.attention_id)?
                    .filter(|i| i.resolved_at.is_none())
                {
                    item.body = Some(entry.body());
                    put(tx, item)?;
                }
                tx.put(INBOX, entry.group, &entry)
            })?;
        }
        Ok(())
    }
}

/// Only the person, on a paired device or at the computer, answers reports.
fn person(caller: &Caller, refusal: &str) -> store::Result<()> {
    if matches!(
        caller.principal,
        Principal::Device { .. } | Principal::LocalOwner
    ) {
        Ok(())
    } else {
        Err(error(ErrorCode::PermissionDenied, refusal).into())
    }
}

fn status(record: &ReportRecord) -> ReportProgress {
    record
        .history
        .last()
        .map(|c| c.status.clone())
        .unwrap_or(ReportProgress::Queued)
}

fn raise(entry: &Entry, group: ReportId, now: Timestamp, quiet: bool) -> AttentionItem {
    AttentionItem {
        child_id: None,
        attention_id: entry.attention_id,
        kind: AttentionKind::WaitingForYou,
        title: format!("New problem report {}", entry.reference),
        body: Some(entry.body()),
        task_id: None,
        run_id: None,
        approval_id: None,
        created_at: now,
        resolved_at: None,
        priority: 50,
        topic: Some(AlertTopic::Reports),
        quiet,
        report_id: Some(group),
    }
}

fn put(tx: &mut store::Write<'_>, item: AttentionItem) -> store::Result<()> {
    tx.put("attention", item.attention_id, &item)?;
    tx.emit(Event::AttentionUpserted { item })?;
    Ok(())
}

/// What a report says, for the owner to read. The words are the
/// reporter's own and untrusted: one line of plain text, cut short.
fn detail(bundle: &ReportBundle) -> String {
    let words = bundle.words.as_deref().map(plain).filter(|w| !w.is_empty());
    let mut lines = vec![match words {
        Some(words) => format!("They wrote: “{}”", protocol::alerts::cut(&words, WORDS)),
        None => "They wrote nothing.".to_owned(),
    }];
    let mut about = vec![
        format!("Screen {}", field(&bundle.signature.screen)),
        format!("build {}", field(&bundle.signature.build)),
    ];
    if let Some(phone) = &bundle.versions.phone {
        let platform = match phone.platform {
            ReportPlatform::Android => "Android",
            ReportPlatform::Ios => "iPhone",
            ReportPlatform::Web => "browser",
        };
        about.push(format!(
            "{platform} {}, {}",
            field(&phone.os),
            field(&phone.model)
        ));
    }
    lines.push(about.join(" · "));
    let mut how = vec![match &bundle.origin {
        ReportOrigin::Error => format!("From an error ({})", field(&bundle.signature.code)),
        ReportOrigin::Crash => format!("After a crash ({})", field(&bundle.signature.code)),
        ReportOrigin::Screenshot => "After a screenshot".to_owned(),
        ReportOrigin::Settings => "From Settings".to_owned(),
        ReportOrigin::FollowUp { report_id } => {
            format!("Following up report {}", report_reference(report_id))
        }
    }];
    if bundle.screenshot.is_some() {
        how.push("with a picture".to_owned());
    }
    lines.push(how.join(", "));
    if let Some(test) = &bundle.test {
        let kind = match test.kind {
            ReportTestKind::RustIntegration => "Rust integration test",
            ReportTestKind::RustUnit => "Rust unit test",
            ReportTestKind::Vitest => "Vitest test",
        };
        let harness = match test.investigator.harness {
            capability::HarnessKind::ClaudeCode => "Claude Code",
            capability::HarnessKind::Codex => "Codex",
            capability::HarnessKind::Hermes => "Hermes",
            capability::HarnessKind::OpenClaw => "OpenClaw",
            capability::HarnessKind::Pi => "Pi",
        };
        lines.push(format!("Their test: {} ({kind})", field(&test.path)));
        lines.push(format!(
            "Written by their model: {harness} · {}",
            field(&test.investigator.model)
        ));
        if test.patch.is_some() {
            lines.push("Their suggested fix is available.".to_owned());
        }
    }
    lines.join("\n")
}

fn report_code(bundle: &ReportBundle) -> Option<InboxReportCode> {
    bundle.test.as_ref().map(|test| InboxReportCode {
        kind: test.kind,
        path: test.path.clone(),
        source: test.source.clone(),
        patch: test.patch.clone(),
    })
}

fn agent_block_text(block: AgentBlock) -> &'static str {
    match block {
        AgentBlock::Unreducible => {
            "Look into it can't use it: its code can't be read safely enough to show an agent."
        }
        AgentBlock::LongLiteral => {
            "Look into it can't use it: it has a string longer than 256 bytes, which no agent is shown."
        }
    }
}

fn outcome_text(outcome: Option<&CheckOutcome>) -> &'static str {
    match outcome {
        None => "not checked yet",
        Some(CheckOutcome::Fails) => "reports an assertion failure",
        Some(CheckOutcome::FailsWithoutAssertion) => "reports a failure without an assertion",
        Some(CheckOutcome::Passes) => "passes",
        Some(CheckOutcome::DoesNotBuild) => "doesn't build",
        Some(CheckOutcome::NoTestRan) => "no test ran",
        Some(CheckOutcome::TimedOut) => "timed out",
        Some(CheckOutcome::OverLimits) => "hit the runner's limits",
        Some(CheckOutcome::Crashed) => "stops abnormally (crash or early exit)",
        Some(CheckOutcome::UnreadableOutput) => "its output couldn't be read as a test report",
        Some(CheckOutcome::Refused { reason }) => match reason {
            CheckRefusal::UnknownCommit => "couldn't check (build commit isn't available)",
            CheckRefusal::NotOnMain => "couldn't check (build commit isn't part of main)",
            CheckRefusal::PathTaken => "couldn't check (test path is already taken)",
            CheckRefusal::NoSuchFolder => "couldn't check (test folder doesn't exist)",
            CheckRefusal::InvalidTest => "couldn't check (test isn't safe to run as sent)",
            CheckRefusal::HostileSignal => "couldn't check (run raised a hostile signal)",
        },
        Some(CheckOutcome::NotRun { reason }) => match reason {
            NotRunReason::MissingDependencies => "not run (offline dependencies aren't available)",
            NotRunReason::RunnerUnavailable => "not run (contained runner isn't available)",
            NotRunReason::SubmissionRefused => "not run (the other run was refused)",
        },
    }
}

/// Preserve code layout, but never invisible controls or direction changes.
fn plain_code(text: &str) -> String {
    text.chars()
        .filter(|c| (*c == '\n' || *c == '\t' || !c.is_control()) && !is_format(*c))
        .collect()
}

fn field(text: &str) -> String {
    protocol::alerts::cut(&plain(text), FIELD)
}

/// `text` as one line with no control or formatting characters.
fn plain(text: &str) -> String {
    let spaced: String = text
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .filter(|c| !c.is_control() && !is_format(*c))
        .collect();
    spaced.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Bidirectional and zero-width characters, which can make text read other
/// than it is.
fn is_format(c: char) -> bool {
    matches!(c,
        '\u{00AD}' | '\u{0600}'..='\u{0605}' | '\u{061C}' | '\u{06DD}' | '\u{070F}' |
        '\u{0890}'..='\u{0891}' | '\u{08E2}' | '\u{180E}' | '\u{200B}'..='\u{200F}' |
        '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{206F}' | '\u{FEFF}' | '\u{FFF9}'..='\u{FFFB}' |
        '\u{110BD}' | '\u{110CD}' | '\u{13430}'..='\u{1343F}' | '\u{1BCA0}'..='\u{1BCA3}' |
        '\u{1D173}'..='\u{1D17A}' | '\u{E0001}' | '\u{E0020}'..='\u{E007F}'
    )
}

fn status_text(status: &ReportProgress) -> String {
    match status {
        ReportProgress::Queued | ReportProgress::Received => "new".into(),
        ReportProgress::Known { fixed_in: Some(b) } => format!("known, fixed in {}", field(b)),
        ReportProgress::Known { fixed_in: None } => "known".into(),
        ReportProgress::Question => "waiting on the reporter".into(),
        ReportProgress::Reproduced => "reproduced".into(),
        ReportProgress::NotReproduced => "not reproduced".into(),
        ReportProgress::FixWritten => "fix written".into(),
        ReportProgress::InReview => "fix in review".into(),
        ReportProgress::Fixed { build } => format!("fixed in {}", field(build)),
        ReportProgress::Shipped { build } => format!("shipped in {}", field(build)),
        ReportProgress::Declined { reason } => match reason {
            DeclineReason::WorksAsDocumented => "declined, works as documented".into(),
            DeclineReason::FeatureRequest => "declined, a feature request".into(),
            DeclineReason::Duplicate => "declined, a duplicate".into(),
            DeclineReason::NotEnoughInformation => "declined, not enough to go on".into(),
            DeclineReason::OutsideOphio => "declined, outside Ophio".into(),
        },
        ReportProgress::Withdrawn => "withdrawn".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{Caller, Hub};
    use protocol::{
        command::{Command, CommandEnvelope},
        reports::{REPORT_SCHEMA, ReportContext, ReportSignature, ReportVersions, StatusChange},
    };
    use std::sync::Mutex;

    /// The intake, as far as the inbox reads it.
    #[derive(Default)]
    struct Fake {
        records: Mutex<Vec<ReportRecord>>,
        bundles: Mutex<Vec<ReportBundle>>,
        /// Refuses what the inbox tells it, as an intake out of reach.
        deaf: std::sync::atomic::AtomicBool,
        /// The groups the fixer may take.
        admitted: Mutex<Vec<ReportId>>,
    }

    impl Fake {
        /// Receives `bundle`, as one more of `group` when given.
        fn send(&self, bundle: ReportBundle, group: Option<ReportId>) -> ReportId {
            let id = bundle.report_id;
            let mut records = self.records.lock().unwrap();
            let group = group.filter(|_| bundle.test.is_none());
            let group_id = group.unwrap_or(id);
            let now = Timestamp(records.len() as i64 + 1);
            records.push(ReportRecord {
                report_id: id,
                key_fingerprint: "key".into(),
                signature: bundle.signature.clone(),
                received_at: now,
                updated_at: now,
                history: vec![StatusChange {
                    status: ReportProgress::Received,
                    at: now,
                }],
                questions: vec![],
                duplicate_of: group,
                group_id,
                group_count: 0,
                lease: None,
                bundle: None,
                check: None,
            });
            let count = records.iter().filter(|r| r.group_id == group_id).count() as u64;
            for r in records.iter_mut().filter(|r| r.group_id == group_id) {
                r.group_count = count;
            }
            self.bundles.lock().unwrap().push(bundle);
            id
        }
        fn set(&self, id: ReportId, status: ReportProgress) {
            let mut records = self.records.lock().unwrap();
            let r = records.iter_mut().find(|r| r.report_id == id).unwrap();
            r.history.push(StatusChange {
                status,
                at: Timestamp(99),
            });
        }
    }

    #[async_trait::async_trait]
    impl Source for Fake {
        async fn page(&self, after: Option<ReportCursor>) -> Option<ReportPage> {
            let records = self.records.lock().unwrap();
            let start = match after {
                Some(c) => records.iter().position(|r| r.report_id == c.report_id)? + 1,
                None => 0,
            };
            // Two at a time, so reading goes over pages.
            let reports: Vec<_> = records.iter().skip(start).take(2).cloned().collect();
            let next = (records.len() > start + 2).then(|| {
                let last = reports.last().unwrap();
                ReportCursor {
                    received_at: last.received_at,
                    report_id: last.report_id,
                }
            });
            Some(ReportPage { reports, next })
        }
        async fn report(&self, report_id: ReportId) -> Option<ReportRecord> {
            let mut record = (self.records.lock().unwrap().iter())
                .find(|r| r.report_id == report_id)?
                .clone();
            record.bundle = (self.bundles.lock().unwrap().iter())
                .find(|b| b.report_id == report_id)
                .cloned();
            Some(record)
        }
        async fn tell(&self, group: ReportId, status: &ReportProgress) -> bool {
            if self.deaf.load(std::sync::atomic::Ordering::SeqCst) {
                return false;
            }
            let mut records = self.records.lock().unwrap();
            for r in records.iter_mut().filter(|r| r.group_id == group) {
                if !super::status(r).closed() && super::status(r) != *status {
                    r.history.push(StatusChange {
                        status: status.clone(),
                        at: Timestamp(99),
                    });
                }
            }
            true
        }
        async fn sharing(&self, group: ReportId, failure_site: &str) -> Option<u64> {
            if self.deaf.load(std::sync::atomic::Ordering::SeqCst) {
                return None;
            }
            Some(
                self.records
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|r| {
                        r.group_id != group
                            && !super::status(r).closed()
                            && r.check.as_ref().and_then(|c| c.failure_site.as_deref())
                                == Some(failure_site)
                    })
                    .map(|r| &r.key_fingerprint)
                    .collect::<HashSet<_>>()
                    .len() as u64,
            )
        }
        async fn admit(&self, group: ReportId) -> Option<bool> {
            if self.deaf.load(std::sync::atomic::Ordering::SeqCst) {
                return None;
            }
            if self.status_of(group).closed() {
                return Some(false);
            }
            let mut admitted = self.admitted.lock().unwrap();
            if !admitted.contains(&group) {
                admitted.push(group);
            }
            Some(true)
        }
    }

    impl Fake {
        fn status_of(&self, id: ReportId) -> ReportProgress {
            let records = self.records.lock().unwrap();
            super::status(records.iter().find(|r| r.report_id == id).unwrap())
        }
    }

    async fn look(
        hub: &CompanionHub,
        caller: &Caller,
        report_id: ReportId,
    ) -> Option<ProtocolError> {
        let envelope = CommandEnvelope::new(Command::LookIntoReport { report_id });
        hub.submit(caller, envelope).await.error
    }

    async fn verdict(
        hub: &CompanionHub,
        caller: &Caller,
        report_id: ReportId,
        verdict: ReportVerdict,
    ) -> Option<ProtocolError> {
        let envelope = CommandEnvelope::new(Command::SetReportVerdict { report_id, verdict });
        hub.submit(caller, envelope).await.error
    }

    fn bundle(words: &str) -> ReportBundle {
        ReportBundle {
            schema: REPORT_SCHEMA,
            report_id: ReportId::new(),
            created_at: Timestamp(1),
            origin: ReportOrigin::Error,
            words: Some(words.into()),
            signature: ReportSignature {
                code: "internal".into(),
                screen: "/work/:task".into(),
                build: "abc123".into(),
            },
            versions: ReportVersions::default(),
            context: ReportContext::default(),
            phone_trail: vec![],
            computer_trail: vec![],
            state: None,
            crashes: vec![],
            screenshot: None,
            extras: vec![],
            test: None,
        }
    }

    async fn hub(dir: &tempfile::TempDir) -> CompanionHub {
        CompanionHub::builder()
            .data_dir(dir.path().join("hub"))
            .build()
            .await
            .unwrap()
    }

    fn open(hub: &CompanionHub) -> Vec<AttentionItem> {
        let mut items: Vec<AttentionItem> = hub
            .read(|r| r.list("attention"))
            .unwrap()
            .into_iter()
            .filter(|i: &AttentionItem| i.resolved_at.is_none())
            .collect();
        items.sort_by_key(|i| i.created_at);
        items
    }

    #[tokio::test]
    async fn reports_from_before_are_quiet_and_new_ones_alert() {
        let dir = tempfile::tempdir().unwrap();
        let hub = hub(&dir).await;
        let intake = Fake::default();
        for words in ["one", "two", "three"] {
            let mut b = bundle(words);
            b.signature.screen = format!("/{words}");
            intake.send(b, None);
        }
        hub.sync_report_inbox(&intake).await.unwrap();
        let items = open(&hub);
        assert_eq!(items.len(), 3);
        assert!(items.iter().all(|i| i.quiet && is_inbox(i)));

        let fresh = intake.send(bundle("It stopped"), None);
        hub.sync_report_inbox(&intake).await.unwrap();
        let item = (open(&hub).into_iter())
            .find(|i| i.report_id == Some(fresh))
            .unwrap();
        assert!(!item.quiet);
        assert_eq!(
            item.title,
            format!("New problem report {}", report_reference(&fresh))
        );
        let body = item.body.clone().unwrap();
        assert!(body.starts_with("They wrote: “It stopped”\nScreen /work/:task · build abc123"));
        assert!(body.ends_with("Reported once · status: new"), "{body}");

        let (alerts, _) = hub.alerts_open(&Caller::local_owner()).unwrap();
        let alert = (alerts.iter())
            .find(|a| a.attention_id == item.attention_id)
            .unwrap();
        assert!(!alert.quiet);
        assert_eq!(alert.project, "Problem reports");
        assert_eq!(alert.headline.as_deref(), Some(item.title.as_str()));
        assert_eq!(alert.topic, Some(AlertTopic::Reports));
        assert!(alert.report.is_none());
        // Reading again raises nothing more.
        hub.sync_report_inbox(&intake).await.unwrap();
        assert_eq!(open(&hub).len(), 4);
        hub.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn repeats_fold_in_and_bring_back_a_report_put_away() {
        let dir = tempfile::tempdir().unwrap();
        let hub = hub(&dir).await;
        let intake = Fake::default();
        hub.sync_report_inbox(&intake).await.unwrap();
        let first = intake.send(bundle("Broke"), None);
        intake.send(bundle("Broke again"), Some(first));
        hub.sync_report_inbox(&intake).await.unwrap();
        let items = open(&hub);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].report_id, Some(first));
        let body = items[0].body.clone().unwrap();
        assert!(
            body.contains("“Broke”") && body.contains("Reported 2 times"),
            "{body}"
        );

        let receipt = hub
            .submit(
                &Caller::local_owner(),
                CommandEnvelope::new(Command::AcknowledgeAttention {
                    attention_id: items[0].attention_id,
                }),
            )
            .await;
        assert_eq!(receipt.error, None);
        assert!(open(&hub).is_empty());
        hub.sync_report_inbox(&intake).await.unwrap();
        assert!(open(&hub).is_empty());

        intake.send(bundle("And again"), Some(first));
        hub.sync_report_inbox(&intake).await.unwrap();
        let items = open(&hub);
        assert_eq!(items.len(), 1);
        assert!(items[0].quiet);
        assert!(
            items[0]
                .body
                .as_deref()
                .unwrap()
                .contains("Reported 3 times")
        );
        hub.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn an_item_follows_its_report_and_goes_when_it_closes() {
        let dir = tempfile::tempdir().unwrap();
        let hub = hub(&dir).await;
        let intake = Fake::default();
        hub.sync_report_inbox(&intake).await.unwrap();
        let id = intake.send(bundle("Broke"), None);
        hub.sync_report_inbox(&intake).await.unwrap();
        intake.set(id, ReportProgress::Reproduced);
        hub.sync_report_inbox(&intake).await.unwrap();
        let items = open(&hub);
        assert!(
            items[0]
                .body
                .as_deref()
                .unwrap()
                .ends_with("status: reproduced")
        );
        assert!(!items[0].quiet);

        intake.set(
            id,
            ReportProgress::Shipped {
                build: "def456".into(),
            },
        );
        hub.sync_report_inbox(&intake).await.unwrap();
        assert!(open(&hub).is_empty());
        assert!(hub.read(|r| r.list::<Entry>(INBOX)).unwrap().is_empty());
        hub.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn a_report_closed_before_it_is_read_raises_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let hub = hub(&dir).await;
        let intake = Fake::default();
        let id = intake.send(bundle("Broke"), None);
        intake.set(id, ReportProgress::Withdrawn);
        hub.sync_report_inbox(&intake).await.unwrap();
        assert!(open(&hub).is_empty());
        hub.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn a_verdict_reaches_everyone_who_sent_the_problem() {
        let dir = tempfile::tempdir().unwrap();
        let hub = hub(&dir).await;
        let intake = Fake::default();
        hub.sync_report_inbox(&intake).await.unwrap();
        let first = intake.send(bundle("Broke"), None);
        let again = intake.send(bundle("Broke again"), Some(first));
        let gone = intake.send(bundle("Me too"), Some(first));
        intake.set(gone, ReportProgress::Withdrawn);
        let other = intake.send(bundle("Something else"), None);
        hub.sync_report_inbox(&intake).await.unwrap();

        // Out of reach: the item says so, and nobody hears yet.
        intake.deaf.store(true, std::sync::atomic::Ordering::SeqCst);
        let owner = Caller::local_owner();
        assert_eq!(
            verdict(&hub, &owner, first, ReportVerdict::Fixed).await,
            None
        );
        let build = hub.inner.reports.config.build.clone();
        let fixed = ReportProgress::Fixed { build };
        let body = |hub: &CompanionHub| {
            (open(hub).into_iter())
                .find(|i| i.report_id == Some(first))
                .and_then(|i| i.body)
                .unwrap()
        };
        assert!(
            body(&hub).ends_with(", not yet sent to reporters"),
            "{}",
            body(&hub)
        );
        hub.sync_report_inbox(&intake).await.unwrap();
        assert_eq!(intake.status_of(again), ReportProgress::Received);
        assert!(body(&hub).ends_with(", not yet sent to reporters"));

        intake
            .deaf
            .store(false, std::sync::atomic::Ordering::SeqCst);
        hub.sync_report_inbox(&intake).await.unwrap();
        assert_eq!(intake.status_of(first), fixed);
        assert_eq!(intake.status_of(again), fixed);
        assert_eq!(intake.status_of(gone), ReportProgress::Withdrawn);
        assert_eq!(intake.status_of(other), ReportProgress::Received);
        let sent = body(&hub);
        assert!(
            sent.contains("status: fixed in ") && !sent.contains("not yet sent"),
            "{sent}"
        );
        hub.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn the_intake_does_not_undo_a_verdict_it_has_yet_to_hear() {
        let dir = tempfile::tempdir().unwrap();
        let hub = hub(&dir).await;
        let intake = Fake::default();
        hub.sync_report_inbox(&intake).await.unwrap();
        let id = intake.send(bundle("Broke"), None);
        hub.sync_report_inbox(&intake).await.unwrap();
        intake.deaf.store(true, std::sync::atomic::Ordering::SeqCst);
        let owner = Caller::local_owner();
        assert_eq!(
            verdict(&hub, &owner, id, ReportVerdict::Reproduced).await,
            None
        );
        // A second report meanwhile changes the intake's side, not the verdict.
        intake.send(bundle("Broke again"), Some(id));
        hub.sync_report_inbox(&intake).await.unwrap();
        let entry: Entry = hub.read(|r| r.get(INBOX, id)).unwrap().unwrap();
        assert_eq!(entry.status, ReportProgress::Reproduced);
        assert_eq!(entry.telling, Some(ReportProgress::Reproduced));
        // The owner's later word replaces the earlier one.
        assert_eq!(
            verdict(&hub, &owner, id, ReportVerdict::NotReproduced).await,
            None
        );
        intake
            .deaf
            .store(false, std::sync::atomic::Ordering::SeqCst);
        hub.sync_report_inbox(&intake).await.unwrap();
        assert_eq!(intake.status_of(id), ReportProgress::NotReproduced);
        let entry: Entry = hub.read(|r| r.get(INBOX, id)).unwrap().unwrap();
        assert_eq!(entry.telling, None);
        assert_eq!(entry.count, 2);
        hub.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn a_declined_report_leaves_the_inbox() {
        let dir = tempfile::tempdir().unwrap();
        let hub = hub(&dir).await;
        let intake = Fake::default();
        hub.sync_report_inbox(&intake).await.unwrap();
        let id = intake.send(bundle("Make it purple"), None);
        hub.sync_report_inbox(&intake).await.unwrap();
        let reason = DeclineReason::FeatureRequest;
        let declined = ReportVerdict::Declined { reason };
        assert_eq!(
            verdict(&hub, &Caller::local_owner(), id, declined).await,
            None
        );
        hub.sync_report_inbox(&intake).await.unwrap();
        assert_eq!(intake.status_of(id), ReportProgress::Declined { reason });
        assert!(open(&hub).is_empty());
        assert!(hub.read(|r| r.list::<Entry>(INBOX)).unwrap().is_empty());
        // Nothing to answer any more.
        let refused = verdict(&hub, &Caller::local_owner(), id, ReportVerdict::Fixed).await;
        assert_eq!(refused.map(|e| e.code), Some(ErrorCode::NotFound));
        hub.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn the_fixer_takes_only_what_the_person_chose() {
        let dir = tempfile::tempdir().unwrap();
        let hub = hub(&dir).await;
        let intake = Fake::default();
        hub.sync_report_inbox(&intake).await.unwrap();
        let id = intake.send(bundle("Broke"), None);
        let gone = intake.send(bundle("Something else"), None);
        hub.sync_report_inbox(&intake).await.unwrap();
        let owner = Caller::local_owner();
        // Nothing to choose for where no fixer runs.
        let refused = look(&hub, &owner, id).await;
        assert_eq!(refused.map(|e| e.code), Some(ErrorCode::Unsupported));

        hub.inner
            .reports
            .fixer
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let refused = look(&hub, &chat(&hub), id).await;
        assert_eq!(refused.map(|e| e.code), Some(ErrorCode::PermissionDenied));
        hub.sync_report_inbox(&intake).await.unwrap();
        assert!(intake.admitted.lock().unwrap().is_empty());

        // Out of reach: the item says so, and the fixer can't take it yet.
        intake.deaf.store(true, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(look(&hub, &owner, id).await, None);
        let body = |hub: &CompanionHub, id| {
            (open(hub).into_iter())
                .find(|i| i.report_id == Some(id))
                .and_then(|i| i.body)
                .unwrap()
        };
        assert!(
            body(&hub, id).ends_with("\nYou chose it for the fixer; not yet sent"),
            "{}",
            body(&hub, id)
        );
        hub.sync_report_inbox(&intake).await.unwrap();
        assert!(intake.admitted.lock().unwrap().is_empty());

        intake
            .deaf
            .store(false, std::sync::atomic::Ordering::SeqCst);
        hub.sync_report_inbox(&intake).await.unwrap();
        assert_eq!(*intake.admitted.lock().unwrap(), vec![id]);
        assert!(body(&hub, id).ends_with("\nOphio is looking into it"));
        // Choosing again changes nothing.
        assert_eq!(look(&hub, &owner, id).await, None);
        hub.sync_report_inbox(&intake).await.unwrap();
        assert_eq!(*intake.admitted.lock().unwrap(), vec![id]);

        // Withdrawn before the intake heard: the fixer never gets it.
        intake.deaf.store(true, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(look(&hub, &owner, gone).await, None);
        intake.set(gone, ReportProgress::Withdrawn);
        intake
            .deaf
            .store(false, std::sync::atomic::Ordering::SeqCst);
        hub.sync_report_inbox(&intake).await.unwrap();
        assert_eq!(*intake.admitted.lock().unwrap(), vec![id]);
        hub.shutdown().await.unwrap();
    }

    /// A chat linked with every grant, which still isn't the person.
    fn chat(hub: &CompanionHub) -> Caller {
        let identity_id = protocol::ids::ChannelIdentityId::new();
        let identity = protocol::auth::ChannelIdentity {
            id: identity_id,
            channel: protocol::auth::ChannelKind::Telegram,
            external_user: "1".into(),
            external_chat: "2".into(),
            display_name: "Synthetic channel".into(),
            linked_at: Timestamp(0),
            revoked_at: None,
            grants: protocol::auth::Grants::owner(),
            project_id: None,
        };
        hub.write(|tx| tx.put("channel", identity_id, &identity))
            .unwrap();
        Caller {
            principal: Principal::Channel { identity_id },
            grants: protocol::auth::Grants::owner(),
        }
    }

    #[tokio::test]
    async fn only_the_person_answers_a_report() {
        let dir = tempfile::tempdir().unwrap();
        let hub = hub(&dir).await;
        let intake = Fake::default();
        hub.sync_report_inbox(&intake).await.unwrap();
        let id = intake.send(bundle("Broke"), None);
        hub.sync_report_inbox(&intake).await.unwrap();
        let refused = verdict(&hub, &chat(&hub), id, ReportVerdict::Fixed).await;
        assert_eq!(refused.map(|e| e.code), Some(ErrorCode::PermissionDenied));
        let entry: Entry = hub.read(|r| r.get(INBOX, id)).unwrap().unwrap();
        assert_eq!(entry.telling, None);
        assert_eq!(entry.status, ReportProgress::Received);
        hub.shutdown().await.unwrap();
    }

    fn test_bundle() -> ReportBundle {
        let mut b = bundle("The save disappeared");
        b.test = Some(protocol::reports::ReportTest {
            source_commit: "a".repeat(40),
            kind: ReportTestKind::RustIntegration,
            path: "crates/store/tests/lost.rs".into(),
            source: "#[test]\nfn lost() {\n\tassert!(false);\n}\n".into(),
            patch: Some("--- a/crates/store/src/lib.rs\n+++ b/crates/store/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n".into()),
            investigator: protocol::reports::ReportInvestigator {
                harness: capability::HarnessKind::Codex,
                model: "some-model".into(),
            },
        });
        b
    }

    fn checked(site: Option<&str>) -> TestCheck {
        TestCheck {
            checked_at: Timestamp(100),
            main_commit: "b".repeat(40),
            at_build: CheckOutcome::Fails,
            on_main: CheckOutcome::Passes,
            failure_site: site.map(str::to_owned),
            agent_block: None,
        }
    }

    fn test_entry() -> Entry {
        let b = test_bundle();
        Entry {
            group: b.report_id,
            attention_id: AttentionId::new(),
            reference: report_reference(&b.report_id),
            detail: detail(&b),
            count: 1,
            status: ReportProgress::Received,
            telling: None,
            look: Look::No,
            code: report_code(&b),
            check: None,
            sharing: None,
        }
    }

    #[test]
    fn test_submissions_show_every_check_outcome_in_plain_words() {
        let mut entry = test_entry();
        let body = entry.body();
        assert!(body.contains("Their test: crates/store/tests/lost.rs (Rust integration test)"));
        assert!(body.contains("Written by their model: Codex · some-model"));
        assert!(body.contains("At their build: not checked yet\nOn main: not checked yet"));
        assert!(body.contains("Their suggested fix is available."));
        let cases = [
            (CheckOutcome::Fails, "reports an assertion failure"),
            (
                CheckOutcome::FailsWithoutAssertion,
                "reports a failure without an assertion",
            ),
            (CheckOutcome::Passes, "passes"),
            (CheckOutcome::DoesNotBuild, "doesn't build"),
            (CheckOutcome::NoTestRan, "no test ran"),
            (CheckOutcome::TimedOut, "timed out"),
            (CheckOutcome::OverLimits, "hit the runner's limits"),
            (
                CheckOutcome::Crashed,
                "stops abnormally (crash or early exit)",
            ),
            (
                CheckOutcome::UnreadableOutput,
                "its output couldn't be read as a test report",
            ),
            (
                CheckOutcome::Refused {
                    reason: CheckRefusal::UnknownCommit,
                },
                "couldn't check (build commit isn't available)",
            ),
            (
                CheckOutcome::Refused {
                    reason: CheckRefusal::NotOnMain,
                },
                "couldn't check (build commit isn't part of main)",
            ),
            (
                CheckOutcome::Refused {
                    reason: CheckRefusal::PathTaken,
                },
                "couldn't check (test path is already taken)",
            ),
            (
                CheckOutcome::Refused {
                    reason: CheckRefusal::NoSuchFolder,
                },
                "couldn't check (test folder doesn't exist)",
            ),
            (
                CheckOutcome::Refused {
                    reason: CheckRefusal::InvalidTest,
                },
                "couldn't check (test isn't safe to run as sent)",
            ),
            (
                CheckOutcome::Refused {
                    reason: CheckRefusal::HostileSignal,
                },
                "couldn't check (run raised a hostile signal)",
            ),
            (
                CheckOutcome::NotRun {
                    reason: NotRunReason::MissingDependencies,
                },
                "not run (offline dependencies aren't available)",
            ),
            (
                CheckOutcome::NotRun {
                    reason: NotRunReason::RunnerUnavailable,
                },
                "not run (contained runner isn't available)",
            ),
            (
                CheckOutcome::NotRun {
                    reason: NotRunReason::SubmissionRefused,
                },
                "not run (the other run was refused)",
            ),
        ];
        for (outcome, text) in cases {
            entry.check = Some(TestCheck {
                at_build: outcome,
                on_main: outcome,
                ..checked(None)
            });
            let body = entry.body();
            assert!(
                body.contains(&format!("At their build: {text}\nOn main: {text}")),
                "{body}"
            );
            assert!(!body.contains("it may already be fixed there"));
        }
        entry.check = Some(checked(Some("crates/store/src/lib.rs:20")));
        entry.sharing = Some(2);
        assert!(
            entry.body().contains(
                "Fails at their build and passes on main: it may already be fixed there."
            )
        );
        assert!(
            entry
                .body()
                .contains("Failure site (a hint): crates/store/src/lib.rs:20")
        );
        assert!(
            entry
                .body()
                .contains("2 other reporters have open reports that fail here.")
        );
        for (kind, name) in [
            (ReportTestKind::RustUnit, "Rust unit test"),
            (ReportTestKind::Vitest, "Vitest test"),
        ] {
            let mut b = test_bundle();
            b.test.as_mut().unwrap().kind = kind;
            assert!(detail(&b).contains(name));
        }
    }

    #[test]
    fn test_metadata_is_plain_and_never_in_the_notification_title() {
        let mut b = test_bundle();
        let test = b.test.as_mut().unwrap();
        test.path = "crates/\u{202e}store/tests/\u{0007}lost.rs".into();
        test.investigator.model = "some\u{061c}-model\u{206f}\u{0000}".into();
        let mut entry = test_entry();
        entry.detail = detail(&b);
        let item = raise(&entry, b.report_id, Timestamp(1), false);
        let body = item.body.unwrap();
        assert!(body.contains("Their test: crates/store/tests/lost.rs (Rust integration test)"));
        assert!(body.contains("Written by their model: Codex · some-model"));
        assert_eq!(
            item.title,
            format!("New problem report {}", entry.reference)
        );
        assert!(!body.contains("assert!"));
        assert!(!body.contains("--- a/"));
        assert_eq!(
            plain_code("a\u{0000}\r\n\tb\u{202e}\u{061c}\u{00ad}\u{e0001}c"),
            "a\n\tbc"
        );
    }

    #[tokio::test]
    async fn checks_and_other_open_reports_update_an_existing_item() {
        let dir = tempfile::tempdir().unwrap();
        let hub = hub(&dir).await;
        let intake = Fake::default();
        let first = intake.send(test_bundle(), None);
        intake.send(bundle("same group"), Some(first));
        let other = intake.send(test_bundle(), None);
        let other_repeat = intake.send(test_bundle(), None);
        let closed = intake.send(test_bundle(), None);
        intake.set(closed, ReportProgress::Withdrawn);
        hub.sync_report_inbox(&intake).await.unwrap();
        let item = || {
            open(&hub)
                .into_iter()
                .find(|i| i.report_id == Some(first))
                .unwrap()
        };
        let attention_id = item().attention_id;
        assert!(item().body.unwrap().contains("not checked yet"));
        for r in intake.records.lock().unwrap().iter_mut() {
            if [first, other, other_repeat, closed].contains(&r.report_id) {
                r.check = Some(checked(Some("crates/store/src/lib.rs:20")));
            }
        }
        intake
            .records
            .lock()
            .unwrap()
            .iter_mut()
            .find(|r| r.report_id == other_repeat)
            .unwrap()
            .key_fingerprint = "second-key".into();
        hub.sync_report_inbox(&intake).await.unwrap();
        let updated = item();
        assert_eq!(updated.attention_id, attention_id);
        let body = updated.body.unwrap();
        assert!(
            body.contains(
                "Fails at their build and passes on main: it may already be fixed there."
            )
        );
        assert!(
            body.contains("2 other reporters have open reports that fail here."),
            "{body}"
        );
        intake.set(other_repeat, ReportProgress::Withdrawn);
        hub.sync_report_inbox(&intake).await.unwrap();
        assert!(
            item()
                .body
                .unwrap()
                .contains("1 other reporter's open report fails here.")
        );
        // A temporary API outage does not turn the last known count into zero.
        intake.deaf.store(true, std::sync::atomic::Ordering::SeqCst);
        hub.sync_report_inbox(&intake).await.unwrap();
        assert!(
            item()
                .body
                .unwrap()
                .contains("1 other reporter's open report fails here.")
        );
        hub.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_submissions_keep_their_own_items_checks_and_admissions() {
        let dir = tempfile::tempdir().unwrap();
        let hub = hub(&dir).await;
        let intake = Fake::default();
        let ordinary = intake.send(bundle("Original without a test"), None);
        // Intake keeps each test as a new root even when signatures match.
        let first = intake.send(test_bundle(), Some(ordinary));
        let mut another = test_bundle();
        another.test.as_mut().unwrap().path = "crates/store/tests/another.rs".into();
        let second = intake.send(another, Some(first));
        intake.send(bundle("Later report without a test"), Some(first));
        for r in intake.records.lock().unwrap().iter_mut() {
            if r.report_id == first {
                r.check = Some(checked(None));
            } else if r.report_id == second {
                r.check = Some(TestCheck {
                    at_build: CheckOutcome::Crashed,
                    on_main: CheckOutcome::UnreadableOutput,
                    ..checked(None)
                });
            }
        }
        hub.sync_report_inbox(&intake).await.unwrap();
        // Re-reading root records must preserve each submitted test and check.
        hub.sync_report_inbox(&intake).await.unwrap();
        let items = open(&hub);
        assert_eq!(items.len(), 3);
        let body = |id| {
            items
                .iter()
                .find(|i| i.report_id == Some(id))
                .unwrap()
                .body
                .as_deref()
                .unwrap()
        };
        assert!(!body(ordinary).contains("Their test:"));
        assert!(body(first).contains("Their test: crates/store/tests/lost.rs"));
        assert!(
            body(first).contains("At their build: reports an assertion failure\nOn main: passes")
        );
        assert!(body(first).contains("Reported 2 times"));
        assert!(body(second).contains("Their test: crates/store/tests/another.rs"));
        assert!(body(second).contains("At their build: stops abnormally (crash or early exit)"));
        assert!(body(second).contains("On main: its output couldn't be read as a test report"));
        hub.inner
            .reports
            .fixer
            .store(true, std::sync::atomic::Ordering::Relaxed);
        for id in [first, second] {
            assert_eq!(look(&hub, &Caller::local_owner(), id).await, None);
            hub.sync_report_inbox(&intake).await.unwrap();
        }
        assert_eq!(*intake.admitted.lock().unwrap(), vec![first, second]);
        hub.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn blocked_tests_explain_why_and_cannot_be_chosen_for_the_fixer() {
        let dir = tempfile::tempdir().unwrap();
        let hub = hub(&dir).await;
        let intake = Fake::default();
        hub.inner
            .reports
            .fixer
            .store(true, std::sync::atomic::Ordering::Relaxed);
        for (block, explanation) in [
            (
                AgentBlock::Unreducible,
                "Look into it can't use it: its code can't be read safely enough to show an agent.",
            ),
            (
                AgentBlock::LongLiteral,
                "Look into it can't use it: it has a string longer than 256 bytes, which no agent is shown.",
            ),
        ] {
            let id = intake.send(test_bundle(), None);
            intake
                .records
                .lock()
                .unwrap()
                .iter_mut()
                .find(|r| r.report_id == id)
                .unwrap()
                .check = Some(TestCheck {
                agent_block: Some(block),
                ..checked(None)
            });
            hub.sync_report_inbox(&intake).await.unwrap();
            let item = open(&hub)
                .into_iter()
                .find(|i| i.report_id == Some(id))
                .unwrap();
            assert!(item.body.unwrap().contains(explanation));
            let refused = look(&hub, &Caller::local_owner(), id).await.unwrap();
            assert_eq!(refused.code, ErrorCode::PermissionDenied);
            assert_eq!(
                refused.message,
                "The fixer can't use this report's test: no agent may be shown its code"
            );
            let entry: Entry = hub.read(|r| r.get(INBOX, id)).unwrap().unwrap();
            assert_eq!(entry.look, Look::No);
            hub.sync_report_inbox(&intake).await.unwrap();
            assert!(intake.admitted.lock().unwrap().is_empty());
            // The restriction is on the agent, not the person's text-only view.
            assert!(
                hub.inbox_report_code(&Caller::local_owner(), id)
                    .unwrap()
                    .is_some()
            );
        }
        hub.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn sharing_counts_distinct_reporter_keys_across_pages_and_groups() {
        let intake = Arc::new(Fake::default());
        let own = intake.send(test_bundle(), None);
        let own_repeat = intake.send(bundle("same group"), Some(own));
        let other = intake.send(test_bundle(), None);
        // The same reporter tries to inflate the count with another group.
        let second_key = intake.send(test_bundle(), None);
        let repeated_key = intake.send(test_bundle(), None);
        let closed = intake.send(test_bundle(), None);
        intake.set(closed, ReportProgress::Withdrawn);
        let site = "crates/store/src/lib.rs:20";
        for r in intake.records.lock().unwrap().iter_mut() {
            r.check = Some(checked(Some(site)));
            r.key_fingerprint = if r.report_id == other || r.report_id == repeated_key {
                "other-key"
            } else if r.report_id == second_key {
                "second-key"
            } else if r.report_id == own_repeat {
                "excluded-group-key"
            } else if r.report_id == closed {
                "closed-key"
            } else {
                "own-key"
            }
            .into();
        }
        assert_eq!(intake.sharing(own, site).await, Some(2));
        async fn page(
            axum::extract::State(intake): axum::extract::State<Arc<Fake>>,
            axum::Json(filter): axum::Json<ReportFilter>,
        ) -> axum::Json<ReportPage> {
            assert_eq!(
                filter.failure_site.as_deref(),
                Some("crates/store/src/lib.rs:20")
            );
            // Fake serves only two records per page, so the repeated key
            // crosses a page boundary in the real client's loop.
            axum::Json(intake.page(filter.after).await.unwrap())
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = axum::Router::new()
            .route(intake::maintainer::PATH_QUERY, axum::routing::post(page))
            .with_state(intake);
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client =
            intake::client::Client::new(&format!("http://{address}"), "test-token").unwrap();
        assert_eq!(client.sharing(own, site).await, Some(2));
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn submitted_code_is_only_served_on_demand_to_the_person() {
        let dir = tempfile::tempdir().unwrap();
        let hub = hub(&dir).await;
        let intake = Fake::default();
        let mut b = test_bundle();
        let test = b.test.as_mut().unwrap();
        test.source.push_str("// untrusted\u{202e}\u{0007}\n\ttext");
        test.patch
            .as_mut()
            .unwrap()
            .push_str("+untrusted\u{061c}\u{0000}\n");
        let expected = report_code(&b).unwrap();
        let id = intake.send(b, None);
        let ordinary = intake.send(bundle("No test"), None);
        hub.sync_report_inbox(&intake).await.unwrap();
        let owner = Caller::local_owner();
        let query = Query::InboxReportCode { report_id: id };
        let QueryData::InboxReportCode { code: Some(code) } =
            hub.query(&owner, query.clone()).await.unwrap()
        else {
            panic!("expected submitted code");
        };
        assert_eq!(code.path, expected.path);
        assert_eq!(code.kind, expected.kind);
        assert_eq!(code.source, plain_code(&expected.source));
        assert_eq!(code.patch, expected.patch.map(|s| plain_code(&s)));
        assert_eq!(
            hub.query(&chat(&hub), query.clone())
                .await
                .unwrap_err()
                .code,
            ErrorCode::PermissionDenied
        );
        assert_eq!(
            hub.query_for_agent(&owner, query).await.unwrap_err().code,
            ErrorCode::PermissionDenied
        );
        for report_id in [ordinary, ReportId::new()] {
            assert!(matches!(
                hub.query(&owner, Query::InboxReportCode { report_id })
                    .await
                    .unwrap(),
                QueryData::InboxReportCode { code: None }
            ));
        }
        intake.set(id, ReportProgress::Withdrawn);
        hub.sync_report_inbox(&intake).await.unwrap();
        assert!(matches!(
            hub.query(&owner, Query::InboxReportCode { report_id: id })
                .await
                .unwrap(),
            QueryData::InboxReportCode { code: None }
        ));
        hub.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn reporter_words_and_test_metadata_never_reach_agents_or_linked_chats() {
        let dir = tempfile::tempdir().unwrap();
        let hub = hub(&dir).await;
        let intake = Fake::default();
        let b = test_bundle();
        let test = b.test.as_ref().unwrap();
        let planted = [
            b.words.clone().unwrap(),
            test.path.clone(),
            test.investigator.model.clone(),
        ];
        let id = intake.send(b, None);
        hub.sync_report_inbox(&intake).await.unwrap();
        let owner = Caller::local_owner();
        let linked = chat(&hub);
        let query = Query::Snapshot {
            reads_withheld: None,
        };
        let person = hub.query(&owner, query.clone()).await.unwrap();
        let agent = hub.query_for_agent(&owner, query.clone()).await.unwrap();
        let channel = hub.query(&linked, query).await.unwrap();
        let person = serde_json::to_string(&person).unwrap();
        let agent = serde_json::to_string(&agent).unwrap();
        let channel = serde_json::to_string(&channel).unwrap();
        for text in planted {
            assert!(person.contains(&text), "owner should see {text}");
            assert!(!agent.contains(&text), "agent must not see {text}");
            assert!(!channel.contains(&text), "chat must not see {text}");
        }
        let item = open(&hub)
            .into_iter()
            .find(|i| i.report_id == Some(id))
            .unwrap();
        assert!(
            !hub.read(|r| CompanionHub::visible(
                r,
                &linked,
                &Event::AttentionUpserted { item: item.clone() }
            ))
            .unwrap()
        );
        assert!(
            hub.read(|r| CompanionHub::visible(r, &owner, &Event::AttentionUpserted { item }))
                .unwrap()
        );
        hub.shutdown().await.unwrap();
    }

    #[test]
    fn the_words_show_as_one_plain_line() {
        let mut b = bundle("first\nsecond\u{202E}evil\u{0007}  \t end");
        b.signature.screen = "/x\n/y".into();
        let detail = detail(&b);
        let lines: Vec<_> = detail.lines().collect();
        assert_eq!(lines[0], "They wrote: “first secondevil end”");
        assert!(lines[1].starts_with("Screen /x /y · build abc123"));
        assert_eq!(lines[2], "From an error (internal)");
        assert_eq!(lines.len(), 3);

        let long = "word ".repeat(200);
        let detail = detail_of(&long);
        assert!(detail.lines().next().unwrap().chars().count() < WORDS + 20);
    }

    fn detail_of(words: &str) -> String {
        detail(&bundle(words))
    }
}
