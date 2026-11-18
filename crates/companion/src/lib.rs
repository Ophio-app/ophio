// SPDX-License-Identifier: Apache-2.0
//! TLS transport and local owner interface for the companion.

/// Prints a line to standard output and carries on if nothing reads it any
/// more, as when `ophio update X | head -3` has its lines: an update or an
/// install must never stop halfway because its reader went away. `println!`
/// panics then instead.
macro_rules! say {
    () => {
        $crate::say_line(format_args!(""))
    };
    ($($arg:tt)*) => {
        $crate::say_line(format_args!($($arg)*))
    };
}

#[doc(hidden)]
pub fn say_line(line: std::fmt::Arguments<'_>) {
    use std::io::Write;
    let _ = writeln!(std::io::stdout().lock(), "{line}");
}

mod agent_release_notes;
pub mod agent_updates;
mod alerts;
pub mod cli;
pub mod compose;
pub mod config;
mod control;
pub mod error;
#[doc(hidden)]
pub mod executable;
pub mod frontdoors;
pub mod identity;
pub mod install;
pub mod local;
mod local_protection;
pub mod network;
mod person_only;
pub mod phone;
pub mod readable;
pub mod release_check;
pub mod reports;
pub mod server;
pub mod setup;
mod side;
pub mod started;
pub mod update;
mod wire;

pub use error::{Error, Result};
pub use server::{Server, ServerDeps};

mod named_targets;

pub mod jev;

mod observed;
mod preview;
mod voice;
