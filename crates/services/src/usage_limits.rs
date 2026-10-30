// SPDX-License-Identifier: Apache-2.0
//! An account out of usage, carried to the agent tool's catalog entry so a
//! device can say so instead of "ready". Starting a run stays allowed: the
//! limit may lift before anyone says so, and only a reply proves it did.
use crate::CompanionHub;
use protocol::{
    ProtocolError,
    capability::{HarnessInfo, HarnessKind, UsageLimit},
    event::Event,
    time::Timestamp,
};

impl CompanionHub {
    pub(crate) fn usage_limited(
        &self,
        harness: HarnessKind,
        resets_at: Option<Timestamp>,
    ) -> Result<(), ProtocolError> {
        let reached_at = self.inner.clock.now();
        self.write(|tx| {
            set(
                tx,
                harness,
                Some(UsageLimit {
                    reached_at,
                    resets_at,
                }),
            )
        })
    }
}

/// A reply from the model on any run proves the account has usage again.
pub(crate) fn usage_returned(tx: &mut store::Write<'_>, harness: HarnessKind) -> store::Result<()> {
    /// The part of a stored catalog entry that says whether there is a limit
    /// to clear. Nearly every agent event finds none, so the rest of the
    /// catalog is not decoded for it.
    #[derive(serde::Deserialize)]
    struct Usage {
        kind: HarnessKind,
        #[serde(default)]
        usage_limit: Option<UsageLimit>,
    }
    // An entry without a limit leaves the key out, so where it appears
    // nowhere there is nothing to clear.
    let Some(json) = tx.read().text("meta", "harnesses")? else {
        return Ok(());
    };
    if !json.contains(r#""usage_limit""#) {
        return Ok(());
    }
    let usage: Vec<Usage> = serde_json::from_str(&json)?;
    if !usage
        .iter()
        .any(|u| u.kind == harness && u.usage_limit.is_some())
    {
        return Ok(());
    }
    set(tx, harness, None)
}

fn set(
    tx: &mut store::Write<'_>,
    harness: HarnessKind,
    limit: Option<UsageLimit>,
) -> store::Result<()> {
    let mut harnesses: Vec<HarnessInfo> = tx.read().get("meta", "harnesses")?.unwrap_or_default();
    let Some(info) = harnesses.iter_mut().find(|h| h.kind == harness) else {
        return Ok(());
    };
    if info.usage_limit == limit {
        return Ok(());
    }
    info.usage_limit = limit;
    tx.put("meta", "harnesses", &harnesses)?;
    tx.emit(Event::HarnessesChanged { harnesses })?;
    Ok(())
}

/// A fresh probe knows nothing of usage; keep a recorded limit until its
/// reset time has passed.
pub(crate) fn carry(probed: &mut [HarnessInfo], stored: &[HarnessInfo], now: Timestamp) {
    for info in probed {
        info.usage_limit = stored
            .iter()
            .find(|s| s.kind == info.kind)
            .and_then(|s| s.usage_limit.clone())
            .filter(|l| l.resets_at.is_none_or(|at| at > now));
    }
}
