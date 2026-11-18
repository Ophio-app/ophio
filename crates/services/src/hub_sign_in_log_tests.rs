// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::testing::FakeHarness;
use ports::HarnessAdapter;
use std::sync::{Arc, Mutex};

async fn info(account: AccountReadiness) -> HarnessInfo {
    let mut info = FakeHarness::default().probe().await;
    info.account = account;
    info
}
fn signed_in(label: &str) -> AccountReadiness {
    AccountReadiness::SignedIn {
        account_label: Some(label.into()),
        plan: None,
    }
}
fn signed_out(reason: &str) -> AccountReadiness {
    AccountReadiness::SignedOut {
        remedy: reason.into(),
    }
}
fn unknown(reason: &str) -> AccountReadiness {
    AccountReadiness::Unknown {
        reason: reason.into(),
    }
}

#[tokio::test]
async fn only_definite_changes_count_as_transitions() {
    let previous = [info(signed_in("private@example.test")).await];
    let out = [info(signed_out("private command output")).await];
    let before = claude_sign_in_state(&previous);
    let after = claude_sign_in_state(&out);
    assert_eq!(claude_sign_in_transition(None, after), None);
    assert_eq!(
        claude_sign_in_transition(before, after),
        Some(ClaudeSignInTransition::SignedOut)
    );
    assert_eq!(
        claude_sign_in_transition(after, before),
        Some(ClaudeSignInTransition::SignedIn)
    );
    assert_eq!(claude_sign_in_transition(after, after), None);
    assert_eq!(
        claude_sign_in_transition(
            before,
            claude_sign_in_state(&[info(signed_in("another@example.test")).await])
        ),
        None
    );
    assert_eq!(
        claude_sign_in_transition(
            after,
            claude_sign_in_state(&[info(signed_out("different remedy")).await])
        ),
        None
    );
    assert_eq!(
        claude_sign_in_state(&[info(unknown("private diagnostics")).await]),
        None
    );
    assert_eq!(claude_sign_in_transition(before, None), None);
    assert_eq!(claude_sign_in_transition(None, after), None);
    assert_eq!(claude_sign_in_state(&[]), None);
}

#[derive(Clone, Default)]
struct LogEvents(Arc<Mutex<Vec<String>>>);
impl tracing::Subscriber for LogEvents {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        struct Fields(String);
        impl tracing::field::Visit for Fields {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                self.0.push_str(field.name());
                self.0.push_str(&format!("={value:?};"));
            }
        }
        let mut fields = Fields(String::new());
        event.record(&mut fields);
        if event.metadata().target() == module_path!().trim_end_matches("::sign_in_log_tests") {
            self.0.lock().unwrap().push(fields.0);
        }
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

#[tokio::test]
async fn refresh_logs_only_committed_definite_changes_without_private_fields() {
    let harness = Arc::new(FakeHarness::default());
    let directory = tempfile::tempdir().unwrap();
    let hub = CompanionHub::builder()
        .data_dir(directory.path())
        .harness(harness.clone())
        .build()
        .await
        .unwrap();
    let events = LogEvents::default();
    let _subscriber = tracing::subscriber::set_default(events.clone());
    *harness.account.lock().unwrap() = signed_out("private command output");
    hub.refresh_catalog().await.unwrap();
    hub.refresh_catalog().await.unwrap();
    *harness.account.lock().unwrap() = unknown("private diagnostics");
    hub.refresh_catalog().await.unwrap();
    *harness.account.lock().unwrap() = signed_in("private@example.test");
    hub.refresh_catalog().await.unwrap(); // report changed since the last definite status
    *harness.account.lock().unwrap() = signed_out("another private remedy");
    hub.refresh_catalog().await.unwrap();
    *harness.account.lock().unwrap() = signed_in("another@example.test");
    hub.refresh_catalog().await.unwrap();
    let seen = events.0.lock().unwrap().clone();
    assert_eq!(seen.len(), 4, "{seen:?}");
    assert!(seen[0].contains("signed in to signed out"));
    assert!(seen[1].contains("signed out to signed in"));
    assert!(seen[2].contains("signed in to signed out"));
    assert!(seen[3].contains("signed out to signed in"));
    for message in seen {
        assert!(!message.contains("private"));
        assert!(!message.contains("example.test"));
        assert!(!message.contains("remedy"));
        assert!(!message.contains("reason"));
    }
}

#[tokio::test]
async fn catalog_refreshes_wait_for_the_previous_observation_to_be_saved() {
    let directory = tempfile::tempdir().unwrap();
    let hub = CompanionHub::builder()
        .data_dir(directory.path())
        .harness(Arc::new(FakeHarness::default()))
        .build()
        .await
        .unwrap();
    let held = hub.inner.catalog_refresh.lock().await;
    let waiting = tokio::spawn({
        let hub = hub.clone();
        async move { hub.refresh_catalog().await }
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(!waiting.is_finished());
    drop(held);
    waiting.await.unwrap().unwrap();
}
