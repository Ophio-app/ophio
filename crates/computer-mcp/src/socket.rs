// SPDX-License-Identifier: Apache-2.0
use crate::BridgeError;
use std::path::Path;

#[cfg(unix)]
pub(crate) type Stream = tokio::net::UnixStream;
#[cfg(windows)]
pub(crate) type Stream = tokio::net::windows::named_pipe::NamedPipeClient;

#[cfg(unix)]
pub(crate) async fn connect(path: &Path) -> Result<Stream, BridgeError> {
    Ok(Stream::connect(path).await?)
}

#[cfg(windows)]
pub(crate) async fn connect(path: &Path) -> Result<Stream, BridgeError> {
    use tokio::net::windows::named_pipe::ClientOptions;
    // A server instance can be briefly busy between accepts. Bound retries.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match ClientOptions::new().open(path) {
                Ok(pipe) => return Ok(pipe),
                Err(e) if e.raw_os_error() == Some(231) => {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await
                }
                Err(e) => return Err(BridgeError::Io(e)),
            }
        }
    })
    .await
    .map_err(|_| BridgeError::Timeout)?
}

#[cfg(windows)]
pub(crate) fn listener(
    path: &Path,
) -> Result<
    interprocess::os::windows::named_pipe::tokio::PipeListener<
        interprocess::os::windows::named_pipe::pipe_mode::Bytes,
        interprocess::os::windows::named_pipe::pipe_mode::Bytes,
    >,
    BridgeError,
> {
    use interprocess::os::windows::{
        named_pipe::{PipeListenerOptions, pipe_mode},
        security_descriptor::SecurityDescriptor,
    };
    if !path.to_string_lossy().starts_with(r"\\.\pipe\") {
        return Err(BridgeError::PrivateDirectory);
    }
    // Protected DACL: only the object's owner and LocalSystem. The safe
    // platform library also claims the first pipe instance before accepting.
    let sddl = widestring::U16CString::from_str("D:P(A;;GA;;;OW)(A;;GA;;;SY)")
        .map_err(|_| BridgeError::Protocol)?;
    let descriptor = SecurityDescriptor::deserialize(&sddl)?;
    Ok(PipeListenerOptions::new()
        .path(path.as_os_str())
        .accept_remote(false)
        .inheritable(false)
        .security_descriptor(Some(descriptor))
        .create_tokio_duplex::<pipe_mode::Bytes>()?)
}
