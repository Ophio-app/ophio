// SPDX-License-Identifier: Apache-2.0
use super::*;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::Signer;
use serde::{Serialize, de::DeserializeOwned};
use std::{
    io::{Read, Write},
    sync::Arc,
    time::Duration,
};

pub(super) fn base_url(value: &str) -> Result<url::Url, ProtocolError> {
    let url = url::Url::parse(value)
        .map_err(|_| error(ErrorCode::InvalidRequest, "Invalid report intake URL"))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || (url.scheme() == "http"
            && !matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]")))
    {
        return Err(error(
            ErrorCode::InvalidRequest,
            "The report intake needs HTTPS (or localhost for development)",
        ));
    }
    Ok(url)
}
#[derive(Debug)]
enum Failure {
    Unreachable,
    Paused,
}
impl CompanionHub {
    fn reporter_key(&self) -> Result<ed25519_dalek::SigningKey, Failure> {
        let mut held = self
            .inner
            .reports
            .key
            .lock()
            .map_err(|_| Failure::Unreachable)?;
        if let Some(key) = held.as_ref() {
            return Ok(key.clone());
        }
        let path = self.inner.reports.data.join("reporter.key");
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
        }
        let key = match options.open(&path) {
            Ok(mut file) => {
                let meta = file.metadata().map_err(|_| Failure::Unreachable)?;
                if !meta.is_file() || meta.len() != 32 {
                    return Err(Failure::Unreachable);
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if meta.permissions().mode() & 0o777 != 0o600 {
                        return Err(Failure::Unreachable);
                    }
                }
                let mut bytes = zeroize::Zeroizing::new([0u8; 32]);
                file.read_exact(bytes.as_mut())
                    .map_err(|_| Failure::Unreachable)?;
                ed25519_dalek::SigningKey::from_bytes(&bytes)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let mut bytes = zeroize::Zeroizing::new([0u8; 32]);
                getrandom::fill(bytes.as_mut()).map_err(|_| Failure::Unreachable)?;
                let mut options = std::fs::OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                let mut file = options.open(&path).map_err(|_| Failure::Unreachable)?;
                file.write_all(bytes.as_ref())
                    .and_then(|()| file.sync_all())
                    .map_err(|_| Failure::Unreachable)?;
                ed25519_dalek::SigningKey::from_bytes(&bytes)
            }
            Err(_) => return Err(Failure::Unreachable),
        };
        self.write(|tx| tx.put(META, "key_location", &path))
            .map_err(|_| Failure::Unreachable)?;
        *held = Some(key.clone());
        Ok(key)
    }
    async fn intake<T: DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Vec<u8>,
    ) -> Result<T, Failure> {
        let base = self
            .inner
            .reports
            .config
            .intake_url
            .as_deref()
            .ok_or(Failure::Unreachable)?;
        let base = base_url(base).map_err(|_| Failure::Unreachable)?;
        let url = base.join(path).map_err(|_| Failure::Unreachable)?;
        let key = self.reporter_key()?;
        let at = self.inner.clock.now().0;
        let signature = key.sign(&signing_payload(method.as_str(), url.path(), at, &body));
        let mut response = self
            .inner
            .reports
            .client
            .request(method, url)
            .header(
                KEY_HEADER,
                URL_SAFE_NO_PAD.encode(key.verifying_key().to_bytes()),
            )
            .header(TIME_HEADER, at.to_string())
            .header(
                SIGNATURE_HEADER,
                URL_SAFE_NO_PAD.encode(signature.to_bytes()),
            )
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .map_err(|_| Failure::Unreachable)?;
        let status = response.status();
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| Failure::Unreachable)? {
            if bytes.len().saturating_add(chunk.len()) > 2 * MAX_REPORT_BYTES {
                return Err(Failure::Unreachable);
            }
            bytes.extend_from_slice(&chunk);
        }
        if !status.is_success() {
            if serde_json::from_slice::<StatusResponse>(&bytes).is_ok_and(|r| r.paused)
                || matches!(status.as_u16(), 423 | 429 | 503)
                    && serde_json::from_slice::<serde_json::Value>(&bytes)
                        .ok()
                        .is_some_and(|v| v.get("paused") == Some(&true.into()))
            {
                return Err(Failure::Paused);
            }
            return Err(Failure::Unreachable);
        }
        serde_json::from_slice(&bytes).map_err(|_| Failure::Unreachable)
    }
    async fn post<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &impl Serialize,
    ) -> Result<T, Failure> {
        self.intake(
            reqwest::Method::POST,
            path,
            serde_json::to_vec(body).map_err(|_| Failure::Unreachable)?,
        )
        .await
    }
    fn link(&self, link: IntakeLink) -> Result<(), ProtocolError> {
        self.write(|tx| {
            if tx.read().get::<IntakeLink>(META, "link")?.as_ref() != Some(&link) {
                tx.put(META, "link", &link)?;
                tx.emit(Event::ReportsChanged {})?;
            }
            Ok(())
        })
    }
    fn reachable(&self) -> Result<(), ProtocolError> {
        self.write(|tx| {
            let link = tx.read().get::<IntakeLink>(META, "link")?;
            if !matches!(link, Some(IntakeLink::Paused | IntakeLink::Reachable)) {
                tx.put(META, "link", &IntakeLink::Reachable)?;
                tx.emit(Event::ReportsChanged {})?;
            }
            Ok(())
        })
    }
    fn failed_link(&self, failure: &Failure) -> Result<(), ProtocolError> {
        if matches!(failure, Failure::Paused) {
            return self.link(IntakeLink::Paused);
        }
        let (since, queued) = self.read(|r| {
            let since = match r.get(META, "link")? {
                Some(IntakeLink::Unreachable { since, .. }) => since,
                _ => self.inner.clock.now(),
            };
            let ids = r
                .list::<ReportDelivery>(QUEUE)?
                .into_iter()
                .map(|r| r.delivery.report_id())
                .collect::<std::collections::HashSet<_>>();
            Ok((since, ids.len().min(u32::MAX as usize) as u32))
        })?;
        self.link(IntakeLink::Unreachable { since, queued })
    }
    pub(super) fn apply_report_updates(
        &self,
        response: StatusResponse,
    ) -> Result<(), ProtocolError> {
        self.write(|tx| {
            for update in response.reports {
                let Some(mut summary) =
                    tx.read().get::<ReportSummary>(SUMMARY, update.report_id)?
                else {
                    continue;
                };
                if summary.status() == ReportProgress::Withdrawn {
                    continue;
                }
                // The local answer wins until its delivery is acknowledged.
                let mut questions = update.questions;
                for q in &mut questions {
                    if let Some(old) = summary
                        .questions
                        .iter()
                        .find(|old| old.question_id == q.question_id)
                        && old.answer.is_some()
                        && q.answer.is_none()
                    {
                        q.answer = old.answer.clone();
                    }
                }
                questions.sort_by_key(|q| (q.answer.is_some(), q.asked_at));
                let old_history = summary.history.clone();
                let old_questions = summary.questions.clone();
                let old = summary.status();
                let old_question = summary
                    .questions
                    .iter()
                    .find(|q| q.answer.is_none())
                    .map(|q| q.question_id);
                if !update.history.is_empty() {
                    let inferred = summary
                        .history
                        .last()
                        .filter(|last| matches!(last.status, ReportProgress::Fixed { .. }))
                        .cloned();
                    summary.history = update.history;
                    if let Some(inferred) = inferred
                        && !matches!(
                            summary.status(),
                            ReportProgress::Fixed { .. }
                                | ReportProgress::Shipped { .. }
                                | ReportProgress::Declined { .. }
                        )
                    {
                        summary.history.push(inferred);
                    }
                }
                summary.questions = questions;
                summary.checked_at = Some(response.now);
                let new_question = summary
                    .questions
                    .iter()
                    .find(|q| q.answer.is_none())
                    .map(|q| q.question_id);
                let alert = summary.status().alerts()
                    && (old != summary.status() || old_question != new_question)
                    && (summary.status() != ReportProgress::Question || new_question.is_some());
                tx.put(SUMMARY, summary.report_id, &summary)?;
                if old_history != summary.history || old_questions != summary.questions {
                    sync_attention(tx, &summary, response.now, alert)?;
                }
                tx.emit(Event::ReportsChanged {})?;
            }
            Ok(())
        })?;
        self.link(if response.paused {
            IntakeLink::Paused
        } else {
            IntakeLink::Reachable
        })
    }
    pub(super) fn apply_release_feed(&self, feed: ReleaseFeed) -> Result<(), ProtocolError> {
        self.write(|tx| {
            if tx.read().get::<ReleaseFeed>(META, "feed")?.as_ref() == Some(&feed) {
                return Ok(());
            }
            let settings = tx.read().report_settings()?;
            let running = feed
                .releases
                .iter()
                .find(|e| e.build == self.inner.reports.config.build)
                .map(|e| e.published_at);
            for mut summary in tx.read().report_summaries()? {
                if summary.status().closed() {
                    continue;
                }
                let fix = feed.releases.iter().find(|e| {
                    e.state != ReleaseState::Halted
                        && settings.ring <= e.ring
                        && e.build != self.inner.reports.config.build
                        && running.is_none_or(|at| e.published_at > at)
                        && e.fixes.contains(&summary.signature.key())
                });
                if let Some(fix) = fix
                    && !matches!(
                        summary.status(),
                        ReportProgress::Fixed { .. }
                            | ReportProgress::Shipped { .. }
                            | ReportProgress::Known { fixed_in: Some(_) }
                    )
                {
                    summary.history.push(StatusChange {
                        status: ReportProgress::Fixed {
                            build: fix.build.clone(),
                        },
                        at: feed.now,
                    });
                    tx.put(SUMMARY, summary.report_id, &summary)?;
                    sync_attention(tx, &summary, feed.now, true)?;
                }
            }
            tx.put(META, "feed", &feed)?;
            tx.emit(Event::ReportsChanged {})?;
            Ok(())
        })
    }
    /// One durable send/poll pass. All retries keep the same reviewed content.
    pub(crate) async fn reports_once(&self) -> Result<Duration, ProtocolError> {
        let _one = self.inner.reports.sending.lock().await;
        if self.inner.reports.config.intake_url.is_none() {
            self.link(IntakeLink::None)?;
            return Ok(Duration::from_secs(3600));
        }
        let now = self.inner.clock.now();
        let (settings, rows, paused) = self.read(|r| {
            Ok((
                r.report_settings()?,
                r.list::<ReportDelivery>(QUEUE)?,
                r.get::<IntakeLink>(META, "link")? == Some(IntakeLink::Paused),
            ))
        })?;
        if !settings.sending {
            return Ok(Duration::from_secs(3600));
        }
        let mut sent = false;
        let mut failure = None;
        for row in rows.iter().filter(|r| {
            r.after <= now && (!paused || !matches!(r.delivery, Delivery::Submit { .. }))
        }) {
            if !self.read(|r| Ok(r.report_settings()?.sending))? {
                break;
            }
            // A withdrawal may have removed a delivery while this pass was running.
            if self
                .read(|r| r.get::<ReportDelivery>(QUEUE, row.delivery.key()))?
                .is_none()
            {
                continue;
            }
            let result = match &row.delivery {
                Delivery::Submit { report_id } => {
                    let bundle = self.read(|r| r.get::<ReportBundle>(BUNDLE, report_id))?;
                    if let Some(bundle) = bundle {
                        match self
                            .post::<IntakeReceipt>(
                                PATH_SUBMIT,
                                &IntakeSubmission {
                                    bundle,
                                    sender: ReportSide::Computer,
                                },
                            )
                            .await
                        {
                            Ok(receipt) if receipt.report_id == *report_id => {
                                self.write(|tx| {
                                    let mut summary: ReportSummary =
                                        tx.read().require(SUMMARY, report_id)?;
                                    if summary.status() != ReportProgress::Withdrawn
                                        && summary.status() != receipt.status
                                    {
                                        summary.history.push(StatusChange {
                                            status: receipt.status,
                                            at: now,
                                        });
                                        tx.put(SUMMARY, report_id, &summary)?;
                                        sync_attention(
                                            tx,
                                            &summary,
                                            now,
                                            summary.status().alerts(),
                                        )?;
                                        tx.emit(Event::ReportsChanged {})?;
                                    }
                                    Ok(())
                                })?;
                                Ok(())
                            }
                            Ok(_) => Err(Failure::Unreachable),
                            Err(e) => Err(e),
                        }
                    } else {
                        Ok(())
                    }
                }
                Delivery::Answer(answer) => {
                    match self.post::<StatusResponse>(PATH_ANSWER, answer).await {
                        Ok(r) => {
                            self.apply_report_updates(r)?;
                            Ok(())
                        }
                        Err(e) => Err(e),
                    }
                }
                Delivery::Withdraw(withdraw) => {
                    match self.post::<StatusResponse>(PATH_WITHDRAW, withdraw).await {
                        Ok(r) => {
                            self.apply_report_updates(r)?;
                            Ok(())
                        }
                        Err(e) => Err(e),
                    }
                }
            };
            match result {
                Ok(()) => {
                    self.write(|tx| {
                        tx.delete(QUEUE, row.delivery.key())?;
                        tx.emit(Event::ReportsChanged {})?;
                        Ok(())
                    })?;
                    sent = true;
                }
                Err(e) => {
                    self.write(|tx| {
                        if let Some(mut current) =
                            tx.read().get::<ReportDelivery>(QUEUE, row.delivery.key())?
                        {
                            current.attempts = current.attempts.saturating_add(1);
                            current.after = Timestamp(now.0 + backoff_ms(current.attempts));
                            tx.put(QUEUE, current.delivery.key(), &current)?;
                        }
                        Ok(())
                    })?;
                    failure = Some(e);
                    break;
                }
            }
        }
        let (poll_after, poll_round) = self.read(|r| {
            Ok((
                r.get::<Timestamp>(META, "poll_after")?
                    .unwrap_or(Timestamp(0)),
                r.get::<u32>(META, "poll_round")?.unwrap_or(0),
            ))
        })?;
        if failure.is_none() && (sent || poll_after <= now) {
            let ids = self.read(|r| {
                Ok(r.report_summaries()?
                    .into_iter()
                    .filter(|s| !s.status().closed() && s.status() != ReportProgress::Queued)
                    .map(|s| s.report_id)
                    .collect::<Vec<_>>())
            })?;
            for chunk in ids
                .chunks(MAX_STATUS_IDS)
                .chain((ids.is_empty() && paused).then_some([].as_slice()))
            {
                match self
                    .post::<StatusResponse>(
                        PATH_STATUS,
                        &StatusRequest {
                            report_ids: chunk.to_vec(),
                            since: None,
                        },
                    )
                    .await
                {
                    Ok(r) => self.apply_report_updates(r)?,
                    Err(e) => {
                        failure = Some(e);
                        break;
                    }
                }
            }
            if failure.is_none() {
                match self
                    .intake::<ReleaseFeed>(reqwest::Method::GET, PATH_RELEASES, Vec::new())
                    .await
                {
                    Ok(feed) => {
                        self.apply_release_feed(feed)?;
                        self.reachable()?;
                    }
                    Err(e) => failure = Some(e),
                }
            }
            let round = if sent {
                0
            } else {
                poll_round.saturating_add(1)
            };
            let delay = if failure.is_some() {
                backoff_ms(round.saturating_add(1))
            } else {
                (30_000i64.saturating_mul(1i64 << round.min(9))).min(4 * 3600 * 1000)
            };
            self.write(|tx| {
                tx.put(META, "poll_after", &Timestamp(now.0 + delay))?;
                tx.put(META, "poll_round", &round)
            })?;
        }
        if let Some(failure) = failure {
            self.failed_link(&failure)?;
        } else if sent {
            self.reachable()?;
        }
        let next = self.read(|r| {
            let queue = r.list::<ReportDelivery>(QUEUE)?;
            let poll = r
                .get::<Timestamp>(META, "poll_after")?
                .unwrap_or(Timestamp(now.0 + 30_000));
            Ok(if paused {
                poll.0
            } else {
                queue
                    .iter()
                    .map(|r| r.after.0)
                    .min()
                    .unwrap_or(poll.0)
                    .min(poll.0)
            })
        })?;
        Ok(Duration::from_millis(
            next.saturating_sub(now.0).max(100) as u64
        ))
    }
    pub(crate) async fn start_reports(&self) -> Result<(), ProtocolError> {
        self.clear_carried_crashes();
        if self.inner.reports.config.intake_url.is_none() {
            self.link(IntakeLink::None)?;
        }
        let mut events = self.inner.reports.events.subscribe();
        self.report_trail(Trail::Service {
            state: ServiceState::Started,
        });
        let weak = Arc::downgrade(&self.inner);
        let mut shutdown = self.inner.shutdown.subscribe();
        self.spawn(async move {
            let mut tick=tokio::time::interval(Duration::from_secs(2));
            loop {tokio::select! {
                _=shutdown.changed()=>break,
                _=tick.tick()=>{let Some(inner)=weak.upgrade() else {break};if let Ok(mut trail)=inner.reports.trail.lock() {trail.evict(inner.clock.now());trail.flush(false);}},
                event=events.recv()=>{let Some(inner)=weak.upgrade() else {break};match event {Ok(e)=>{(Self {inner}).observe_report_event(&e);},Err(tokio::sync::broadcast::error::RecvError::Closed)=>break,_=>{}}}
            }}
        });
        let weak = Arc::downgrade(&self.inner);
        let mut shutdown = self.inner.shutdown.subscribe();
        self.spawn(async move {
            let mut delay=Duration::ZERO;
            loop {
                let Some(inner)=weak.upgrade() else {break};
                tokio::select! {_=shutdown.changed()=>break,_=inner.reports.wake.notified()=>{},_=tokio::time::sleep(delay)=>{}}
                let hub=Self {inner};
                tokio::select! {_=shutdown.changed()=>break,result=hub.reports_once()=>{delay=result.unwrap_or(Duration::from_secs(60));}}
            }
        });
        Ok(())
    }
}
fn backoff_ms(attempt: u32) -> i64 {
    let base = (1000i64 << attempt.min(13)).min(4 * 3600 * 1000);
    let mut random = [0; 2];
    let _ = getrandom::fill(&mut random);
    base + base * i64::from(u16::from_le_bytes(random)) / u16::MAX as i64 / 4
}
