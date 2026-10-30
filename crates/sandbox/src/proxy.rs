// SPDX-License-Identifier: Apache-2.0
//! CONNECT-only proxy. Host policy is checked before DNS and throughout tunnels.
use ports::sandbox::{Blocked, OnBlocked};
use protocol::capability::HarnessKind;
use std::{
    io,
    path::{Path, PathBuf},
    thread,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpStream, UnixListener, UnixStream},
    sync::watch,
    task::JoinSet,
};

const CLAUDE: &[&str] = &[
    "api.anthropic.com",
    "statsig.anthropic.com",
    "console.anthropic.com",
    "claude.ai",
    "platform.claude.com",
];
const CODEX: &[&str] = &[
    "api.openai.com",
    "chatgpt.com",
    "auth.openai.com",
    "ab.chatgpt.com",
];

pub fn allowed(kind: HarnessKind, internet: bool, host: &str, port: u16) -> bool {
    internet
        || (port == 443
            && match kind {
                HarnessKind::ClaudeCode => CLAUDE,
                HarnessKind::Codex => CODEX,
                _ => &[],
            }
            .contains(&host))
}

/// What a run may reach through the proxy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Policy {
    pub internet: bool,
    /// With the internet, the person's own networks too: private addresses
    /// such as their LAN or tailnet, never this computer's own loopback.
    pub local: bool,
}

