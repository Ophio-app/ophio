// SPDX-License-Identifier: Apache-2.0
//! What an approval card says a request would do, worked out by the
//! computer from the exact request and the project it runs in.
use ports::request_facts::{Place, Subject, facts};
use protocol::model::{ApprovalFact, Project};
use std::path::PathBuf;

/// Where a run's requests land: its project, and the home folder `~` means.
pub(crate) fn place(project: Option<&Project>) -> Place {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| std::fs::canonicalize(&home).unwrap_or(home));
    match project {
        Some(project) => Place {
            project: Some(PathBuf::from(&project.host_path)),
            project_name: if project.home {
                "your home folder".into()
            } else {
                project.name.clone()
            },
            home,
        },
        None => Place {
            project: None,
            project_name: String::new(),
            home,
        },
    }
}

/// A request working out its facts fails on, in tests.
#[cfg(test)]
pub(crate) const FAILS: &str = "ophio-test: the facts line fails on this";

/// What the card can't say, when working it out failed.
pub(crate) const CANT_TELL: &str = "Ophio can't tell what this does";

/// What `subject` would do, as the card says it. Working it out never takes
/// anything down with it: should it fail, as a parser bug might on some
/// request, the card says Ophio can't tell, highlighted, and the request is
/// shown as usual.
pub(crate) fn approval_facts(subject: &Subject, project: Option<&Project>) -> Vec<ApprovalFact> {
    let place = place(project);
    let work_out = || {
        #[cfg(test)]
        if matches!(subject, Subject::Command { command, .. } if command == FAILS) {
            panic!("a request the facts line can't take");
        }
        facts(subject, &place)
    };
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(work_out)) {
        Ok(facts) => facts,
        Err(_) => {
            tracing::error!("working out what a request does failed");
            vec![ApprovalFact {
                text: CANT_TELL.into(),
                risky: true,
            }]
        }
    }
}
