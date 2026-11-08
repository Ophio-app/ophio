// SPDX-License-Identifier: Apache-2.0
use super::*;

fn rename(name: &str) -> CommandEnvelope {
    CommandEnvelope::new(Command::RenameComputer { name: name.into() })
}

async fn snapshot_host(f: &Fixture, caller: &Caller) -> HostInfo {
    let QueryData::Snapshot(snapshot) = f
        .hub
        .query(
            caller,
            Query::Snapshot {
                reads_withheld: None,
                account_items: false,
            },
        )
        .await
        .unwrap()
    else {
        panic!("snapshot expected");
    };
    snapshot.host
}

#[tokio::test]
async fn a_new_name_reaches_every_phone_at_once_and_lasts_a_restart() {
    let f = Fixture::new().await;
    let manager = f.device().await;
    // A phone that can't manage the computer is shown its name all the same.
    let phone = pair(&f.hub, 2).await;
    assert!(!phone.grants.admin);
    assert_eq!(f.hub.host_name(), "This computer");
    let host = snapshot_host(&f, &phone).await;
    // Older phones see no new field until the owner renames the computer.
    assert!(!host.renamed);
    assert!(
        serde_json::to_value(&host)
            .unwrap()
            .get("renamed")
            .is_none()
    );
    let mut events = f
        .hub
        .subscribe(&phone, Some(f.hub.journal_head()))
        .await
        .unwrap();

    let receipt = f.hub.submit(&manager, rename("  Studio Mac  ")).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    assert!(receipt.result.is_none());
    let published = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let crate::api::Outbound::Event(e) = events.live.recv().await.unwrap()
                && let Event::HostUpdated { host } = &e.event
            {
                return host.clone();
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(
        (published.name.as_str(), published.renamed),
        ("Studio Mac", true)
    );
    let host = snapshot_host(&f, &phone).await;
    assert_eq!((host.name.as_str(), host.renamed), ("Studio Mac", true));
    assert_eq!(serde_json::to_value(&host).unwrap()["renamed"], true);
    assert_eq!(host.capabilities, published.capabilities);
    assert_eq!(f.hub.host_name(), "Studio Mac");
    // A phone paired now is told the new name.
    let ticket = f.hub.open_pairing(&Caller::local_owner()).await.unwrap();
    let paired = crate::testing::pair_confirmed(
        &f.hub,
        PairRequest {
            pairing_secret: ticket.secret,
            device_public_key: URL_SAFE_NO_PAD.encode([3; 32]),
            device_name: "Tablet".into(),
            device_platform: "android".into(),
            client_version: "test".into(),
            protocol: ProtocolVersion::CURRENT,
            shows_code: true,
            replaces: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(paired.host_name, "Studio Mac");

    // The computer starts again with the name, though it is not the hostname.
    let f = f.reopened(|builder| builder).await;
    assert_eq!(f.hub.host_name(), "Studio Mac");
    let host = snapshot_host(&f, &phone).await;
    assert_eq!((host.name.as_str(), host.renamed), ("Studio Mac", true));

    // The hostname puts the computer back on it.
    let receipt = f.hub.submit(&manager, rename("This computer")).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    let host = snapshot_host(&f, &phone).await;
    assert_eq!((host.name.as_str(), host.renamed), ("This computer", false));
    let f = f
        .reopened(|builder| builder.host_name("studio-linux"))
        .await;
    let host = snapshot_host(&f, &phone).await;
    assert_eq!((host.name.as_str(), host.renamed), ("studio-linux", false));
    assert_eq!(f.hub.host_name(), "studio-linux");
    f.close().await;
}

#[tokio::test]
async fn only_a_manager_renames_the_computer() {
    let f = Fixture::new().await;
    let _manager = f.device().await;
    let phone = pair(&f.hub, 2).await;
    let receipt = f.hub.submit(&phone, rename("Studio Mac")).await;
    assert_eq!(receipt.status, ReceiptStatus::Rejected);
    assert_eq!(receipt.error.unwrap().code, ErrorCode::PermissionDenied);
    let host = snapshot_host(&f, &phone).await;
    assert_eq!((host.name.as_str(), host.renamed), ("This computer", false));
    assert_eq!(f.hub.host_name(), "This computer");
    f.close().await;
}

#[tokio::test]
async fn a_computer_name_is_one_to_sixty_plain_characters() {
    let f = Fixture::new().await;
    let manager = f.device().await;
    for name in [
        String::new(),
        " \t ".into(),
        "a".repeat(61),
        // Trimmed, but still too long.
        format!(" {} ", "é".repeat(61)),
        "Studio\u{7}".into(),
        "Studio\nMac".into(),
        "Studio\u{1b}[8m".into(),
        "Studio\u{0}".into(),
        // Unseen, these turn the name, or a path shown beside it, around.
        "Studio\u{202E}oiduts".into(),
        "Studio \u{2067}x\u{2069}".into(),
        "\u{200F}Studio".into(),
        "Studio\u{061C}".into(),
        // Line and paragraph separators break a line as a newline does.
        "Studio\u{2028}Mac".into(),
        "Studio\u{2029}Mac".into(),
    ] {
        let receipt = f.hub.submit(&manager, rename(&name)).await;
        assert_eq!(receipt.status, ReceiptStatus::Rejected, "{name:?}");
        let error = receipt.error.unwrap();
        assert_eq!(error.code, ErrorCode::InvalidRequest, "{name:?}");
        assert!(error.message.starts_with("A computer's name "), "{error:?}");
    }
    assert_eq!(f.hub.host_name(), "This computer");
    // Sixty characters, counted as characters rather than bytes.
    let long = format!("{}{}", "ß".repeat(30), "日本".repeat(15));
    assert_eq!(long.chars().count(), 60);
    assert!(long.len() > 60);
    let receipt = f.hub.submit(&manager, rename(&format!(" {long}\n"))).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    let host = snapshot_host(&f, &manager).await;
    assert_eq!((host.name.as_str(), host.renamed), (long.as_str(), true));
    // Right-to-left letters and an emoji's joiner are text, not controls.
    let written = "مكتب سام 👩\u{200D}💻";
    let receipt = f.hub.submit(&manager, rename(written)).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    assert_eq!(f.hub.host_name(), written);
    f.close().await;
}

#[tokio::test]
async fn the_computer_says_it_can_be_renamed() {
    let f = Fixture::new().await;
    let features = f.hub.capabilities().await.features;
    assert!(
        features.iter().any(|f| f == "computer_name_v1"),
        "{features:?}"
    );
    let phone = f.device().await;
    let host = snapshot_host(&f, &phone).await;
    assert!(
        host.capabilities
            .features
            .iter()
            .any(|f| f == "computer_name_v1")
    );
    f.close().await;
}
