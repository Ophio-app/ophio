// SPDX-License-Identifier: Apache-2.0
use protocol::voice::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use voice_control::{Driver, Error};

#[tokio::test]
async fn private_pipe_snapshot_click_type_stale_sensitive_picture_and_detach() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let fixture = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut request = [0u8; 8192];
                let _ = socket.read(&mut request).await;
                let body = r#"<!doctype html><title>Fixture</title><button type="button" onclick="this.textContent='Clicked'">Click me</button><input aria-label="Search" oninput="document.getElementById('result').textContent=this.value"><button type="button" id="result">Result</button><input type="password" aria-label="Password" value="must-never-leave"><iframe srcdoc="<button type='button'>Inside frame</button>"></iframe>"#;
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(reply.as_bytes()).await;
            });
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let executable = crate::discover().expect("a Chromium-based browser is installed");
    let browser = crate::Browser::launch(&executable, dir.path(), true)
        .await
        .unwrap();
    let result: Result<(), Error> = async {
        let initial = browser.snapshot().await?;
        let action = VoiceAction {
            kind: VoiceActionKind::Navigate,
            target: None,
            literal: Some(format!("http://{address}")),
            scroll_y: 0,
            previous_tab: false,
        };
        browser.act(&action, &initial).await?;
        let mut snapshot = browser.snapshot().await?;
        for _ in 0..100 {
            if snapshot.elements.iter().any(|e| e.name == "Inside frame") {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            snapshot = browser.snapshot().await?;
        }
        assert!(snapshot.elements.iter().any(|e| e.name == "Inside frame"));
        assert!(!format!("{snapshot:?}").contains("must-never-leave"));
        let target = snapshot
            .elements
            .iter()
            .find(|e| e.name == "Click me")
            .cloned()
            .unwrap();
        let click = VoiceAction {
            kind: VoiceActionKind::Click,
            target: Some(target),
            literal: None,
            scroll_y: 0,
            previous_tab: false,
        };
        browser.act(&click, &snapshot).await?;
        assert!(matches!(
            browser.act(&click, &snapshot).await,
            Err(Error::StaleTarget)
        ));
        let snapshot = browser.snapshot().await?;
        assert!(snapshot.elements.iter().any(|e| e.name == "Clicked"));
        let target = snapshot
            .elements
            .iter()
            .find(|e| e.name == "Search")
            .cloned()
            .unwrap();
        let mut typing = VoiceAction {
            kind: VoiceActionKind::Type,
            target: Some(target),
            literal: Some("Verbatim words".into()),
            scroll_y: 0,
            previous_tab: false,
        };
        browser.act(&typing, &snapshot).await?;
        let snapshot = browser.snapshot().await?;
        assert!(snapshot.elements.iter().any(|e| e.name == "Verbatim words"));
        typing.target = snapshot.elements.iter().find(|e| e.sensitive).cloned();
        assert!(browser.act(&typing, &snapshot).await.is_err());
        assert!(
            browser
                .picture()
                .await?
                .is_some_and(|s| s.starts_with("iVBOR"))
        );
        browser.barrier().await?;
        browser.detach().await?;
        let detached = browser
            .pipe
            .call(None, "Target.getTargets", serde_json::json!({}))
            .await?;
        assert!(
            detached["targetInfos"]
                .as_array()
                .is_some_and(|targets| targets.iter().any(|target| target["type"] == "page"))
        );
        assert!(matches!(browser.snapshot().await, Err(Error::Stopped)));
        Ok(())
    }
    .await;
    let closed_while_open = browser.is_closed();
    browser.close_for_test().await;
    fixture.abort();
    let _ = fixture.await;
    result.unwrap();
    assert!(!closed_while_open);
    // The window's cookies and history leave the disk once Chromium and its
    // helpers have exited, and then the window counts as closed.
    for _ in 0..400 {
        if browser.is_closed() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(browser.is_closed());
    let profiles = crate::profile::directory(dir.path());
    assert_eq!(std::fs::read_dir(&profiles).unwrap().count(), 0);
}

#[tokio::test]
async fn a_browser_that_cannot_start_leaves_no_profile() {
    let dir = tempfile::tempdir().unwrap();
    let launched = crate::Browser::launch(
        std::path::Path::new("/nonexistent/chromium"),
        dir.path(),
        true,
    )
    .await;
    assert!(launched.is_err());
    let profiles = crate::profile::directory(dir.path());
    for _ in 0..100 {
        if std::fs::read_dir(&profiles).unwrap().next().is_none() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(std::fs::read_dir(&profiles).unwrap().count(), 0);
}
