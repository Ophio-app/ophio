// SPDX-License-Identifier: Apache-2.0
//! Authoritative companion application services.
mod access;
mod accounts;
mod agent_browser;
mod agent_files;
mod agent_logins;
mod agent_preferences;
mod agent_updates;
mod alerts;
pub mod api;
mod app_start;
mod asking;
pub mod bridge;
mod calm;
mod carry_on;
mod checklist;
mod children;
mod commands;
mod contribution;
mod control;
mod desktop_lock;
mod diagnostics;
mod disabled;
mod drawn;
mod expiry;
mod findings;
mod follow_up;
mod followups;
mod found_previews;
mod git;
mod github;
mod github_access;
mod github_after;
mod github_dupes;
mod github_run;
mod github_together;
mod github_tools;
mod github_train;
mod github_watch;
mod hand_back;
mod history;
mod html_references;
mod hub;
mod identity;
pub mod inbox;
mod launchers;
mod library_workflows;
mod native_text;
mod notifications;
mod observed;
mod plumbing;
pub mod preview;
pub mod preview_forward;
mod preview_server;
pub mod report_fixes;
pub mod report_inbox;
mod reported;
pub mod reports;
mod results;
mod review_gate;
mod review_loop;
mod review_rules;
mod runs;
mod runtime;
mod scratch;
mod setup_agents;
mod shown;
mod system;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
mod transfer_collisions;
mod transfers;
mod usage_limits;
mod waiting;
mod watchdog;
mod watching;
mod work_tools;

pub use api::{Caller, Hub};
pub use checklist::Additions as ChecklistAdditions;
pub use hub::{CompanionHub, HubBuilder};
pub use launchers::LauncherDirs;

use protocol::{ErrorCode, ProtocolError};
fn error(code: ErrorCode, message: impl Into<String>) -> ProtocolError {
    ProtocolError::new(code, message)
}
fn internal(cause: impl diagnostics::SafeCause) -> ProtocolError {
    let cause = cause.safe_cause();
    tracing::error!(error = %cause, "Storage operation failed");
    error(
        ErrorCode::Internal,
        format!("Øphio's storage on the computer failed: {cause}"),
    )
}
fn stored(e: store::Error) -> ProtocolError {
    match e {
        store::Error::Rule(e) => e,
        other => internal(other),
    }
}
fn unsupported(feature: &str) -> ProtocolError {
    error(
        ErrorCode::Unsupported,
        format!("{feature} is not configured"),
    )
}

#[cfg(test)]
mod tests;

pub mod voice;
mod windows;

pub mod device_activity;
mod project_folders;
mod project_stage;
mod questions;

#[cfg(all(test, target_os = "linux"))]
mod force_stop_tests;

#[cfg(test)]
mod inbox_tests;
