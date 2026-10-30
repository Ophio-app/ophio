// SPDX-License-Identifier: Apache-2.0
//! Private diagnostics, reviewed bundles and their durable deliveries.
pub(crate) mod github;
mod sender;
mod trail;
mod validation;
use crate::{CompanionHub, error, internal};
use protocol::{model::*, reports::*, time::Timestamp, *};
use std::{
    path::{Path, PathBuf},
    sync::Mutex,
};
use store::reports::{BUNDLE, Delivery, META, QUEUE, ReportDelivery, SUMMARY};

/// Composition supplies install information without giving the sender updater authority.
#[derive(Debug, Clone)]
pub struct ReportsConfig {
    pub intake_url: Option<String>,
    pub build: String,
    pub previous_build: Option<String>,
    pub data_dir: Option<PathBuf>,
    pub os: String,
}
impl Default for ReportsConfig {
    fn default() -> Self {
        Self {
            intake_url: None,
            build: env!("CARGO_PKG_VERSION").into(),
            previous_build: None,
            data_dir: None,
            os: std::env::consts::OS.into(),
        }
    }
}
pub(crate) struct Runtime {
    pub config: ReportsConfig,
    pub data: PathBuf,
    pub trail: Mutex<trail::Recorder>,
    pub wake: tokio::sync::Notify,
    /// Wakes the owner's report inbox for a verdict or choice to send.
    pub inbox_wake: tokio::sync::Notify,
    /// Whether the report fixer runs here, taking only what the owner
    /// chooses from the inbox.
    pub fixer: std::sync::atomic::AtomicBool,
    pub events: tokio::sync::broadcast::Sender<std::sync::Arc<EventEnvelope>>,
    pub client: reqwest::Client,
    pub key: Mutex<Option<ed25519_dalek::SigningKey>>,
    pub sending: tokio::sync::Mutex<()>,
}
impl Runtime {
    pub fn new(config: ReportsConfig, data: &Path, now: Timestamp) -> Result<Self, ProtocolError> {
        if let Some(url) = &config.intake_url {
            sender::base_url(url)?;
        }
        let data = config.data_dir.clone().unwrap_or_else(|| data.to_owned());
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(3))
            .timeout(std::time::Duration::from_secs(8))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| error(ErrorCode::Internal, "Could not prepare report sending"))?;
        Ok(Self {
            trail: Mutex::new(trail::Recorder::load(&data, now)),
            config,
            data,
            wake: tokio::sync::Notify::new(),
            inbox_wake: tokio::sync::Notify::new(),
            fixer: std::sync::atomic::AtomicBool::new(false),
            events: tokio::sync::broadcast::channel(1024).0,
            client,
            key: Mutex::new(None),
            sending: tokio::sync::Mutex::new(()),
        })
    }
}
/// Atomic replacement also keeps trail and crash files private.
pub fn private_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let temp = path.with_extension(format!("{}.tmp", uuid::Uuid::now_v7()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options.open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temp);
    }
    result
}
impl CompanionHub {
    pub fn record_report_update(&self, event: Trail) {
        if matches!(event, Trail::Update { .. }) {
            self.report_trail(event);
        }
    }
    pub(crate) fn observe_report_event(&self, envelope: &EventEnvelope) {
        let mut envelope = envelope.clone();
        if let Event::AttentionUpserted { item } = &mut envelope.event {
            let quiet = self
                .read(|r| {
                    let settings = Self::notification_settings(r)?;
                    let project = item
                        .task_id
                        .map(|id| r.get::<Task>("task", id))
                        .transpose()?
                        .flatten()
                        .map(|t| t.project_id);
                    Ok(item.topic().is_some_and(|topic| {
                        project.map_or_else(
                            || settings.off.contains(&topic),
                            |id| !settings.alerts(id, topic),
                        )
                    }))
                })
                .unwrap_or(true);
            item.quiet |= quiet;
        }
        if let Ok(mut trail) = self.inner.reports.trail.lock() {
            trail.observe(&envelope);
        }
    }
    pub(crate) fn report_trail(&self, event: Trail) {
        if let Ok(mut trail) = self.inner.reports.trail.lock() {
            trail.push(self.inner.clock.now(), event);
        }
    }
    pub(crate) fn reports_view(&self) -> Result<ReportsView, ProtocolError> {
        let mut view = self.read(|r| {
            let mut reports = r.report_summaries()?;
            reports.truncate(MAX_LISTED_REPORTS);
            let feed: Option<ReleaseFeed> = r.get(META, "feed")?;
            let halted = feed
                .and_then(|f| {
                    f.releases.into_iter().find(|e| {
                        e.build == self.inner.reports.config.build
                            && e.state == ReleaseState::Halted
                    })
                })
                .map(|e| HaltedRelease {
                    build: e.build,
                    back_to: self.inner.reports.config.previous_build.clone(),
                });
            Ok(ReportsView {
                reports,
                settings: r.report_settings()?,
                intake: r.get(META, "link")?.unwrap_or(IntakeLink::None),
                crashes: Vec::new(),
                halted,
            })
        })?;
        view.crashes = self.unsent_crashes()?;
        Ok(view)
    }
    fn unsent_crashes(&self) -> Result<Vec<CrashRecord>, ProtocolError> {
        let consumed: Vec<String> =
            self.read(|r| Ok(r.get(META, "carried_crashes")?.unwrap_or_default()))?;
        let mut crashes = read_crashes(&self.inner.reports.data);
        crashes.retain(|c| !consumed.contains(&crash_key(c)));
        crashes.sort_by_key(|c| std::cmp::Reverse(c.at));
        Ok(crashes)
    }
    pub(crate) async fn report_preview(
        &self,
        salt: &str,
        extras: &[ReportExtra],
    ) -> Result<ComputerPart, ProtocolError> {
        if salt.len() != 32 || !salt.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err(error(ErrorCode::InvalidRequest, "Invalid report salt"));
        }
        let (trail, tasks) = {
            let mut t = self.inner.reports.trail.lock().map_err(internal)?;
            (t.preview(salt, self.inner.clock.now()), t.tasks())
        };
        let mut part = self.read(|r| {
            let host: Option<HostInfo> = r.get("meta", "host")?;
            let harnesses: Vec<protocol::capability::HarnessInfo> =
                r.get("meta", "harnesses")?.unwrap_or_default();
            let versions = ComputerVersions {
                build: self.inner.reports.config.build.clone(),
                os: self.inner.reports.config.os.clone(),
                protocol: ProtocolVersion::CURRENT,
                features: host.map(|h| h.capabilities.features).unwrap_or_default(),
                harnesses: harnesses
                    .into_iter()
                    .map(|h| HarnessVersion {
                        harness: h.kind,
                        version: h.native_version.as_deref().and_then(scrub),
                    })
                    .collect(),
            };
            let mut parts = Vec::new();
            for extra in extras {
                let extra = match extra {
                    ReportExtra::TaskTranscript { task_id } => {
                        let _: Task = r.require("task", task_id)?;
                        let (items, more) = r.timeline(*task_id, None, 1000)?;
                        let entries = items
                            .into_iter()
                            .filter_map(|i| {
                                let (role, text) = match i.item {
                                    TimelineBody::UserMessage { text, .. } => ("person", text),
                                    TimelineBody::AgentMessage { text, .. } => ("agent", text),
                                    TimelineBody::Activity {
                                        summary, detail, ..
                                    } => ("tool", detail.unwrap_or(summary)),
                                    TimelineBody::Note { text } => ("system", text),
                                    _ => return None,
                                };
                                Some(TranscriptEntry {
                                    at: i.at,
                                    role: role.into(),
                                    text,
                                })
                            })
                            .collect();
                        ReportExtraPart::TaskTranscript {
                            task: pseudonym(salt, PseudonymKind::Task, &task_id.to_string()),
                            entries,
                            cut: more,
                        }
                    }
                    ReportExtra::Names => {
                        let mut names = Vec::new();
                        for id in &tasks {
                            let Ok(id) = id.parse::<TaskId>() else {
                                continue;
                            };
                            if let Some(task) = r.get::<Task>("task", id)? {
                                names.push(NamedPseudonym {
                                    pseudonym: pseudonym(
                                        salt,
                                        PseudonymKind::Task,
                                        &id.to_string(),
                                    ),
                                    name: protocol::alerts::cut(&task.title, MAX_REPORT_CODE),
                                });
                                if let Some(p) = r.get::<Project>("project", task.project_id)? {
                                    names.push(NamedPseudonym {
                                        pseudonym: pseudonym(
                                            salt,
                                            PseudonymKind::Project,
                                            &p.project_id.to_string(),
                                        ),
                                        name: protocol::alerts::cut(&p.name, MAX_REPORT_CODE),
                                    });
                                }
                            }
                        }
                        names.sort_by(|a, b| a.pseudonym.cmp(&b.pseudonym));
                        names.dedup_by(|a, b| a.pseudonym == b.pseudonym);
                        ReportExtraPart::Names { names }
                    }
                };
                if !parts.contains(&extra) {
                    parts.push(extra);
                }
            }
            Ok(ComputerPart {
                versions,
                trail,
                crashes: Vec::new(),
                extras: parts,
                sending: r.report_settings()?.sending,
            })
        })?;
        part.crashes = self.unsent_crashes()?;
        // Leave room for a full phone trail, image and envelope. The final
        // submission validates the entire reviewed bundle without modifying it.
        const PREVIEW_BYTES: usize = MAX_REPORT_BYTES - MAX_SCREENSHOT_BYTES * 4 / 3 - 96 * 1024;
        while serde_json::to_vec(&part)
            .map_err(|_| error(ErrorCode::Internal, "Could not prepare report"))?
            .len()
            > PREVIEW_BYTES
        {
            let Some(ReportExtraPart::TaskTranscript {entries,cut,..}) = part.extras.iter_mut().find(|e|matches!(e, ReportExtraPart::TaskTranscript {entries,..} if !entries.is_empty())) else { return Err(error(ErrorCode::LimitExceeded,"The report preview is too large")) };
            entries.remove(0);
            *cut = true;
        }
        Ok(part)
    }
    pub(crate) fn submit_report(
        &self,
        tx: &mut store::Write<'_>,
        bundle: &ReportBundle,
    ) -> store::Result<()> {
        if tx
            .read()
            .get::<ReportSummary>(SUMMARY, bundle.report_id)?
            .is_some()
        {
            return Ok(());
        }
        validation::validate(bundle)?;
        let now = self.inner.clock.now();
        tx.keep_report(bundle, now)?;
        let mut consumed: Vec<String> = tx.read().get(META, "carried_crashes")?.unwrap_or_default();
        for c in &bundle.crashes {
            if c.side == ReportSide::Computer && !consumed.contains(&crash_key(c)) {
                consumed.push(crash_key(c));
            }
        }
        // Only local crash records need acknowledgement, never arbitrary phone data.
        let local = read_crashes(&self.inner.reports.data)
            .iter()
            .map(crash_key)
            .collect::<Vec<_>>();
        consumed.retain(|c| local.contains(c));
        tx.put(META, "carried_crashes", &consumed)?;
        tx.emit(Event::ReportsChanged {})?;
        Ok(())
    }
    pub(crate) fn answer_report(
        &self,
        tx: &mut store::Write<'_>,
        id: ReportId,
        qid: ReportQuestionId,
        answer: &ReportAnswer,
    ) -> store::Result<()> {
        if answer
            .words
            .as_ref()
            .is_some_and(|s| s.chars().count() > MAX_REPORT_WORDS)
        {
            return Err(error(ErrorCode::InvalidRequest, "The answer is too long").into());
        }
        let mut summary: ReportSummary = tx.read().require(SUMMARY, id)?;
        let question = summary
            .questions
            .iter_mut()
            .find(|q| q.question_id == qid)
            .ok_or_else(|| error(ErrorCode::NotFound, "Question not found"))?;
        if question.answer.is_some() {
            return Ok(());
        }
        if summary.history.last().is_some_and(|s| s.status.closed())
            || !question.template.choices().contains(&answer.choice)
        {
            return Err(error(ErrorCode::InvalidRequest, "That answer is not available").into());
        }
        question.answer = Some(answer.clone());
        tx.put(SUMMARY, id, &summary)?;
        tx.enqueue_report(
            Delivery::Answer(IntakeAnswer {
                report_id: id,
                question_id: qid,
                answer: answer.clone(),
            }),
            self.inner.clock.now(),
        )?;
        sync_attention(tx, &summary, self.inner.clock.now(), false)?;
        tx.emit(Event::ReportsChanged {})?;
        Ok(())
    }
    pub(crate) fn withdraw_report(
        &self,
        tx: &mut store::Write<'_>,
        id: ReportId,
    ) -> store::Result<()> {
        if tx.withdraw_report(id, self.inner.clock.now())? {
            let summary = tx.read().require(SUMMARY, id)?;
            sync_attention(tx, &summary, self.inner.clock.now(), false)?;
            tx.emit(Event::ReportsChanged {})?;
        }
        Ok(())
    }
    pub(crate) fn set_report_settings(
        &self,
        tx: &mut store::Write<'_>,
        sending: bool,
        ring: ReleaseRing,
        revision: Option<u64>,
    ) -> store::Result<()> {
        let old = tx.read().report_settings()?;
        domain::revision(revision, old.revision)?;
        tx.put(
            META,
            "settings",
            &ReportSettings {
                sending,
                ring,
                revision: old.revision + 1,
            },
        )?;
        tx.emit(Event::ReportsChanged {})?;
        Ok(())
    }
}
fn crash_key(crash: &CrashRecord) -> String {
    use sha2::Digest;
    format!(
        "{:x}",
        sha2::Sha256::digest(serde_json::to_vec(crash).unwrap_or_default())
    )
}
static CRASH_FILE: Mutex<()> = Mutex::new(());
pub fn append_crash(data: &Path, crash: &CrashRecord) -> std::io::Result<()> {
    let _lock = CRASH_FILE
        .lock()
        .map_err(|_| std::io::Error::other("Crash capture unavailable"))?;
    write_crash(data, crash)
}
pub fn try_append_crash(data: &Path, crash: &CrashRecord) -> std::io::Result<()> {
    let Ok(_lock) = CRASH_FILE.try_lock() else {
        return Ok(());
    };
    write_crash(data, crash)
}
fn write_crash(data: &Path, crash: &CrashRecord) -> std::io::Result<()> {
    let mut crashes = read_crashes(data);
    crashes.push(crash.clone());
    crashes.sort_by_key(|c| std::cmp::Reverse(c.at));
    crashes.truncate(16);
    std::fs::create_dir_all(data)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(data, std::fs::Permissions::from_mode(0o700))?;
    }
    let bytes = serde_json::to_vec(&crashes).map_err(std::io::Error::other)?;
    private_write(&data.join("report-crashes.json"), &bytes)
}
impl CompanionHub {
    pub(crate) fn clear_carried_crashes(&self) {
        let Ok(_lock) = CRASH_FILE.lock() else { return };
        if let Ok(crashes) = self.unsent_crashes()
            && let Ok(bytes) = serde_json::to_vec(&crashes)
        {
            let _ = private_write(&self.inner.reports.data.join("report-crashes.json"), &bytes);
        }
    }
}
pub fn read_crashes(data: &Path) -> Vec<CrashRecord> {
    std::fs::read(data.join("report-crashes.json"))
        .ok()
        .filter(|b| b.len() <= 256 * 1024)
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn sync_attention(
    tx: &mut store::Write<'_>,
    summary: &ReportSummary,
    now: Timestamp,
    alert: bool,
) -> store::Result<()> {
    use protocol::notifications::AlertTopic;
    let question = summary.questions.iter().find(|q| q.answer.is_none());
    let mut old = Vec::new();
    for a in tx.read().list::<AttentionItem>("attention")? {
        // Only those raised here for the reporter: the owner's inbox and a
        // fix's request name the report too.
        if a.report_id == Some(summary.report_id)
            && a.resolved_at.is_none()
            && (tx.read())
                .get::<ReportAlert>("report_alert", a.attention_id)?
                .is_some()
        {
            old.push(a);
        }
    }
    for mut item in old {
        item.resolved_at = Some(now);
        tx.put("attention", item.attention_id, &item)?;
        tx.emit(Event::AttentionResolved {
            attention_id: item.attention_id,
        })?;
    }
    if !alert && question.is_none() {
        return Ok(());
    }
    if summary.status().closed() && !alert {
        return Ok(());
    }
    let item = AttentionItem {
        child_id: None,
        attention_id: AttentionId::new(),
        kind: if question.is_some() {
            AttentionKind::Question
        } else {
            AttentionKind::Completed
        },
        title: format!("Report {}", summary.reference),
        body: None,
        task_id: None,
        run_id: None,
        approval_id: None,
        created_at: now,
        resolved_at: None,
        priority: if question.is_some() { 90 } else { 30 },
        topic: Some(AlertTopic::Reports),
        quiet: !alert,
        report_id: Some(summary.report_id),
    };
    let report = ReportAlert {
        report_id: summary.report_id,
        reference: summary.reference.clone(),
        status: summary.status(),
        question: question.cloned(),
    };
    tx.put("report_alert", item.attention_id, &report)?;
    tx.put("attention", item.attention_id, &item)?;
    tx.emit(Event::AttentionUpserted { item })?;
    Ok(())
}

#[cfg(test)]
mod tests;
