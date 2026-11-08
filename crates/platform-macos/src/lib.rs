// SPDX-License-Identifier: Apache-2.0
//! Mac adapters for the logged-in user's session.
#![cfg(target_os = "macos")]
pub mod desktop;
pub mod process;
pub mod protection;
pub mod servers;
pub mod system;
