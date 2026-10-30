// SPDX-License-Identifier: Apache-2.0
use intake::{
    Intake, Store,
    client::Client,
    config::Config,
    maintainer::{KnownIssue, ReportFilter},
};
use protocol::{
    Command, CommandEnvelope, Query, QueryData, ReportId, TaskId, alerts::Alert,
    command::ReceiptStatus, reports::*, time::Timestamp,
};
use services::{Caller, CompanionHub, Hub, reports::ReportsConfig};
use sha2::{Digest, Sha256};

const TOKEN: &str = "synthetic-maintainer-token-for-local-tests-only";

struct Fixture {
    dir: tempfile::TempDir,
    hub: CompanionHub,
    admin: Client,
    server: tokio::task::JoinHandle<intake::Result<()>>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let intake = Intake::new(
            Store::open(dir.path().join("intake.sqlite")).unwrap(),
            Config::default(),
            TOKEN,
        )
        .unwrap();
        let reporter = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let maintainer = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", reporter.local_addr().unwrap());
        let admin = Client::new(
            &format!("http://{}", maintainer.local_addr().unwrap()),
            TOKEN,
        )
        .unwrap();
        let server = tokio::spawn(intake.serve(reporter, maintainer));
        let hub = CompanionHub::builder()
            .data_dir(dir.path().join("companion"))
            .reports(ReportsConfig {
                intake_url: Some(url),
                build: "running".into(),
                previous_build: Some("previous".into()),
                ..Default::default()
            })
            .build()
            .await
            .unwrap();
        Self {
            dir,
            hub,
            admin,
            server,
        }
    }
    async fn command(&self, command: Command) {
        let receipt = self
            .hub
            .submit(&Caller::local_owner(), CommandEnvelope::new(command))
            .await;
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
        assert_eq!(receipt.error, None);
    }
    async fn submit(&self, bundle: &ReportBundle) {
        self.command(Command::SubmitReport {
            bundle: Box::new(bundle.clone()),
        })
        .await;
        self.poll().await;
    }
    async fn poll(&self) {
        self.hub.check_reports_now().await.unwrap();
    }
    async fn view(&self) -> ReportsView {
        match self
            .hub
            .query(&Caller::local_owner(), Query::Reports)
            .await
            .unwrap()
        {
            QueryData::Reports { view } => view,
            other => panic!("unexpected reports view: {other:?}"),
        }
    }
    async fn summary(&self, id: ReportId) -> ReportSummary {
        self.view()
            .await
            .reports
            .into_iter()
            .find(|r| r.report_id == id)
            .unwrap()
    }
    async fn alerts(&self) -> Vec<Alert> {
        self.hub
            .open_alerts(&Caller::local_owner())
            .await
            .unwrap()
            .0
    }
    async fn needs_you(&self, id: ReportId) -> bool {
        let QueryData::Snapshot(snapshot) = self
            .hub
            .query(
                &Caller::local_owner(),
                Query::Snapshot {
                    reads_withheld: Some(true),
                },
            )
            .await
            .unwrap()
        else {
            panic!()
        };
        snapshot
            .attention
            .iter()
            .any(|item| item.report_id == Some(id) && item.resolved_at.is_none())
    }
    async fn bundle(&self) -> ReportBundle {
        let part = match self
            .hub
            .query(
                &Caller::local_owner(),
                Query::ReportPreview {
                    salt: "00112233445566778899aabbccddeeff".into(),
                    extras: vec![],
                },
            )
            .await
            .unwrap()
        {
            QueryData::ReportPreview { part } => part,
            other => panic!("unexpected report preview: {other:?}"),
        };
        ReportBundle {
            schema: REPORT_SCHEMA,
            report_id: ReportId::new(),
            created_at: Timestamp::now(),
            origin: ReportOrigin::Error,
            words: Some("The screen stayed empty".into()),
            signature: ReportSignature {
                code: "empty".into(),
                screen: "/work/:task".into(),
                build: "running".into(),
            },
            versions: ReportVersions {
                computer: Some(part.versions),
                ..Default::default()
            },
            context: ReportContext {
                screen: "/work/:task".into(),
                task: Some(pseudonym(
                    "00112233445566778899aabbccddeeff",
                    PseudonymKind::Task,
                    &TaskId::new().to_string(),
                )),
                error: Some(ReportedError {
                    code: "empty".into(),
                    message: scrub(
                        "Could not read /workspace/private at https://private.example with 'private name'",
                    ),
                }),
                ..Default::default()
            },
            phone_trail: vec![],
            computer_trail: part.trail,
            state: None,
            crashes: part.crashes,
            screenshot: None,
            extras: part.extras,
            test: None,
        }
    }
}

