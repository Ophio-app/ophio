// SPDX-License-Identifier: Apache-2.0
use super::*;
use std::os::windows::io::AsRawHandle;
use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
};
use windows::Win32::{
    Storage::FileSystem::{SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT},
    System::Pipes::*,
};

#[derive(Debug)]
pub struct OwnerListener {
    pending: NamedPipeServer,
    owner: UserSid,
    name: String,
}
fn instance(name: &str, owner: &UserSid, first: bool) -> io::Result<NamedPipeServer> {
    let sd = Descriptor::new(owner, ObjectKind::Pipe)?;
    let sa = sd.attributes();
    // SAFETY: Tokio passes this descriptor only to synchronous CreateNamedPipe.
    // The server is byte-mode, overlapped, local-only and reserves the name.
    unsafe {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .max_instances(34)
            .create_with_security_attributes_raw(
                name,
                (&sa as *const SECURITY_ATTRIBUTES).cast_mut().cast(),
            )
    }
}
fn verify_peer(handle: HANDLE, owner: &UserSid, client: bool) -> io::Result<()> {
    let mut pid = 0;
    // SAFETY: live connected pipe and valid output PID pointer.
    unsafe {
        if client {
            GetNamedPipeClientProcessId(handle, &mut pid)
        } else {
            GetNamedPipeServerProcessId(handle, &mut pid)
        }
        .map_err(failure)?;
        let process =
            Handle(OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).map_err(failure)?);
        if &process_sid(process.0)? != owner {
            return Err(denied());
        }
    }
    Ok(())
}
impl OwnerListener {
    pub fn bind() -> io::Result<Self> {
        let owner = owner_sid()?;
        let name = crate::security::pipe_name(&owner);
        Ok(Self {
            pending: instance(&name, &owner, true)?,
            owner,
            name,
        })
    }
    /// Connected stream has passed token-user verification before it is returned.
    pub async fn accept(&mut self) -> io::Result<NamedPipeServer> {
        self.pending.connect().await?;
        // Retain at least one pipe instance for the entire listener lifetime.
        let next = instance(&self.name, &self.owner, false)?;
        let connected = std::mem::replace(&mut self.pending, next);
        verify_peer(HANDLE(connected.as_raw_handle()), &self.owner, true).map_err(|_| denied())?;
        Ok(connected)
    }
}
/// The process id of a connected owner pipe's client.
pub fn client_process_id(pipe: &NamedPipeServer) -> io::Result<u32> {
    let mut pid = 0;
    // SAFETY: live connected pipe and valid output PID pointer.
    unsafe { GetNamedPipeClientProcessId(HANDLE(pipe.as_raw_handle()), &mut pid) }
        .map_err(failure)?;
    Ok(pid)
}
/// The process id of a connected owner pipe's server.
pub fn server_process_id(pipe: &NamedPipeClient) -> io::Result<u32> {
    let mut pid = 0;
    // SAFETY: live connected pipe and valid output PID pointer.
    unsafe { GetNamedPipeServerProcessId(HANDLE(pipe.as_raw_handle()), &mut pid) }
        .map_err(failure)?;
    Ok(pid)
}
pub async fn connect() -> io::Result<NamedPipeClient> {
    let owner = owner_sid()?;
    let name = crate::security::pipe_name(&owner);
    // Never allow the server to impersonate the CLI. Busy is returned to the
    // caller; there is no unbounded retry or fallback to a different transport.
    let client = ClientOptions::new()
        .security_qos_flags(SECURITY_IDENTIFICATION.0 | SECURITY_SQOS_PRESENT.0)
        .open(&name)?;
    verify_peer(HANDLE(client.as_raw_handle()), &owner, false)?;
    Ok(client)
}
