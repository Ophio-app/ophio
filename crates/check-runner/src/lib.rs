// SPDX-License-Identifier: Apache-2.0
//! Project snapshots and independently run checks.
#[cfg(target_os = "macos")]
mod macos_process;
pub mod runner;
pub mod snapshot;
pub mod toolchains;
