// SPDX-License-Identifier: Apache-2.0
//! Exercise the non-cfg(test) library used by downstream fixture tests.
#![cfg(all(unix, feature = "test-fixtures"))]

#[tokio::test]
async fn downstream_checks_lock_the_fixture_home() {
    let fixture = tempfile::tempdir().unwrap();
    // This arbitrary name is not used to recognize fixtures; the Cargo feature is.
    let binary = fixture.path().join("arbitrary-program");
    test_support::executable(&binary, "#!/bin/sh\nprintf 'fixture-version\\n'\n").unwrap();

    let output = harness_claude::version(&binary).await.unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"fixture-version\n");
    assert!(fixture.path().join(".ophio/claude-check.lock").is_file());
}
