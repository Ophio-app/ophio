// SPDX-License-Identifier: Apache-2.0
//! Typed asynchronous client for the private listener. Debug output omits credentials.
use crate::maintainer::*;
use protocol::{ReportId, ReportQuestionId, reports::*};
use reqwest::{Method, Url};
use serde::{Serialize, de::DeserializeOwned};
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("maintainer transport failed")]
    Transport,
    #[error("maintainer request refused ({0})")]
    Status(u16),
    #[error("invalid maintainer URL or token")]
    Invalid,
}
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base: Url,
    token: reqwest::header::HeaderValue,
}
impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client").finish_non_exhaustive()
    }
}
impl Client {
    pub fn new(base: &str, token: &str) -> Result<Self> {
        let base = Url::parse(base).map_err(|_| Error::Invalid)?;
        if !matches!(base.scheme(), "http" | "https")
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
        {
            return Err(Error::Invalid);
        }
        let mut token = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| Error::Invalid)?;
        token.set_sensitive(true);
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(30))
            .no_proxy()
            .build()
            .map_err(|_| Error::Transport)?;
        Ok(Self { http, base, token })
    }
    async fn request<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&impl Serialize>,
    ) -> Result<T> {
        let url = self.base.join(path).map_err(|_| Error::Invalid)?;
        let mut request = self
            .http
            .request(method, url)
            .header(reqwest::header::AUTHORIZATION, self.token.clone());
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().await.map_err(|_| Error::Transport)?;
        if !response.status().is_success() {
            return Err(Error::Status(response.status().as_u16()));
        }
        response.json().await.map_err(|_| Error::Transport)
    }
    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        self.request(Method::GET, path, None::<&()>).await
    }
    pub async fn list(&self, filter: &ReportFilter) -> Result<ReportPage> {
        self.request(Method::POST, PATH_QUERY, Some(filter)).await
    }
    pub async fn report(&self, id: ReportId) -> Result<ReportRecord> {
        self.get(&format!("/maintainer/v1/reports/{id}")).await
    }
    pub async fn switches(&self) -> Result<Switches> {
        self.get(PATH_STATE).await
    }
    pub async fn counters(&self) -> Result<Counters> {
        self.get(PATH_COUNTERS).await
    }
    pub async fn known_issues(&self) -> Result<Vec<KnownIssue>> {
        self.get("/maintainer/v1/known").await
    }
    pub async fn releases(&self) -> Result<ReleaseFeed> {
        self.get("/maintainer/v1/releases").await
    }
    pub async fn command(&self, command: &Command) -> Result<CommandResult> {
        self.request(Method::POST, PATH_COMMAND, Some(command))
            .await
    }
    pub async fn claim(&self, worker: &str, lease_ms: u64) -> Result<Option<ReportRecord>> {
        match self
            .command(&Command::Claim {
                worker: worker.into(),
                lease_ms,
            })
            .await?
        {
            CommandResult::Claimed { report } => Ok(report.map(|r| *r)),
            _ => Err(Error::Transport),
        }
    }
    /// Takes the oldest unchecked test submission, admitted or not.
    pub async fn claim_check(&self, worker: &str, lease_ms: u64) -> Result<Option<ReportRecord>> {
        match self
            .command(&Command::ClaimCheck {
                worker: worker.into(),
                lease_ms,
            })
            .await?
        {
            CommandResult::Claimed { report } => Ok(report.map(|r| *r)),
            _ => Err(Error::Transport),
        }
    }
    /// Records a check under this worker's live check lease; idempotent.
    pub async fn record_check(
        &self,
        report_id: ReportId,
        worker: &str,
        check: TestCheck,
    ) -> Result<()> {
        self.command(&Command::RecordCheck {
            report_id,
            worker: worker.into(),
            check,
        })
        .await
        .map(|_| ())
    }
    /// Puts this worker's check off for `retry_ms`.
    pub async fn defer_check(
        &self,
        report_id: ReportId,
        worker: &str,
        retry_ms: u64,
    ) -> Result<()> {
        self.command(&Command::DeferCheck {
            report_id,
            worker: worker.into(),
            retry_ms,
        })
        .await
        .map(|_| ())
    }
    /// Lets the fixer claim this report, at the owner's word.
    pub async fn admit(&self, report_id: ReportId) -> Result<()> {
        self.command(&Command::Admit { report_id })
            .await
            .map(|_| ())
    }
    pub async fn set_status(&self, report_id: ReportId, status: ReportProgress) -> Result<()> {
        self.command(&Command::SetStatus { report_id, status })
            .await
            .map(|_| ())
    }
    /// Idempotently retire this worker's claim, including unanswered questions.
    pub async fn finish_report(&self, report_id: ReportId, worker: &str) -> Result<()> {
        self.command(&Command::FinishReport {
            report_id,
            worker: worker.into(),
        })
        .await
        .map(|_| ())
    }
    pub async fn renew(&self, report_id: ReportId, worker: &str, lease_ms: u64) -> Result<()> {
        self.command(&Command::Renew {
            report_id,
            worker: worker.into(),
            lease_ms,
        })
        .await
        .map(|_| ())
    }
    pub async fn resume_lease(
        &self,
        report_id: ReportId,
        worker: &str,
        lease_ms: u64,
    ) -> Result<()> {
        self.command(&Command::ResumeLease {
            report_id,
            worker: worker.into(),
            lease_ms,
        })
        .await
        .map(|_| ())
    }
    pub async fn claim_next(
        &self,
        worker: &str,
        lease_ms: u64,
        excluding: Vec<ReportId>,
    ) -> Result<Option<ReportRecord>> {
        match self
            .command(&Command::ClaimNext {
                worker: worker.into(),
                lease_ms,
                excluding,
            })
            .await?
        {
            CommandResult::Claimed { report } => Ok(report.map(|r| *r)),
            _ => Err(Error::Transport),
        }
    }
    pub async fn ask(
        &self,
        report_id: ReportId,
        template: QuestionTemplate,
    ) -> Result<ReportQuestion> {
        match self
            .command(&Command::Ask {
                report_id,
                template,
            })
            .await?
        {
            CommandResult::Question { question } => Ok(question),
            _ => Err(Error::Transport),
        }
    }
    pub async fn answer_question(
        &self,
        report_id: ReportId,
        question_id: ReportQuestionId,
        answer: ReportAnswer,
    ) -> Result<()> {
        self.command(&Command::AnswerQuestion {
            report_id,
            question_id,
            answer,
        })
        .await
        .map(|_| ())
    }
    pub async fn close_question(
        &self,
        report_id: ReportId,
        question_id: ReportQuestionId,
    ) -> Result<()> {
        self.command(&Command::CloseQuestion {
            report_id,
            question_id,
        })
        .await
        .map(|_| ())
    }
    pub async fn pipeline_paused(&self, paused: bool) -> Result<()> {
        self.command(&Command::PipelinePaused { paused })
            .await
            .map(|_| ())
    }
    pub async fn intake_closed(&self, closed: bool) -> Result<()> {
        self.command(&Command::IntakeClosed { closed })
            .await
            .map(|_| ())
    }
    pub async fn ban(&self, key_fingerprint: &str, banned: bool) -> Result<()> {
        self.command(&Command::Ban {
            key_fingerprint: key_fingerprint.into(),
            banned,
        })
        .await
        .map(|_| ())
    }
    pub async fn put_known(&self, issue: KnownIssue) -> Result<()> {
        self.command(&Command::PutKnown { issue }).await.map(|_| ())
    }
    pub async fn delete_known(&self, signature: &str) -> Result<()> {
        self.command(&Command::DeleteKnown {
            signature: signature.into(),
        })
        .await
        .map(|_| ())
    }
    pub async fn publish(&self, release: ReleaseEntry) -> Result<()> {
        self.command(&Command::Publish { release })
            .await
            .map(|_| ())
    }
    pub async fn widen(&self, build: &str, ring: ReleaseRing) -> Result<()> {
        self.command(&Command::Widen {
            build: build.into(),
            ring,
        })
        .await
        .map(|_| ())
    }
    pub async fn halt(&self, build: &str) -> Result<()> {
        self.command(&Command::Halt {
            build: build.into(),
        })
        .await
        .map(|_| ())
    }
    pub async fn complete(&self, build: &str) -> Result<()> {
        self.command(&Command::Complete {
            build: build.into(),
        })
        .await
        .map(|_| ())
    }
    pub async fn sweep(&self, retention_days: u32) -> Result<usize> {
        match self.command(&Command::Sweep { retention_days }).await? {
            CommandResult::Swept { reports } => Ok(reports),
            _ => Err(Error::Transport),
        }
    }
}
