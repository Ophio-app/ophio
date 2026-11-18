// SPDX-License-Identifier: Apache-2.0
use super::*;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixListener,
};

#[tokio::test]
async fn closed_receiver_during_shutdown_still_closes_stdin() {
    let (dir, _, mut spec) = fixture();
    let binary = dir.path().join("shutdown-claude");
    spec.model = None;
    spec.effort = None;
    spec.resume_session = None;
    spec.bridge = None;
    test_support::executable(
        &binary,
        r#"#!/usr/bin/env python3
import json, pathlib, socket, sys
if 'auth' in sys.argv:
    print(json.dumps({'loggedIn': True, 'authMethod': 'claude.ai'}), flush=True)
    sys.exit(0)
root = pathlib.Path(__file__).parent
json.loads(sys.stdin.readline())
gate = socket.socket(socket.AF_UNIX)
gate.connect(str(root / 'emission.sock'))
gate.sendall(b'ready')
assert gate.recv(1) == b'x'
print('not a JSON frame', flush=True)
print(json.dumps({'type': 'control_request', 'request_id': 'closed-events', 'request': {}}), flush=True)
response = json.loads(sys.stdin.readline())
assert response['response']['request_id'] == 'closed-events'
assert response['response']['subtype'] == 'error'
gate.sendall(b'passed')
sys.stdin.read()
(root / 'stdin-closed').touch()
"#,
    )
    .unwrap();
    let listener = UnixListener::bind(dir.path().join("emission.sock")).unwrap();
    let run = ClaudeAdapter::with_binary(binary)
        .start_run(spec)
        .await
        .unwrap();
    let mut gate = boundary::gate(&listener).await;

    run.control.close_admission();
    drop(run.events);
    gate.write_all(b"x").await.unwrap();
    // The response proves the actor handled a frame after losing its event
    // receiver, before shutdown is requested. No scheduler race can skip it.
    let mut passed = [0; 6];
    tokio::time::timeout(Duration::from_secs(10), gate.read_exact(&mut passed))
        .await
        .unwrap()
        .expect("actor must survive the closed event receiver until shutdown");
    assert_eq!(&passed, b"passed");
    tokio::time::timeout(Duration::from_secs(10), run.control.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert!(dir.path().join("stdin-closed").exists());
}