pub(crate) struct Proxy {
    directory: tempfile::TempDir,
    policy: watch::Sender<Option<Policy>>,
    worker: Option<thread::JoinHandle<()>>,
}
impl std::fmt::Debug for Proxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Proxy")
            .field("socket", &self.socket())
            .finish_non_exhaustive()
    }
}
impl Proxy {
    pub fn start(
        state: &Path,
        kind: HarnessKind,
        policy: Policy,
        blocked: Option<OnBlocked>,
    ) -> io::Result<Self> {
        Self::start_inner(state, kind, policy, blocked, false)
    }
    fn start_inner(
        state: &Path,
        kind: HarnessKind,
        policy: Policy,
        blocked: Option<OnBlocked>,
        allow_loopback: bool,
    ) -> io::Result<Self> {
        let directory = tempfile::Builder::new()
            .prefix("proxy-")
            .tempdir_in(state)?;
        let listener = crate::sys::bind_unix(directory.path(), "connect.sock")?;
        listener.set_nonblocking(true)?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let (policy, mut changes) = watch::channel(Some(policy));
        let worker = thread::Builder::new().name("sandbox-proxy".into()).spawn(move || runtime.block_on(async move {
            let Ok(listener) = UnixListener::from_std(listener) else { return };
            let mut tasks = JoinSet::new();
            loop {
                tokio::select! {
                    biased;
                    changed = changes.changed() => {
                        if changed.is_err() || changes.borrow_and_update().is_none() { break; }
                    }
                    accepted = listener.accept(), if tasks.len() < 128 => {
                        let Ok((stream, _)) = accepted else { break };
                        let changes = changes.clone();
                        let blocked = blocked.clone();
                        tasks.spawn(async move { let _ = serve(stream, kind, changes, blocked, allow_loopback).await; });
                    }
                    _ = tasks.join_next(), if !tasks.is_empty() => {}
                }
            }
            tasks.abort_all();
            while tasks.join_next().await.is_some() {}
        }))?;
        Ok(Self {
            directory,
            policy,
            worker: Some(worker),
        })
    }
    pub fn socket(&self) -> PathBuf {
        self.directory.path().join("connect.sock")
    }
    pub fn update(&self, policy: Policy) {
        self.policy.send_replace(Some(policy));
    }
}
impl Drop for Proxy {
    fn drop(&mut self) {
        self.policy.send_replace(None);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn authority(text: &str) -> io::Result<(String, u16)> {
    let (host, port) = text
        .rsplit_once(':')
        .ok_or_else(|| io::Error::other("missing port"))?;
    if host.is_empty()
        || !host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
    {
        return Err(io::Error::other("invalid host"));
    }
    let port = port.parse::<u16>().map_err(io::Error::other)?;
    if port == 0 {
        return Err(io::Error::other("invalid port"));
    }
    Ok((host.to_ascii_lowercase(), port))
}
async fn serve(
    mut stream: UnixStream,
    kind: HarnessKind,
    mut changes: watch::Receiver<Option<Policy>>,
    blocked: Option<OnBlocked>,
    allow_loopback: bool,
) -> io::Result<()> {
    let header = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let mut bytes = Vec::new();
        loop {
            if bytes.len() == 8192 {
                return Err(io::Error::other("oversized CONNECT"));
            }
            bytes.push(stream.read_u8().await?);
            if bytes.ends_with(b"\r\n\r\n") {
                return Ok(bytes);
            }
        }
    })
    .await??;
    let header = std::str::from_utf8(&header).map_err(io::Error::other)?;
    let parts: Vec<_> = header
        .lines()
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .collect();
    if parts.len() != 3 || parts[0] != "CONNECT" || !["HTTP/1.0", "HTTP/1.1"].contains(&parts[2]) {
        stream.write_all(b"HTTP/1.1 405 Method Not Allowed\r\nConnection: close\r\nContent-Length: 0\r\n\r\n").await?;
        return Ok(());
    }
    let (host, port) = authority(parts[1])?;
    // A literal address on the person's own network asks for that, not just
    // the internet; a name is only known to be one after DNS, which waits for
    // the internet.
    let literal_local = host.parse().is_ok_and(private_network);
    // Loopback names never go to DNS: this computer's own services are never
    // reachable, so there is nothing the person could allow and nothing asks.
    if !allow_loopback && (host == "localhost" || host.ends_with(".localhost")) {
        stream
            .write_all(b"HTTP/1.1 403 Forbidden\r\nConnection: close\r\nContent-Length: 0\r\n\r\n")
            .await?;
        return Ok(());
    }
    let permits = |policy: Option<Policy>, local: bool| {
        policy.is_some_and(|p| allowed(kind, p.internet, &host, port) && (!local || p.local))
    };
    if !permits(*changes.borrow_and_update(), literal_local) {
        if let Some(blocked) = blocked {
            blocked(match literal_local {
                true => Blocked::Local(host),
                false => Blocked::Host(host),
            });
        }
        stream
            .write_all(b"HTTP/1.1 403 Forbidden\r\nConnection: close\r\nContent-Length: 0\r\n\r\n")
            .await?;
        return Ok(());
    }
    let local_allowed = changes.borrow().is_some_and(|p| p.local);
    let connection = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let addresses: Vec<_> = tokio::net::lookup_host((host.as_str(), port))
            .await?
            .collect();
        let local = local_needed(&addresses, allow_loopback).ok_or(Refused::Loopback)?;
        if local && !local_allowed {
            return Err(Refused::Local);
        }
        let upstream = TcpStream::connect(addresses.as_slice()).await?;
        Ok((upstream, local))
    });
    tokio::pin!(connection);
    let (mut upstream, local) = loop {
        tokio::select! {
            result = &mut connection => match result {
                Ok(Ok(connected)) => break connected,
                Ok(Err(refused @ (Refused::Local | Refused::Loopback))) => {
                    // This computer's own services are never reachable, so
                    // there is nothing the person could allow.
                    if let (Refused::Local, Some(blocked)) = (refused, &blocked) {
                        blocked(Blocked::Local(host.clone()));
                    }
                    stream.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n").await?; return Ok(());
                }
                _ => { stream.write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n").await?; return Ok(()); }
            },
            changed = changes.changed() => {
                if changed.is_err() || !permits(*changes.borrow_and_update(), literal_local) { return Ok(()); }
            }
        }
    };
    if !permits(*changes.borrow_and_update(), local) {
        return Ok(());
    }
    stream
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await?;
    let relay = tokio::io::copy_bidirectional(&mut stream, &mut upstream);
    tokio::pin!(relay);
    loop {
        tokio::select! {
            result = &mut relay => { result?; return Ok(()); },
            changed = changes.changed() => {
                if changed.is_err() || !permits(*changes.borrow_and_update(), local) { return Ok(()); }
            }
        }
    }
}

enum Refused {
    Local,
    Loopback,
    Failed,
}
impl From<io::Error> for Refused {
    fn from(_: io::Error) -> Self {
        Self::Failed
    }
}

/// Whether reaching these addresses needs the person's own network, or `None`
/// when one is this computer's own or otherwise never reachable.
fn local_needed(addresses: &[std::net::SocketAddr], allow_loopback: bool) -> Option<bool> {
    let mut local = false;
    for ip in addresses.iter().map(|a| a.ip()) {
        if public_address(ip) || allow_loopback {
            continue;
        }
        if !private_network(ip) {
            return None;
        }
        local = true;
    }
    Some(local)
}

