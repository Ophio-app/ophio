// SPDX-License-Identifier: Apache-2.0
//! The job every program the companion starts inherits, so the owner pipe can
//! tell those programs from the owner's own.
#[cfg(windows)]
pub use crate::win::started::Job;
