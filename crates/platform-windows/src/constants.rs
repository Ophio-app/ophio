// SPDX-License-Identifier: Apache-2.0
//! Persisted crypto context is versioned independently of the program's name,
//! so renaming the program does not orphan stored identities.
pub const IDENTITY_ENTROPY: &[u8] = b"host-companion.identity.dpapi.v1";
pub const IDENTITY_LIMIT: usize = 64 * 1024;
/// Bounded room for the DPAPI allocation and versioned envelope overhead.
pub const SEALED_IDENTITY_LIMIT: usize = IDENTITY_LIMIT + 4096;
