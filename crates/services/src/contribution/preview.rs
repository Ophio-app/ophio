// SPDX-License-Identifier: Apache-2.0
//! Read-only rule inspection. No workspace, approval or trust is created here.
use super::*;

// Fit inside the phone's 20-second query timeout. The normal acquisition byte,
// object and concurrency ceilings apply even though nothing reaches disk.
const TIME: Duration = Duration::from_secs(18);

impl CompanionHub {
    fn check_policy_preview(&self, caller: &Caller, id: &str) -> Result<Record, ProtocolError> {
        self.read(|r| {
            let caller = Self::current_caller(r, caller)?;
            let rec = owned(r, &caller, id)?;
            let (packet, handoff) = rec.parsed().map_err(failure)?;
            validate::lifetime(&packet, seconds(self)).map_err(failure)?;
            validate::compatibility(&handoff).map_err(failure)?;
            if *self.inner.shutdown.borrow() || rec.view.code == Code::Canceled {
                return Err(failure(Code::Canceled).into());
            }
            if rec.input.host_id != self.inner.host_id {
                return Err(failure(Code::HostUnavailable).into());
            }
            check_saved_root(r, &rec).map_err(failure)?;
            Ok(rec)
        })
    }

    fn check_preview_guard(
        &self,
        caller: &Caller,
        rec: &Record,
        packet: &validate::Packet,
    ) -> Result<(), Code> {
        self.read(|r| {
            let current = Self::current_caller(r, caller)?;
            local(&current)?;
            let guard: Guard = r.require("contribution_guard", &rec.view.operation_id)?;
            // Preparation can converge onto an older operation. Its new command
            // must not revive a preview canceled during the previous attempt.
            if guard.owner != current.principal
                || guard.root != rec.root
                || guard.command != rec.command
            {
                return Err(failure(Code::TrustRequired).into());
            }
            if guard.code == Code::Canceled || *self.inner.shutdown.borrow() {
                return Err(failure(Code::Canceled).into());
            }
            validate::lifetime(packet, seconds(self)).map_err(failure)?;
            check_saved_root(r, rec).map_err(failure)?;
            Ok(())
        })
        .map_err(|_| Code::TrustRequired)
    }

    pub(crate) async fn contribution_policy(
        &self,
        caller: &Caller,
        id: &str,
        path: &str,
    ) -> Result<ContributionPolicyDocument, ProtocolError> {
        let rec = self.check_policy_preview(caller, id)?;
        let (packet, handoff) = rec.parsed().map_err(failure)?;
        let policy = handoff
            .policies
            .iter()
            .find(|p| p.path == path)
            .ok_or_else(|| failure(Code::PolicyMismatch))?
            .clone();
        let check = || self.check_preview_guard(caller, &rec, &packet);
        let slot = self
            .inner
            .contributions
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| failure(Code::Busy))?;
        let start = Instant::now();
        let source = self
            .inner
            .contributions
            .source
            .lock()
            .map_err(|_| failure(Code::ReconcileRequired))?
            .clone();
        let received = AtomicU64::new(0);
        let bytes = {
            let fetch = source.pack(
                &handoff.source.repository_url,
                &handoff.source.commit,
                &received,
            );
            tokio::pin!(fetch);
            let mut interval = tokio::time::interval(Duration::from_millis(100));
            loop {
                tokio::select! {
                    response = &mut fetch => break response.map_err(failure)?,
                    _ = interval.tick() => {
                        if start.elapsed() >= TIME { return Err(failure(Code::AcquisitionFailed)); }
                        check().map_err(failure)?;
                    }
                }
            }
        };
        let hub = self.clone();
        let caller = caller.clone();
        tokio::task::spawn_blocking(move || {
            // Keep the slot until decoding actually stops, even if the caller
            // disconnects while this blocking work is in flight.
            let _slot = slot;
            let check = || {
                if start.elapsed() >= TIME {
                    return Err(Code::AcquisitionFailed);
                }
                hub.check_preview_guard(&caller, &rec, &packet)
            };
            check().map_err(failure)?;
            let tree = pack::decode(&bytes, &handoff.source.commit, &check).map_err(failure)?;
            let document = document(&tree, &handoff, &policy).map_err(failure)?;
            check().map_err(failure)?;
            Ok(document)
        })
        .await
        .map_err(|_| failure(Code::AcquisitionFailed))?
    }
}

fn document(
    tree: &pack::Tree,
    handoff: &validate::Handoff,
    selected: &ContributionPolicy,
) -> Result<ContributionPolicyDocument, Code> {
    let mut total = 0;
    let mut result = None;
    // Check the complete policy set's pins and limits before disclosing any
    // file. pack::decode already rejects links, submodules and unsafe paths.
    for policy in &handoff.policies {
        let bytes = &tree
            .files
            .get(&policy.path)
            .and_then(|file| tree.objects.get(&file.oid))
            .ok_or(Code::PolicyMismatch)?
            .bytes;
        total += bytes.len();
        if bytes.len() > 256 * 1024
            || total > 2 * 1024 * 1024
            || validate::digest(bytes) != policy.sha256
        {
            return Err(Code::PolicyMismatch);
        }
        if policy == selected {
            let text = std::str::from_utf8(bytes).map_err(|_| Code::UnsafeObject)?;
            // A JSON control frame must fit even with string escaping. NUL
            // and other binary controls are not readable rule documents.
            if text
                .chars()
                .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
            {
                return Err(Code::UnsafeObject);
            }
            result = Some(ContributionPolicyDocument {
                policy: policy.clone(),
                size_bytes: bytes.len() as u32,
                text: text.into(),
            });
        }
    }
    result.ok_or(Code::PolicyMismatch)
}
