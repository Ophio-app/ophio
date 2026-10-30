// SPDX-License-Identifier: Apache-2.0
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use ed25519_dalek::{Signer, SigningKey};
use intake::{
    Intake, Store,
    client::Client,
    config::{Bucket, Config},
    maintainer::*,
};
use protocol::{ReportId, ReportQuestionId, reports::*, time::Timestamp};
use reqwest::{Method, StatusCode};
use sha2::{Digest, Sha256};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicI64, Ordering},
    time::Duration,
};

const TOKEN: &str = "synthetic-maintainer-token-for-local-tests-only";
static TIME: AtomicI64 = AtomicI64::new(0);
fn now() -> i64 {
    let wall = Timestamp::now().0;
    TIME.fetch_max(wall, Ordering::SeqCst);
    TIME.fetch_add(1, Ordering::SeqCst) + 1
}
fn key(n: u8) -> SigningKey {
    SigningKey::from_bytes(&[n; 32])
}
fn fingerprint(key: &SigningKey) -> String {
    Sha256::digest(key.verifying_key().as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn bundle() -> ReportBundle {
    ReportBundle {
        schema: REPORT_SCHEMA,
        report_id: ReportId::new(),
        created_at: Timestamp::now(),
        origin: ReportOrigin::Settings,
        words: Some("The screen stayed empty".into()),
        signature: ReportSignature {
            code: "empty".into(),
            screen: "/work/:task".into(),
            build: "test-1".into(),
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
fn generous() -> Config {
    Config {
        reports_per_key: Bucket {
            capacity: 1000,
            refill_seconds: 1,
        },
        reports_per_ip: Bucket {
            capacity: 1000,
            refill_seconds: 1,
        },
        requests_per_key: Bucket {
            capacity: 1000,
            refill_seconds: 1,
        },
        requests_per_ip: Bucket {
            capacity: 1000,
            refill_seconds: 1,
        },
        ..Default::default()
    }
}
struct Server {
    _dir: tempfile::TempDir,
    db: PathBuf,
    public: String,
    private: String,
    http: reqwest::Client,
    admin: Client,
    intake: Intake,
    task: tokio::task::JoinHandle<intake::Result<()>>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Server {
    async fn start(config: Config) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("intake.sqlite");
        let intake = Intake::new(Store::open(&db).unwrap(), config, TOKEN).unwrap();
        let reporter = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let maintainer = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let public = format!("http://{}", reporter.local_addr().unwrap());
        let private = format!("http://{}", maintainer.local_addr().unwrap());
        let task = tokio::spawn(intake.clone().serve(reporter, maintainer));
        Self {
            _dir: dir,
            db,
            public,
            admin: Client::new(&private, TOKEN).unwrap(),
            private,
            http: reqwest::Client::builder().no_proxy().build().unwrap(),
            intake,
            task,
        }
    }
    fn signed_at(
        &self,
        key: &SigningKey,
        method: Method,
        path: &str,
        body: Vec<u8>,
        time: i64,
    ) -> reqwest::Request {
        let signature = key.sign(&signing_payload(method.as_str(), path, time, &body));
        self.http
            .request(method, format!("{}{path}", self.public))
            .header(
                KEY_HEADER,
                URL_SAFE_NO_PAD.encode(key.verifying_key().as_bytes()),
            )
            .header(TIME_HEADER, time)
            .header(
                SIGNATURE_HEADER,
                URL_SAFE_NO_PAD.encode(signature.to_bytes()),
            )
            .header("content-type", "application/json")
            .body(body)
            .build()
            .unwrap()
    }
    async fn send<T: serde::Serialize>(
        &self,
        key: &SigningKey,
        path: &str,
        body: &T,
    ) -> reqwest::Response {
        self.http
            .execute(self.signed_at(
                key,
                Method::POST,
                path,
                serde_json::to_vec(body).unwrap(),
                now(),
            ))
            .await
            .unwrap()
    }
    async fn submit(&self, key: &SigningKey, bundle: ReportBundle) -> IntakeReceipt {
        let response = self
            .send(
                key,
                PATH_SUBMIT,
                &IntakeSubmission {
                    bundle,
                    sender: ReportSide::Phone,
                },
            )
            .await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{}",
            response.text().await.unwrap()
        );
        response.json().await.unwrap()
    }
    async fn status(
        &self,
        key: &SigningKey,
        ids: Vec<ReportId>,
        since: Option<Timestamp>,
    ) -> StatusResponse {
        self.send(
            key,
            PATH_STATUS,
            &StatusRequest {
                report_ids: ids,
                since,
            },
        )
        .await
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap()
    }
}

#[tokio::test]
async fn reporter_happy_path_and_withdrawal_delete_content() {
    let server = Server::start(generous()).await;
    let key = key(1);
    let b = bundle();
    let id = b.report_id;
    let receipt = server.submit(&key, b.clone()).await;
    assert_eq!(receipt.reference, report_reference(&id));
    assert_eq!(receipt.status, ReportProgress::Received);
    let status = server.status(&key, vec![id], None).await;
    assert_eq!(status.reports.len(), 1);
    assert!(!status.paused);
    assert!(
        server
            .status(&key, vec![id], Some(Timestamp(i64::MAX)))
            .await
            .reports
            .is_empty()
    );
    let q = server
        .admin
        .ask(id, QuestionTemplate::HappenedAgain)
        .await
        .unwrap();
    let answer = IntakeAnswer {
        report_id: id,
        question_id: q.question_id,
        answer: ReportAnswer {
            choice: QuestionChoice::No,
            words: Some("é".repeat(MAX_REPORT_WORDS + 50)),
            at: Timestamp(0),
        },
    };
    let response = server.send(&key, PATH_ANSWER, &answer).await;
    assert_eq!(response.status(), StatusCode::OK);
    let record = server.admin.report(id).await.unwrap();
    assert_eq!(
        record.questions[0]
            .answer
            .as_ref()
            .unwrap()
            .words
            .as_ref()
            .unwrap()
            .chars()
            .count(),
        MAX_REPORT_WORDS
    );
    assert!(record.questions[0].answer.as_ref().unwrap().at.0 > 0);
    let feed = server
        .http
        .execute(server.signed_at(&key, Method::GET, PATH_RELEASES, vec![], now()))
        .await
        .unwrap();
    assert_eq!(feed.status(), StatusCode::OK);
    assert!(
        feed.json::<ReleaseFeed>()
            .await
            .unwrap()
            .releases
            .is_empty()
    );
    assert_eq!(
        server
            .send(&key, PATH_WITHDRAW, &WithdrawRequest { report_id: id })
            .await
            .status(),
        StatusCode::OK
    );
    let record = server.admin.report(id).await.unwrap();
    assert!(record.bundle.is_none());
    assert!(record.questions.is_empty());
    assert!(record.signature.code.is_empty());
    assert_eq!(
        record.history.last().unwrap().status,
        ReportProgress::Withdrawn
    );
    assert_eq!(
        server.submit(&key, b).await.status,
        ReportProgress::Withdrawn
    );
    assert!(server.admin.report(id).await.unwrap().bundle.is_none());
}

#[tokio::test]
async fn resubmission_keeps_first_bundle_and_refuses_other_key() {
    let server = Server::start(generous()).await;
    let b = bundle();
    let id = b.report_id;
    let first = server.submit(&key(2), b.clone()).await;
    let mut changed = b.clone();
    changed.words = Some("changed".into());
    assert_eq!(server.submit(&key(2), changed).await, first);
    assert_eq!(
        server.admin.report(id).await.unwrap().bundle,
        Some(b.clone())
    );
    assert_eq!(
        server
            .send(
                &key(3),
                PATH_SUBMIT,
                &IntakeSubmission {
                    bundle: b,
                    sender: ReportSide::Computer
                }
            )
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    assert!(
        server
            .status(&key(3), vec![id], None)
            .await
            .reports
            .is_empty()
    );
    assert_eq!(
        server
            .send(&key(3), PATH_WITHDRAW, &WithdrawRequest { report_id: id })
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn authentication_rejects_unsigned_bad_key_signature_skew_and_replay() {
    let server = Server::start(generous()).await;
    let body = serde_json::to_vec(&StatusRequest {
        report_ids: vec![],
        since: None,
    })
    .unwrap();
    let unsigned = server
        .http
        .post(format!("{}{PATH_STATUS}", server.public))
        .body(body.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(unsigned.status(), StatusCode::UNAUTHORIZED);
    let generic = unsigned.text().await.unwrap();
    let k = key(4);
    for time in [
        now() - MAX_CLOCK_SKEW_MS - 1000,
        now() + MAX_CLOCK_SKEW_MS + 1000,
        i64::MIN,
        i64::MAX,
    ] {
        let response = server
            .http
            .execute(server.signed_at(&k, Method::POST, PATH_STATUS, body.clone(), time))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(response.text().await.unwrap(), generic);
    }
    let mut request = server.signed_at(&k, Method::POST, PATH_STATUS, body.clone(), now());
    request.headers_mut().insert(
        KEY_HEADER,
        URL_SAFE_NO_PAD
            .encode(key(5).verifying_key().as_bytes())
            .parse()
            .unwrap(),
    );
    assert_eq!(
        server.http.execute(request).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    let mut request = server.signed_at(&k, Method::POST, PATH_STATUS, body.clone(), now());
    request
        .headers_mut()
        .insert(SIGNATURE_HEADER, "invalid".parse().unwrap());
    assert_eq!(
        server.http.execute(request).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    let mut request = server.signed_at(&k, Method::POST, PATH_STATUS, body.clone(), now());
    *request.body_mut() = Some(b"changed".to_vec().into());
    assert_eq!(
        server.http.execute(request).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    let mut request = server.signed_at(&k, Method::POST, PATH_STATUS, body.clone(), now());
    request
        .headers_mut()
        .append(TIME_HEADER, "1".parse().unwrap());
    assert_eq!(
        server.http.execute(request).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    let time = now() + MAX_CLOCK_SKEW_MS - 2000;
    let request = server.signed_at(&k, Method::POST, PATH_STATUS, body, time);
    let replay = request.try_clone().unwrap();
    assert_eq!(
        server.http.execute(request).await.unwrap().status(),
        StatusCode::OK
    );
    assert_eq!(
        server.http.execute(replay).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    let db = rusqlite::Connection::open(&server.db).unwrap();
    assert_eq!(
        db.query_row("SELECT max(expires) FROM replays", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        time + MAX_CLOCK_SKEW_MS
    );
}

#[tokio::test]
async fn body_limits_and_status_count_are_enforced() {
    let server = Server::start(generous()).await;
    for (path, size) in [
        (PATH_SUBMIT, MAX_REPORT_BYTES + 4097),
        (PATH_STATUS, 32 * 1024 + 1),
        (PATH_ANSWER, 32 * 1024 + 1),
        (PATH_WITHDRAW, 32 * 1024 + 1),
        (PATH_RELEASES, 32 * 1024 + 1),
    ] {
        let method = if path == PATH_RELEASES {
            Method::GET
        } else {
            Method::POST
        };
        let request = server.signed_at(&key(6), method, path, vec![b' '; size], now());
        assert_eq!(
            server.http.execute(request).await.unwrap().status(),
            StatusCode::PAYLOAD_TOO_LARGE
        );
    }
    assert_eq!(
        server
            .send(
                &key(6),
                PATH_STATUS,
                &StatusRequest {
                    report_ids: vec![ReportId::new(); MAX_STATUS_IDS + 1],
                    since: None
                }
            )
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn invalid_bundles_are_rejected_beyond_serde() {
    let server = Server::start(generous()).await;
    let mut cases = vec![];
    let mut b = bundle();
    b.schema += 1;
    cases.push(b);
    let mut b = bundle();
    b.words = Some("x".repeat(MAX_REPORT_WORDS + 1));
    cases.push(b);
    let mut b = bundle();
    b.signature.code = "a\nb".into();
    cases.push(b);
    let mut b = bundle();
    b.context.model = Some("x".repeat(MAX_REPORT_CODE + 1));
    cases.push(b);
    let event = TrailEvent {
        at: Timestamp(0),
        event: Trail::Route { route: "/".into() },
    };
    let mut b = bundle();
    b.phone_trail = vec![event.clone(); MAX_TRAIL_EVENTS + 1];
    cases.push(b);
    let mut b = bundle();
    b.computer_trail = vec![event; MAX_TRAIL_EVENTS + 1];
    cases.push(b);
    let mut b = bundle();
    b.crashes.push(CrashRecord {
        at: Timestamp(0),
        side: ReportSide::Phone,
        kind: CrashKind::Panic,
        code: "panic".into(),
        build: "test".into(),
        message: None,
        frames: vec![
            StackFrame {
                function: None,
                file: "lib.rs".into(),
                line: 1,
                column: None
            };
            MAX_FRAMES + 1
        ],
    });
    cases.push(b);
    for (media, data, width, height) in [
        ("image/png", b"invalid".to_vec(), 1, 1),
        ("image/jpeg", png(1, 1), 1, 1),
        ("image/png", png(1, 1), 2, 1),
        ("image/png", png(0, 1), 0, 1),
        ("image/png", png(9000, 1), 9000, 1),
        ("image/png", png(8192, 8192), 8192, 8192),
        ("image/png", vec![0; MAX_SCREENSHOT_BYTES + 1], 1, 1),
    ] {
        let mut b = bundle();
        b.screenshot = Some(ReportScreenshot {
            media_type: media.into(),
            data: STANDARD.encode(data),
            width,
            height,
            marked: false,
        });
        cases.push(b);
    }
    for b in cases {
        assert_eq!(
            server
                .send(
                    &key(7),
                    PATH_SUBMIT,
                    &IntakeSubmission {
                        bundle: b,
                        sender: ReportSide::Phone
                    }
                )
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
    }
    let mut valid = bundle();
    valid.screenshot = Some(ReportScreenshot {
        media_type: "image/png".into(),
        data: STANDARD.encode(png(10, 20)),
        width: 10,
        height: 20,
        marked: true,
    });
    server.submit(&key(7), valid).await;
}
fn png(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
    bytes.extend_from_slice(&width.to_be_bytes());
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes
}

#[tokio::test]
async fn answers_must_belong_to_an_open_question_and_template() {
    let server = Server::start(generous()).await;
    let b = bundle();
    let id = b.report_id;
    server.submit(&key(8), b).await;
    let mut answer = IntakeAnswer {
        report_id: id,
        question_id: ReportQuestionId::new(),
        answer: ReportAnswer {
            choice: QuestionChoice::Yes,
            words: None,
            at: Timestamp(0),
        },
    };
    assert_eq!(
        server.send(&key(8), PATH_ANSWER, &answer).await.status(),
        StatusCode::NOT_FOUND
    );
    let q = server
        .admin
        .ask(
            id,
            QuestionTemplate::MayWeSee {
                part: ReportExtraKind::Names,
            },
        )
        .await
        .unwrap();
    answer.question_id = q.question_id;
    assert_eq!(
        server.send(&key(8), PATH_ANSWER, &answer).await.status(),
        StatusCode::BAD_REQUEST
    );
    answer.answer.choice = QuestionChoice::Allow;
    assert_eq!(
        server.send(&key(9), PATH_ANSWER, &answer).await.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        server.send(&key(8), PATH_ANSWER, &answer).await.status(),
        StatusCode::OK
    );
    assert_eq!(
        server.send(&key(8), PATH_ANSWER, &answer).await.status(),
        StatusCode::NOT_FOUND
    );
    let q = server
        .admin
        .ask(id, QuestionTemplate::AfterUpdate)
        .await
        .unwrap();
    server
        .admin
        .close_question(id, q.question_id)
        .await
        .unwrap();
    answer.question_id = q.question_id;
    assert_eq!(
        server.send(&key(8), PATH_ANSWER, &answer).await.status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn known_issues_match_signature_and_affected_builds() {
    let server = Server::start(generous()).await;
    let b = bundle();
    server
        .admin
        .put_known(KnownIssue {
            signature: b.signature.key(),
            affected_builds: vec!["test-1".into()],
            fixed_in: Some("test-2".into()),
        })
        .await
        .unwrap();
    assert_eq!(
        server.submit(&key(10), b).await.status,
        ReportProgress::Known {
            fixed_in: Some("test-2".into())
        }
    );
    let mut b = bundle();
    b.signature.build = "unaffected".into();
    assert_eq!(
        server.submit(&key(10), b).await.status,
        ReportProgress::Received
    );
    assert_eq!(server.admin.known_issues().await.unwrap().len(), 1);
    server
        .admin
        .delete_known(&bundle().signature.key())
        .await
        .unwrap();
    assert!(server.admin.known_issues().await.unwrap().is_empty());
}

#[tokio::test]
async fn duplicates_form_a_group_visible_to_maintainers() {
    let server = Server::start(generous()).await;
    let a = bundle();
    let aid = a.report_id;
    let b = bundle();
    let bid = b.report_id;
    server.submit(&key(11), a).await;
    server.submit(&key(12), b).await;
    let a = server.admin.report(aid).await.unwrap();
    let b = server.admin.report(bid).await.unwrap();
    assert_eq!(b.duplicate_of, Some(aid));
    assert_eq!(b.group_id, aid);
    assert_eq!(a.group_count, 2);
    assert_eq!(b.group_count, 2);
    assert!(
        server
            .status(&key(11), vec![aid, bid], None)
            .await
            .reports
            .iter()
            .all(|r| r.report_id == aid)
    );
}

#[tokio::test]
async fn reports_on_no_error_stay_apart() {
    let server = Server::start(generous()).await;
    let mut a = bundle();
    a.signature.code = NO_ERROR_CODE.into();
    let aid = a.report_id;
    let mut b = bundle();
    b.signature.code = NO_ERROR_CODE.into();
    b.words = Some("The Speak button covers the box".into());
    let bid = b.report_id;
    server.submit(&key(11), a).await;
    server.submit(&key(12), b).await;
    let b = server.admin.report(bid).await.unwrap();
    assert_eq!(b.duplicate_of, None);
    assert_eq!(b.group_id, bid);
    assert_eq!(b.group_count, 1);
    assert_eq!(server.admin.report(aid).await.unwrap().group_count, 1);
}

#[tokio::test]
async fn release_fixes_update_open_reports_as_rings_widen() {
    let server = Server::start(generous()).await;
    let a = bundle();
    let id = a.report_id;
    let signature = a.signature.key();
    server.submit(&key(13), a).await;
    server
        .admin
        .publish(ReleaseEntry {
            build: "test-2".into(),
            published_at: Timestamp::now(),
            ring: ReleaseRing::Owner,
            state: ReleaseState::Rolling,
            fixes: vec![signature],
        })
        .await
        .unwrap();
    assert_eq!(
        server
            .admin
            .report(id)
            .await
            .unwrap()
            .history
            .last()
            .unwrap()
            .status,
        ReportProgress::Fixed {
            build: "test-2".into()
        }
    );
    server
        .admin
        .widen("test-2", ReleaseRing::Early)
        .await
        .unwrap();
    assert!(
        server
            .admin
            .widen("test-2", ReleaseRing::Owner)
            .await
            .is_err()
    );
    server.admin.complete("test-2").await.unwrap();
    assert_eq!(
        server
            .admin
            .report(id)
            .await
            .unwrap()
            .history
            .last()
            .unwrap()
            .status,
        ReportProgress::Shipped {
            build: "test-2".into()
        }
    );
    let feed = server
        .http
        .execute(server.signed_at(&key(13), Method::GET, PATH_RELEASES, vec![], now()))
        .await
        .unwrap()
        .json::<ReleaseFeed>()
        .await
        .unwrap();
    assert_eq!(feed.releases[0].state, ReleaseState::Complete);
    assert_eq!(feed.releases[0].ring, ReleaseRing::Everyone);
    server.admin.halt("test-2").await.unwrap();
    assert_eq!(
        server.admin.releases().await.unwrap().releases[0].state,
        ReleaseState::Halted
    );
    assert!(server.admin.complete("test-2").await.is_err());
}

#[tokio::test]
async fn switches_are_separate_and_bans_are_private() {
    let server = Server::start(generous()).await;
    let b = bundle();
    let id = b.report_id;
    server.submit(&key(14), b).await;
    server.admin.admit(id).await.unwrap();
    server.admin.pipeline_paused(true).await.unwrap();
    assert!(
        server
            .admin
            .claim("test-worker", 1000)
            .await
            .unwrap()
            .is_none()
    );
    assert!(!server.status(&key(14), vec![id], None).await.paused);
    server.submit(&key(14), bundle()).await;
    server.admin.intake_closed(true).await.unwrap();
    assert!(server.status(&key(14), vec![id], None).await.paused);
    let closed = server
        .send(
            &key(14),
            PATH_SUBMIT,
            &IntakeSubmission {
                bundle: bundle(),
                sender: ReportSide::Phone,
            },
        )
        .await;
    assert_eq!(closed.status(), StatusCode::SERVICE_UNAVAILABLE);
    let paused: StatusResponse = closed.json().await.unwrap();
    assert!(paused.paused);
    assert!(paused.reports.is_empty());
    assert!(paused.now.0 > 0);
    server
        .admin
        .ban(&fingerprint(&key(14)), true)
        .await
        .unwrap();
    assert_eq!(
        server
            .send(
                &key(14),
                PATH_STATUS,
                &StatusRequest {
                    report_ids: vec![id],
                    since: None
                }
            )
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    server
        .admin
        .ban(&fingerprint(&key(14)), false)
        .await
        .unwrap();
    server.admin.intake_closed(false).await.unwrap();
    server.admin.pipeline_paused(false).await.unwrap();
    assert!(
        server
            .admin
            .claim("test-worker", 1000)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn per_key_and_per_ip_buckets_return_retry_after() {
    for per_key in [true, false] {
        let mut config = generous();
        let bucket = Bucket {
            capacity: 1,
            refill_seconds: 3600,
        };
        if per_key {
            config.reports_per_key = bucket;
        } else {
            config.reports_per_ip = bucket;
        }
        let server = Server::start(config).await;
        server.submit(&key(15), bundle()).await;
        let k = key(if per_key { 15 } else { 16 });
        let response = server
            .send(
                &k,
                PATH_SUBMIT,
                &IntakeSubmission {
                    bundle: bundle(),
                    sender: ReportSide::Phone,
                },
            )
            .await;
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(
            response.headers()["retry-after"]
                .to_str()
                .unwrap()
                .parse::<u64>()
                .unwrap()
                > 0
        );
    }
}

#[tokio::test]
async fn request_rate_limit_applies_to_status_too() {
    let config = Config {
        requests_per_key: Bucket {
            capacity: 1,
            refill_seconds: 3600,
        },
        ..generous()
    };
    let server = Server::start(config).await;
    server.status(&key(17), vec![], None).await;
    let response = server
        .send(
            &key(17),
            PATH_STATUS,
            &StatusRequest {
                report_ids: vec![],
                since: None,
            },
        )
        .await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(response.headers().contains_key("retry-after"));
}

#[tokio::test]
async fn untrusted_forwarding_headers_cannot_change_ip_limits() {
    let config = Config {
        reports_per_ip: Bucket {
            capacity: 1,
            refill_seconds: 3600,
        },
        ..generous()
    };
    let server = Server::start(config).await;
    server.submit(&key(18), bundle()).await;
    let mut request = server.signed_at(
        &key(19),
        Method::POST,
        PATH_SUBMIT,
        serde_json::to_vec(&IntakeSubmission {
            bundle: bundle(),
            sender: ReportSide::Phone,
        })
        .unwrap(),
        now(),
    );
    request
        .headers_mut()
        .insert("x-forwarded-for", "192.0.2.2".parse().unwrap());
    assert_eq!(
        server.http.execute(request).await.unwrap().status(),
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn trusted_proxy_accepts_only_one_valid_forwarded_ip() {
    let config = Config {
        trusted_proxy: Some("127.0.0.1".parse().unwrap()),
        reports_per_ip: Bucket {
            capacity: 1,
            refill_seconds: 3600,
        },
        ..generous()
    };
    let server = Server::start(config).await;
    for (ip, wanted) in [
        ("192.0.2.1", StatusCode::OK),
        ("192.0.2.2", StatusCode::OK),
        ("192.0.2.1, 192.0.2.3", StatusCode::BAD_REQUEST),
    ] {
        let mut request = server.signed_at(
            &key(20),
            Method::POST,
            PATH_SUBMIT,
            serde_json::to_vec(&IntakeSubmission {
                bundle: bundle(),
                sender: ReportSide::Phone,
            })
            .unwrap(),
            now(),
        );
        request
            .headers_mut()
            .insert("x-forwarded-for", ip.parse().unwrap());
        assert_eq!(server.http.execute(request).await.unwrap().status(), wanted);
    }
}

#[tokio::test]
async fn maintainer_auth_is_required_and_listener_is_separate() {
    let server = Server::start(generous()).await;
    for auth in [None, Some("Bearer synthetic-wrong-token")] {
        let mut request = server.http.get(format!("{}{PATH_STATE}", server.private));
        if let Some(auth) = auth {
            request = request.header("authorization", auth);
        }
        let response = request.send().await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(
            !response
                .headers()
                .contains_key("access-control-allow-origin")
        );
    }
    assert!(server.admin.switches().await.is_ok());
    assert_eq!(
        server
            .http
            .get(format!("{}{PATH_STATE}", server.public))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        server
            .http
            .post(format!("{}{PATH_SUBMIT}", server.private))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn leases_expire_and_every_successful_write_is_audited() {
    let server = Server::start(generous()).await;
    let b = bundle();
    let id = b.report_id;
    server.submit(&key(21), b).await;
    server.admin.admit(id).await.unwrap();
    let claimed = server
        .admin
        .claim("worker-one", 250)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.report_id, id);
    assert!(claimed.bundle.is_some());
    assert!(
        server
            .admin
            .claim("worker-two", 1000)
            .await
            .unwrap()
            .is_none()
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        server
            .admin
            .claim("worker-two", 1000)
            .await
            .unwrap()
            .unwrap()
            .lease
            .unwrap()
            .worker,
        "worker-two"
    );
    server
        .admin
        .set_status(id, ReportProgress::Reproduced)
        .await
        .unwrap();
    assert_eq!(server.admin.counters().await.unwrap().audit_rows, 5);
    assert_eq!(
        server.admin.counters().await.unwrap().reports_per_key[&fingerprint(&key(21))],
        1
    );
    assert_eq!(
        server
            .admin
            .list(&ReportFilter {
                status: Some(ReportProgress::Reproduced),
                ..Default::default()
            })
            .await
            .unwrap()
            .reports
            .len(),
        1
    );
    assert!(
        server
            .admin
            .list(&ReportFilter {
                signature: Some("missing@/".into()),
                ..Default::default()
            })
            .await
            .unwrap()
            .reports
            .is_empty()
    );
}

#[tokio::test]
async fn the_fixer_claims_only_what_the_owner_chose() {
    let server = Server::start(generous()).await;
    let first = server.submit(&key(93), bundle()).await.report_id;
    let second = server.submit(&key(94), bundle()).await.report_id;
    assert!(
        server
            .admin
            .claim("worker-a", 30000)
            .await
            .unwrap()
            .is_none()
    );
    server.admin.admit(second).await.unwrap();
    server.admin.admit(second).await.unwrap();
    let claimed = server
        .admin
        .claim("worker-a", 30000)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.report_id, second);
    assert!(
        server
            .admin
            .claim_next("worker-b", 30000, vec![second])
            .await
            .unwrap()
            .is_none()
    );
    server
        .admin
        .set_status(first, ReportProgress::Withdrawn)
        .await
        .unwrap();
    assert!(server.admin.admit(first).await.is_err());
    assert!(server.admin.admit(ReportId::new()).await.is_err());
}

#[tokio::test]
async fn worker_renews_only_its_live_lease_and_skips_completed_local_records() {
    let server = Server::start(generous()).await;
    let one = bundle();
    let id = one.report_id;
    server.submit(&key(91), one).await;
    server.admin.admit(id).await.unwrap();
    server
        .admin
        .claim("worker-a", 30000)
        .await
        .unwrap()
        .unwrap();
    assert!(server.admin.renew(id, "worker-b", 30000).await.is_err());
    server.admin.renew(id, "worker-a", 1).await.unwrap();
    tokio::time::sleep(Duration::from_millis(3)).await;
    assert!(server.admin.renew(id, "worker-a", 30000).await.is_err());
    assert!(
        server
            .admin
            .claim_next("worker-b", 30000, vec![id])
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        server
            .admin
            .claim_next("worker-b", 30000, vec![])
            .await
            .unwrap()
            .unwrap()
            .report_id,
        id
    );
}

#[tokio::test]
async fn cors_preflights_exist_only_on_reporter_paths() {
    let server = Server::start(generous()).await;
    for path in [
        PATH_SUBMIT,
        PATH_STATUS,
        PATH_ANSWER,
        PATH_WITHDRAW,
        PATH_RELEASES,
    ] {
        let response = server
            .http
            .request(Method::OPTIONS, format!("{}{path}", server.public))
            .header("origin", "https://phone.example.invalid")
            .header("access-control-request-method", "POST")
            .header(
                "access-control-request-headers",
                format!("content-type,{KEY_HEADER},{TIME_HEADER},{SIGNATURE_HEADER}"),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(response.headers()["access-control-allow-origin"], "*");
        assert!(
            !response
                .headers()
                .contains_key("access-control-allow-credentials")
        );
        assert!(
            response.headers()["access-control-allow-headers"]
                .to_str()
                .unwrap()
                .contains(SIGNATURE_HEADER)
        );
    }
    let response = server
        .http
        .request(Method::OPTIONS, format!("{}{PATH_COMMAND}", server.private))
        .header("origin", "https://phone.example.invalid")
        .send()
        .await
        .unwrap();
    assert!(
        !response
            .headers()
            .contains_key("access-control-allow-origin")
    );
}

#[tokio::test]
async fn retention_removes_closed_bundles_and_answer_words_only_after_deadline() {
    let server = Server::start(generous()).await;
    let b = bundle();
    let id = b.report_id;
    server.submit(&key(22), b).await;
    let q = server
        .admin
        .ask(id, QuestionTemplate::HappenedAgain)
        .await
        .unwrap();
    assert_eq!(
        server
            .send(
                &key(22),
                PATH_ANSWER,
                &IntakeAnswer {
                    report_id: id,
                    question_id: q.question_id,
                    answer: ReportAnswer {
                        choice: QuestionChoice::Yes,
                        words: Some("test-private-answer".into()),
                        at: Timestamp(0)
                    }
                }
            )
            .await
            .status(),
        StatusCode::OK
    );
    server
        .admin
        .set_status(
            id,
            ReportProgress::Shipped {
                build: "test-2".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(server.intake.sweep().await.unwrap(), 0);
    let mut store = Store::open(&server.db).unwrap();
    assert_eq!(
        store
            .retention_sweep(Timestamp(Timestamp::now().0 + 91 * 86_400_000), 90)
            .unwrap(),
        1
    );
    let report = server.admin.report(id).await.unwrap();
    assert!(report.bundle.is_none());
    assert!(report.questions[0].answer.as_ref().unwrap().words.is_none());
    assert!(matches!(
        report.history.last().unwrap().status,
        ReportProgress::Shipped { .. }
    ));
    assert_eq!(
        server.status(&key(22), vec![id], None).await.reports.len(),
        1
    );
}

#[tokio::test]
async fn pagination_and_since_do_not_expose_bundles() {
    let server = Server::start(generous()).await;
    server.submit(&key(23), bundle()).await;
    server.submit(&key(23), bundle()).await;
    let first = server
        .admin
        .list(&ReportFilter {
            limit: Some(1),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(first.reports.len(), 1);
    assert!(first.reports[0].bundle.is_none());
    let next = server
        .admin
        .list(&ReportFilter {
            after: first.next,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(next.reports.len(), 1);
    assert_ne!(first.reports[0].report_id, next.reports[0].report_id);
    assert!(next.next.is_none());
    assert!(
        server
            .admin
            .list(&ReportFilter {
                since: Some(Timestamp(i64::MAX)),
                ..Default::default()
            })
            .await
            .unwrap()
            .reports
            .is_empty()
    );
}

#[tokio::test]
async fn concurrent_claims_grant_a_single_lease() {
    let server = Server::start(generous()).await;
    let b = bundle();
    let id = b.report_id;
    server.submit(&key(24), b).await;
    server.admin.admit(id).await.unwrap();
    let (a, b) = tokio::join!(
        server.admin.claim("worker-a", 30000),
        server.admin.claim("worker-b", 30000)
    );
    let claims: Vec<_> = [a.unwrap(), b.unwrap()].into_iter().flatten().collect();
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].report_id, id);
}

#[tokio::test]
async fn retention_preserves_open_reports_and_withdrawn_reports_cannot_reopen() {
    let server = Server::start(generous()).await;
    let b = bundle();
    let id = b.report_id;
    server.submit(&key(25), b).await;
    let mut store = Store::open(&server.db).unwrap();
    assert_eq!(store.retention_sweep(Timestamp(i64::MAX), 0).unwrap(), 0);
    assert!(server.admin.report(id).await.unwrap().bundle.is_some());
    server
        .admin
        .set_status(id, ReportProgress::Withdrawn)
        .await
        .unwrap();
    assert!(server.admin.report(id).await.unwrap().bundle.is_none());
    assert!(
        server
            .admin
            .set_status(id, ReportProgress::Received)
            .await
            .is_err()
    );
    assert!(
        server
            .admin
            .ask(id, QuestionTemplate::HappenedAgain)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn malformed_json_and_unknown_status_never_reach_storage() {
    let server = Server::start(generous()).await;
    let request = server.signed_at(
        &key(26),
        Method::POST,
        PATH_SUBMIT,
        b"not json".to_vec(),
        now(),
    );
    assert_eq!(
        server.http.execute(request).await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
    let response = server.http.post(format!("{}{PATH_COMMAND}",server.private)).bearer_auth(TOKEN).json(&serde_json::json!({"action":"set_status","report_id":ReportId::new(),"status":{"state":"arbitrary text"}})).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(server.admin.counters().await.unwrap().audit_rows, 0);
}

#[tokio::test]
async fn duplicates_link_to_an_open_member_after_the_first_is_withdrawn() {
    let server = Server::start(generous()).await;
    let a = bundle();
    let aid = a.report_id;
    let b = bundle();
    let bid = b.report_id;
    server.submit(&key(27), a).await;
    server.submit(&key(27), b).await;
    assert_eq!(
        server
            .send(&key(27), PATH_WITHDRAW, &WithdrawRequest { report_id: aid })
            .await
            .status(),
        StatusCode::OK
    );
    let c = bundle();
    let cid = c.report_id;
    server.submit(&key(27), c).await;
    let report = server.admin.report(cid).await.unwrap();
    assert_eq!(report.duplicate_of, Some(bid));
    assert_eq!(report.group_id, aid);
}

#[tokio::test]
async fn reopened_reports_still_lose_new_answer_words_on_retention() {
    let server = Server::start(generous()).await;
    let b = bundle();
    let id = b.report_id;
    server.submit(&key(28), b).await;
    server
        .admin
        .set_status(
            id,
            ReportProgress::Shipped {
                build: "test-2".into(),
            },
        )
        .await
        .unwrap();
    let mut store = Store::open(&server.db).unwrap();
    assert_eq!(store.retention_sweep(Timestamp(i64::MAX), 90).unwrap(), 1);
    server
        .admin
        .set_status(id, ReportProgress::Received)
        .await
        .unwrap();
    let q = server
        .admin
        .ask(id, QuestionTemplate::AfterUpdate)
        .await
        .unwrap();
    assert_eq!(
        server
            .send(
                &key(28),
                PATH_ANSWER,
                &IntakeAnswer {
                    report_id: id,
                    question_id: q.question_id,
                    answer: ReportAnswer {
                        choice: QuestionChoice::Yes,
                        words: Some("synthetic follow-up".into()),
                        at: Timestamp(0)
                    }
                }
            )
            .await
            .status(),
        StatusCode::OK
    );
    server
        .admin
        .set_status(
            id,
            ReportProgress::Shipped {
                build: "test-3".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(store.retention_sweep(Timestamp(i64::MAX), 90).unwrap(), 1);
    assert!(
        server.admin.report(id).await.unwrap().questions[0]
            .answer
            .as_ref()
            .unwrap()
            .words
            .is_none()
    );
    let before = server.admin.counters().await.unwrap().audit_rows;
    assert_eq!(server.admin.sweep(90).await.unwrap(), 0);
    assert_eq!(
        server.admin.counters().await.unwrap().audit_rows,
        before + 1
    );
}

#[tokio::test]
async fn completed_claims_survive_reopen_and_expired_unfinished_leases_are_reoffered() {
    let server = Server::start(generous()).await;
    let id = server.submit(&key(92), bundle()).await.report_id;
    server.admin.admit(id).await.unwrap();
    server
        .admin
        .claim("worker-a", 30000)
        .await
        .unwrap()
        .unwrap();
    assert!(
        server
            .admin
            .claim("worker-b", 30000)
            .await
            .unwrap()
            .is_none()
    );
    let q = server
        .admin
        .ask(id, QuestionTemplate::HappenedAgain)
        .await
        .unwrap();
    assert!(server.admin.finish_report(id, "worker-b").await.is_err());
    server.admin.finish_report(id, "worker-a").await.unwrap();
    server.admin.finish_report(id, "worker-a").await.unwrap();
    assert!(server.admin.report(id).await.unwrap().questions.is_empty());
    assert!(
        server
            .admin
            .answer_question(
                id,
                q.question_id,
                ReportAnswer {
                    choice: QuestionChoice::Yes,
                    words: None,
                    at: Timestamp::now(),
                }
            )
            .await
            .is_err()
    );
    let next = server.submit(&key(92), bundle()).await.report_id;
    server.admin.admit(next).await.unwrap();
    // Finished, it can't be chosen again.
    assert!(server.admin.admit(id).await.is_err());
    let mut reopened = Store::open(&server.db).unwrap();
    let future = Timestamp(Timestamp::now().0 + 60000);
    match reopened
        .command(
            Command::Claim {
                worker: "worker-b".into(),
                lease_ms: 1000,
            },
            future,
        )
        .unwrap()
    {
        CommandResult::Claimed { report: Some(r) } => assert_eq!(r.report_id, next),
        other => panic!("{other:?}"),
    }
    match reopened
        .command(
            Command::Claim {
                worker: "worker-c".into(),
                lease_ms: 1000,
            },
            Timestamp(future.0 + 1000),
        )
        .unwrap()
    {
        CommandResult::Claimed { report: Some(r) } => assert_eq!(r.report_id, next),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn maintainer_answers_use_reporter_choice_validation_and_text_bounds() {
    let server = Server::start(generous()).await;
    let id = server.submit(&key(93), bundle()).await.report_id;
    let q = server
        .admin
        .ask(id, QuestionTemplate::HappenedAgain)
        .await
        .unwrap();
    assert!(
        server
            .admin
            .answer_question(
                id,
                q.question_id,
                ReportAnswer {
                    choice: QuestionChoice::Allow,
                    words: None,
                    at: Timestamp(0),
                }
            )
            .await
            .is_err()
    );
    server
        .admin
        .answer_question(
            id,
            q.question_id,
            ReportAnswer {
                choice: QuestionChoice::Yes,
                words: Some("界".repeat(MAX_REPORT_WORDS + 1)),
                at: Timestamp(0),
            },
        )
        .await
        .unwrap();
    let report = server.admin.report(id).await.unwrap();
    let answer = report.questions[0].answer.as_ref().unwrap();
    assert_eq!(
        answer.words.as_ref().unwrap().chars().count(),
        MAX_REPORT_WORDS
    );
    assert!(answer.at.0 > 0);
    assert_eq!(
        report.history.last().unwrap().status,
        ReportProgress::Received
    );
}

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
fn test_submission() -> ReportBundle {
    let mut b = bundle();
    b.words = None;
    b.test = Some(ReportTest {
        source_commit: COMMIT.into(),
        kind: ReportTestKind::RustIntegration,
        path: "crates/store/tests/lost_write.rs".into(),
        source: "#[test]\nfn lost() {\n    assert_eq!(1, 2);\n}\n".into(),
        patch: Some("--- a/crates/store/src/lib.rs\n+++ b/crates/store/src/lib.rs\n".into()),
        investigator: ReportInvestigator {
            harness: protocol::capability::HarnessKind::Codex,
            model: "some-model".into(),
        },
    });
    b
}
#[tokio::test]
async fn test_submissions_keep_independent_roots_even_with_identical_signatures() {
    let server = Server::start(generous()).await;
    let ordinary = bundle();
    let ordinary_id = ordinary.report_id;
    let first = test_submission();
    let first_id = first.report_id;
    let mut second = test_submission();
    second.test.as_mut().unwrap().path = "crates/store/tests/another.rs".into();
    let second_id = second.report_id;

    server.submit(&key(71), ordinary).await;
    server.submit(&key(72), first).await;
    server.submit(&key(73), second).await;

    for id in [ordinary_id, first_id, second_id] {
        let report = server.admin.report(id).await.unwrap();
        assert_eq!(report.duplicate_of, None);
        assert_eq!(report.group_id, id);
        assert_eq!(report.group_count, 1);
    }
}

#[tokio::test]
async fn ordinary_report_can_join_a_test_submission_root() {
    let server = Server::start(generous()).await;
    let first = test_submission();
    let first_id = first.report_id;
    let second = bundle();
    let second_id = second.report_id;

    server.submit(&key(74), first).await;
    server.submit(&key(75), second).await;

    let root = server.admin.report(first_id).await.unwrap();
    let member = server.admin.report(second_id).await.unwrap();
    assert_eq!(root.duplicate_of, None);
    assert_eq!(root.group_id, first_id);
    assert_eq!(member.duplicate_of, Some(first_id));
    assert_eq!(member.group_id, first_id);
    assert_eq!(root.group_count, 2);
    assert_eq!(member.group_count, 2);
}

fn check(at_build: CheckOutcome) -> TestCheck {
    TestCheck {
        checked_at: Timestamp(1),
        main_commit: "fedcba9876543210fedcba9876543210fedcba98".into(),
        at_build,
        on_main: CheckOutcome::Passes,
        failure_site: Some("crates/store/src/lib.rs:12".into()),
        agent_block: None,
    }
}

#[tokio::test]
async fn test_submissions_follow_the_shared_rules() {
    let server = Server::start(generous()).await;
    let edit = |f: &dyn Fn(&mut ReportTest)| {
        let mut b = test_submission();
        f(b.test.as_mut().unwrap());
        b
    };
    let mut cases = vec![];
    for path in [
        "crates/store/tests/../src/lib.rs",
        "/crates/store/tests/lost.rs",
        "crates/store/tests/.hidden/lost.rs",
        "crates/store/src/lost.rs",
        "client/src/lost.ts",
        "docs/lost.rs",
    ] {
        cases.push(edit(&|t| t.path = path.into()));
    }
    cases.push(edit(&|t| t.source = "x".repeat(MAX_TEST_BYTES + 1)));
    cases.push(edit(&|t| t.patch = Some("x".repeat(MAX_PATCH_BYTES + 1))));
    cases.push(edit(&|t| t.source = "fn a() {}\0".into()));
    cases.push(edit(&|t| t.source = "let s = \"\u{202e}\";".into()));
    cases.push(edit(&|t| t.patch = Some("+\u{2066}".into())));
    cases.push(edit(&|t| t.source_commit = "abc123".into()));
    cases.push(edit(&|t| t.source_commit = COMMIT.to_uppercase()));
    cases.push(edit(&|t| t.investigator.model = "a\u{7}b".into()));
    // The whole bundle, test and fix included, still fits the report limit.
    let mut too_big = edit(&|t| {
        t.source = "x".repeat(MAX_TEST_BYTES);
        t.patch = Some("x".repeat(MAX_PATCH_BYTES));
    });
    too_big.extras.push(ReportExtraPart::TaskTranscript {
        task: "t:12345678".into(),
        entries: vec![TranscriptEntry {
            at: Timestamp(0),
            role: "person".into(),
            text: "x".repeat(MAX_REPORT_BYTES - MAX_TEST_BYTES - MAX_PATCH_BYTES + 1024),
        }],
        cut: false,
    });
    cases.push(too_big);
    for b in cases {
        assert_eq!(
            server
                .send(
                    &key(61),
                    PATH_SUBMIT,
                    &IntakeSubmission {
                        bundle: b,
                        sender: ReportSide::Computer
                    }
                )
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
    }
    // A test submission needs no words, and the test may be long and multi-line.
    let mut good = test_submission();
    good.test.as_mut().unwrap().source =
        format!("#[test]\nfn long() {{\n{}}}\n", "    // a\n".repeat(700));
    let id = good.report_id;
    server.submit(&key(61), good.clone()).await;
    let stored = server.admin.report(id).await.unwrap();
    assert_eq!(stored.bundle.unwrap().test, good.test);
    assert!(stored.check.is_none());
}

#[tokio::test]
async fn checks_are_claimed_apart_from_admission_and_recorded_once() {
    let server = Server::start(generous()).await;
    let plain = server.submit(&key(62), bundle()).await.report_id;
    let first = server.submit(&key(63), test_submission()).await.report_id;
    let second = server.submit(&key(64), test_submission()).await.report_id;
    // Nothing is admitted, so the fixer takes nothing; checks don't wait for it.
    assert!(server.admin.claim("fixer", 30_000).await.unwrap().is_none());
    server.admin.pipeline_paused(true).await.unwrap();
    assert!(
        server
            .admin
            .claim_check("checker", 30_000)
            .await
            .unwrap()
            .is_none()
    );
    server.admin.pipeline_paused(false).await.unwrap();
    let claimed = server
        .admin
        .claim_check("checker", 30_000)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.report_id, first);
    assert!(claimed.bundle.unwrap().test.is_some());
    let next = server
        .admin
        .claim_check("other", 30_000)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(next.report_id, second);
    // The ordinary report is never offered as a check.
    assert!(
        server
            .admin
            .claim_check("other", 30_000)
            .await
            .unwrap()
            .is_none()
    );
    assert!(server.admin.claim_check("checker", 0).await.is_err());
    assert!(
        server
            .admin
            .claim_check("checker", MAX_CHECK_LEASE_MS + 1)
            .await
            .is_err()
    );

    let result = check(CheckOutcome::Fails);
    assert!(matches!(
        server
            .admin
            .record_check(first, "other", result.clone())
            .await,
        Err(intake::client::Error::Status(403))
    ));
    let mut malformed = result.clone();
    malformed.failure_site = Some("crates/a b.rs:1".into());
    assert!(matches!(
        server.admin.record_check(first, "checker", malformed).await,
        Err(intake::client::Error::Status(400))
    ));
    server
        .admin
        .record_check(first, "checker", result.clone())
        .await
        .unwrap();
    server
        .admin
        .record_check(first, "checker", result.clone())
        .await
        .unwrap();
    assert!(matches!(
        server
            .admin
            .record_check(first, "checker", check(CheckOutcome::Passes))
            .await,
        Err(intake::client::Error::Status(403))
    ));
    assert!(matches!(
        server
            .admin
            .record_check(plain, "checker", result.clone())
            .await,
        Err(intake::client::Error::Status(403))
    ));
    assert!(matches!(
        server
            .admin
            .record_check(ReportId::new(), "checker", result.clone())
            .await,
        Err(intake::client::Error::Status(404))
    ));
    assert_eq!(
        server.admin.report(first).await.unwrap().check,
        Some(result.clone())
    );
    let page = server
        .admin
        .list(&ReportFilter {
            failure_site: Some("crates/store/src/lib.rs:12".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(page.reports.len(), 1);
    assert_eq!(page.reports[0].check, Some(result.clone()));
    assert!(page.reports[0].bundle.is_none());

    // Admission and the fixer's claim leave a recorded check alone.
    server.admin.admit(first).await.unwrap();
    let fixing = server.admin.claim("fixer", 30_000).await.unwrap().unwrap();
    assert_eq!(fixing.report_id, first);
    assert_eq!(fixing.check, Some(result));
    assert!(
        server
            .admin
            .claim_check("checker", 30_000)
            .await
            .unwrap()
            .is_none()
    );

    // Withdrawal removes the check with the bundle, and a withdrawn
    // submission is never offered again.
    server
        .send(
            &key(64),
            PATH_WITHDRAW,
            &WithdrawRequest { report_id: second },
        )
        .await
        .error_for_status()
        .unwrap();
    assert!(matches!(
        server
            .admin
            .record_check(second, "other", check(CheckOutcome::Passes))
            .await,
        Err(intake::client::Error::Status(403))
    ));
    server
        .send(
            &key(63),
            PATH_WITHDRAW,
            &WithdrawRequest { report_id: first },
        )
        .await
        .error_for_status()
        .unwrap();
    let gone = server.admin.report(first).await.unwrap();
    assert!(gone.check.is_none() && gone.bundle.is_none());
    assert!(
        server
            .admin
            .list(&ReportFilter {
                failure_site: Some("crates/store/src/lib.rs:12".into()),
                ..Default::default()
            })
            .await
            .unwrap()
            .reports
            .is_empty()
    );
    let db = rusqlite::Connection::open(&server.db).unwrap();
    let rows: i64 = db
        .query_row("SELECT count(*) FROM checks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 0);
    let audited: i64 = db
        .query_row(
            "SELECT count(*) FROM audit WHERE operation IN ('claim_check','record_check')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    // Paused, two takes, two empty, two recordings of the same check.
    assert_eq!(audited, 7);
}

#[tokio::test]
async fn check_leases_expire_and_deferred_checks_wait() {
    let server = Server::start(generous()).await;
    let first = server.submit(&key(65), test_submission()).await.report_id;
    let second = server.submit(&key(66), test_submission()).await.report_id;
    let held = server
        .admin
        .claim_check("worker-one", 200)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(held.report_id, first);
    tokio::time::sleep(Duration::from_millis(250)).await;
    // An expired lease is offered again, and its old holder can't record.
    let again = server
        .admin
        .claim_check("worker-two", 30_000)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(again.report_id, first);
    assert!(
        server
            .admin
            .record_check(first, "worker-one", check(CheckOutcome::Fails))
            .await
            .is_err()
    );
    // Putting a check off keeps it from every worker until then.
    assert!(
        server
            .admin
            .defer_check(first, "worker-one", 60_000)
            .await
            .is_err()
    );
    assert!(
        server
            .admin
            .defer_check(first, "worker-two", 0)
            .await
            .is_err()
    );
    assert!(
        server
            .admin
            .defer_check(first, "worker-two", MAX_CHECK_DEFER_MS + 1)
            .await
            .is_err()
    );
    server
        .admin
        .defer_check(first, "worker-two", 300)
        .await
        .unwrap();
    let next = server
        .admin
        .claim_check("worker-three", 30_000)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(next.report_id, second);
    assert!(
        server
            .admin
            .claim_check("worker-three", 30_000)
            .await
            .unwrap()
            .is_none()
    );
    tokio::time::sleep(Duration::from_millis(350)).await;
    assert_eq!(
        server
            .admin
            .claim_check("worker-three", 30_000)
            .await
            .unwrap()
            .unwrap()
            .report_id,
        first
    );
    // A closed submission is not offered, though its check could still be recorded.
    server
        .admin
        .set_status(
            second,
            ReportProgress::Declined {
                reason: DeclineReason::OutsideOphio,
            },
        )
        .await
        .unwrap();
    server
        .admin
        .record_check(second, "worker-three", check(CheckOutcome::NoTestRan))
        .await
        .unwrap();
    assert!(
        server
            .admin
            .claim_check("worker-four", 30_000)
            .await
            .unwrap()
            .is_none()
    );
}
