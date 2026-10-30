// SPDX-License-Identifier: Apache-2.0
//! What `ophio run` needs to know to keep running without a window: whether
//! Windows opened a console for this process alone, as it does at login or
//! from Explorer, and whether the process runs as administrator; how to start
//! Øphio without handing it this command's output; and, for
//! `ophio service uninstall`, when the Øphio it stopped has ended.
#[cfg(windows)]
pub use crate::win::background::{Process, elevated, keep_std_handles, own_console};
