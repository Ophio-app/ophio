// SPDX-License-Identifier: Apache-2.0
//! Fully synthetic catalog/authority fixtures. Never invoke a native app.
use super::*;
fn packaged_setup() -> (Arc<Fake>, WindowsLauncher) {
    let (fake, launcher) = setup();
    fake.entries.lock().unwrap().clear();
    fake.packaged.lock().unwrap().push(PackagedApp {
        aumid: "Fixture.Editor_123456789abcd!App".into(),
        package: "Fixture.Editor_1.0.0.0_x64__123456789abcd".into(),
        name: "Fixture Editor".into(),
    });
    (fake, launcher)
}
#[tokio::test]
async fn packaged_catalog_ids_are_opaque_and_only_selected_native_identity_launches() {
    let (fake, launcher) = packaged_setup();
    let selected = selected(&launcher).await;
    assert!(!selected.id.contains("Fixture"));
    assert_eq!(selected, super::selected(&launcher).await);
    launcher
        .launch_authorized(&selected, Arc::new(Allow))
        .await
        .unwrap();
    assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
    for id in [
        "shell:AppsFolder\\Fixture.Editor_123456789abcd!App",
        "Fixture.Editor_123456789abcd!App",
        "cmd.exe /c anything",
    ] {
        assert!(matches!(
            launcher
                .launch_authorized(
                    &VoiceApp {
                        id: id.into(),
                        name: selected.name.clone()
                    },
                    Arc::new(Allow)
                )
                .await,
            Err(Error::StaleTarget)
        ));
    }
    assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn package_upgrade_rename_and_removal_reject_old_selection_then_refresh_recovers() {
    let (fake, launcher) = packaged_setup();
    let before = selected(&launcher).await;
    fake.packaged.lock().unwrap()[0].package = "Fixture.Editor_2.0.0.0_x64__123456789abcd".into();
    assert!(matches!(
        launcher.launch_authorized(&before, Arc::new(Allow)).await,
        Err(Error::StaleTarget)
    ));
    let upgraded = selected(&launcher).await;
    assert_ne!(upgraded.id, before.id);
    launcher
        .launch_authorized(&upgraded, Arc::new(Allow))
        .await
        .unwrap();
    fake.packaged.lock().unwrap()[0].name = "Renamed Fixture".into();
    assert!(matches!(
        launcher.launch_authorized(&upgraded, Arc::new(Allow)).await,
        Err(Error::StaleTarget)
    ));
    let renamed = selected(&launcher).await;
    fake.packaged.lock().unwrap().clear();
    assert!(matches!(
        launcher.launch_authorized(&renamed, Arc::new(Allow)).await,
        Err(Error::StaleTarget)
    ));
    assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
    assert!(launcher.apps().await.unwrap().is_empty());
}
#[tokio::test]
async fn packaged_final_authority_and_native_prepare_refusal_block_activation() {
    let (fake, launcher) = packaged_setup();
    let app = selected(&launcher).await;
    assert!(matches!(
        launcher.launch_authorized(&app, Arc::new(Stop)).await,
        Err(Error::Stopped)
    ));
    assert!(matches!(
        launcher
            .launch_authorized(&app, Arc::new(StopLast(AtomicUsize::new(0))))
            .await,
        Err(Error::Stopped)
    ));
    fake.fail_prepare.store(true, Ordering::SeqCst);
    assert!(matches!(
        launcher.launch_authorized(&app, Arc::new(Allow)).await,
        Err(Error::StaleTarget)
    ));
    assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
    fake.fail_prepare.store(false, Ordering::SeqCst);
    launcher
        .launch_authorized(&app, Arc::new(Allow))
        .await
        .unwrap();
    assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn only_exact_unambiguous_current_os_defaults_supply_roles() {
    let (fake, launcher) = packaged_setup();
    let aumid = fake.packaged.lock().unwrap()[0].aumid.clone();
    fake.defaults
        .lock()
        .unwrap()
        .insert(AppRole::Browser, aumid.clone());
    fake.defaults
        .lock()
        .unwrap()
        .insert(AppRole::Email, "Unmatched.App_123456789abcd!App".into());
    let apps = launcher.apps().await.unwrap();
    assert_eq!(apps[0].defaults, vec![AppRole::Browser]);
    assert_eq!(apps[0].roles, vec![AppRole::Browser]);
    assert!(matches!(
        voice_control::apps::resolve("the browser", &apps),
        voice_control::apps::Resolution::One(_)
    ));
    let mut ambiguous = fake.packaged.lock().unwrap()[0].clone();
    ambiguous.package = "Fixture.Editor_2.0.0.0_x64__123456789abcd".into();
    fake.packaged.lock().unwrap().push(ambiguous);
    let apps = launcher.apps().await.unwrap();
    assert!(
        apps.iter()
            .all(|app| app.defaults.is_empty() && app.roles.is_empty())
    );
    fake.defaults.lock().unwrap().clear();
    fake.packaged.lock().unwrap().truncate(1);
    assert!(launcher.apps().await.unwrap()[0].defaults.is_empty());
}
#[tokio::test]
async fn cancel_during_packaged_preparation_cannot_activate_late() {
    let (fake, launcher) = packaged_setup();
    let app = selected(&launcher).await;
    *fake.delay_prepare.lock().unwrap() = Duration::from_millis(100);
    let launcher = Arc::new(launcher);
    let task = tokio::spawn({
        let launcher = launcher.clone();
        let app = app.clone();
        async move { launcher.launch_authorized(&app, Arc::new(Allow)).await }
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    task.abort();
    let _ = task.await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
    *fake.delay_prepare.lock().unwrap() = Duration::ZERO;
    launcher
        .launch_authorized(&app, Arc::new(Allow))
        .await
        .unwrap();
    assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
}
