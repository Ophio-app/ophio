// SPDX-License-Identifier: Apache-2.0
//! The agent browser against local sites. Every `.test` name points at a
//! fixture server here, over https with a certificate of its own and over
//! plain http.
use super::*;
use async_trait::async_trait;
use ports::desktop::{
    CapturedFrame, DesktopBackend, DesktopCapabilities, FrameStream, InputSink, PixelFormat,
    PlatformError,
};
use protocol::{
    capability::{Availability, InputScope},
    control::{ControlTarget, InputAction, PointerButton, TargetKind, TargetPoint},
    ids::TargetId,
};
use serde_json::json;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// How long the late answer takes.
const LATE: Duration = Duration::from_secs(2);

const PASSWORD: &str = "hunter2-correct-horse";

#[derive(Debug, Clone)]
struct Seen {
    host: String,
    path: String,
    cookie: Option<String>,
    body: String,
}

struct Fixture {
    https: u16,
    http: u16,
    seen: Arc<Mutex<Vec<Seen>>>,
    _dir: tempfile::TempDir,
    data: PathBuf,
}

impl Fixture {
    async fn start() -> Self {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let secure = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let plain = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let https = secure.local_addr().unwrap().port();
        let http = plain.local_addr().unwrap().port();
        let certified = rcgen::generate_simple_self_signed(vec![
            "site.test".into(),
            "*.site.test".into(),
            "sso.test".into(),
            "faceb00k.test".into(),
            "other.test".into(),
            "blocked.test".into(),
        ])
        .unwrap();
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![certified.cert.der().clone()],
            rustls::pki_types::PrivateKeyDer::Pkcs8(certified.signing_key.serialize_der().into()),
        )
        .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let ports = (https, http);
        let recorder = seen.clone();
        tokio::spawn(async move {
            while let Ok((socket, _)) = secure.accept().await {
                let acceptor = acceptor.clone();
                let recorder = recorder.clone();
                tokio::spawn(async move {
                    if let Ok(stream) = acceptor.accept(socket).await {
                        serve(stream, recorder, ports).await;
                    }
                });
            }
        });
        let recorder = seen.clone();
        tokio::spawn(async move {
            while let Ok((socket, _)) = plain.accept().await {
                tokio::spawn(serve(socket, recorder.clone(), ports));
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().to_owned();
        Self {
            https,
            http,
            seen,
            _dir: dir,
            data,
        }
    }
    fn browser(&self) -> AgentBrowser {
        let executable = crate::discover().expect("a Chromium-based browser is installed");
        AgentBrowser::with_arguments(
            executable,
            &self.data,
            vec![
                "--host-resolver-rules=MAP *.test 127.0.0.1".into(),
                "--ignore-certificate-errors".into(),
                "--disable-features=HttpsUpgrades,HttpsFirstBalancedModeAutoEnable".into(),
            ],
        )
    }
    fn url(&self, host: &str, path: &str) -> String {
        format!("https://{host}:{}{path}", self.https)
    }
    fn register(&self, browser: &AgentBrowser, blocked: &[&str]) -> (TaskId, PathBuf) {
        let task = TaskId::new();
        let downloads = self.data.join(format!("downloads-{task}"));
        let blocked: Vec<String> = blocked.iter().map(|s| (*s).to_owned()).collect();
        browser.register(
            task,
            TaskBrowsing {
                blocked: Arc::new(move || blocked.clone()),
                downloads: downloads.clone(),
            },
        );
        (task, downloads)
    }
    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
    fn saw_host(&self, host: &str) -> bool {
        self.seen().iter().any(|s| s.host == host)
    }
    fn posted(&self, host: &str) -> Option<String> {
        self.seen()
            .into_iter()
            .find(|s| s.host == host && s.path == "/submitted")
            .map(|s| s.body)
    }
}

/// A document that keeps the site's session and puts it back while it runs,
/// for a page to put somewhere other than an address on the site.
const RESTORES_INSIDE: &str = r#"<title>Inside</title><script>
const kept = localStorage.getItem('session') || 'kept';
setInterval(() => localStorage.setItem('session', kept), 5);
document.title = 'inside ' + kept;
<\/script>"#;

const LOGIN: &str = r#"<!doctype html><title>Sign in</title>
<form method=post action=/submitted>
<input name=user autocomplete=username aria-label=Email>
<input type=password name=pass aria-label=Password>
<input type=password name=decoy style="display:none">
<button type=button onclick="document.querySelector('[name=pass]').type='text'">Show</button>
<button>Sign in</button>
</form>"#;

fn page(host: &str, path: &str, (https, _): (u16, u16)) -> (String, String, String) {
    let ok = |body: String| {
        (
            "200 OK".to_owned(),
            "Content-Type: text/html\r\n".to_owned(),
            body,
        )
    };
    let site = |host: &str, path: &str| format!("https://{host}:{https}{path}");
    match (host, path.split('?').next().unwrap_or_default()) {
        (_, "/red") => ok("<title>Red</title><style>html {background:red}</style>".into()),
        (_, "/blue") => ok("<title>Blue</title><style>html {background:blue}</style>".into()),
        (_, "/login") => ok(LOGIN.into()),
        (_, "/submitted") => ok("<title>Welcome</title>Signed in".into()),
        ("other.test", "/embed-login") => ok(format!(
            r#"<title>Other</title><iframe src="{}" width=700 height=500></iframe>"#,
            site("site.test", "/login")
        )),
        ("site.test", "/framed-foreign") => ok(format!(
            r#"<title>Framed</title><iframe src="{}" width=700 height=500></iframe>"#,
            site("other.test", "/login")
        )),
        ("site.test", "/sso") => ok(format!(
            r#"<title>Single sign-on</title><p style="height:100px">Sign in below</p><iframe src="{}" width=700 height=500></iframe>"#,
            site("sso.test", "/login")
        )),
        ("site.test", "/reflects") => ok(r#"<title>Reflects</title><form method=post action=/submitted>
<input name=user aria-label=Email>
<input type=password name=pass aria-label=Password
 oninput="document.getElementById('echo').textContent=this.value">
<button type=button onclick="window.open('/popup')">Pop up</button></form>
<p id=echo></p>"#
            .into()),
        ("site.test", "/code") => ok(r#"<title>Code</title><form method=post action=/submitted>
<input name=otp autocomplete=one-time-code aria-label=Code
 oninput="document.getElementById('echo').textContent=this.value"><button>Go</button></form>
<p id=echo></p>"#
            .into()),
        (_, "/store") => ok(r#"<title>Storing</title><script>
localStorage.setItem("session", "kept"); document.title = localStorage.getItem("session");
</script>"#
            .into()),
        // A page that keeps its session and puts it back while it runs.
        (_, "/restores") => ok(r#"<title>Restoring</title><script>
const kept = localStorage.getItem("session") || "kept";
setInterval(() => localStorage.setItem("session", kept), 5);
document.title = kept;
</script>"#
            .into()),
        ("site.test", "/login-and-file") => ok(r#"<title>Files after</title><form>
<input name=user aria-label=Email><input type=password name=pass aria-label=Password></form>
<a href="/report">Get the report</a>"#
            .into()),
        (_, "/peek") => ok(r#"<title>Peeking</title><script>
document.title = localStorage.getItem("session") || "empty";
</script>"#
            .into()),
        ("site.test", "/decoys") => ok(r#"<title>Decoys</title>
<input type=password name=faded style="opacity:0">
<input type=password name=tiny style="width:2px;height:2px;padding:0;border:0">
<div style="position:relative;display:inline-block"><input type=password name=covered>
<div style="position:absolute;inset:0;background:white"></div></div>"#
            .into()),
        ("site.test", "/turns-text") => ok(r#"<title>Turns</title><form method=post action=/submitted>
<input name=user aria-label=Email>
<input type=password name=pass onfocus="this.type='text'"
 oninput="fetch('/captured',{method:'POST',body:this.value})"><button>Go</button></form>"#
            .into()),
        ("site.test", "/swaps") => ok(r#"<title>Swaps</title><form method=post action=/submitted>
<input name=user aria-label=Email oninput="const p=document.querySelector('[name=pass]');p.remove();this.form.insertAdjacentHTML('beforeend','<input type=password name=pass>')">
<input type=password name=pass><button>Go</button></form>"#
            .into()),
        ("site.test", "/redirect") => (
            "302 Found".into(),
            format!("Location: {}\r\n", site("blocked.test", "/landing")),
            String::new(),
        ),
        ("site.test", "/reaches") => ok(format!(
            r#"<title>Reaches</title><img src="{0}">
<script>new Worker(URL.createObjectURL(new Blob(["fetch('{1}')"])));</script>
<button onclick="window.open('{2}')">Pop up</button>"#,
            site("blocked.test", "/pixel"),
            site("blocked.test", "/worker"),
            site("blocked.test", "/popup"),
        )),
        ("site.test", "/download") => ok(r#"<title>Files</title><a href="/report">Get the report</a>"#.into()),
        ("site.test", "/report") => (
            "200 OK".into(),
            "Content-Type: application/octet-stream\r\nContent-Disposition: attachment; filename=\"report.txt\"\r\n".into(),
            "quarterly numbers".into(),
        ),
        ("site.test", "/third-party") => ok(format!(
            r#"<title>Third party</title><script>
fetch("{0}", {{credentials: "include", mode: "no-cors"}});
new WebSocket("wss://other.test:{https}/socket");
</script>"#,
            site("other.test", "/echo"),
        )),
        ("public.site.test", "/same-site-socket") => ok(format!(
            r#"<title>Same site</title><script>
new WebSocket("wss://secure.site.test:{https}/socket");
</script>"#
        )),
        // A page that moves into a document of its own making, which keeps
        // the site's session and puts it back while it runs.
        (_, "/to-blob") => ok(format!(
            r#"<title>To blob</title><script>
location = URL.createObjectURL(new Blob([`{RESTORES_INSIDE}`], {{type: "text/html"}}));
</script>"#
        )),
        // A page that writes the same into a blank tab it opens.
        (_, "/opens-blank") => ok(format!(
            r#"<title>Opens blank</title><script>
function popUp() {{
  const w = window.open();
  w.document.write(`{RESTORES_INSIDE}`);
  w.document.close();
}}
</script><button onclick="popUp()">Pop up</button>"#
        )),
        // A page whose frame from the site opens a blank tab and writes
        // into it.
        ("other.test", "/frames-opener") => ok(format!(
            r#"<title>Frames opener</title><iframe src="{}" width=700 height=300></iframe>"#,
            site("auth.site.test", "/opens-blank")
        )),
        // A page that makes an address of its own, which opens a socket to
        // its site named for whatever follows the address's `#`, and shows
        // the address.
        (_, "/makes-blob") => ok(format!(
            r#"<title>Making</title><script>
document.title = URL.createObjectURL(new Blob([`<title>Made</title><script>
new WebSocket("wss://auth.site.test:{https}/socket-" + location.hash.slice(1));
<\/script>`], {{type: "text/html"}}));
</script>"#
        )),
        // A link to whatever address follows the `?`.
        (_, "/link") => ok(format!(
            r#"<title>Link</title><a href="{0}">Go</a> <a href="{0}" target=_blank>New</a> <button onclick="window.open('{0}')">Pop</button>"#,
            path.split_once('?').map_or(String::new(), |(_, to)| to.replace("%23", "#"))
        )),
        // A request that outlives its page, answered late with a session.
        (_, "/refreshes-late") => ok(r#"<title>Refreshing</title><script>
fetch("/late-cookie", {keepalive: true, method: "POST", body: "refresh"});
</script>"#
            .into()),
        (_, "/late-cookie") => (
            "200 OK".into(),
            "Content-Type: text/plain\r\nSet-Cookie: sid=late; Path=/; Secure; SameSite=None\r\n".into(),
            "refreshed".into(),
        ),
        ("other.test", "/set-cookie") => (
            "200 OK".into(),
            "Content-Type: text/html\r\nSet-Cookie: sid=session-of-other; Path=/; Secure; SameSite=None\r\n".into(),
            "<title>Cookie set</title>".into(),
        ),
        _ => ok(format!("<title>{host}</title>{path}")),
    }
}

async fn serve<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    seen: Arc<Mutex<Vec<Seen>>>,
    ports: (u16, u16),
) {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break end + 4;
        }
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(n) => buffer.extend_from_slice(&chunk[..n]),
        }
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
    let mut lines = head.lines();
    let path = lines
        .next()
        .and_then(|l| l.split(' ').nth(1))
        .unwrap_or("/")
        .to_owned();
    let mut host = String::new();
    let mut cookie = None;
    let mut length = 0;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match name.to_ascii_lowercase().as_str() {
            "host" => host = value.split(':').next().unwrap_or_default().to_owned(),
            "cookie" => cookie = Some(value.to_owned()),
            "content-length" => length = value.parse().unwrap_or(0),
            _ => {}
        }
    }
    while buffer.len() < head_end + length {
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => buffer.extend_from_slice(&chunk[..n]),
        }
    }
    let body = String::from_utf8_lossy(&buffer[head_end..]).into_owned();
    seen.lock().unwrap().push(Seen {
        host: host.clone(),
        path: path.clone(),
        cookie,
        body,
    });
    if path == "/late-cookie" {
        tokio::time::sleep(LATE).await;
    }
    let (status, headers, body) = page(&host, &path, ports);
    let reply = format!(
        "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(reply.as_bytes()).await;
    let _ = stream.shutdown().await;
}

