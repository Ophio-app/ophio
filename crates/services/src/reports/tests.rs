// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::api::{Caller, Hub};
use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, Method, StatusCode, Uri},
    response::IntoResponse,
};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use ed25519_dalek::{Signature, VerifyingKey};
use protocol::{
    capability::HarnessKind,
    command::{Command, CommandEnvelope, ReceiptStatus},
};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Default)]
struct IntakeState {
    bundles: Vec<ReportBundle>,
    answers: Vec<IntakeAnswer>,
    withdrawals: Vec<ReportId>,
    updates: Vec<ReportUpdate>,
    requests: Vec<String>,
    status_ids: Vec<Vec<ReportId>>,
    paused: bool,
    fail: bool,
    feed: Option<ReleaseFeed>,
}
struct Intake {
    url: String,
    state: Arc<Mutex<IntakeState>>,
    job: tokio::task::JoinHandle<()>,
}
impl Drop for Intake {
    fn drop(&mut self) {
        self.job.abort();
    }
}
impl Intake {
    async fn new() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(IntakeState::default()));
        let router = Router::new().fallback(handle).with_state(state.clone());
        let job = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self { url, state, job }
    }
}
async fn handle(
    State(state): State<Arc<Mutex<IntakeState>>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    let key = URL_SAFE_NO_PAD
        .decode(headers[KEY_HEADER].as_bytes())
        .unwrap();
    let key = VerifyingKey::from_bytes(&key.try_into().unwrap()).unwrap();
    let signature = Signature::from_slice(
        &URL_SAFE_NO_PAD
            .decode(headers[SIGNATURE_HEADER].as_bytes())
            .unwrap(),
    )
    .unwrap();
    let time = headers[TIME_HEADER]
        .to_str()
        .unwrap()
        .parse::<i64>()
        .unwrap();
    assert!((Timestamp::now().0 - time).abs() <= MAX_CLOCK_SKEW_MS);
    key.verify_strict(
        &signing_payload(method.as_str(), uri.path(), time, &body),
        &signature,
    )
    .unwrap();
    let mut state = state.lock().unwrap();
    state.requests.push(uri.path().into());
    let now = Timestamp::now();
    let response = |reports, paused| StatusResponse {
        reports,
        now,
        paused,
    };
    if state.fail {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(serde_json::json!({"paused":false})),
        )
            .into_response();
    }
    if state.paused && uri.path() == PATH_SUBMIT {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(response(vec![], true)),
        )
            .into_response();
    }
    match uri.path() {
        PATH_SUBMIT => {
            let submission: IntakeSubmission = serde_json::from_slice(&body).unwrap();
            assert_eq!(submission.sender, ReportSide::Computer);
            let id = submission.bundle.report_id;
            if !state.bundles.iter().any(|b| b.report_id == id) {
                state.bundles.push(submission.bundle);
            }
            if !state.updates.iter().any(|u| u.report_id == id) {
                state.updates.push(ReportUpdate {
                    report_id: id,
                    history: vec![StatusChange {
                        status: ReportProgress::Received,
                        at: now,
                    }],
                    questions: vec![],
                });
            }
            axum::Json(IntakeReceipt {
                report_id: id,
                reference: report_reference(&id),
                status: ReportProgress::Received,
            })
            .into_response()
        }
        PATH_STATUS => {
            let request: StatusRequest = serde_json::from_slice(&body).unwrap();
            assert!(request.report_ids.len() <= MAX_STATUS_IDS);
            state.status_ids.push(request.report_ids.clone());
            axum::Json(response(
                state
                    .updates
                    .iter()
                    .filter(|u| request.report_ids.contains(&u.report_id))
                    .cloned()
                    .collect(),
                state.paused,
            ))
            .into_response()
        }
        PATH_ANSWER => {
            let answer: IntakeAnswer = serde_json::from_slice(&body).unwrap();
            if let Some(update) = state
                .updates
                .iter_mut()
                .find(|u| u.report_id == answer.report_id)
                && let Some(question) = update
                    .questions
                    .iter_mut()
                    .find(|q| q.question_id == answer.question_id)
            {
                question.answer = Some(answer.answer.clone());
            }
            state.answers.push(answer);
            axum::Json(response(state.updates.clone(), state.paused)).into_response()
        }
        PATH_WITHDRAW => {
            let request: WithdrawRequest = serde_json::from_slice(&body).unwrap();
            state.withdrawals.push(request.report_id);
            state.bundles.retain(|b| b.report_id != request.report_id);
            axum::Json(response(vec![], state.paused)).into_response()
        }
        PATH_RELEASES => axum::Json(state.feed.clone().unwrap_or(ReleaseFeed {
            releases: vec![],
            now,
        }))
        .into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}
