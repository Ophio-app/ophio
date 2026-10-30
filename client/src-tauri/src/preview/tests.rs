// SPDX-License-Identifier: Apache-2.0
use super::*;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    net::TcpStream,
};

#[derive(Default)]
struct Fake {
    requests: Mutex<Vec<String>>,
}
#[async_trait]
impl Connector for Arc<Fake> {
    async fn open(&self) -> Result<(DuplexStream, String)> {
        let (client, peer) = tokio::io::duplex(MAX_PREVIEW_CHUNK);
        let fake = self.clone();
        tokio::spawn(async move {
            let mut peer = BufReader::new(peer);
            let mut text = String::new();
            loop {
                let mut line = String::new();
                if peer.read_line(&mut line).await.unwrap_or(0) == 0 {
                    return;
                }
                text.push_str(&line);
                if line == "\r\n" {
                    break;
                }
            }
            let upgrade = text.to_ascii_lowercase().contains("upgrade: websocket");
            fake.requests.lock().unwrap().push(text);
            if upgrade {
                peer.write_all(b"HTTP/1.1 101 Switching Protocols\r\nConnection: upgrade\r\nUpgrade: websocket\r\n\r\n").await.unwrap();
                let (mut reader, mut writer) = tokio::io::split(peer);
                let _ = tokio::io::copy(&mut reader, &mut writer).await;
            } else {
                let _ = peer.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nSet-Cookie: preview_attack=bad\r\n\r\nhello").await;
            }
        });
        Ok((client, "[::1]:5173".into()))
    }
}
#[derive(Debug)]
struct FakeClock(Mutex<Instant>);
impl ports::Clock for FakeClock {
    fn now(&self) -> protocol::time::Timestamp {
        protocol::time::Timestamp::now()
    }
    fn monotonic(&self) -> Instant {
        *self.0.lock().unwrap()
    }
}
#[derive(Default)]
struct Seen(Mutex<Vec<PreviewState>>);
impl Events for Seen {
    fn emit(&self, event: PreviewState) {
        self.0.lock().unwrap().push(event);
    }
}
async fn request(authority: &str, path: &str, headers: &str) -> String {
    let mut stream = TcpStream::connect(authority).await.unwrap();
    stream
        .write_all(
            format!(
                "GET {path} HTTP/1.1\r\nHost: {authority}\r\n{headers}Connection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut answer = String::new();
    tokio::time::timeout(Duration::from_secs(3), stream.read_to_string(&mut answer))
        .await
        .unwrap()
        .unwrap();
    answer
}
#[tokio::test]
async fn http_cookie_origin_filtering_upgrade_and_control_loss() {
    let manager = Arc::new(Previews::default());
    let fake = Arc::new(Fake::default());
    let control = CancellationToken::new();
    let seen = Arc::new(Seen::default());
    let id = manager
        .open(
            Arc::new(fake.clone()),
            control.clone(),
            Arc::new(AtomicBool::new(true)),
            Arc::new(ports::SystemClock),
            seen.clone(),
        )
        .await
        .unwrap();
    manager.initial_path(&id, "/deep/path?query=yes").unwrap();
    let url = url::Url::parse(&manager.url(&id).unwrap()).unwrap();
    let authority = format!("127.0.0.1:{}", url.port().unwrap());
    assert!(
        request(&authority, "/", "")
            .await
            .starts_with("HTTP/1.1 403")
    );
    assert!(fake.requests.lock().unwrap().is_empty());
    let bootstrap = request(&authority, &format!("{}?query=yes", url.path()), "").await;
    assert!(bootstrap.starts_with("HTTP/1.1 303"));
    assert!(bootstrap.contains("location: /deep/path?query=yes\r\n"));
    let cookie = bootstrap
        .lines()
        .find_map(|s| s.strip_prefix("set-cookie: "))
        .unwrap();
    assert!(cookie.contains("HttpOnly; SameSite=Strict; Path=/"));
    let cookie = cookie.split(';').next().unwrap();
    assert!(
        request(
            &authority,
            "/",
            &format!("Cookie: {cookie}\r\nOrigin: http://127.0.0.1:1\r\n")
        )
        .await
        .starts_with("HTTP/1.1 403")
    );
    assert!(
        request(
            &authority,
            "/",
            &format!("Cookie: {cookie}\r\nSec-Fetch-Site: cross-site\r\n")
        )
        .await
        .starts_with("HTTP/1.1 403")
    );
    let answer = request(
        &authority,
        "/asset.js",
        &format!("Cookie: {cookie}; site=kept\r\nReferer: secret\r\n"),
    )
    .await;
    assert!(answer.ends_with("hello"));
    assert!(!answer.contains("preview_attack"));
    let text = fake.requests.lock().unwrap()[0].clone();
    assert!(text.contains("GET /asset.js"));
    assert!(text.contains("host: [::1]:5173"));
    assert!(text.contains("site=kept"));
    assert!(!text.contains("preview_"));
    assert!(!text.contains("secret"));
    let mut stream = BufReader::new(TcpStream::connect(&authority).await.unwrap());
    stream.write_all(format!("GET /socket HTTP/1.1\r\nHost: {authority}\r\nCookie: {cookie}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\n").as_bytes()).await.unwrap();
    let mut upgrade_headers = String::new();
    loop {
        let mut line = String::new();
        stream.read_line(&mut line).await.unwrap();
        upgrade_headers.push_str(&line.to_ascii_lowercase());
        if line == "\r\n" {
            break;
        }
    }
    assert!(upgrade_headers.contains("connection: upgrade\r\n"));
    stream.write_all(b"hot-reload").await.unwrap();
    let mut data = [0; 10];
    stream.read_exact(&mut data).await.unwrap();
    assert_eq!(&data, b"hot-reload");
    control.cancel();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), stream.read(&mut data))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    assert!(manager.url(&id).is_err());
    assert_eq!(fake.requests.lock().unwrap().len(), 2); // No byte stream was replayed.
}
#[tokio::test]
async fn background_idle_deadline_closes_origin_and_reports_reason() {
    let manager = Arc::new(Previews::default());
    let fake = Arc::new(Fake::default());
    let clock = Arc::new(FakeClock(Mutex::new(Instant::now())));
    let seen = Arc::new(Seen::default());
    let id = manager
        .open(
            Arc::new(fake),
            CancellationToken::new(),
            Arc::new(AtomicBool::new(false)),
            clock.clone(),
            seen.clone(),
        )
        .await
        .unwrap();
    tokio::task::yield_now().await;
    *clock.0.lock().unwrap() += Duration::from_millis(PREVIEW_IDLE_TIMEOUT_MS as u64 + 1);
    tokio::time::timeout(Duration::from_secs(3), async {
        while manager.url(&id).is_ok() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        seen.0
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e, PreviewState::Closed { reason } if reason == "idle_timeout"))
    );
}