fn refusal(outcome: Result<PageInfo, AgentError>) -> String {
    match outcome {
        Err(AgentError::Refused(message)) => message,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

async fn eventually(mut check: impl FnMut() -> bool) -> bool {
    for _ in 0..100 {
        if check() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    check()
}

fn sites(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_owned()).collect()
}

#[tokio::test]
async fn observing_a_background_tasks_tab_needs_no_switch() {
    let fixture = Fixture::start().await;
    let browser = fixture.browser();
    let (a, _) = fixture.register(&browser, &[]);
    let (b, _) = fixture.register(&browser, &[]);
    let url_a = fixture.url("site.test", "/login");
    let url_b = fixture.url("other.test", "/submitted");
    let tab_a = browser.open(a, &url_a, false).await.unwrap().tab;
    let tab_b = browser.open(b, &url_b, false).await.unwrap().tab;

    let look_b = browser.observe(b).await.unwrap();
    let result_a = browser.observe(a).await;
    browser.shut_down().await;
    let look_a = result_a.unwrap();
    assert_eq!(look_b.tab, tab_b);
    assert_eq!(look_b.url, url_b);
    assert_eq!(look_b.title, "Welcome");
    assert!(!look_b.screenshot.is_empty());
    assert_eq!(look_a.tab, tab_a);
    assert_eq!(look_a.url, url_a);
    assert_eq!(look_a.title, "Sign in");
    assert!(!look_a.screenshot.is_empty());
    assert_ne!(look_a.screenshot, look_b.screenshot);
    assert!(look_a.controls.iter().any(|c| c.name == "Email"));
}

#[tokio::test]
async fn concurrent_observations_keep_each_tasks_page() {
    use base64::Engine;

    let fixture = Fixture::start().await;
    let browser = fixture.browser();
    let (a, _) = fixture.register(&browser, &[]);
    let (b, _) = fixture.register(&browser, &[]);
    let url_a = fixture.url("site.test", "/red");
    let url_b = fixture.url("other.test", "/blue");
    let tab_a = browser.open(a, &url_a, false).await.unwrap().tab;
    let tab_b = browser.open(b, &url_b, false).await.unwrap().tab;
    let owner_tab = browser
        .owner_tabs()
        .1
        .into_iter()
        .find(|t| t.url == url_a)
        .unwrap()
        .id;
    let check = |look: Observation, tab: &str, url: &str, color: [u8; 4]| {
        assert_eq!(look.tab, tab);
        assert_eq!(look.url, url);
        let png = base64::engine::general_purpose::STANDARD
            .decode(look.screenshot)
            .unwrap();
        let image = paint::decode(&png).unwrap();
        assert_eq!(&image.data[..4], &color);
    };
    for _ in 0..6 {
        let (look_a, look_b, owner) = tokio::join!(
            browser.observe(a),
            browser.observe(b),
            browser.capture(owner_tab),
        );
        check(look_a.unwrap(), &tab_a, &url_a, [255, 0, 0, 255]);
        check(look_b.unwrap(), &tab_b, &url_b, [0, 0, 255, 255]);
        assert_eq!(&owner.unwrap().data[..4], &[255, 0, 0, 255]);
    }
    let (look_a, opened) = tokio::join!(browser.observe(a), browser.open(b, &url_b, true));
    check(look_a.unwrap(), &tab_a, &url_a, [255, 0, 0, 255]);
    let opened = opened.unwrap();
    let (look_a, switched) = tokio::join!(browser.observe(a), browser.switch_tab(b, &tab_b));
    check(look_a.unwrap(), &tab_a, &url_a, [255, 0, 0, 255]);
    assert_eq!(switched.unwrap().tab, tab_b);
    let (look_a, closed) =
        tokio::join!(browser.observe(a), browser.close_tab(b, Some(&opened.tab)),);
    check(look_a.unwrap(), &tab_a, &url_a, [255, 0, 0, 255]);
    closed.unwrap();
    browser.shut_down().await;
}

#[tokio::test]
async fn an_unresponsive_page_still_times_out() {
    let fixture = Fixture::start().await;
    let browser = fixture.browser();
    let (task, _) = fixture.register(&browser, &[]);
    browser
        .open(task, &fixture.url("site.test", "/login"), false)
        .await
        .unwrap();
    let running = browser.shared.current().unwrap();
    let (target, session) = {
        let state = running.lock();
        let target = state.current[&task].clone();
        let session = state.tabs[&target].session.clone();
        (target, session)
    };
    // A busy renderer must keep the normal CDP timeout, rather than being
    // mistaken for a tab that only needs to be brought to the front.
    assert!(matches!(
        running
            .pipe
            .call(
                Some(&session),
                "Runtime.evaluate",
                json!({"expression":"while (true) {}"}),
            )
            .await,
        Err(voice_control::Error::OutcomeUnknown)
    ));
    let result = tokio::time::timeout(Duration::from_secs(20), browser.observe(task)).await;
    let _ = running
        .pipe
        .call(None, "Target.closeTarget", json!({"targetId":target}))
        .await;
    browser.shut_down().await;
    assert!(matches!(
        result.expect("observing a busy renderer must time out"),
        Err(AgentError::Failed(message)) if message == "The page stopped responding"
    ));
}

#[tokio::test]
async fn input_to_a_background_tasks_tab_needs_no_switch() {
    let fixture = Fixture::start().await;
    let browser = fixture.browser();
    let (a, _) = fixture.register(&browser, &[]);
    let (b, _) = fixture.register(&browser, &[]);
    browser
        .open(a, &fixture.url("site.test", "/login"), false)
        .await
        .unwrap();
    let look_a = browser.observe(a).await.unwrap();
    browser
        .open(b, &fixture.url("other.test", "/login"), false)
        .await
        .unwrap();
    let look_b = browser.observe(b).await.unwrap();
    let email = |look: &Observation| {
        look.controls
            .iter()
            .find(|c| c.name == "Email")
            .unwrap()
            .reference
            .clone()
    };
    let (email_a, email_b) = (email(&look_a), email(&look_b));
    let (typed_a, typed_b) = tokio::join!(
        browser.type_text(a, &email_a, "alice", false),
        browser.type_text(b, &email_b, "bob", false),
    );
    typed_a.unwrap();
    typed_b.unwrap();
    let submit = look_a
        .controls
        .iter()
        .find(|c| c.name == "Sign in")
        .unwrap();
    browser.click(a, &submit.reference).await.unwrap();
    assert!(eventually(|| fixture.posted("site.test").is_some()).await);
    assert_eq!(
        fixture.posted("site.test").unwrap(),
        "user=alice&pass=&decoy="
    );
    assert!(fixture.posted("other.test").is_none());
    let look_b = browser.observe(b).await.unwrap();
    assert_eq!(look_b.title, "Sign in");
    assert!(
        look_b
            .controls
            .iter()
            .any(|c| c.name == "Email" && c.value.as_deref() == Some("bob"))
    );
    browser.shut_down().await;
}

#[tokio::test]
async fn logins_go_only_into_the_right_field_on_the_right_site() {
    let fixture = Fixture::start().await;
    let browser = fixture.browser();
    let (task, _) = fixture.register(&browser, &[]);
    let site = sites(&["site.test"]);

    // The right page: the password goes in the visible field only, and is
    // never seen afterwards, even shown as text.
    browser
        .open(task, &fixture.url("site.test", "/login"), false)
        .await
        .unwrap();
    browser
        .fill_login(task, &site, "alice@example.com", PASSWORD, false)
        .await
        .unwrap();
    let look = browser.observe(task).await.unwrap();
    let show = look.controls.iter().find(|c| c.name == "Show").unwrap();
    browser.click(task, &show.reference.clone()).await.unwrap();
    let look = browser.observe(task).await.unwrap();
    let everything = serde_json::to_string(&look).unwrap();
    assert!(!everything.contains(PASSWORD));
    // Neither field Ophio filled is shown, the username either.
    let user = look.controls.iter().find(|c| c.name == "Email").unwrap();
    assert_eq!(user.value, None);
    let field = look.controls.iter().find(|c| c.name == "Password").unwrap();
    assert_eq!(field.value, None);
    assert!(look.screenshot.is_empty());
    let submit = look.controls.iter().find(|c| c.name == "Sign in").unwrap();
    browser
        .click(task, &submit.reference.clone())
        .await
        .unwrap();
    assert!(eventually(|| fixture.posted("site.test").is_some()).await);
    let posted = fixture.posted("site.test").unwrap();
    assert!(posted.contains("user=alice%40example.com"), "{posted}");
    assert!(posted.contains(&format!("pass={PASSWORD}")), "{posted}");
    assert!(
        posted.contains("decoy=&") || posted.ends_with("decoy="),
        "{posted}"
    );

    // A lookalike site.
    browser
        .open(task, &fixture.url("faceb00k.test", "/login"), false)
        .await
        .unwrap();
    let refused = refusal(browser.fill_login(task, &site, "a", PASSWORD, true).await);
    assert!(refused.contains("not on the login's site"), "{refused}");

    // Plain http.
    browser
        .open(
            task,
            &format!("http://site.test:{}/login", fixture.http),
            false,
        )
        .await
        .unwrap();
    let refused = refusal(browser.fill_login(task, &site, "a", PASSWORD, true).await);
    assert!(refused.contains("not secure"), "{refused}");

    // The login's page inside another site's page, and another site's form
    // inside the login's page.
    browser
        .open(task, &fixture.url("other.test", "/embed-login"), false)
        .await
        .unwrap();
    let refused = refusal(browser.fill_login(task, &site, "a", PASSWORD, true).await);
    assert!(refused.contains("not on the login's site"), "{refused}");
    browser
        .open(task, &fixture.url("site.test", "/framed-foreign"), false)
        .await
        .unwrap();
    let refused = refusal(browser.fill_login(task, &site, "a", PASSWORD, true).await);
    assert!(refused.contains("not on the login's site"), "{refused}");

    // Fields no one could see or type in.
    browser
        .open(task, &fixture.url("site.test", "/decoys"), false)
        .await
        .unwrap();
    let refused = refusal(browser.fill_login(task, &site, "a", PASSWORD, true).await);
    assert!(refused.contains("no sign-in field"), "{refused}");

    // A password field that turns into text, and one swapped for another
    // while the username goes in.
    for path in ["/turns-text", "/swaps"] {
        browser
            .open(task, &fixture.url("site.test", path), false)
            .await
            .unwrap();
        let refused = refusal(browser.fill_login(task, &site, "a", PASSWORD, true).await);
        assert!(
            refused.contains("password did not go in"),
            "{path}: {refused}"
        );
    }
    // The field that turned into text never received it.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !fixture.seen().iter().any(|s| s.path == "/captured"),
        "{:?}",
        fixture.seen()
    );

    // A login form in a frame of another site the login also covers, which
    // runs in a process of its own.
    browser
        .open(task, &fixture.url("site.test", "/sso"), false)
        .await
        .unwrap();
    browser
        .fill_login(
            task,
            &sites(&["site.test", "sso.test"]),
            "alice",
            PASSWORD,
            true,
        )
        .await
        .unwrap();
    assert!(eventually(|| fixture.posted("sso.test").is_some()).await);
    assert!(
        fixture
            .posted("sso.test")
            .unwrap()
            .contains(&format!("pass={PASSWORD}"))
    );

    // Nothing reached a site it shouldn't have.
    let leaked: Vec<_> = fixture
        .seen()
        .into_iter()
        .filter(|s| s.body.contains(PASSWORD) && s.host != "site.test" && s.host != "sso.test")
        .collect();
    assert!(leaked.is_empty(), "{leaked:?}");
    assert!(
        fixture
            .seen()
            .iter()
            .filter(|s| s.host == "site.test" && s.body.contains(PASSWORD))
            .count()
            == 1
    );
    browser.shut_down().await;
}

/// The task's current tab once it is no longer `old`.
async fn moved_from(browser: &AgentBrowser, task: TaskId, old: &str) -> PageInfo {
    for _ in 0..100 {
        let now = browser.page(task).await.unwrap();
        if now.tab != old {
            return now;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the tab stayed {old}");
}

#[tokio::test]
async fn pages_that_could_show_what_was_typed_get_no_screenshot() {
    let fixture = Fixture::start().await;
    let browser = fixture.browser();
    let (task, _) = fixture.register(&browser, &[]);
    let site = sites(&["site.test"]);
    let look = || async { browser.observe(task).await.unwrap() };
    let typed_here = |look: &Observation| {
        look.screenshot.is_empty()
            && (look.notice.as_deref().unwrap_or_default())
                .contains("typed a password or code into this page")
    };
    let typed_beside = |look: &Observation| {
        look.screenshot.is_empty()
            && (look.notice.as_deref().unwrap_or_default())
                .contains("open while Ophio typed a password or code into another")
    };
    let go = |path: &'static str| {
        let url = fixture.url("site.test", path);
        let browser = &browser;
        async move { browser.open(task, &url, false).await.unwrap() }
    };

    // A tab open before anything is typed, and the page that copies the
    // password where no painting covers it.
    let beside = go("/before").await.tab;
    let typed = (browser.open(task, &fixture.url("site.test", "/reflects"), true))
        .await
        .unwrap()
        .tab;
    assert!(!look().await.screenshot.is_empty());
    browser
        .fill_login(task, &site, "alice", PASSWORD, false)
        .await
        .unwrap();
    let seen = look().await;
    assert!(typed_here(&seen), "{:?}", seen.notice);

    // A tab the page opens is born beside it.
    let popup = seen.controls.iter().find(|c| c.name == "Pop up").unwrap();
    browser.click(task, &popup.reference.clone()).await.unwrap();
    let opened = moved_from(&browser, task, &typed).await;
    assert_ne!(opened.tab, beside);
    let seen = look().await;
    assert!(typed_beside(&seen), "{:?}", seen.notice);

    // The tab that was open beside it, even on a page it opens next.
    browser.switch_tab(task, &beside).await.unwrap();
    assert!(typed_beside(&look().await));
    go("/elsewhere").await;
    assert!(typed_beside(&look().await));

    // Screenshots come back for a tab when it opens another page, once
    // the typed page has gone.
    browser.switch_tab(task, &typed).await.unwrap();
    assert!(typed_here(&look().await));
    go("/after").await;
    assert!(!look().await.screenshot.is_empty());
    browser.switch_tab(task, &beside).await.unwrap();
    assert!(typed_beside(&look().await));
    go("/later").await;
    let seen = look().await;
    assert!(
        !seen.screenshot.is_empty() && seen.notice.is_none(),
        "{:?}",
        seen.notice
    );

    // A code goes only into the page it was asked for, and hides that page.
    go("/code").await;
    let asked = browser.mark(task).await.unwrap();
    go("/code").await;
    let refused = refusal(
        browser
            .fill_code(task, &site, "481516", false, Some(&asked))
            .await,
    );
    assert!(refused.contains("page changed"), "{refused}");
    let asked = browser.mark(task).await.unwrap();
    browser.switch_tab(task, &typed).await.unwrap();
    let refused = refusal(
        browser
            .fill_code(task, &site, "481516", false, Some(&asked))
            .await,
    );
    assert!(refused.contains("page changed"), "{refused}");
    browser.switch_tab(task, &beside).await.unwrap();
    browser
        .fill_code(task, &site, "481516", false, Some(&asked))
        .await
        .unwrap();
    let seen = look().await;
    assert!(typed_here(&seen), "{:?}", seen.notice);
    let code = seen.controls.iter().find(|c| c.name == "Code").unwrap();
    assert_eq!(code.value, None);
    browser.shut_down().await;
}

#[tokio::test]
async fn a_page_that_changes_between_finding_and_filling_gets_nothing() {
    let fixture = Fixture::start().await;
    let browser = fixture.browser();
    let (task, _) = fixture.register(&browser, &[]);
    let site = sites(&["site.test"]);
    for next in [
        fixture.url("site.test", "/login?again"),
        fixture.url("faceb00k.test", "/login"),
    ] {
        browser
            .open(task, &fixture.url("site.test", "/login"), false)
            .await
            .unwrap();
        let running = browser.shared.current().unwrap();
        let target = running.lock().current[&task].clone();
        let frame = running.frames(&target).await.remove(0);
        let found = running
            .script(
                &frame.session,
                &frame.id,
                &[json!("findLogin"), json!(site)],
            )
            .await
            .unwrap();
        assert!(found["password"].is_u64(), "{found}");
        browser.open(task, &next, false).await.unwrap();
        let filled = running
            .script(
                &frame.session,
                &frame.id,
                &[
                    json!("fillLogin"),
                    json!(site),
                    found["username"].clone(),
                    found["password"].clone(),
                    json!({"username": "alice", "password": PASSWORD}),
                ],
            )
            .await
            .unwrap();
        assert!(filled["error"].is_string(), "{next}: {filled}");
    }
    browser.shut_down().await;
}

#[tokio::test]
async fn a_task_reaches_no_site_it_is_kept_from() {
    let fixture = Fixture::start().await;
    let browser = fixture.browser();
    let (task, downloads) = fixture.register(&browser, &["blocked.test"]);

    let refused = refusal(
        browser
            .open(task, &fixture.url("blocked.test", "/"), false)
            .await,
    );
    assert!(refused.contains("blocked.test"), "{refused}");
    let refused = refusal(
        browser
            .open(task, &fixture.url("site.test", "/redirect"), false)
            .await,
    );
    assert!(refused.contains("blocked.test"), "{refused}");

    // Images, workers and pop-ups.
    browser
        .open(task, &fixture.url("site.test", "/reaches"), false)
        .await
        .unwrap();
    let look = browser.observe(task).await.unwrap();
    let pop = look.controls.iter().find(|c| c.name == "Pop up").unwrap();
    browser.click(task, &pop.reference.clone()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(!fixture.saw_host("blocked.test"), "{:?}", fixture.seen());
    let tabs = browser.tabs(task).await.unwrap();
    assert_eq!(tabs.len(), 2, "{tabs:?}");
    let look = browser.observe(task).await.unwrap();
    assert!(
        look.notice
            .as_deref()
            .is_some_and(|n| n.contains("blocked.test")),
        "{look:?}"
    );

    // Downloads land in the task's folder.
    browser
        .open(task, &fixture.url("site.test", "/download"), false)
        .await
        .unwrap();
    let look = browser.observe(task).await.unwrap();
    let link = look
        .controls
        .iter()
        .find(|c| c.name == "Get the report")
        .unwrap();
    browser.click(task, &link.reference.clone()).await.unwrap();
    let mut arrived = Vec::new();
    for _ in 0..100 {
        arrived.extend(browser.observe(task).await.unwrap().downloads);
        if !arrived.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(arrived, ["report.txt"]);
    assert_eq!(
        std::fs::read_to_string(downloads.join("report.txt")).unwrap(),
        "quarterly numbers"
    );

    // Another task isn't kept from it.
    let (other, _) = fixture.register(&browser, &[]);
    browser
        .open(other, &fixture.url("blocked.test", "/"), false)
        .await
        .unwrap();
    assert!(fixture.saw_host("blocked.test"));
    browser.shut_down().await;
}

#[tokio::test]
async fn failed_download_delivery_reaches_only_its_tasks_observations() {
    let fixture = Fixture::start().await;
    let browser = fixture.browser();
    let (task, downloads) = fixture.register(&browser, &[]);
    let (other, _) = fixture.register(&browser, &[]);
    // A regular file blocks the destination, including when CI runs as root.
    std::fs::write(&downloads, b"destination is unavailable").unwrap();
    let other_page = browser
        .open(other, &fixture.url("site.test", "/"), false)
        .await
        .unwrap();
    let task_page = browser
        .open(task, &fixture.url("site.test", "/download"), false)
        .await
        .unwrap();
    let look = browser.observe(task).await.unwrap();
    let link = look
        .controls
        .iter()
        .find(|c| c.name == "Get the report")
        .unwrap();
    browser.click(task, &link.reference).await.unwrap();
    let mut notice = String::new();
    for _ in 0..100 {
        let look = browser.observe(task).await.unwrap();
        assert!(look.downloads.is_empty(), "{look:?}");
        notice = look.notice.unwrap_or_default();
        if notice.contains("could not be saved") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(notice.contains("report.txt"), "{notice}");
    assert!(notice.contains(downloads.to_str().unwrap()), "{notice}");
    assert!(notice.contains("After fixing the destination"), "{notice}");
    let staged: Vec<_> = std::fs::read_dir(&browser.shared.staging)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(staged.len(), 1);
    assert!(notice.contains(staged[0].to_str().unwrap()), "{notice}");
    assert_eq!(std::fs::read(&staged[0]).unwrap(), b"quarterly numbers");
    browser.switch_tab(other, &other_page.tab).await.unwrap();
    let other_observation = browser.observe(other).await;
    browser.switch_tab(task, &task_page.tab).await.unwrap();
    let task_observation = browser.observe(task).await;
    browser.shut_down().await;
    assert!(other_observation.unwrap().notice.is_none());
    assert!(task_observation.unwrap().notice.is_none());
}

#[tokio::test]
async fn a_task_kept_from_a_site_is_kept_from_its_whole_site() {
    let fixture = Fixture::start().await;
    let browser = fixture.browser();
    // A page on a sibling subdomain could open a WebSocket to the site with
    // its cookies, and no request interception sees a WebSocket.
    let (task, _) = fixture.register(&browser, &["secure.site.test"]);
    for host in ["public.site.test", "site.test"] {
        let refused = refusal(
            browser
                .open(task, &fixture.url(host, "/same-site-socket"), false)
                .await,
        );
        assert!(refused.contains(host), "{refused}");
    }
    assert!(!fixture.saw_host("secure.site.test"));

    // Another task may open it, and its socket does reach the site.
    let (other, _) = fixture.register(&browser, &[]);
    browser
        .open(
            other,
            &fixture.url("public.site.test", "/same-site-socket"),
            false,
        )
        .await
        .unwrap();
    assert!(
        eventually(|| fixture.saw_host("secure.site.test")).await,
        "{:?}",
        fixture.seen()
    );
    browser.shut_down().await;
}

#[tokio::test]
async fn sites_get_no_other_sites_cookies_and_signing_out_clears_them() {
    let fixture = Fixture::start().await;
    let browser = fixture.browser();
    let (task, _) = fixture.register(&browser, &[]);
    let with_cookie = |path: &str| {
        fixture
            .seen()
            .iter()
            .filter(|s| s.host == "other.test" && s.path == path)
            .map(|s| s.cookie.clone())
            .collect::<Vec<_>>()
    };
    browser
        .open(task, &fixture.url("other.test", "/set-cookie"), false)
        .await
        .unwrap();
    browser
        .open(task, &fixture.url("other.test", "/first"), false)
        .await
        .unwrap();
    assert_eq!(with_cookie("/first"), [Some("sid=session-of-other".into())]);

    browser
        .open(task, &fixture.url("site.test", "/third-party"), false)
        .await
        .unwrap();
    assert!(
        eventually(|| with_cookie("/echo").len() == 1 && with_cookie("/socket").len() == 1).await
    );
    assert_eq!(with_cookie("/echo"), [None]);
    assert_eq!(with_cookie("/socket"), [None]);

    browser.sign_out(&sites(&["other.test"])).await.unwrap();
    browser
        .open(task, &fixture.url("other.test", "/after"), false)
        .await
        .unwrap();
    assert_eq!(with_cookie("/after"), [None]);
    browser.shut_down().await;
}

/// A computer with no screen to share.
#[derive(Debug)]
struct NoScreen;

#[async_trait]
impl DesktopBackend for NoScreen {
    async fn capabilities(&self) -> DesktopCapabilities {
        DesktopCapabilities {
            backend: "none".into(),
            capture: Availability::Unsupported {
                reason: "none".into(),
            },
            input: Availability::Unsupported {
                reason: "none".into(),
            },
            input_scope: InputScope::None,
            lock_session: Availability::Unsupported {
                reason: "none".into(),
            },
            unlock_session: Availability::Unsupported {
                reason: "none".into(),
            },
        }
    }
    async fn targets(&self) -> Result<Vec<ControlTarget>, PlatformError> {
        Err(PlatformError::Unsupported("none".into()))
    }
    async fn capture_still(&self, _: TargetId) -> Result<CapturedFrame, PlatformError> {
        Err(PlatformError::TargetNotFound)
    }
    async fn open_stream(&self, _: TargetId, _: u8) -> Result<Box<dyn FrameStream>, PlatformError> {
        Err(PlatformError::TargetNotFound)
    }
    async fn input_sink(&self, _: TargetId) -> Result<Box<dyn InputSink>, PlatformError> {
        Err(PlatformError::TargetNotFound)
    }
    async fn lock_session(&self) -> Result<(), PlatformError> {
        Err(PlatformError::Unsupported("none".into()))
    }
}

/// The title of the task's page once it has one of `titles`.
async fn titled(browser: &AgentBrowser, task: TaskId, titles: &[&str]) -> String {
    for _ in 0..100 {
        let title = browser.page(task).await.unwrap().title;
        if titles.contains(&title.as_str()) {
            return title;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("no page titled {titles:?}");
}

#[tokio::test]
async fn signing_out_clears_what_the_whole_site_stored_anywhere() {
    let fixture = Fixture::start().await;
    let browser = fixture.browser();
    let (task, _) = fixture.register(&browser, &[]);
    // Stored without a cookie, on a subdomain, on a port of its own.
    for host in ["auth.site.test", "other.test"] {
        browser
            .open(task, &fixture.url(host, "/store"), false)
            .await
            .unwrap();
        assert_eq!(titled(&browser, task, &["kept"]).await, "kept");
    }
    browser.shut_down().await;

    // The browser starts again only to sign out.
    let browser = fixture.browser();
    browser.sign_out(&sites(&["site.test"])).await.unwrap();
    let (task, _) = fixture.register(&browser, &[]);
    let peek = |host: &'static str| {
        let (browser, url) = (&browser, fixture.url(host, "/peek"));
        async move {
            browser.open(task, &url, false).await.unwrap();
            titled(browser, task, &["kept", "empty"]).await
        }
    };
    assert_eq!(peek("auth.site.test").await, "empty");
    assert_eq!(peek("other.test").await, "kept");
    browser.shut_down().await;
}

#[tokio::test]
async fn signing_out_closes_every_page_before_clearing() {
    let fixture = Fixture::start().await;
    let browser = fixture.browser();
    let (task, _) = fixture.register(&browser, &[]);
    let (other, _) = fixture.register(&browser, &[]);
    // Pages that put the session back: at the site's own address, at one of
    // their own making, and in a blank tab one opened and wrote into.
    browser
        .open(task, &fixture.url("auth.site.test", "/restores"), false)
        .await
        .unwrap();
    assert_eq!(titled(&browser, task, &["kept"]).await, "kept");
    browser
        .open(task, &fixture.url("auth.site.test", "/to-blob"), true)
        .await
        .unwrap();
    assert_eq!(
        titled(&browser, task, &["inside kept"]).await,
        "inside kept"
    );
    assert!(browser.page(task).await.unwrap().url.starts_with("blob:"));
    browser
        .open(other, &fixture.url("auth.site.test", "/opens-blank"), false)
        .await
        .unwrap();
    let look = browser.observe(other).await.unwrap();
    let pop = (look.controls.iter()).find(|c| c.name == "Pop up").unwrap();
    browser.click(other, &pop.reference.clone()).await.unwrap();
    assert_eq!(
        titled(&browser, other, &["inside kept"]).await,
        "inside kept"
    );
    assert_eq!(browser.page(other).await.unwrap().url, "about:blank");

    browser.sign_out(&sites(&["site.test"])).await.unwrap();
    for task in [task, other] {
        assert!(browser.tabs(task).await.unwrap_or_default().is_empty());
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    browser
        .open(task, &fixture.url("auth.site.test", "/peek"), false)
        .await
        .unwrap();
    assert_eq!(titled(&browser, task, &["kept", "empty"]).await, "empty");
    browser.shut_down().await;
}

#[tokio::test]
async fn a_request_sent_before_signing_out_gets_no_session_back() {
    let fixture = Fixture::start().await;
    let browser = fixture.browser();
    let (task, _) = fixture.register(&browser, &[]);
    browser
        .open(
            task,
            &fixture.url("auth.site.test", "/refreshes-late"),
            false,
        )
        .await
        .unwrap();
    // The site has the request, and answers it with a session only later.
    assert!(eventually(|| fixture.seen().iter().any(|s| s.path == "/late-cookie")).await);
    browser.sign_out(&sites(&["site.test"])).await.unwrap();
    tokio::time::sleep(LATE + Duration::from_secs(1)).await;
    browser
        .open(task, &fixture.url("auth.site.test", "/first"), false)
        .await
        .unwrap();
    let sent: Vec<_> = (fixture.seen().into_iter())
        .filter(|s| s.path == "/first")
        .map(|s| s.cookie)
        .collect();
    assert_eq!(sent, [None]);
    // The new browser doesn't reopen the page from before, which would ask again.
    let opened = (fixture.seen().into_iter())
        .filter(|s| s.path == "/refreshes-late")
        .count();
    assert_eq!(opened, 1);
    browser.shut_down().await;
}

#[tokio::test]
async fn a_page_closes_once_its_task_may_no_longer_use_the_site() {
    let fixture = Fixture::start().await;
    let browser = fixture.browser();
    let blocked = Arc::new(Mutex::new(Vec::<String>::new()));
    let task = TaskId::new();
    let kept = blocked.clone();
    browser.register(
        task,
        TaskBrowsing {
            blocked: Arc::new(move || kept.lock().unwrap().clone()),
            downloads: fixture.data.join("downloads"),
        },
    );
    let (other, _) = fixture.register(&browser, &[]);
    let blank = |task| {
        let browser = &browser;
        async move {
            for _ in 0..100 {
                if browser.page(task).await.unwrap().url == "about:blank" {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            false
        }
    };
    let login = fixture.url("auth.site.test", "/login");
    browser.open(task, &login, false).await.unwrap();

    // The login is paused: the task's next step finds its page closed.
    *blocked.lock().unwrap() = sites(&["site.test"]);
    let refused = match browser.observe(task).await {
        Err(AgentError::Refused(message)) => message,
        other => panic!("expected a refusal, got {other:?}"),
    };
    assert!(refused.contains("auth.site.test"), "{refused}");
    assert!(blank(task).await);
    let refused = refusal(browser.click(task, "e1").await);
    assert!(!refused.contains("site.test"), "{refused}");

    // Paused again with the page open: it closes without waiting for a step,
    // and another task's page on the site stays.
    blocked.lock().unwrap().clear();
    browser.open(task, &login, false).await.unwrap();
    browser.open(other, &login, false).await.unwrap();
    *blocked.lock().unwrap() = sites(&["site.test"]);
    browser.recheck_access().await;
    assert!(blank(task).await);
    assert_eq!(browser.page(other).await.unwrap().url, login);
    browser.shut_down().await;
}

#[tokio::test]
async fn a_download_from_a_page_ophio_typed_into_is_not_kept() {
    let fixture = Fixture::start().await;
    let browser = fixture.browser();
    let (task, downloads) = fixture.register(&browser, &[]);
    browser
        .open(task, &fixture.url("site.test", "/login-and-file"), false)
        .await
        .unwrap();
    browser
        .fill_login(task, &sites(&["site.test"]), "agent", PASSWORD, false)
        .await
        .unwrap();
    let look = browser.observe(task).await.unwrap();
    let link = (look.controls.iter())
        .find(|c| c.name == "Get the report")
        .unwrap();
    browser.click(task, &link.reference.clone()).await.unwrap();
    let mut notice = String::new();
    for _ in 0..100 {
        let look = browser.observe(task).await.unwrap();
        assert!(look.downloads.is_empty(), "{look:?}");
        notice = look.notice.unwrap_or_default();
        if notice.contains("wasn't kept") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(notice.contains("wasn't kept"), "{notice}");
    assert!(!downloads.join("report.txt").exists());
    browser.shut_down().await;
}

#[tokio::test]
async fn the_owner_watches_a_tab_and_takes_it_over() {
    let fixture = Fixture::start().await;
    let browser = fixture.browser();
    let (task, _) = fixture.register(&browser, &[]);
    let desktop = WithAgentBrowser::new(Arc::new(NoScreen), browser.clone());
    assert!(matches!(
        desktop.targets().await,
        Err(PlatformError::Unsupported(_))
    ));

    browser
        .open(task, &fixture.url("site.test", "/login"), false)
        .await
        .unwrap();
    browser
        .fill_login(task, &sites(&["site.test"]), "", PASSWORD, false)
        .await
        .unwrap();
    // The tab is named for its page once the page has its title.
    assert!(eventually(|| browser.owner_tabs().1.iter().any(|t| t.title == "Sign in")).await);
    let targets = desktop.targets().await.unwrap();
    assert_eq!(targets.len(), 1);
    let tab = &targets[0];
    assert_eq!(tab.kind, TargetKind::Browser);
    assert_eq!(tab.label, "Agent browser — Sign in");

    // The owner sees the page, the password painted over.
    let look = browser.observe(task).await.unwrap();
    let field = look.controls.iter().find(|c| c.name == "Password").unwrap();
    let frame = desktop.capture_still(tab.target_id).await.unwrap();
    assert_eq!(frame.format, PixelFormat::Rgba8);
    let scale = f64::from(frame.width) / 1280.;
    let [x, y, w, h] = field.bounds.map(|n| n as f64);
    let (px, py) = (
        ((x + w / 2.) * scale) as usize,
        ((y + h / 2.) * scale) as usize,
    );
    let i = py * frame.stride as usize + px * 4;
    assert_eq!(frame.data[i..i + 4], paint::FILL);
    let mut stream = desktop.open_stream(tab.target_id, 30).await.unwrap();
    assert!(stream.next_frame().await.unwrap().is_some());

    // Taking over keeps the agent out of the tab until it's handed back,
    // even input that was waiting to go in when it was taken.
    let user = look.controls.iter().find(|c| c.name == "Email").unwrap();
    let running = browser.shared.current().unwrap();
    let in_flight = running.input.lock().await;
    let waiting = tokio::spawn({
        let (browser, reference) = (browser.clone(), user.reference.clone());
        async move { browser.type_text(task, &reference, "agent", false).await }
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(browser.set_taken_over(tab.target_id, true));
    drop(in_flight);
    let refused = waiting.await.unwrap().unwrap_err().to_string();
    assert!(refused.contains("owner is using"), "{refused}");
    let refused = refusal(browser.click(task, &user.reference.clone()).await);
    assert!(refused.contains("owner is using"), "{refused}");
    let refused = refusal(
        browser
            .fill_login(task, &sites(&["site.test"]), "", PASSWORD, false)
            .await,
    );
    assert!(refused.contains("owner is using"), "{refused}");

    // The owner's input waits for the agent's already on its way.
    let mut input = desktop.input_sink(tab.target_id).await.unwrap();
    let in_flight = running.input.lock().await;
    let early = InputAction::Text {
        text: "early".into(),
    };
    let typed = input.inject(&early, &tab.geometry);
    assert!(
        tokio::time::timeout(Duration::from_millis(300), typed)
            .await
            .is_err()
    );
    drop(in_flight);
    let [x, y, w, h] = user.bounds.map(|n| n as f64);
    let at = TargetPoint {
        x: (x + w / 2.) / 1280.,
        y: (y + h / 2.) / 800.,
    };
    input
        .inject(
            &InputAction::Click {
                button: PointerButton::Left,
                at,
                count: 1,
            },
            &tab.geometry,
        )
        .await
        .unwrap();
    input
        .inject(
            &InputAction::Text {
                text: "owner@example.com".into(),
            },
            &tab.geometry,
        )
        .await
        .unwrap();
    input
        .inject(
            &InputAction::KeyPress {
                key: "shift".into(),
                pressed: true,
            },
            &tab.geometry,
        )
        .await
        .unwrap();
    input.release_all().await.unwrap();

    assert!(browser.set_taken_over(tab.target_id, false));
    let look = browser.observe(task).await.unwrap();
    let user = look.controls.iter().find(|c| c.name == "Email").unwrap();
    assert_eq!(user.value.as_deref(), Some("owner@example.com"));

    // A closed tab is no longer a target.
    browser.close_tab(task, None).await.unwrap();
    assert!(eventually(|| browser.owner_tabs().1.is_empty()).await);
    assert!(stream.next_frame().await.unwrap().is_none());
    browser.shut_down().await;
}

#[tokio::test]
async fn a_page_at_an_address_of_its_own_closes_with_its_site() {
    let fixture = Fixture::start().await;
    let browser = fixture.browser();
    let blocked = Arc::new(Mutex::new(Vec::<String>::new()));
    let task = TaskId::new();
    let kept = blocked.clone();
    browser.register(
        task,
        TaskBrowsing {
            blocked: Arc::new(move || kept.lock().unwrap().clone()),
            downloads: fixture.data.join("downloads"),
        },
    );
    let titles = |task| {
        let browser = &browser;
        async move {
            let mut titles: Vec<String> = (browser.tabs(task).await.unwrap().into_iter())
                .map(|t| t.title)
                .collect();
            titles.sort();
            titles
        }
    };
    browser
        .open(task, &fixture.url("auth.site.test", "/to-blob"), false)
        .await
        .unwrap();
    titled(&browser, task, &["inside kept"]).await;
    browser
        .open(task, &fixture.url("auth.site.test", "/opens-blank"), true)
        .await
        .unwrap();
    let look = browser.observe(task).await.unwrap();
    let pop = (look.controls.iter()).find(|c| c.name == "Pop up").unwrap();
    browser.click(task, &pop.reference.clone()).await.unwrap();
    titled(&browser, task, &["inside kept"]).await;
    assert_eq!(
        titles(task).await,
        ["Opens blank", "inside kept", "inside kept"]
    );

    // The login is paused: the blank tab the site wrote into refuses the
    // task's next step, and every page that had the site's origin goes.
    *blocked.lock().unwrap() = sites(&["site.test"]);
    let refused = refusal(browser.press(task, "Tab").await);
    assert!(refused.contains("auth.site.test"), "{refused}");
    browser.recheck_access().await;
    let mut left = Vec::new();
    for _ in 0..100 {
        left = titles(task).await;
        if left
            .iter()
            .all(|t| !t.contains("inside") && t != "Opens blank")
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        left.iter()
            .all(|t| !t.contains("inside") && t != "Opens blank"),
        "{left:?}"
    );
    browser.shut_down().await;
}

#[tokio::test]
async fn a_tab_a_sites_frame_opens_closes_with_the_site() {
    let fixture = Fixture::start().await;
    let browser = fixture.browser();
    let blocked = Arc::new(Mutex::new(Vec::<String>::new()));
    let task = TaskId::new();
    let kept = blocked.clone();
    browser.register(
        task,
        TaskBrowsing {
            blocked: Arc::new(move || kept.lock().unwrap().clone()),
            downloads: fixture.data.join("downloads"),
        },
    );
    let titles = |task| {
        let browser = &browser;
        async move {
            let mut titles: Vec<String> = (browser.tabs(task).await.unwrap().into_iter())
                .map(|t| t.title)
                .collect();
            titles.sort();
            titles
        }
    };
    // Another site's page holds a frame from the site, which opens a blank
    // tab with the site's origin and writes into it.
    browser
        .open(task, &fixture.url("other.test", "/frames-opener"), false)
        .await
        .unwrap();
    let mut pop = None;
    for _ in 0..100 {
        let look = browser.observe(task).await.unwrap();
        pop = (look.controls.into_iter()).find(|c| c.name == "Pop up");
        if pop.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    browser.click(task, &pop.unwrap().reference).await.unwrap();
    titled(&browser, task, &["inside kept"]).await;

    // The login is paused: the blank tab refuses the task's next step, and
    // both it and the page holding the site's frame go.
    *blocked.lock().unwrap() = sites(&["site.test"]);
    let refused = refusal(browser.press(task, "Tab").await);
    assert!(refused.contains("auth.site.test"), "{refused}");
    browser.recheck_access().await;
    let gone = |left: &[String]| {
        left.iter()
            .all(|t| !t.contains("inside") && t != "Frames opener")
    };
    let mut left = Vec::new();
    for _ in 0..100 {
        left = titles(task).await;
        if gone(&left) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(gone(&left), "{left:?}");
    browser.shut_down().await;
}

#[tokio::test]
async fn a_task_kept_from_a_site_opens_none_of_its_own_addresses() {
    let fixture = Fixture::start().await;
    let browser = fixture.browser();
    let (allowed, _) = fixture.register(&browser, &[]);
    let (kept, _) = fixture.register(&browser, &["site.test"]);
    // A task that may use the site keeps an address of the site's own alive.
    browser
        .open(
            allowed,
            &fixture.url("auth.site.test", "/makes-blob"),
            false,
        )
        .await
        .unwrap();
    let mut made = String::new();
    for _ in 0..100 {
        made = browser.page(allowed).await.unwrap().title;
        if made.starts_with("blob:") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(made.starts_with("blob:"), "{made}");

    // A task kept from the site follows that address from another site, in
    // its tab, in a new one and in a pop-up. Chromium keeps a blob address to
    // pages of the site that made it, so none of them opens.
    let link = fixture.url("other.test", &format!("/link?{made}%23kept"));
    for name in ["Go", "New", "Pop"] {
        browser.open(kept, &link, false).await.unwrap();
        let look = browser.observe(kept).await.unwrap();
        let control = (look.controls.iter()).find(|c| c.name == name).unwrap();
        browser
            .click(kept, &control.reference.clone())
            .await
            .unwrap();
    }
    // Followed from the site itself, it opens; by the time it has, whatever
    // the kept task's clicks started has had its chance.
    let same = fixture.url("auth.site.test", &format!("/link?{made}%23same"));
    browser.open(allowed, &same, true).await.unwrap();
    let look = browser.observe(allowed).await.unwrap();
    let go = (look.controls.iter()).find(|c| c.name == "Go").unwrap();
    browser.click(allowed, &go.reference.clone()).await.unwrap();
    for _ in 0..100 {
        if fixture.seen().iter().any(|s| s.path == "/socket-same") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let seen: Vec<String> = (fixture.seen().into_iter()).map(|s| s.path).collect();
    assert!(seen.iter().any(|p| p == "/socket-same"), "{seen:?}");
    assert!(!seen.iter().any(|p| p == "/socket-kept"), "{seen:?}");
    browser.shut_down().await;
}

#[tokio::test]
async fn a_page_stays_when_a_frame_its_task_is_kept_from_is_refused() {
    let fixture = Fixture::start().await;
    let browser = fixture.browser();
    let (task, _) = fixture.register(&browser, &["other.test"]);
    browser
        .open(task, &fixture.url("site.test", "/framed-foreign"), false)
        .await
        .unwrap();
    let mut notice = None;
    for _ in 0..100 {
        notice = browser.observe(task).await.unwrap().notice;
        if notice.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(notice.is_some_and(|n| n.contains("other.test")));
    // The frame shows the browser's own page, which has no site, so the page
    // holding it isn't closed.
    tokio::time::sleep(Duration::from_millis(500)).await;
    browser.press(task, "Tab").await.unwrap();
    assert_eq!(browser.page(task).await.unwrap().title, "Framed");
    browser.shut_down().await;
}
