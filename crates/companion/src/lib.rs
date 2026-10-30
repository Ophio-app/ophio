// SPDX-License-Identifier: Apache-2.0
//! TLS transport and local owner interface for the companion.

pub mod agent_updates;
mod alerts;
pub mod cli;
pub mod compose;
pub mod config;
mod control;
pub mod error;
pub mod frontdoors;
pub mod identity;
pub mod install;
pub mod local;
mod local_protection;
pub mod network;
mod person_only;
pub mod phone;
pub mod readable;
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
