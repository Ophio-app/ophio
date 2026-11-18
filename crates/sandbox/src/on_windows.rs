// SPDX-License-Identifier: Apache-2.0
//! Windows has no conversation sandbox yet. This says so, and refuses every
//! chat or task with limits, so none starts without them. It builds on every
//! system so the refusal is tested everywhere; only Windows composes it.
use crate::error;
use ports::{
    HarnessError,
    sandbox::{Sandbox, SandboxSpec, Sandboxes},
};
use protocol::capability::Availability;
use std::sync::Arc;

/// Said to the person, on the phone, where a chat's limits would be chosen.
const REASON: &str = "Limited chats aren’t available on Windows yet, so Øphio can’t keep a chat to one project or off the internet on this computer.";

/// The sandbox provider on Windows: unavailable, and it refuses every sandbox.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoWindowsSandbox;

impl Sandboxes for NoWindowsSandbox {
    fn availability(&self) -> Availability {
        Availability::Unsupported {
            reason: REASON.into(),
        }
    }
    fn create(&self, spec: SandboxSpec) -> Result<Arc<dyn Sandbox>, HarnessError> {
        if spec.reach.unrestricted() {
            return Err(error("unrestricted runs must not create a sandbox"));
        }
        Err(error(
            "This chat has limits, and limited chats aren’t available on Windows yet, so it wasn’t started.",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ports::sandbox::Reach;
    use protocol::capability::HarnessKind;
    use std::path::PathBuf;

    fn spec(reach: Reach) -> SandboxSpec {
        SandboxSpec {
            offline: false,
            reach,
            harness: HarnessKind::ClaudeCode,
            cwd: PathBuf::from("project"),
            state: PathBuf::from("state"),
            always: Vec::new(),
            programs: Vec::new(),
            candidates: Vec::new(),
            private: Vec::new(),
            given: Vec::new(),
            on_blocked: None,
            relay: None,
        }
    }

    #[test]
    fn says_it_is_unavailable_in_plain_words() {
        let Availability::Unsupported { reason } = NoWindowsSandbox.availability() else {
            panic!("Windows must not say limited chats are available");
        };
        assert!(reason.contains("Windows"));
        assert!(!reason.contains("Linux"));
    }

    #[test]
    fn refuses_every_sandbox() {
        let project = Reach {
            read: vec![PathBuf::from("project")],
            write: Vec::new(),
            whole_read: false,
            whole_write: false,
            internet: false,
            local_network: false,
        };
        let everything = Reach {
            read: Vec::new(),
            write: Vec::new(),
            whole_read: true,
            whole_write: true,
            internet: true,
            local_network: true,
        };
        let offline_everything = Reach {
            internet: false,
            ..everything.clone()
        };
        for reach in [project, offline_everything, everything] {
            let refused = NoWindowsSandbox.create(spec(reach.clone()));
            assert!(
                matches!(refused, Err(HarnessError::SpawnFailed(_))),
                "{reach:?} was not refused"
            );
        }
    }
}