/// The preview's address, and the cookie its bootstrap link sets.
async fn bootstrap(manager: &Previews, id: &str) -> (String, String) {
    let url = url::Url::parse(&manager.url(id).unwrap()).unwrap();
    let authority = format!("127.0.0.1:{}", url.port().unwrap());
    let answer = request(&authority, url.path(), "").await;
    let cookie = answer
        .lines()
        .find_map(|s| s.strip_prefix("set-cookie: "))
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    (authority, cookie)
}
/// A WebSocket through the preview to the fake's echo, checked end to end.
async fn websocket(authority: &str, cookie: &str) -> BufReader<TcpStream> {
    let mut stream = BufReader::new(TcpStream::connect(authority).await.unwrap());
    stream.write_all(format!("GET /socket HTTP/1.1\r\nHost: {authority}\r\nCookie: {cookie}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\n").as_bytes()).await.unwrap();
    let mut status = String::new();
    stream.read_line(&mut status).await.unwrap();
    assert!(status.starts_with("HTTP/1.1 101"), "{status}");
    loop {
        let mut line = String::new();
        stream.read_line(&mut line).await.unwrap();
        if line == "\r\n" {
            break;
        }
    }
    stream.write_all(b"ping").await.unwrap();
    let mut echo = [0; 4];
    stream.read_exact(&mut echo).await.unwrap();
    assert_eq!(&echo, b"ping");
    stream
}
/// Whether the preview answers a request on a new connection.
async fn answered(authority: &str, cookie: &str) -> bool {
    let mut stream = TcpStream::connect(authority).await.unwrap();
    let _ = stream
        .write_all(
            format!("GET / HTTP/1.1\r\nHost: {authority}\r\nCookie: {cookie}\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await;
    let mut answer = String::new();
    let _ = tokio::time::timeout(Duration::from_secs(3), stream.read_to_string(&mut answer)).await;
    answer.starts_with("HTTP/1.1 200")
}
#[tokio::test]
async fn websocket_relays_count_against_the_connection_limit() {
    let manager = Arc::new(Previews::default());
    let id = manager
        .open(
            Arc::new(Arc::new(Fake::default())),
            CancellationToken::new(),
            Arc::new(AtomicBool::new(true)),
            Arc::new(ports::SystemClock),
            Arc::new(Seen::default()),
        )
        .await
        .unwrap();
    let (authority, cookie) = bootstrap(&manager, &id).await;
    let mut sockets = Vec::new();
    for _ in 0..LIMIT {
        sockets.push(websocket(&authority, &cookie).await);
    }
    // Every connection the preview allows is an open relay.
    assert!(!answered(&authority, &cookie).await);
    // Closing one lets another connection in.
    drop(sockets.pop());
    tokio::time::timeout(Duration::from_secs(5), async {
        while !answered(&authority, &cookie).await {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}

/// Streams a response to a GET one chunk at a time, as the test lets each
/// go, and counts the bytes of a POST's body as they arrive.
struct Streaming {
    chunks: tokio::sync::Semaphore,
    received: std::sync::atomic::AtomicUsize,
}
impl Default for Streaming {
    fn default() -> Self {
        Self {
            chunks: tokio::sync::Semaphore::new(0),
            received: Default::default(),
        }
    }
}
#[async_trait]
impl Connector for Arc<Streaming> {
    async fn open(&self) -> Result<(DuplexStream, String)> {
        let (client, peer) = tokio::io::duplex(MAX_PREVIEW_CHUNK);
        let fake = self.clone();
        tokio::spawn(async move {
            let (reader, mut writer) = tokio::io::split(peer);
            let mut reader = BufReader::new(reader);
            let mut head = String::new();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                    return;
                }
                head.push_str(&line);
                if line == "\r\n" {
                    break;
                }
            }
            if head.starts_with("POST") {
                let mut bytes = [0; 64];
                while let Ok(count @ 1..) = reader.read(&mut bytes).await {
                    fake.received.fetch_add(count, Ordering::SeqCst);
                }
                return;
            }
            let _ = writer
                .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
                .await;
            while let Ok(permit) = fake.chunks.acquire().await {
                permit.forget();
                if writer.write_all(b"1\r\nx\r\n").await.is_err() {
                    return;
                }
            }
        });
        Ok((client, "127.0.0.1:5173".into()))
    }
}
#[tokio::test]
async fn bytes_streaming_either_way_keep_a_preview_open() {
    let manager = Arc::new(Previews::default());
    let fake = Arc::new(Streaming::default());
    let clock = Arc::new(FakeClock(Mutex::new(Instant::now())));
    let seen = Arc::new(Seen::default());
    let id = manager
        .open(
            Arc::new(fake.clone()),
            CancellationToken::new(),
            Arc::new(AtomicBool::new(true)),
            clock.clone(),
            seen.clone(),
        )
        .await
        .unwrap();
    let (authority, cookie) = bootstrap(&manager, &id).await;
    let most = Duration::from_millis(u64::from(PREVIEW_IDLE_TIMEOUT_MS) * 3 / 4);
    let advance = |by: Duration| *clock.0.lock().unwrap() += by;
    // The idle deadline is checked each second.
    let still_open = || async {
        tokio::time::sleep(Duration::from_millis(1200)).await;
        assert!(manager.url(&id).is_ok());
    };

    // A long download: each chunk comes most of the idle time after the last.
    let mut download = BufReader::new(TcpStream::connect(&authority).await.unwrap());
    download
        .write_all(
            format!("GET /events HTTP/1.1\r\nHost: {authority}\r\nCookie: {cookie}\r\n\r\n")
                .as_bytes(),
        )
        .await
        .unwrap();
    loop {
        let mut line = String::new();
        download.read_line(&mut line).await.unwrap();
        if line == "\r\n" {
            break;
        }
    }
    for _ in 0..2 {
        advance(most);
        fake.chunks.add_permits(1);
        let mut chunk = String::new();
        download.read_line(&mut chunk).await.unwrap();
        download.read_line(&mut chunk).await.unwrap();
        assert_eq!(chunk, "1\r\nx\r\n");
        still_open().await;
    }

    // A long upload, the same way.
    let mut upload = TcpStream::connect(&authority).await.unwrap();
    upload
        .write_all(
            format!("POST /upload HTTP/1.1\r\nHost: {authority}\r\nCookie: {cookie}\r\nTransfer-Encoding: chunked\r\n\r\n")
                .as_bytes(),
        )
        .await
        .unwrap();
    for _ in 0..2 {
        advance(most);
        let before = fake.received.load(Ordering::SeqCst);
        upload.write_all(b"1\r\ny\r\n").await.unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            while fake.received.load(Ordering::SeqCst) == before {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        still_open().await;
    }

    // With nothing moving, the preview still closes when idle.
    advance(most * 2);
    tokio::time::timeout(Duration::from_secs(3), async {
        while manager.url(&id).is_ok() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        seen.0
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e, PreviewState::Closed { reason } if reason == "idle_timeout"))
    );
}

// The companion reports the socket it reached, and the phone sends that as
// the Host header. Any loopback socket is accepted, including IPv4 loopback
// written as IPv6, which the companion and the app both treat as loopback;
// nothing else is.
#[test]
fn the_computer_may_report_a_loopback_socket_and_nothing_else() {
    for address in [
        "127.0.0.1:3000",
        "127.8.9.10:1",
        "[::1]:5173",
        "[::ffff:127.0.0.1]:3000",
    ] {
        assert!(loopback_socket(address).is_ok(), "{address}");
    }
    for address in [
        "10.0.0.1:3000",
        "[::ffff:10.0.0.1]:3000",
        "[::2]:3000",
        "localhost:3000",
        "127.0.0.1",
        "127.0.0.1:3000\r\nX-Other: 1",
    ] {
        assert!(loopback_socket(address).is_err(), "{address}");
    }
}
