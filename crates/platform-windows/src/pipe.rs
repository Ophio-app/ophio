// SPDX-License-Identifier: Apache-2.0
//! Local byte-stream owner pipe. Authentication precedes application framing.
pub use crate::security::pipe_name;
#[cfg(windows)]
pub use crate::win::pipe::{OwnerListener, client_process_id, connect, server_process_id};
