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
    connect_to(&name, &owner).await
}

async fn connect_to(name: &str, owner: &UserSid) -> io::Result<NamedPipeClient> {
    use std::time::Duration;
    use tokio::time::{Instant, sleep_until};
    use windows::Win32::Foundation::ERROR_PIPE_BUSY;

    // A connection consumes the listener's pending instance until accept creates
    // its replacement. Yield without blocking the runtime while that short gap
    // closes. A fixed deadline also bounds a server that never accepts again.
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        // Never allow the server to impersonate the CLI, including on retries.
        match ClientOptions::new()
            .security_qos_flags(SECURITY_IDENTIFICATION.0 | SECURITY_SQOS_PRESENT.0)
            .open(name)
        {
            Ok(client) => {
                verify_peer(HANDLE(client.as_raw_handle()), owner, false)?;
                return Ok(client);
            }
            Err(error)
                if error.raw_os_error() == Some(ERROR_PIPE_BUSY.0 as i32)
                    && Instant::now() < deadline =>
            {
                sleep_until((Instant::now() + Duration::from_millis(10)).min(deadline)).await;
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::time::Instant;

    fn fixture() -> OwnerListener {
        let owner = owner_sid().unwrap();
        // Never occupy the running companion's exclusive owner pipe.
        let name = format!(
            "{}-busy-test-{}",
            crate::security::pipe_name(&owner),
            protocol::ids::HostId::new()
        );
        OwnerListener {
            pending: instance(&name, &owner, true).unwrap(),
            owner,
            name,
        }
    }

    #[tokio::test]
    async fn busy_pipe_waits_for_the_next_pending_instance() {
        let mut listener = fixture();
        let held = connect_to(&listener.name, &listener.owner).await.unwrap();
        let busy = ClientOptions::new().open(&listener.name).unwrap_err();
        assert_eq!(busy.raw_os_error(), Some(231));
        let started = Instant::now();
        let name = listener.name.clone();
        let owner = listener.owner.clone();
        let (client, accepted) = tokio::join!(connect_to(&name, &owner), async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            listener.accept().await
        });
        let client = client.expect("a briefly busy companion is still available");
        let accepted = accepted.unwrap();
        assert!(started.elapsed() >= Duration::from_millis(100));
        assert!(started.elapsed() < Duration::from_secs(3));
        assert_eq!(server_process_id(&client).unwrap(), std::process::id());
        assert_eq!(client_process_id(&accepted).unwrap(), std::process::id());
        drop((held, accepted, client));
    }

    #[tokio::test]
    async fn permanently_busy_pipe_has_a_bounded_wait() {
        let listener = fixture();
        let _held = connect_to(&listener.name, &listener.owner).await.unwrap();
        let started = Instant::now();
        let error = connect_to(&listener.name, &listener.owner)
            .await
            .unwrap_err();
        assert_eq!(error.raw_os_error(), Some(231));
        assert!(started.elapsed() >= Duration::from_secs(2));
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[tokio::test]
    async fn wrong_server_identity_is_not_retried() {
        let listener = fixture();
        let other = UserSid::parse("S-1-5-32-544").unwrap();
        let started = Instant::now();
        let error = connect_to(&listener.name, &other).await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn missing_pipe_does_not_wait() {
        let listener = fixture();
        let name = listener.name.clone();
        let owner = listener.owner.clone();
        drop(listener);
        let started = Instant::now();
        let error = connect_to(&name, &owner).await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