#[tokio::test]
async fn companion_submits_status_questions_releases_and_withdrawal_to_real_intake() {
    let f = Fixture::new().await;
    let b = f.bundle().await;
    let id = b.report_id;
    f.submit(&b).await;
    let summary = f.summary(id).await;
    assert_eq!(summary.status(), ReportProgress::Received);
    assert_eq!(summary.reference, report_reference(&id));
    let record = f.admin.report(id).await.unwrap();
    assert_eq!(record.bundle, Some(b.clone()));
    let bytes: [u8; 32] = std::fs::read(f.dir.path().join("companion/reporter.key"))
        .unwrap()
        .try_into()
        .unwrap();
    let key = ed25519_dalek::SigningKey::from_bytes(&bytes);
    assert_eq!(
        record.key_fingerprint,
        format!("{:x}", Sha256::digest(key.verifying_key().as_bytes()))
    );
    let json = serde_json::to_string(&record.bundle).unwrap();
    for private in [
        "/workspace/private",
        "https://private.example",
        "private name",
        "00112233445566778899aabbccddeeff",
    ] {
        assert!(!json.contains(private));
    }
    assert!(json.contains("<path>"));
    f.submit(&b).await;
    assert_eq!(
        f.admin
            .list(&ReportFilter::default())
            .await
            .unwrap()
            .reports
            .len(),
        1
    );
    assert_eq!(f.admin.report(id).await.unwrap().history, record.history);
    assert!(
        f.admin
            .claim("synthetic-worker", 30_000)
            .await
            .unwrap()
            .is_none()
    );
    f.admin.admit(id).await.unwrap();
    assert_eq!(
        f.admin
            .claim("synthetic-worker", 30_000)
            .await
            .unwrap()
            .unwrap()
            .report_id,
        id
    );

    for progress in [
        ReportProgress::Reproduced,
        ReportProgress::NotReproduced,
        ReportProgress::FixWritten,
        ReportProgress::InReview,
    ] {
        f.admin.set_status(id, progress.clone()).await.unwrap();
        f.poll().await;
        assert_eq!(f.summary(id).await.status(), progress);
        let alerts = f.alerts().await;
        assert_eq!(alerts.len(), usize::from(progress.alerts()));
        if let Some(alert) = alerts.first() {
            assert_eq!(alert.report.as_ref().unwrap().report_id, id);
            assert_eq!(alert.report.as_ref().unwrap().status, progress);
        }
        f.poll().await;
        assert_eq!(f.alerts().await, alerts);
    }

    let question = f
        .admin
        .ask(id, QuestionTemplate::HappenedAgain)
        .await
        .unwrap();
    f.poll().await;
    let alerts = f.alerts().await;
    assert_eq!(alerts.len(), 1);
    assert_eq!(
        alerts[0]
            .report
            .as_ref()
            .unwrap()
            .question
            .as_ref()
            .unwrap()
            .question_id,
        question.question_id
    );
    assert!(f.needs_you(id).await);
    f.poll().await;
    assert_eq!(f.alerts().await, alerts);
    f.command(Command::AnswerReportQuestion {
        report_id: id,
        question_id: question.question_id,
        answer: ReportAnswer {
            choice: QuestionChoice::Yes,
            words: Some("It happened again".into()),
            at: Timestamp::now(),
        },
    })
    .await;
    f.poll().await;
    let answered = f
        .admin
        .report(id)
        .await
        .unwrap()
        .questions
        .into_iter()
        .find(|q| q.question_id == question.question_id)
        .unwrap()
        .answer
        .unwrap();
    assert_eq!(answered.choice, QuestionChoice::Yes);
    assert_eq!(answered.words.as_deref(), Some("It happened again"));
    assert!(f.alerts().await.is_empty());
    assert!(!f.needs_you(id).await);
    let question = f
        .admin
        .ask(id, QuestionTemplate::AfterUpdate)
        .await
        .unwrap();
    f.poll().await;
    assert_eq!(f.alerts().await.len(), 1);
    f.admin
        .close_question(id, question.question_id)
        .await
        .unwrap();
    f.poll().await;
    assert!(f.alerts().await.is_empty());
    assert!(!f.needs_you(id).await);
    assert!(
        !f.summary(id)
            .await
            .questions
            .iter()
            .any(|q| q.question_id == question.question_id)
    );

    f.admin
        .publish(ReleaseEntry {
            build: "fixed-build".into(),
            published_at: Timestamp::now(),
            ring: ReleaseRing::Owner,
            state: ReleaseState::Rolling,
            fixes: vec![b.signature.key()],
        })
        .await
        .unwrap();
    f.poll().await;
    assert_eq!(
        f.summary(id).await.status(),
        ReportProgress::Fixed {
            build: "fixed-build".into()
        }
    );
    let alerts = f.alerts().await;
    assert_eq!(alerts.len(), 1);
    f.poll().await;
    assert_eq!(f.alerts().await, alerts);
    f.admin
        .widen("fixed-build", ReleaseRing::Everyone)
        .await
        .unwrap();
    f.poll().await;
    assert_eq!(
        f.summary(id).await.status(),
        ReportProgress::Shipped {
            build: "fixed-build".into()
        }
    );
    let alerts = f.alerts().await;
    assert_eq!(alerts.len(), 1);
    f.poll().await;
    assert_eq!(f.alerts().await, alerts);
    f.admin
        .publish(ReleaseEntry {
            build: "running".into(),
            published_at: Timestamp::now(),
            ring: ReleaseRing::Owner,
            state: ReleaseState::Rolling,
            fixes: vec![],
        })
        .await
        .unwrap();
    f.admin.halt("running").await.unwrap();
    f.poll().await;
    assert_eq!(
        f.view().await.halted,
        Some(HaltedRelease {
            build: "running".into(),
            back_to: Some("previous".into())
        })
    );
    f.command(Command::WithdrawReport { report_id: id }).await;
    f.poll().await;
    let record = f.admin.report(id).await.unwrap();
    assert!(record.bundle.is_none());
    assert!(record.questions.is_empty());
    assert_eq!(
        record.history.last().unwrap().status,
        ReportProgress::Withdrawn
    );
    f.hub.shutdown().await.unwrap();
}

