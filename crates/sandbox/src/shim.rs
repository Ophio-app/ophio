// SPDX-License-Identifier: Apache-2.0
//! Unprivileged in-namespace TCP-to-Unix relay and agent supervisor, and the
//! helpers that change a running sandbox or connect inside it.
use std::{io, path::Path};
use tokio::net::{TcpListener, UnixStream};

pub async fn forward(listener: TcpListener, socket: &Path) -> io::Result<()> {
    let mut tasks = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept(), if tasks.len() < 128 => {
                let (mut tcp, _) = accepted?;
                let path = socket.to_owned();
                tasks.spawn(async move {
                    if let Ok(mut unix) = UnixStream::connect(path).await {
                        let _ = tokio::io::copy_bidirectional(&mut tcp, &mut unix).await;
                    }
                });
            }
            _ = tasks.join_next(), if !tasks.is_empty() => {}
        }
    }
}

pub fn main() -> io::Result<i32> {
    let mut args = std::env::args_os().skip(1);
    let mode = args
        .next()
        .ok_or_else(|| io::Error::other("missing shim mode"))?;
    if mode == "mount" {
        return crate::sys::mount_helper(args.collect());
    }
    if mode == "connect" {
        return crate::sys::connect_helper(args.collect());
    }
    if mode != "run" {
        return Err(io::Error::other("unknown shim mode"));
    }
    let socket = args
        .next()
        .ok_or_else(|| io::Error::other("missing socket"))?;
    let program = args
        .next()
        .ok_or_else(|| io::Error::other("missing program"))?;
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let socket = std::path::PathBuf::from(socket);
            let task = if socket == Path::new("-") {
                None
            } else {
                let listener = TcpListener::bind("127.0.0.1:18080").await?;
                Some(tokio::spawn(
                    async move { forward(listener, &socket).await },
                ))
            };
            let status = tokio::process::Command::new(program)
                .args(args)
                .spawn()?
                .wait()
                .await?;
            if let Some(task) = task {
                task.abort();
            }
            use std::os::unix::process::ExitStatusExt;
            Ok(status
                .code()
                .unwrap_or_else(|| 128 + status.signal().unwrap_or(1)))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    #[tokio::test]
    async fn forwards_both_directions_and_half_close() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("socket");
        let unix = tokio::net::UnixListener::bind(&socket).unwrap();
        let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = tcp.local_addr().unwrap();
        let forwarder = tokio::spawn(async move { forward(tcp, &socket).await });
        let server = tokio::spawn(async move {
            let (mut connection, _) = unix.accept().await.unwrap();
            let mut message = Vec::new();
            connection.read_to_end(&mut message).await.unwrap();
            connection.write_all(&message).await.unwrap();
        });
        let mut client = tokio::net::TcpStream::connect(address).await.unwrap();
        client.write_all(b"hello relay").await.unwrap();
        client.shutdown().await.unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        assert_eq!(response, b"hello relay");
        server.await.unwrap();
        forwarder.abort();
    }
}