/// The person's own networks: private LAN ranges and the tailnet's, but not
/// this computer's loopback, link-local addresses or cloud metadata.
fn private_network(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(ip) => {
            let [a, b, _, _] = ip.octets();
            ip.is_private() || (a == 100 && (64..128).contains(&b))
        }
        std::net::IpAddr::V6(ip) => ip
            .to_ipv4_mapped()
            .map(|ip| private_network(ip.into()))
            .unwrap_or_else(|| ip.segments()[0] & 0xfe00 == 0xfc00),
    }
}

fn public_address(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(ip) => {
            let [a, b, _, _] = ip.octets();
            !ip.is_private()
                && !ip.is_loopback()
                && !ip.is_link_local()
                && !ip.is_documentation()
                && !ip.is_broadcast()
                && a != 0
                && a < 224
                && !(a == 100 && (64..128).contains(&b))
        }
        std::net::IpAddr::V6(ip) => ip
            .to_ipv4_mapped()
            .map(|ip| public_address(ip.into()))
            .unwrap_or_else(|| {
                ip.segments()[0] & 0xe000 == 0x2000 && ip.segments()[..2] != [0x2001, 0x0db8]
            }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    #[test]
    fn internet_cannot_reach_host_services_or_private_networks() {
        for address in [
            "127.0.0.1",
            "10.0.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "::1",
            "::ffff:127.0.0.1",
            "fe80::1",
            "fc00::1",
        ] {
            assert!(!public_address(address.parse().unwrap()), "{address}");
        }
        assert!(public_address("93.184.215.14".parse().unwrap()));
    }
    const ALL: Policy = Policy {
        internet: true,
        local: true,
    };
    const INTERNET: Policy = Policy {
        internet: true,
        local: false,
    };
    const NONE: Policy = Policy {
        internet: false,
        local: false,
    };
    async fn connect(proxy: &Proxy, authority: &str) -> String {
        let mut client = UnixStream::connect(proxy.socket()).await.unwrap();
        client
            .write_all(format!("CONNECT {authority} HTTP/1.1\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).await.unwrap();
        response
    }
    #[tokio::test]
    async fn listens_under_a_home_folder_too_long_for_a_socket_address() {
        // A seven-letter user's sandbox state put connect.sock one byte past
        // what a socket address holds, and every limited chat failed to start.
        use std::os::unix::fs::FileTypeExt;
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("h".repeat(100));
        std::fs::create_dir(&state).unwrap();
        let proxy = Proxy::start(&state, HarnessKind::Codex, NONE, None).unwrap();
        assert!(proxy.socket().as_os_str().len() > 108);
        assert!(
            std::fs::metadata(proxy.socket())
                .unwrap()
                .file_type()
                .is_socket()
        );
        // The sandbox reaches it through a bind mount; here, through its folder.
        let folder = std::fs::File::open(proxy.socket().parent().unwrap()).unwrap();
        let alias = format!(
            "/proc/self/fd/{}/connect.sock",
            std::os::fd::AsRawFd::as_raw_fd(&folder)
        );
        let mut client = UnixStream::connect(alias).await.unwrap();
        client
            .write_all(b"CONNECT 127.0.0.1:1 HTTP/1.1\r\n\r\n")
            .await
            .unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).await.unwrap();
        assert!(response.starts_with("HTTP/1.1 403"));
    }
    #[tokio::test]
    async fn the_persons_own_network_still_refuses_loopback_tunnels() {
        let state = tempfile::tempdir().unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let proxy = Proxy::start(
            state.path(),
            HarnessKind::Codex,
            ALL,
            Some(Arc::new(move |b| {
                let _ = tx.send(b);
            })),
        )
        .unwrap();
        for authority in ["127.0.0.1:12345", "localhost:12345", "app.localhost:12345"] {
            assert!(connect(&proxy, authority).await.starts_with("HTTP/1.1 403"));
        }
        // Nothing the person could allow reaches them, so nothing asks.
        assert!(rx.try_recv().is_err());
    }
    #[tokio::test]
    async fn a_tailnet_address_asks_for_the_persons_own_network() {
        let state = tempfile::tempdir().unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let proxy = Proxy::start(
            state.path(),
            HarnessKind::Codex,
            NONE,
            Some(Arc::new(move |b| {
                let _ = tx.send(b);
            })),
        )
        .unwrap();
        for policy in [NONE, INTERNET] {
            proxy.update(policy);
            assert!(
                connect(&proxy, "100.101.102.103:443")
                    .await
                    .starts_with("HTTP/1.1 403")
            );
            assert_eq!(
                rx.recv().await,
                Some(Blocked::Local("100.101.102.103".into()))
            );
        }
        proxy.update(NONE);
        assert!(
            connect(&proxy, "example.org:443")
                .await
                .starts_with("HTTP/1.1 403")
        );
        assert_eq!(rx.recv().await, Some(Blocked::Host("example.org".into())));
    }
    #[test]
    fn private_addresses_need_the_persons_own_network() {
        let needed = |addresses: &[&str], loopback| {
            let addresses: Vec<_> = addresses.iter().map(|a| a.parse().unwrap()).collect();
            local_needed(&addresses, loopback)
        };
        assert_eq!(needed(&["93.184.215.14:443"], false), Some(false));
        for tailnet in [
            "100.101.102.103:443",
            "[fd7a:115c:a1e0::1]:443",
            "192.168.1.5:80",
        ] {
            assert_eq!(needed(&[tailnet], false), Some(true), "{tailnet}");
        }
        assert_eq!(
            needed(&["10.0.0.1:80", "93.184.215.14:80"], false),
            Some(true)
        );
        for never in [
            "127.0.0.1:80",
            "[::1]:80",
            "169.254.169.254:80",
            "[fe80::1]:80",
            "0.0.0.0:80",
        ] {
            assert_eq!(needed(&[never], false), None, "{never}");
            assert_eq!(needed(&["10.0.0.1:80", never], false), None, "{never}");
        }
        assert_eq!(needed(&["127.0.0.1:80"], true), Some(false));
        assert!(private_network("::ffff:172.16.0.1".parse().unwrap()));
        assert!(!private_network("172.32.0.1".parse().unwrap()));
    }
    #[test]
    fn exact_provider_policy() {
        for host in CLAUDE {
            assert!(allowed(HarnessKind::ClaudeCode, false, host, 443));
        }
        for host in CODEX {
            assert!(allowed(HarnessKind::Codex, false, host, 443));
        }
        for host in [
            "api.openai.com.evil",
            "evilapi.openai.com",
            "127.0.0.1",
            "api.openai.com.",
        ] {
            assert!(!allowed(HarnessKind::Codex, false, host, 443));
        }
        assert!(!allowed(HarnessKind::Codex, false, "api.openai.com", 80));
        assert!(allowed(HarnessKind::Codex, true, "example.org", 80));
        for host in ["a@api.openai.com:443", "a/b:443", "x:0", "x", "x:65536"] {
            assert!(authority(host).is_err());
        }
    }
    #[tokio::test]
    async fn unix_connect_policy_relay_and_live_revoke() {
        let state = tempfile::tempdir().unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let proxy = Proxy::start_inner(
            state.path(),
            HarnessKind::Codex,
            NONE,
            Some(Arc::new(move |b| {
                let _ = tx.send(b);
            })),
            true,
        )
        .unwrap();
        let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = tcp.local_addr().unwrap().port();
        let request = format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\n\r\n");
        let mut denied = UnixStream::connect(proxy.socket()).await.unwrap();
        denied.write_all(request.as_bytes()).await.unwrap();
        let mut response = String::new();
        denied.read_to_string(&mut response).await.unwrap();
        assert!(response.starts_with("HTTP/1.1 403"));
        assert_eq!(rx.recv().await, Some(Blocked::Host("127.0.0.1".into())));
        proxy.update(INTERNET);
        let server = tokio::spawn(async move {
            let (mut stream, _) = tcp.accept().await.unwrap();
            let mut buf = [0; 4];
            stream.read_exact(&mut buf).await.unwrap();
            stream.write_all(&buf).await.unwrap();
            let _ = stream.read_u8().await;
        });
        let mut client = UnixStream::connect(proxy.socket()).await.unwrap();
        client.write_all(request.as_bytes()).await.unwrap();
        let mut response = vec![0; b"HTTP/1.1 200 Connection Established\r\n\r\n".len()];
        client.read_exact(&mut response).await.unwrap();
        client.write_all(b"ping").await.unwrap();
        let mut reply = [0; 4];
        client.read_exact(&mut reply).await.unwrap();
        assert_eq!(&reply, b"ping");
        proxy.update(NONE);
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(2), client.read_u8())
                .await
                .unwrap()
                .is_err()
        );
        server.await.unwrap();
    }
}