#[tokio::test]
async fn intake_closure_keeps_a_report_local_until_reopened() {
    let f = Fixture::new().await;
    f.admin.intake_closed(true).await.unwrap();
    assert!(f.admin.switches().await.unwrap().intake_closed);
    let b = f.bundle().await;
    f.submit(&b).await;
    assert_eq!(
        f.summary(b.report_id).await.status(),
        ReportProgress::Queued
    );
    assert_eq!(f.view().await.intake, IntakeLink::Paused);
    assert!(
        f.admin
            .list(&ReportFilter::default())
            .await
            .unwrap()
            .reports
            .is_empty()
    );
    f.admin.intake_closed(false).await.unwrap();
    f.poll().await;
    f.poll().await;
    assert_eq!(
        f.summary(b.report_id).await.status(),
        ReportProgress::Received
    );
    assert_eq!(f.admin.report(b.report_id).await.unwrap().bundle, Some(b));
    f.hub.shutdown().await.unwrap();
}

#[tokio::test]
async fn known_issue_receipts_and_declines_reach_the_companion() {
    let f = Fixture::new().await;
    let b = f.bundle().await;
    f.admin
        .put_known(KnownIssue {
            signature: b.signature.key(),
            affected_builds: vec!["running".into()],
            fixed_in: Some("fixed-build".into()),
        })
        .await
        .unwrap();
    assert_eq!(f.admin.known_issues().await.unwrap().len(), 1);
    f.submit(&b).await;
    assert_eq!(
        f.summary(b.report_id).await.status(),
        ReportProgress::Known {
            fixed_in: Some("fixed-build".into())
        }
    );
    assert!(f.alerts().await.is_empty());
    let status = ReportProgress::Declined {
        reason: DeclineReason::WorksAsDocumented,
    };
    f.admin
        .set_status(b.report_id, status.clone())
        .await
        .unwrap();
    f.poll().await;
    assert_eq!(f.summary(b.report_id).await.status(), status);
    assert!(f.alerts().await.is_empty());
    f.hub.shutdown().await.unwrap();
}
