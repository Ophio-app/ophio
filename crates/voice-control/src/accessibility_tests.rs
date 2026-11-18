// SPDX-License-Identifier: Apache-2.0
//! Existing accessibility implementations keep their default activation path.
use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst},
};
#[derive(Debug, Default)]
struct ExistingAdapter {
    calls: AtomicUsize,
    focused: AtomicBool,
}
#[async_trait]
impl Accessibility for ExistingAdapter {
    async fn snapshot(&self) -> Result<Snapshot, Error> {
        Ok(Snapshot::new(vec![], "fixture".into()))
    }
    async fn activate(&self, _: &VoiceElement, focus: bool) -> Result<bool, Error> {
        self.calls.fetch_add(1, SeqCst);
        self.focused.store(focus, SeqCst);
        Ok(focus)
    }
}
struct Permission(bool);
impl Authority for Permission {
    fn check(&self) -> Result<(), Error> {
        if self.0 { Ok(()) } else { Err(Error::Stopped) }
    }
}
fn element() -> VoiceElement {
    VoiceElement {
        id: "existing-native-identity".into(),
        role: "button".into(),
        name: "Fixture".into(),
        bounds: VoiceBounds {
            x: 0.,
            y: 0.,
            width: 40.,
            height: 20.,
        },
        enabled: true,
        visible: true,
        focused: false,
        sensitive: false,
        destructive: false,
    }
}
#[tokio::test]
async fn accessibility_default_authorized_path_preserves_existing_adapters_and_fallback() {
    let existing = ExistingAdapter::default();
    assert!(
        existing
            .activate_authorized(&element(), true, Arc::new(Permission(true)))
            .await
            .unwrap()
    );
    assert!(existing.focused.load(SeqCst));
    assert!(
        !existing
            .activate_authorized(&element(), false, Arc::new(Permission(true)))
            .await
            .unwrap()
    );
    assert_eq!(existing.calls.load(SeqCst), 2);
}
#[tokio::test]
async fn accessibility_default_authorized_path_refuses_stopped_before_existing_adapter() {
    let existing = ExistingAdapter::default();
    assert!(matches!(
        existing
            .activate_authorized(&element(), true, Arc::new(Permission(false)))
            .await,
        Err(Error::Stopped)
    ));
    assert_eq!(existing.calls.load(SeqCst), 0);
}