struct Fixture {
    dir: tempfile::TempDir,
    hub: CompanionHub,
}
impl Fixture {
    async fn new(url: Option<String>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let hub = CompanionHub::builder()
            .data_dir(dir.path())
            .reports(ReportsConfig {
                intake_url: url,
                build: "running".into(),
                previous_build: Some("previous".into()),
                ..Default::default()
            })
            .build()
            .await
            .unwrap();
        Self { dir, hub }
    }
    async fn command(&self, command: Command) {
        let mut envelope = CommandEnvelope::new(command);
        if matches!(envelope.body, Command::SetReportSettings { .. }) {
            envelope.expected_revision = Some(self.hub.reports_view().unwrap().settings.revision);
        }
        let receipt = self.hub.submit(&Caller::local_owner(), envelope).await;
        assert_eq!(receipt.error, None, "{receipt:?}");
    }
    async fn submit(&self, bundle: &ReportBundle) {
        self.command(Command::SubmitReport {
            bundle: Box::new(bundle.clone()),
        })
        .await;
    }
    async fn close(self) {
        self.hub.shutdown().await.unwrap();
    }
}
fn bundle() -> ReportBundle {
    ReportBundle {
        schema: REPORT_SCHEMA,
        report_id: ReportId::new(),
        created_at: Timestamp::now(),
        origin: ReportOrigin::Settings,
        words: Some("It did not finish".into()),
        signature: ReportSignature {
            code: "none".into(),
            screen: "/settings".into(),
            build: "running".into(),
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
async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
fn status(f: &Fixture, id: ReportId) -> ReportProgress {
    f.hub
        .read(|r| Ok(r.require::<ReportSummary>(SUMMARY, id)?.status()))
        .unwrap()
}
async fn force_poll(f: &Fixture) {
    let _one = f.hub.inner.reports.sending.lock().await;
    f.hub
        .write(|tx| tx.put(META, "poll_after", &Timestamp(0)))
        .unwrap();
}

#[tokio::test]
async fn submit_is_reviewed_queued_signed_received_and_idempotent() {
    let intake = Intake::new().await;
    let f = Fixture::new(Some(intake.url.clone())).await;
    let locked = f.hub.inner.reports.sending.lock().await;
    let b = bundle();
    f.submit(&b).await;
    assert_eq!(status(&f, b.report_id), ReportProgress::Queued);
    assert_eq!(
        f.hub
            .read(|r| Ok(r.list::<ReportDelivery>(QUEUE)?.len()))
            .unwrap(),
        1
    );
    let duplicate = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::SubmitReport {
                bundle: Box::new(b.clone()),
            }),
        )
        .await;
    assert_eq!(duplicate.error, None);
    drop(locked);
    until(|| status(&f, b.report_id) == ReportProgress::Received).await;
    assert_eq!(intake.state.lock().unwrap().bundles, vec![b.clone()]);
    f.submit(&b).await;
    assert_eq!(intake.state.lock().unwrap().bundles.len(), 1);
    let detail = f
        .hub
        .query(
            &Caller::local_owner(),
            Query::Report {
                report_id: b.report_id,
            },
        )
        .await
        .unwrap();
    assert!(matches!(detail,QueryData::Report {detail:Some(d)} if d.bundle==b));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(f.dir.path().join("reporter.key"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    f.close().await;
}

#[tokio::test]
async fn unreachable_retries_back_off_and_recover() {
    let intake = Intake::new().await;
    intake.state.lock().unwrap().fail = true;
    let f = Fixture::new(Some(intake.url.clone())).await;
    let b = bundle();
    f.submit(&b).await;
    until(|| {
        matches!(
            f.hub.reports_view().unwrap().intake,
            IntakeLink::Unreachable { queued: 1, .. }
        )
    })
    .await;
    let row = f
        .hub
        .read(|r| Ok(r.list::<ReportDelivery>(QUEUE)?[0].clone()))
        .unwrap();
    assert!(row.attempts >= 1 && row.after > Timestamp::now());
    f.hub.reports_once().await.unwrap();
    assert_eq!(
        f.hub
            .read(|r| Ok(r.list::<ReportDelivery>(QUEUE)?[0].attempts))
            .unwrap(),
        row.attempts
    );
    intake.state.lock().unwrap().fail = false;
    let lock = f.hub.inner.reports.sending.lock().await;
    f.hub
        .write(|tx| {
            let mut row = row.clone();
            row.after = Timestamp(0);
            tx.put(QUEUE, row.delivery.key(), &row)
        })
        .unwrap();
    drop(lock);
    f.hub.reports_once().await.unwrap();
    assert_eq!(status(&f, b.report_id), ReportProgress::Received);
    assert_eq!(f.hub.reports_view().unwrap().intake, IntakeLink::Reachable);
    f.close().await;
}

#[tokio::test]
async fn paused_intake_and_sending_setting_keep_reports_local() {
    let intake = Intake::new().await;
    intake.state.lock().unwrap().paused = true;
    let f = Fixture::new(Some(intake.url.clone())).await;
    let b = bundle();
    f.submit(&b).await;
    until(|| f.hub.reports_view().unwrap().intake == IntakeLink::Paused).await;
    assert_eq!(status(&f, b.report_id), ReportProgress::Queued);
    assert!(intake.state.lock().unwrap().bundles.is_empty());
    intake.state.lock().unwrap().paused = false;
    force_poll(&f).await;
    f.hub.reports_once().await.unwrap();
    f.hub
        .write(|tx| {
            for mut row in tx.read().list::<ReportDelivery>(QUEUE)? {
                row.after = Timestamp(0);
                tx.put(QUEUE, row.delivery.key(), &row)?;
            }
            Ok(())
        })
        .unwrap();
    f.hub.reports_once().await.unwrap();
    assert_eq!(status(&f, b.report_id), ReportProgress::Received);
    f.command(Command::SetReportSettings {
        sending: false,
        ring: ReleaseRing::Early,
    })
    .await;
    let b2 = bundle();
    f.submit(&b2).await;
    f.hub.reports_once().await.unwrap();
    assert_eq!(status(&f, b2.report_id), ReportProgress::Queued);
    let view = f.hub.reports_view().unwrap();
    assert_eq!(view.settings.revision, 1);
    assert_eq!(view.settings.ring, ReleaseRing::Early);
    let mut stale = CommandEnvelope::new(Command::SetReportSettings {
        sending: true,
        ring: ReleaseRing::Everyone,
    });
    stale.expected_revision = Some(0);
    assert_eq!(
        f.hub.submit(&Caller::local_owner(), stale).await.status,
        ReceiptStatus::Rejected
    );
    f.close().await;
}

#[tokio::test]
async fn invalid_reports_are_rejected_without_being_kept() {
    let f = Fixture::new(None).await;
    let mut cases = Vec::new();
    let mut b = bundle();
    b.schema += 1;
    cases.push(b);
    let mut b = bundle();
    b.words = Some("x".repeat(MAX_REPORT_WORDS + 1));
    cases.push(b);
    let mut b = bundle();
    b.state = Some(StateShape {
        version: 1,
        store: serde_json::json!("x".repeat(MAX_REPORT_BYTES)),
    });
    cases.push(b);
    let mut b = bundle();
    b.signature.code = "x".repeat(MAX_REPORT_CODE + 1);
    cases.push(b);
    let mut b = bundle();
    b.phone_trail = vec![
        TrailEvent {
            at: Timestamp(1),
            event: Trail::Service {
                state: ServiceState::Started
            }
        };
        MAX_TRAIL_EVENTS + 1
    ];
    cases.push(b);
    let mut b = bundle();
    b.screenshot = Some(ReportScreenshot {
        media_type: "image/png".into(),
        data: STANDARD.encode(b"not an image"),
        width: 1,
        height: 1,
        marked: false,
    });
    cases.push(b);
    let mut b = bundle();
    b.screenshot = Some(ReportScreenshot {
        media_type: "image/jpeg".into(),
        data: "%%%".into(),
        width: 1,
        height: 1,
        marked: false,
    });
    cases.push(b);
    let mut b = bundle();
    b.crashes = vec![crash()];
    b.crashes[0].frames = vec![
        StackFrame {
            function: None,
            file: "crates/companion/src/main.rs".into(),
            line: 1,
            column: None
        };
        MAX_FRAMES + 1
    ];
    cases.push(b);
    for b in cases {
        let id = b.report_id;
        let receipt = f
            .hub
            .submit(
                &Caller::local_owner(),
                CommandEnvelope::new(Command::SubmitReport {
                    bundle: Box::new(b),
                }),
            )
            .await;
        assert_eq!(receipt.status, ReceiptStatus::Rejected);
        assert!(
            f.hub
                .read(|r| r.get::<ReportSummary>(SUMMARY, id))
                .unwrap()
                .is_none()
        );
    }
    let mut valid = bundle();
    valid.screenshot = Some(ReportScreenshot {
        media_type: "image/png".into(),
        data: STANDARD.encode(b"\x89PNG\r\n\x1a\nfixture"),
        width: 1,
        height: 1,
        marked: false,
    });
    f.submit(&valid).await;
    f.close().await;
}
fn crash() -> CrashRecord {
    CrashRecord {
        at: Timestamp::now(),
        side: ReportSide::Computer,
        kind: CrashKind::Panic,
        code: "panic:crates/companion/src/main.rs:1".into(),
        build: "running".into(),
        message: Some("fixture panic".into()),
        frames: vec![],
    }
}

#[tokio::test]
async fn crash_records_are_in_views_until_a_submitted_report_carries_them() {
    let f = Fixture::new(None).await;
    let crash = crash();
    append_crash(f.dir.path(), &crash).unwrap();
    assert_eq!(f.hub.reports_view().unwrap().crashes, vec![crash.clone()]);
    let part = f
        .hub
        .report_preview("00112233445566778899aabbccddeeff", &[])
        .await
        .unwrap();
    assert_eq!(part.crashes, vec![crash.clone()]);
    let mut b = bundle();
    b.crashes = vec![crash];
    f.submit(&b).await;
    assert!(f.hub.reports_view().unwrap().crashes.is_empty());
    assert!(read_crashes(f.dir.path()).is_empty());
    assert_eq!(f.hub.reports_view().unwrap().intake, IntakeLink::None);
    f.close().await;
}

#[tokio::test]
async fn status_questions_and_progress_alert_once_and_answers_clear_attention() {
    let intake = Intake::new().await;
    let f = Fixture::new(Some(intake.url.clone())).await;
    let b = bundle();
    f.submit(&b).await;
    until(|| status(&f, b.report_id) == ReportProgress::Received).await;
    let update = |status| ReportUpdate {
        report_id: b.report_id,
        history: vec![StatusChange {
            status,
            at: Timestamp(10),
        }],
        questions: vec![],
    };
    intake.state.lock().unwrap().updates = vec![update(ReportProgress::Reproduced)];
    force_poll(&f).await;
    f.hub.reports_once().await.unwrap();
    let (alerts, _) = f.hub.open_alerts(&Caller::local_owner()).await.unwrap();
    assert_eq!(alerts.len(), 1);
    let alert_id = alerts[0].attention_id;
    assert!(alerts[0].report.is_some());
    assert!(alerts[0].project.is_empty());
    assert_eq!(alerts[0].task_id, TaskId(uuid::Uuid::nil()));
    force_poll(&f).await;
    f.hub.reports_once().await.unwrap();
    assert_eq!(
        f.hub.open_alerts(&Caller::local_owner()).await.unwrap().0[0].attention_id,
        alert_id
    );
    intake.state.lock().unwrap().updates = vec![update(ReportProgress::InReview)];
    force_poll(&f).await;
    f.hub.reports_once().await.unwrap();
    assert!(
        f.hub
            .open_alerts(&Caller::local_owner())
            .await
            .unwrap()
            .0
            .is_empty()
    );
    let question = ReportQuestion {
        question_id: ReportQuestionId::new(),
        template: QuestionTemplate::HappenedAgain,
        asked_at: Timestamp(12),
        answer: None,
    };
    let mut asked = update(ReportProgress::Question);
    asked.questions = vec![question.clone()];
    intake.state.lock().unwrap().updates = vec![asked];
    force_poll(&f).await;
    f.hub.reports_once().await.unwrap();
    assert!(
        f.hub
            .read(|r| Ok(r
                .list::<AttentionItem>("attention")?
                .iter()
                .any(|a| a.report_id == Some(b.report_id)
                    && a.resolved_at.is_none()
                    && a.kind == AttentionKind::Question)))
            .unwrap()
    );
    f.command(Command::AnswerReportQuestion {
        report_id: b.report_id,
        question_id: question.question_id,
        answer: ReportAnswer {
            choice: QuestionChoice::Yes,
            words: None,
            at: Timestamp::now(),
        },
    })
    .await;
    until(|| !intake.state.lock().unwrap().answers.is_empty()).await;
    assert!(
        f.hub
            .open_alerts(&Caller::local_owner())
            .await
            .unwrap()
            .0
            .is_empty()
    );
    f.close().await;
}

#[tokio::test]
async fn withdrawal_drops_bundle_and_queues_while_unreachable() {
    let intake = Intake::new().await;
    let f = Fixture::new(Some(intake.url.clone())).await;
    let mut b = bundle();
    b.words = Some("unique-removable-report-content".into());
    f.submit(&b).await;
    until(|| status(&f, b.report_id) == ReportProgress::Received).await;
    intake.state.lock().unwrap().fail = true;
    f.command(Command::WithdrawReport {
        report_id: b.report_id,
    })
    .await;
    assert_eq!(status(&f, b.report_id), ReportProgress::Withdrawn);
    assert!(
        f.hub
            .read(|r| r.report_detail(b.report_id))
            .unwrap()
            .is_none()
    );
    assert!(
        f.hub
            .read(|r| Ok(r.commands()?.iter().all(|r| match r {
                store::CommandRead::Decoded(c) => !serde_json::to_string(c)
                    .unwrap()
                    .contains("unique-removable-report-content"),
                _ => true,
            })))
            .unwrap()
    );
    for entry in std::fs::read_dir(f.dir.path()).unwrap().flatten() {
        if entry.path().is_file() {
            let bytes = std::fs::read(entry.path()).unwrap();
            assert!(
                !bytes
                    .windows("unique-removable-report-content".len())
                    .any(|b| b == b"unique-removable-report-content")
            );
        }
    }
    until(|| {
        matches!(
            f.hub.reports_view().unwrap().intake,
            IntakeLink::Unreachable { queued: 1, .. }
        )
    })
    .await;
    intake.state.lock().unwrap().fail = false;
    let lock = f.hub.inner.reports.sending.lock().await;
    f.hub
        .write(|tx| {
            for mut row in tx.read().list::<ReportDelivery>(QUEUE)? {
                row.after = Timestamp(0);
                tx.put(QUEUE, row.delivery.key(), &row)?;
            }
            Ok(())
        })
        .unwrap();
    drop(lock);
    f.hub.reports_once().await.unwrap();
    assert!(
        intake
            .state
            .lock()
            .unwrap()
            .withdrawals
            .contains(&b.report_id)
    );
    f.close().await;
}

#[tokio::test]
async fn release_feed_shows_halted_build_and_a_fix_in_the_selected_ring() {
    let intake = Intake::new().await;
    let f = Fixture::new(Some(intake.url.clone())).await;
    let b = bundle();
    f.submit(&b).await;
    until(|| status(&f, b.report_id) == ReportProgress::Received).await;
    let now = Timestamp::now();
    let feed = ReleaseFeed {
        now,
        releases: vec![
            ReleaseEntry {
                build: "new-build".into(),
                published_at: Timestamp(now.0 + 10),
                ring: ReleaseRing::Early,
                state: ReleaseState::Rolling,
                fixes: vec![b.signature.key()],
            },
            ReleaseEntry {
                build: "running".into(),
                published_at: Timestamp(now.0 - 10),
                ring: ReleaseRing::Everyone,
                state: ReleaseState::Halted,
                fixes: vec![],
            },
        ],
    };
    intake.state.lock().unwrap().feed = Some(feed.clone());
    force_poll(&f).await;
    f.hub.reports_once().await.unwrap();
    assert_eq!(status(&f, b.report_id), ReportProgress::Received);
    let halted = f.hub.reports_view().unwrap().halted.unwrap();
    assert_eq!(halted.back_to.as_deref(), Some("previous"));
    f.command(Command::SetReportSettings {
        sending: true,
        ring: ReleaseRing::Early,
    })
    .await;
    let mut feed = feed;
    feed.now.0 += 1;
    intake.state.lock().unwrap().feed = Some(feed);
    force_poll(&f).await;
    f.hub.reports_once().await.unwrap();
    assert_eq!(
        status(&f, b.report_id),
        ReportProgress::Fixed {
            build: "new-build".into()
        }
    );
    let alerts = f.hub.open_alerts(&Caller::local_owner()).await.unwrap().0;
    assert_eq!(alerts.len(), 1);
    let id = alerts[0].attention_id;
    force_poll(&f).await;
    f.hub.reports_once().await.unwrap();
    assert_eq!(
        f.hub.open_alerts(&Caller::local_owner()).await.unwrap().0[0].attention_id,
        id
    );
    f.close().await;
}

fn task(id: TaskId, project: ProjectId) -> Task {
    serde_json::from_value(serde_json::json!({"task_id":id,"project_id":project,"kind":"coding","title":"private task name","state":"accepted","revision":1,"harness":"claude_code","model":null,"effort":null,"created_by":{"kind":"local_owner"},"created_at":1,"updated_at":1,"current_run":null,"target_id":null,"mission":null,"attention_count":0,"status_line":null})).unwrap()
}
#[tokio::test]
async fn preview_pseudonyms_and_transcript_budget_preserve_privacy() {
    let f = Fixture::new(None).await;
    let id = TaskId::new();
    let project = ProjectId::new();
    let t = task(id, project);
    f.hub
        .write(|tx| {
            tx.put("task", id, &t)?;
            for i in 0..30 {
                tx.emit(Event::TimelineAppended {
                    task_id: id,
                    item: TimelineItem {
                        seq: 0,
                        at: Timestamp(i),
                        run_id: None,
                        item: TimelineBody::AgentMessage {
                            text: format!("entry-{i}-{}", "x".repeat(10_000)),
                            partial: false,
                        },
                    },
                })?;
            }
            Ok(())
        })
        .unwrap();
    f.hub.report_trail(Trail::TaskState {
        task: id.to_string(),
        state: "working".into(),
        harness: Some(HarnessKind::ClaudeCode),
        model: Some("claude-opus-5-5".into()),
    });
    f.hub.report_trail(Trail::Error {
        error: ReportedError {
            code: "access_blocked".into(),
            message: Some("open /workspace/private failed 'quoted secret'".into()),
        },
    });
    let salt = "00112233445566778899aabbccddeeff";
    let part = f.hub.report_preview(salt, &[]).await.unwrap();
    let json = serde_json::to_string(&part).unwrap();
    assert!(json.contains(&pseudonym(salt, PseudonymKind::Task, &id.to_string())));
    for raw in [
        id.to_string(),
        project.to_string(),
        "private task name".into(),
        "/workspace/private".into(),
        "quoted secret".into(),
    ] {
        assert!(!json.contains(&raw), "{raw}");
    }
    let extra = f
        .hub
        .report_preview(salt, &[ReportExtra::TaskTranscript { task_id: id }])
        .await
        .unwrap();
    let ReportExtraPart::TaskTranscript { entries, cut, .. } = &extra.extras[0] else {
        panic!()
    };
    assert!(*cut);
    assert!(entries.len() < 30);
    assert!(entries.last().unwrap().text.starts_with("entry-29-"));
    assert_eq!(f.hub.report_preview(salt, &[]).await.unwrap(), part);
    f.close().await;
}

#[test]
fn trail_updates_keep_build_ids_whole() {
    let dir = tempfile::tempdir().unwrap();
    let mut trail = trail::Recorder::load(dir.path(), Timestamp(0));
    trail.push(
        Timestamp(0),
        Trail::Update {
            from: "4074f37458cd".into(),
            to: "/home/sam/build".into(),
            outcome: UpdateOutcome::Installed,
        },
    );
    let events = trail.preview("salt", Timestamp(0));
    let Trail::Update { from, to, .. } = &events[0].event else {
        panic!("{events:?}");
    };
    assert_eq!(from, "4074f37458cd");
    assert_eq!(to, "<path>");
}

#[test]
fn trail_evicts_by_age_and_count_and_reloads_raw_ids() {
    let dir = tempfile::tempdir().unwrap();
    let task = TaskId::new().to_string();
    let mut trail = trail::Recorder::load(dir.path(), Timestamp(0));
    trail.push(
        Timestamp(0),
        Trail::TaskState {
            task: task.clone(),
            state: "working".into(),
            harness: None,
            model: None,
        },
    );
    for at in 1..=MAX_TRAIL_EVENTS as i64 {
        trail.push(
            Timestamp(at),
            Trail::Service {
                state: ServiceState::Started,
            },
        );
    }
    assert_eq!(
        trail
            .preview("salt", Timestamp(MAX_TRAIL_EVENTS as i64))
            .len(),
        MAX_TRAIL_EVENTS
    );
    trail.push(
        Timestamp(COMPUTER_TRAIL_MS + 1000),
        Trail::TaskState {
            task: task.clone(),
            state: "failed".into(),
            harness: None,
            model: None,
        },
    );
    trail.flush(true);
    let mut loaded = trail::Recorder::load(dir.path(), Timestamp(COMPUTER_TRAIL_MS + 1001));
    let events = loaded.preview("salt", Timestamp(COMPUTER_TRAIL_MS + 1001));
    assert_eq!(events.len(), 1);
    assert!(serde_json::to_string(&events).unwrap().contains(&pseudonym(
        "salt",
        PseudonymKind::Task,
        &task
    )));
    assert!(
        std::fs::read_to_string(dir.path().join("report-trail.json"))
            .unwrap()
            .contains(&task)
    );
}

#[tokio::test]
async fn the_subscription_records_state_blocks_and_quiet_alerts_without_content() {
    let f = Fixture::new(None).await;
    let id = TaskId::new();
    let project = ProjectId::new();
    let mut task = task(id, project);
    task.access_blocked = Some(Box::new(protocol::access::AccessBlocked {
        path: Some("/workspace/private".into()),
        host: None,
        local: false,
        write: true,
        project_id: None,
        at: Timestamp::now(),
    }));
    let attention = AttentionItem {
        child_id: None,
        attention_id: AttentionId::new(),
        kind: AttentionKind::Completed,
        title: "private title".into(),
        body: Some("private body".into()),
        task_id: Some(id),
        run_id: None,
        approval_id: None,
        created_at: Timestamp::now(),
        resolved_at: None,
        priority: 30,
        topic: None,
        quiet: false,
        report_id: None,
    };
    f.hub
        .write(|tx| {
            tx.put("task", id, &task)?;
            tx.put(
                "meta",
                "notification_settings",
                &protocol::notifications::NotificationSettings {
                    off: vec![protocol::notifications::AlertTopic::Finished],
                    ..Default::default()
                },
            )?;
            tx.emit(Event::TaskUpserted { task })?;
            tx.put("attention", attention.attention_id, &attention)?;
            tx.emit(Event::AttentionUpserted { item: attention })?;
            Ok(())
        })
        .unwrap();
    until(|| {
        let events = f
            .hub
            .inner
            .reports
            .trail
            .lock()
            .unwrap()
            .preview("salt", Timestamp::now());
        events.iter().any(|e| {
            matches!(
                e.event,
                Trail::SandboxBlock {
                    kind: BlockKind::Path,
                    ..
                }
            )
        }) && events
            .iter()
            .any(|e| matches!(e.event, Trail::AlertRaised { quiet: true, .. }))
    })
    .await;
    let part = f
        .hub
        .report_preview("00112233445566778899aabbccddeeff", &[ReportExtra::Names])
        .await
        .unwrap();
    let ReportExtraPart::Names { names } = &part.extras[0] else {
        panic!()
    };
    assert!(names.iter().any(|n| n.name == "private task name"));
    let part = f
        .hub
        .report_preview("00112233445566778899aabbccddeeff", &[])
        .await
        .unwrap();
    let json = serde_json::to_string(&part).unwrap();
    for content in [
        "private task name",
        "private title",
        "private body",
        "/workspace/private",
    ] {
        assert!(!json.contains(content));
    }
    f.close().await;
}

#[tokio::test]
async fn queued_reports_settings_and_the_key_survive_a_restart() {
    let intake = Intake::new().await;
    let dir = tempfile::tempdir().unwrap();
    let config = ReportsConfig {
        intake_url: Some(intake.url.clone()),
        ..Default::default()
    };
    let hub = CompanionHub::builder()
        .data_dir(dir.path())
        .reports(config.clone())
        .build()
        .await
        .unwrap();
    let mut set = CommandEnvelope::new(Command::SetReportSettings {
        sending: false,
        ring: ReleaseRing::Early,
    });
    set.expected_revision = Some(0);
    assert_eq!(hub.submit(&Caller::local_owner(), set).await.error, None);
    let b = bundle();
    assert_eq!(
        hub.submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::SubmitReport {
                bundle: Box::new(b.clone())
            })
        )
        .await
        .error,
        None
    );
    hub.shutdown().await.unwrap();
    drop(hub);
    let hub = CompanionHub::builder()
        .data_dir(dir.path())
        .reports(config)
        .build()
        .await
        .unwrap();
    assert!(!hub.reports_view().unwrap().settings.sending);
    assert_eq!(
        hub.reports_view().unwrap().reports[0].status(),
        ReportProgress::Queued
    );
    let mut set = CommandEnvelope::new(Command::SetReportSettings {
        sending: true,
        ring: ReleaseRing::Early,
    });
    set.expected_revision = Some(1);
    assert_eq!(hub.submit(&Caller::local_owner(), set).await.error, None);
    until(|| hub.reports_view().unwrap().reports[0].status() == ReportProgress::Received).await;
    let key = std::fs::read(dir.path().join("reporter.key")).unwrap();
    hub.shutdown().await.unwrap();
    drop(hub);
    let hub = CompanionHub::builder()
        .data_dir(dir.path())
        .reports(ReportsConfig {
            intake_url: Some(intake.url.clone()),
            ..Default::default()
        })
        .build()
        .await
        .unwrap();
    hub.reports_once().await.unwrap();
    assert_eq!(std::fs::read(dir.path().join("reporter.key")).unwrap(), key);
    hub.shutdown().await.unwrap();
}

#[tokio::test]
async fn closed_questions_clear_attention_and_closed_reports_stop_polling() {
    let intake = Intake::new().await;
    let f = Fixture::new(Some(intake.url.clone())).await;
    let b = bundle();
    f.submit(&b).await;
    until(|| status(&f, b.report_id) == ReportProgress::Received).await;
    let update = ReportUpdate {
        report_id: b.report_id,
        history: vec![StatusChange {
            status: ReportProgress::Question,
            at: Timestamp(10),
        }],
        questions: vec![ReportQuestion {
            question_id: ReportQuestionId::new(),
            template: QuestionTemplate::AfterUpdate,
            asked_at: Timestamp(10),
            answer: None,
        }],
    };
    intake.state.lock().unwrap().updates = vec![update.clone()];
    force_poll(&f).await;
    f.hub.reports_once().await.unwrap();
    assert_eq!(
        f.hub
            .open_alerts(&Caller::local_owner())
            .await
            .unwrap()
            .0
            .len(),
        1
    );
    let mut closed = update;
    closed.questions.clear();
    intake.state.lock().unwrap().updates = vec![closed];
    force_poll(&f).await;
    f.hub.reports_once().await.unwrap();
    assert!(
        f.hub
            .open_alerts(&Caller::local_owner())
            .await
            .unwrap()
            .0
            .is_empty()
    );
    intake.state.lock().unwrap().updates[0]
        .history
        .push(StatusChange {
            status: ReportProgress::Shipped {
                build: "fix".into(),
            },
            at: Timestamp(12),
        });
    force_poll(&f).await;
    f.hub.reports_once().await.unwrap();
    let calls = intake.state.lock().unwrap().status_ids.len();
    force_poll(&f).await;
    f.hub.reports_once().await.unwrap();
    assert_eq!(intake.state.lock().unwrap().status_ids.len(), calls);
    f.close().await;
}

#[tokio::test]
async fn a_status_pause_survives_the_release_fetch_and_withdrawals_still_send() {
    let intake = Intake::new().await;
    let f = Fixture::new(Some(intake.url.clone())).await;
    let b = bundle();
    f.submit(&b).await;
    until(|| status(&f, b.report_id) == ReportProgress::Received).await;
    intake.state.lock().unwrap().paused = true;
    force_poll(&f).await;
    f.hub.reports_once().await.unwrap();
    assert_eq!(f.hub.reports_view().unwrap().intake, IntakeLink::Paused);
    f.command(Command::WithdrawReport {
        report_id: b.report_id,
    })
    .await;
    until(|| {
        intake
            .state
            .lock()
            .unwrap()
            .withdrawals
            .contains(&b.report_id)
    })
    .await;
    assert_eq!(f.hub.reports_view().unwrap().intake, IntakeLink::Paused);
    f.close().await;
}
