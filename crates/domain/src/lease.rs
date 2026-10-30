// SPDX-License-Identifier: Apache-2.0
use protocol::{BootEpoch, control::*};

#[derive(Debug, Clone)]
pub struct Fence {
    pub lease: LeaseView,
    pub geometry_revision: u64,
    pub last_seq: u64,
    pub deadline_ms: u64,
}
impl Fence {
    pub fn admit(
        &mut self,
        input: &InputEnvelope,
        epoch: BootEpoch,
        now_ms: u64,
    ) -> Result<(), InputOutcome> {
        if self.lease.state != LeaseState::Active
            || self.lease.boot_epoch != epoch
            || input.lease_id != self.lease.lease_id
            || input.generation != self.lease.generation
            || now_ms >= self.deadline_ms
        {
            return Err(InputOutcome::StaleLease);
        }
        if input.geometry_revision != self.geometry_revision {
            return Err(InputOutcome::StaleGeometry);
        }
        if input.seq <= self.last_seq {
            return Err(InputOutcome::OutOfOrder);
        }
        if !valid_action(&input.action) {
            return Err(InputOutcome::Failed);
        }
        self.last_seq = input.seq;
        Ok(())
    }
}
pub fn private_ready(view: &PrivateStepView) -> bool {
    view.input_stopped == CoverageFact::Acknowledged
        && view.managed_screenshots_stopped == CoverageFact::Acknowledged
        && view.uncovered_routes.is_empty()
}
pub fn valid_action(action: &InputAction) -> bool {
    let point = |p: &TargetPoint| {
        p.x.is_finite()
            && p.y.is_finite()
            && (0.0..=1.0).contains(&p.x)
            && (0.0..=1.0).contains(&p.y)
    };
    match action {
        InputAction::PointerMove { to } => point(to),
        InputAction::PointerButton { at, .. } => point(at),
        InputAction::Click { at, count, .. } => point(at) && (1..=3).contains(count),
        InputAction::Scroll { at, dx, dy } => {
            point(at)
                && dx.is_finite()
                && dy.is_finite()
                && dx.abs() <= 10000.0
                && dy.abs() <= 10000.0
        }
        InputAction::Text { text } => text.len() <= 65536,
        InputAction::KeyChord { keys } => {
            !keys.is_empty()
                && keys.len() <= 16
                && keys.iter().all(|k| !k.is_empty() && k.len() <= 64)
        }
        InputAction::KeyPress { key, .. } => !key.is_empty() && key.len() <= 64,
        InputAction::ReleaseAll => true,
        InputAction::Paste { text } => text.len() <= 65536,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{DeviceId, LeaseId, TargetId, time::Timestamp};
    #[test]
    fn all_fences_and_private_coverage_are_independent() {
        let epoch = BootEpoch::new();
        let mut fence = Fence {
            lease: LeaseView {
                lease_id: LeaseId::new(),
                target_id: TargetId::new(),
                owner: LeaseOwner::Human {
                    device_id: DeviceId::new(),
                },
                generation: 7,
                boot_epoch: epoch,
                state: LeaseState::Active,
                granted_at: Timestamp(0),
                renewal_window_ms: 3000,
            },
            geometry_revision: 2,
            last_seq: 0,
            deadline_ms: 3000,
        };
        let mut input = InputEnvelope {
            lease_id: fence.lease.lease_id,
            generation: 7,
            geometry_revision: 2,
            seq: 1,
            action: InputAction::ReleaseAll,
        };
        assert_eq!(
            fence.admit(&input, BootEpoch::new(), 0),
            Err(InputOutcome::StaleLease)
        );
        input.generation = 6;
        assert_eq!(fence.admit(&input, epoch, 0), Err(InputOutcome::StaleLease));
        input.generation = 7;
        input.geometry_revision = 1;
        assert_eq!(
            fence.admit(&input, epoch, 0),
            Err(InputOutcome::StaleGeometry)
        );
        input.geometry_revision = 2;
        assert_eq!(fence.admit(&input, epoch, 0), Ok(()));
        assert_eq!(fence.admit(&input, epoch, 0), Err(InputOutcome::OutOfOrder));
        input.seq = 2;
        assert_eq!(
            fence.admit(&input, epoch, 3000),
            Err(InputOutcome::StaleLease)
        );
        let mut private = PrivateStepView {
            requested_at: Timestamp(0),
            input_stopped: CoverageFact::Acknowledged,
            managed_screenshots_stopped: CoverageFact::Acknowledged,
            uncovered_routes: Vec::new(),
            prior_observations_delivered: 3,
            ready: false,
        };
        assert!(private_ready(&private));
        for fact in [
            CoverageFact::Requested,
            CoverageFact::Failed,
            CoverageFact::Unknown,
            CoverageFact::NotApplicable,
        ] {
            private.input_stopped = fact;
            assert!(!private_ready(&private));
            private.input_stopped = CoverageFact::Acknowledged;
            private.managed_screenshots_stopped = fact;
            assert!(!private_ready(&private));
            private.managed_screenshots_stopped = CoverageFact::Acknowledged;
        }
        private.uncovered_routes.push("Independent capture".into());
        assert!(!private_ready(&private));
    }
}
