// SPDX-License-Identifier: Apache-2.0
mod common;
use common::*;
use report_pipeline::{honeytoken::Honeytokens, provenance::*, signals::Signal};
#[test]
fn honeytokens_are_unique_planted_private_and_detected_anywhere_in_bytes() {
    let root = tempfile::tempdir().unwrap();
    let tokens = Honeytokens::generate().unwrap();
    let other = Honeytokens::generate().unwrap();
    let planted = tokens.plant(root.path()).unwrap();
    assert_eq!(planted.files.len(), 5);
    for path in &planted.files {
        let b = std::fs::read(path).unwrap();
        assert!(String::from_utf8_lossy(&b).contains("GENERATED_CANARY"));
        assert_eq!(tokens.scan(&b), Some(Signal::CanarySeen));
        assert_eq!(other.scan(&b), None);
        let mut body = vec![b'x'; 1024 * 1024];
        body.extend(&b);
        body.extend([255, 0, 1]);
        assert_eq!(tokens.scan(&body), Some(Signal::CanarySeen));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
    assert_eq!(tokens.scan(b""), None);
    assert_eq!(tokens.scan(b"GENERATED_CANARY_unrelated"), None);
    assert!(tokens.plant(root.path()).is_err());
    assert!(!format!("{tokens:?}").contains("GENERATED_CANARY"));
}
#[test]
#[cfg(unix)]
fn honeytoken_planting_refuses_symlink_escape_and_never_overwrites() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join("home")).unwrap();
    let tokens = Honeytokens::generate().unwrap();
    assert!(tokens.plant(root.path()).is_err());
    assert!(std::fs::read_dir(outside.path()).unwrap().next().is_none());
}
#[test]
fn quarantine_stops_hostile_signals_and_does_not_blame_reporter_for_infrastructure() {
    for s in [
        Signal::OffSchemaOutput,
        Signal::BlockedNetwork,
        Signal::BlockedPath,
        Signal::CanarySeen,
        Signal::GateTampering,
    ] {
        let d = s.decision();
        assert!(d.stop && d.quarantine_report && d.count_against_reporter);
    }
    for s in [Signal::BudgetSpent, Signal::RelayRefusal] {
        let d = s.decision();
        assert!(d.stop);
        assert!(!d.quarantine_report && !d.count_against_reporter);
    }
}
fn id(s: &str) -> Identifier {
    s.to_string().try_into().unwrap()
}
fn statement(repo: &Repo) -> Statement {
    let t = repo.triage();
    let test = repo.repro();
    let diff = repo.fixed();
    let gates = report_pipeline::gates::evaluate(
        &diff,
        &test,
        &evidence(),
        report_pipeline::gates::GateConfig::default(),
    )
    .unwrap();
    let (review, _) = report_pipeline::schema::parse_review(br#"{"decision":"pass","flags":[]}"#);
    Statement::new(FixEvidence {
        report_id: "0199f1c2-3b4d-7e5f-8a6b-1c2d3e4f5a6b".parse().unwrap(),
        bundle_hash: [1; 32],
        triage: &t,
        test: &test,
        base_commit: id(&repo.base),
        diff: diff.text().as_bytes(),
        gates: &gates,
        review: &review,
        ci: CiRun {
            id: id("ci-42"),
            result: CiResult::Passed,
        },
        approval: Approval {
            who: id("owner-key-id"),
            when_ms: 1000,
            merge_commit: id(diff.fix()),
        },
        release_build: id("build-42"),
    })
    .unwrap()
}
#[test]
fn provenance_key_is_persistent_and_all_link_and_metadata_tampering_is_rejected() {
    let repo = Repo::new();
    let dir = tempfile::tempdir().unwrap();
    let key = PipelineKey::load_or_create(dir.path()).unwrap();
    let signed = key.sign(statement(&repo)).unwrap();
    verify(&signed, key.public_key()).unwrap();
    assert_eq!(
        PipelineKey::load_or_create(dir.path())
            .unwrap()
            .public_key(),
        key.public_key()
    );
    assert!(!format!("{key:?}").contains(&report_pipeline::schema::triage_schema().to_string()));
    for i in 0..signed.statement.links.len() {
        let mut altered = signed.clone();
        altered.statement.links[i].content_hash[0] ^= 1;
        assert!(verify(&altered, key.public_key()).is_err());
        let mut altered = signed.clone();
        altered.statement.links[i].previous[0] ^= 1;
        assert!(verify(&altered, key.public_key()).is_err());
        let mut altered = signed.clone();
        altered.statement.links[i].hash[0] ^= 1;
        assert!(verify(&altered, key.public_key()).is_err());
    }
    let mut altered = signed.clone();
    altered.statement.links.swap(0, 1);
    assert!(verify(&altered, key.public_key()).is_err());
    let mut altered = signed.clone();
    altered.statement.ci.id = id("ci-43");
    assert!(verify(&altered, key.public_key()).is_err());
    let mut altered = signed.clone();
    altered.statement.approval.who = id("someone-else");
    assert!(verify(&altered, key.public_key()).is_err());
    let mut altered = signed.clone();
    altered.statement.release_build = id("build-43");
    assert!(verify(&altered, key.public_key()).is_err());
    let mut altered = signed.clone();
    altered.statement.report_id = protocol::ReportId::new();
    assert!(verify(&altered, key.public_key()).is_err());
    assert!(verify(&signed, [0; 32]).is_err());
    let mut altered = signed.clone();
    altered.signature[0] ^= 1;
    assert!(verify(&altered, key.public_key()).is_err());
    let raw = serde_json::to_string(&signed).unwrap();
    let decoded: SignedStatement = serde_json::from_str(&raw).unwrap();
    verify(&decoded, key.public_key()).unwrap();
    for banned in [SOURCE, "The screen", "phone_trail", "screenshot", "words"] {
        assert!(!raw.contains(banned));
    }
    assert!(
        !signed
            .statement
            .matches(LinkKind::Bundle, b"altered bundle")
    );
}
#[test]
#[cfg(unix)]
fn provenance_refuses_insecure_key_permissions_symlinks_and_wrong_size() {
    use std::os::unix::fs::PermissionsExt;
    let d = tempfile::tempdir().unwrap();
    PipelineKey::load_or_create(d.path()).unwrap();
    let path = d.path().join("report-pipeline.ed25519");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(PipelineKey::load_or_create(d.path()).is_err());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(&path, b"short").unwrap();
    assert!(PipelineKey::load_or_create(d.path()).is_err());
    let d = tempfile::tempdir().unwrap();
    let outside = tempfile::NamedTempFile::new().unwrap();
    std::os::unix::fs::symlink(outside.path(), d.path().join("report-pipeline.ed25519")).unwrap();
    assert!(PipelineKey::load_or_create(d.path()).is_err());
}
