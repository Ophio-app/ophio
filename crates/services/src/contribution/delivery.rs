// SPDX-License-Identifier: Apache-2.0
//! Fetch only the inert handoff record named by an explicit phone link/scan.
//! The source is untrusted; HTTPS and its packet digest are transport integrity,
//! never catalog authentication or approval to prepare a workspace.
use super::*;

fn packet(raw: &str, now: u64) -> Result<validate::Packet, Code> {
    let p: validate::Packet = validate::decode(raw)?;
    if p.schema_version != 1
        || p.kind != "contribution-import"
        || !validate::id(&p.packet_id)
        || !validate::hex(&p.handoff_sha256, 64)
        || p.issued_at > protocol::MAX_SAFE_INTEGER
        || p.expires_at > protocol::MAX_SAFE_INTEGER
    {
        return Err(Code::Malformed);
    }
    validate::lifetime(&p, now)?;
    // CH-060 emits a fresh ten-minute packet on each new Open/Resume.
    if p.expires_at - p.issued_at > 600 {
        return Err(Code::InvalidLifetime);
    }
    Ok(p)
}

impl CompanionHub {
    pub(crate) async fn contribution_delivery(
        &self,
        caller: &Caller,
        packet_json: &str,
        handoff_url: &str,
    ) -> Result<String, ProtocolError> {
        let check = || -> Result<(), ProtocolError> {
            self.read(|r| local(&Self::current_caller(r, caller)?).map_err(Into::into))?;
            if *self.inner.shutdown.borrow() {
                return Err(failure(Code::Canceled));
            }
            packet(packet_json, seconds(self)).map_err(failure)?;
            Ok(())
        };
        check()?;
        validate::repository(handoff_url).map_err(failure)?;
        let _slot = self
            .inner
            .contributions
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| failure(Code::Busy))?;
        let source = self
            .inner
            .contributions
            .source
            .lock()
            .map_err(|_| failure(Code::ReconcileRequired))?
            .clone();
        let fetch = source.handoff(handoff_url);
        tokio::pin!(fetch);
        let start = Instant::now();
        let mut interval = tokio::time::interval(Duration::from_millis(100));
        let bytes = loop {
            tokio::select! {
                response = &mut fetch => break response.map_err(failure)?,
                _ = interval.tick() => {
                    if start.elapsed() >= Duration::from_secs(18) { return Err(failure(Code::AcquisitionFailed)); }
                    check()?;
                }
            }
        };
        check()?;
        if bytes.len() > RECORD_BYTES {
            return Err(failure(Code::LimitExceeded));
        }
        let raw = String::from_utf8(bytes).map_err(|_| failure(Code::InvalidJson))?;
        validate::records(packet_json, &raw).map_err(failure)?;
        // Return the exact UTF-8 string, including whitespace. No write to the
        // receiver journal, workspace, approvals, project list or queue occurs.
        Ok(raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn delivery_packets_are_bounded_and_fresh() {
        let raw = |issued, expires| {
            serde_json::json!({"schemaVersion":1,"kind":"contribution-import",
            "packetId":"fixture","issuedAt":issued,"expiresAt":expires,"handoffSha256":"a".repeat(64)}).to_string()
        };
        assert!(packet(&raw(10, 610), 10).is_ok());
        assert_eq!(
            packet(&raw(10, 611), 10).unwrap_err(),
            Code::InvalidLifetime
        );
        assert_eq!(packet(&raw(10, 610), 610).unwrap_err(), Code::Expired);
        assert_eq!(packet(&raw(10, 610), 9).unwrap_err(), Code::NotYetValid);
        assert_eq!(
            packet(&" ".repeat(RECORD_BYTES + 1), 10).unwrap_err(),
            Code::LimitExceeded
        );
    }
}
