// SPDX-License-Identifier: Apache-2.0
use super::*;

fn check_fixture() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("check-claude");
    test_support::executable(&binary, r#"#!/usr/bin/env python3
import json, pathlib, sys, time
root = pathlib.Path(__file__).parent
active = root / 'active'
try:
    active.mkdir()
except FileExistsError:
    (root / 'overlap').touch()
(root / 'started').touch()
if '-p' in sys.argv:
    request = json.loads(sys.stdin.readline())
    print(json.dumps({'type':'control_response', 'response':{'subtype':'success', 'request_id':request['request_id'], 'response':{'models':[]}}}), flush=True)
    sys.stdin.read()
    (root / 'eof').touch()
    # Model discovery answered, but a credential write could still be running.
    time.sleep(0.15)
    (root / 'saved').touch()
else:
    time.sleep(0.05)
    print('{"loggedIn": true, "authMethod": "claude.ai"}', flush=True)
active.rmdir()
"#).unwrap();
    (dir, binary)
}

async fn appeared(path: &Path) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn model_and_status_checks_share_a_lock_until_eof_cleanup_finishes() {
    let (dir, binary) = check_fixture();
    let (models, status, help) = tokio::join!(
        catalog::listed(&binary),
        io::inspect(&binary, &["auth", "status", "--json"]),
        io::inspect(&binary, &["--help"]),
    );
    assert!(models.is_some());
    assert!(status.unwrap().status.success());
    assert!(help.unwrap().status.success());
    assert!(dir.path().join("eof").exists());
    assert!(dir.path().join("saved").exists());
    assert!(!dir.path().join("overlap").exists());
    assert!(!dir.path().join("active").exists());
}

#[tokio::test]
async fn cancelling_a_catalog_request_does_not_interrupt_its_login_write() {
    let (dir, binary) = check_fixture();
    let request = tokio::spawn({
        let binary = binary.clone();
        async move { catalog::listed(&binary).await }
    });
    appeared(&dir.path().join("eof")).await;
    request.abort();
    let status = io::inspect(&binary, &["auth", "status", "--json"]).await;
    assert!(status.unwrap().status.success());
    assert!(dir.path().join("saved").exists());
    assert!(!dir.path().join("overlap").exists());
    assert!(!dir.path().join("active").exists());
}

#[tokio::test]
async fn eof_allows_a_delayed_write_before_successful_exit() {
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("finish");
    test_support::executable(
        &binary,
        r#"#!/usr/bin/env python3
import pathlib, sys, time
sys.stdin.read()
time.sleep(0.05)
pathlib.Path(__file__).with_name('saved').touch()
"#,
    )
    .unwrap();
    let mut child = io::command(&binary).spawn().unwrap();
    let status = io::finish_check(&mut child, "fixture", Duration::from_secs(3)).await;
    assert!(status.unwrap().success());
    assert!(dir.path().join("saved").exists());
}

#[tokio::test]
async fn a_stuck_check_is_killed_only_after_eof_and_the_full_grace() {
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("stuck");
    test_support::executable(
        &binary,
        r#"#!/usr/bin/env python3
import pathlib, sys, time
sys.stdin.read()
pathlib.Path(__file__).with_name('eof').touch()
time.sleep(60)
"#,
    )
    .unwrap();
    let mut child = io::command(&binary).spawn().unwrap();
    // Separate startup from the measured grace to avoid timing-dependent tests.
    drop(child.stdin.take());
    appeared(&dir.path().join("eof")).await;
    let grace = Duration::from_millis(150);
    let started = std::time::Instant::now();
    assert!(
        io::finish_check(&mut child, "fixture", grace)
            .await
            .is_none()
    );
    assert!(started.elapsed() >= grace);
    assert!(child.try_wait().unwrap().is_some());
}

#[tokio::test]
async fn a_signed_out_account_points_to_the_phone_without_starting_model_discovery() {
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("signed-out");
    test_support::executable(
        &binary,
        r#"#!/usr/bin/env python3
import pathlib, sys
if sys.argv[1:] == ['--version']:
    print('fixture')
elif sys.argv[1:3] == ['auth', 'status']:
    print('{"loggedIn":false}')
elif sys.argv[1:] == ['--help']:
    print("  --model <model> 'opus'")
else:
    pathlib.Path(__file__).with_name('unexpected-model-check').touch()
"#,
    )
    .unwrap();
    let info = ClaudeAdapter::with_binary(binary).probe().await;
    let AccountReadiness::SignedOut { remedy } = info.account else {
        panic!("expected a signed-out account")
    };
    assert!(remedy.contains("Accounts"));
    assert!(remedy.contains("phone"));
    assert!(remedy.find("phone").unwrap() < remedy.find("computer").unwrap());
    assert!(!dir.path().join("unexpected-model-check").exists());
}

#[tokio::test]
async fn a_status_check_can_save_its_login_after_the_old_five_second_limit() {
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("slow-status");
    test_support::executable(
        &binary,
        r#"#!/usr/bin/env python3
import pathlib, time
time.sleep(5.2)
pathlib.Path(__file__).with_name('saved').touch()
print('{"loggedIn":true}')
"#,
    )
    .unwrap();
    let started = std::time::Instant::now();
    let output = io::inspect(&binary, &["auth", "status", "--json"]).await;
    assert!(
        output.is_none(),
        "the answer deadline must still bound the caller"
    );
    assert!(started.elapsed() < Duration::from_secs(7));
    appeared(&dir.path().join("saved")).await;
}
