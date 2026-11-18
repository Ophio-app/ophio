// SPDX-License-Identifier: Apache-2.0
//! Mac adapters for the logged-in user's session.
pub mod a11y;
#[cfg(target_os = "macos")]
pub mod desktop;
pub mod launch;
#[cfg(target_os = "macos")]
pub mod process;
#[cfg(target_os = "macos")]
pub mod protection;
#[cfg(target_os = "macos")]
pub mod servers;
#[cfg(target_os = "macos")]
pub mod system;
pub mod windows;
