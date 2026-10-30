// SPDX-License-Identifier: Apache-2.0
//! Current-user DPAPI envelopes with a stable application entropy context.
//! No plaintext fallback or machine-wide protection is supported.
#[cfg(windows)]
pub use crate::win::dpapi::{SecretStore, protect, unprotect};

#[cfg(any(windows, test))]
pub(crate) mod envelope {
    use crate::constants::SEALED_IDENTITY_LIMIT;
    use std::io;

    // Persistent format: never accept raw plaintext or silently try other versions.
    const HEADER: &[u8] = b"DPAPI-IDENTITY-1\0";
    pub(crate) const SEALED_LIMIT: usize = SEALED_IDENTITY_LIMIT;

    fn invalid() -> io::Error {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid protected secret envelope",
        )
    }
    pub(crate) fn encode(ciphertext: &[u8]) -> io::Result<Vec<u8>> {
        if ciphertext.is_empty() || ciphertext.len() > SEALED_LIMIT - HEADER.len() {
            return Err(invalid());
        }
        let mut bytes = Vec::with_capacity(HEADER.len() + ciphertext.len());
        bytes.extend_from_slice(HEADER);
        bytes.extend_from_slice(ciphertext);
        Ok(bytes)
    }
    pub(crate) fn payload(bytes: &[u8]) -> io::Result<&[u8]> {
        if bytes.len() > SEALED_LIMIT {
            return Err(invalid());
        }
        bytes
            .strip_prefix(HEADER)
            .filter(|b| !b.is_empty())
            .ok_or_else(invalid)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn version_one_is_stable_and_round_trips_opaque_bytes() {
            let bytes = encode(b"synthetic ciphertext").unwrap();
            assert_eq!(bytes, b"DPAPI-IDENTITY-1\0synthetic ciphertext");
            assert_eq!(payload(&bytes).unwrap(), b"synthetic ciphertext");
            assert_eq!(
                crate::constants::IDENTITY_ENTROPY,
                b"host-companion.identity.dpapi.v1"
            );
        }
        #[test]
        fn malformed_unknown_empty_and_oversized_envelopes_fail_closed() {
            for bytes in [
                b"plaintext".as_slice(),
                b"DPAPI-IDENTITY-2\0opaque",
                HEADER,
                &HEADER[..5],
                b"",
            ] {
                assert!(payload(bytes).is_err());
            }
            assert!(encode(b"").is_err());
            let at_limit = encode(&vec![0x53; SEALED_LIMIT - HEADER.len()]).unwrap();
            assert!(payload(&at_limit).is_ok());
            let mut oversized = at_limit;
            oversized.push(0);
            assert!(payload(&oversized).is_err());
            assert!(encode(&vec![0x53; SEALED_LIMIT]).is_err());
        }
    }
}
