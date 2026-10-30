// SPDX-License-Identifier: Apache-2.0
//! Servers that programs working in a project's folder listen with on this
//! computer. Only where they listen is read: nothing here connects to them.

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::PathBuf;

/// One listening socket reachable from this computer itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listening {
    /// Loopback, or every address, which loopback reaches too.
    pub address: IpAddr,
    pub port: u16,
    /// The folder the listening program works in.
    pub dir: PathBuf,
    /// The socket itself: a server started again on the same port has a new one.
    pub socket: u64,
}

pub trait LocalServers: Send + Sync + std::fmt::Debug + 'static {
    /// Sockets listened on now by programs working in one of `folders` or a
    /// folder inside one. Other programs' sockets are not looked at.
    /// Blocking: it reads the system's files.
    fn listening(&self, folders: &[PathBuf]) -> Vec<Listening>;
    /// Every socket reachable from this computer itself that is listened on
    /// now, with its port, whatever program holds it: only their numbers are
    /// read, so a server is known to run on while its program works
    /// elsewhere. Blocking.
    fn open(&self) -> HashMap<u64, u16>;
    /// The boot the sockets' numbers belong to, when known: another boot
    /// may number others the same. Blocking.
    fn boot(&self) -> Option<String>;
}
