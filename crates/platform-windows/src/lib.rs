// SPDX-License-Identifier: Apache-2.0
//! Windows user-session adapters. Native calls are available only on Windows;
//! geometry, input sequencing and security policy are testable on every host.
pub mod background;
pub mod constants;
pub mod desktop;
pub mod dpapi;
pub mod environment;
pub mod fs;
#[cfg(any(windows, test))]
mod gpu;
pub mod launch;
pub mod open_url;
pub mod pipe;
pub mod protection;
pub mod security;
pub mod servers;
pub mod started;
pub mod startup;
pub mod system;

#[cfg(windows)]
#[allow(unsafe_code)]
mod win;

pub mod a11y;
pub mod windows;
