// SPDX-License-Identifier: Apache-2.0
//! Preview checking is injectable. Implementations must not follow redirects,
//! perform DNS rebinding, or contact any endpoint other than the validated URL.
use async_trait::async_trait;
use protocol::{ErrorCode, ProtocolError};
use std::net::IpAddr;

/// How long a server may take to answer whether it serves a page: one
/// compiling its first page on request can take a while.
pub const PAGE_WAIT: std::time::Duration = std::time::Duration::from_secs(20);
/// How much of a page is read for its title.
pub(crate) const TITLE_WITHIN: usize = 64 * 1024;
/// The longest title kept as a preview's name.
const TITLE_CHARS: usize = 60;

/// What answered a request for a page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageAnswer {
    /// Nothing, or not yet: worth asking again.
    Silent,
    /// Something other than a site, like an API, a debugger or a database.
    Other,
    /// A web page, and its title when it has one.
    Page(Option<String>),
}

#[async_trait]
pub trait PreviewChecker: Send + Sync + std::fmt::Debug + 'static {
    /// Probe this exact URL, with no redirect following. The hub adds a timeout.
    /// `false` means nothing answered there.
    async fn check(&self, url: &url::Url) -> Result<bool, ProtocolError>;

    /// Whether a web page is served at this exact URL: an HTML answer that
    /// isn't an error, or a redirect within the same server. A server error
    /// may pass, so it is worth asking again.
    async fn page(&self, url: &url::Url) -> Result<PageAnswer, ProtocolError> {
        Ok(match self.check(url).await? {
            true => PageAnswer::Page(None),
            false => PageAnswer::Silent,
        })
    }
}

/// The text of a page's `<title>`, read as a browser reads it. It is kept
/// whole and as written, spaces and all, so what is hidden in it is found as
/// written; [`spaced`] is how it shows, and [`short`] what is shown.
pub fn title(html: &[u8]) -> Option<String> {
    let html = String::from_utf8_lossy(html);
    let lower = html.to_ascii_lowercase();
    let open = lower.find("<title")?;
    let start = open + lower[open..].find('>')? + 1;
    let end = start + lower[start..].find("</title")?;
    let text = references_read(&html[start..end]);
    (!text.trim().is_empty() && !text.contains('\0')).then_some(text)
}

/// Text with each run of spaces, line breaks and other white space made one
/// space, and none at either end, as a title shows.
pub fn spaced(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Character references read in one pass, as a browser reads them in a
/// title: `&#49;`, `&#x31;`, `&num;` and `&eacute;` are what they stand for,
/// so a key, code or password written that way is found when what is held
/// is hidden. One that names nothing is left as written.
fn references_read(text: &str) -> String {
    let mut read = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        read.push_str(&rest[..at]);
        rest = &rest[at + 1..];
        match reference(rest) {
            Some((stands_for, used)) => {
                read.push_str(&stands_for);
                rest = &rest[used..];
            }
            None => read.push('&'),
        }
    }
    read.push_str(rest);
    read
}

/// What the reference after a `&` stands for, and how much of it was read.
fn reference(after: &str) -> Option<(String, usize)> {
    let Some(number) = after.strip_prefix('#') else {
        return named(after).map(|(stands_for, used)| (stands_for.to_string(), used));
    };
    let (radix, digits) = match number.strip_prefix(['x', 'X']) {
        Some(hex) => (16, hex),
        None => (10, number),
    };
    let len = digits
        .find(|c: char| !c.is_digit(radix))
        .unwrap_or(digits.len());
    if len == 0 {
        return None;
    }
    let c = u32::from_str_radix(&digits[..len], radix)
        .ok()
        .map(windows_1252)
        // Nothing, past Unicode or half a surrogate pair.
        .filter(|&n| n != 0)
        .and_then(char::from_u32)
        .unwrap_or(char::REPLACEMENT_CHARACTER);
    let prefix = number.len() - digits.len();
    let semi = digits[len..].starts_with(';');
    Some((c.to_string(), 1 + prefix + len + usize::from(semi)))
}

/// The longest named reference `after` begins with, and its length. Some
/// are read without their `;`, even before more letters: `&notit;` is
/// `¬it;`.
fn named(after: &str) -> Option<(&'static str, usize)> {
    use crate::html_references::{LONGEST, NAMED};
    let letters = after
        .find(|c: char| !c.is_ascii_alphanumeric())
        .unwrap_or(after.len())
        .min(LONGEST);
    let find = |name: &str| {
        let at = NAMED.binary_search_by(|(n, _)| n.cmp(&name)).ok()?;
        Some((NAMED[at].1, name.len()))
    };
    (1..=letters).rev().find_map(|len| {
        let closed = after.get(..=len).filter(|name| name.ends_with(';'));
        closed.and_then(find).or_else(|| find(&after[..len]))
    })
}

/// A browser reads `&#128;` to `&#159;` as Windows-1252 has them.
fn windows_1252(n: u32) -> u32 {
    const C1: [u32; 32] = [
        0x20AC, 0x81, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160,
        0x2039, 0x0152, 0x8D, 0x017D, 0x8F, 0x90, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013,
        0x2014, 0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x9D, 0x017E, 0x0178,
    ];
    match n {
        0x80..=0x9F => C1[(n - 0x80) as usize],
        _ => n,
    }
}

/// A page's name kept short enough to show.
pub fn short(title: &str) -> String {
    let mut chars = title.chars();
    let short: String = chars.by_ref().take(TITLE_CHARS).collect();
    match chars.next() {
        Some(_) => format!("{}…", short.trim_end()),
        None => short,
    }
}

pub fn validate(raw: &str) -> Result<url::Url, ProtocolError> {
    let refused = || {
        ProtocolError::new(
            ErrorCode::PathNotAllowed,
            "Preview requires an HTTP(S) private numeric address or localhost, with no credentials or fragment",
        )
    };
    let url = url::Url::parse(raw).map_err(|_| refused())?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.port_or_known_default().is_none_or(|p| p == 0)
    {
        return Err(refused());
    }
    let allowed = match url.host() {
        Some(url::Host::Domain("localhost")) => true,
        Some(url::Host::Ipv4(ip)) => private(IpAddr::V4(ip)),
        Some(url::Host::Ipv6(ip)) => private(IpAddr::V6(ip)),
        _ => false,
    };
    if !allowed {
        return Err(refused());
    }
    Ok(url)
}
fn private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            ip.is_loopback()
                || ip.is_private()
                || (ip.octets()[0] == 100 && (64..=127).contains(&ip.octets()[1]))
        }
        IpAddr::V6(ip) => {
            ip.is_loopback()
                || (ip.segments()[0] & 0xfe00 == 0xfc00)
                || ip
                    .to_ipv4_mapped()
                    .is_some_and(|v4| private(IpAddr::V4(v4)))
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restrict_destinations() {
        for bad in [
            "http://example.com",
            "file:///etc/passwd",
            "http://169.254.169.254",
            "http://127.0.0.1@8.8.8.8",
            "https://8.8.8.8",
            "http://localhost:0",
            "http://localhost/#token",
        ] {
            assert!(validate(bad).is_err(), "{bad}");
        }
        for good in [
            "http://127.0.0.1:3000",
            "http://100.64.0.1:8080",
            "https://[::1]",
            "http://192.168.1.2",
        ] {
            assert!(validate(good).is_ok(), "{good}");
        }
    }

    #[test]
    fn names_a_page_by_its_title() {
        // Kept as written, and shown with its spaces collapsed.
        let written = "<html><HEAD><Title data-x>\n  Vite &amp;&nbsp;React\n  &lt;dev&gt; </TITLE>";
        let kept = title(written.as_bytes()).unwrap();
        assert_eq!(kept, "\n  Vite &\u{A0}React\n  <dev> ");
        assert_eq!(spaced(&kept), "Vite & React <dev>");
        let page = |html: &str| title(html.as_bytes()).map(|t| spaced(&t));
        assert_eq!(page("<title></title>"), None);
        assert_eq!(page("<title>   </title>"), None);
        assert_eq!(page("<h1>No title</h1>"), None);
        assert_eq!(page("<title>Never closed"), None);
        // Read once, as a browser reads it.
        assert_eq!(
            page("<title>&#49;23&#x34;&#X35;&#0054; &#55&#56; &num;&lowbar;&amp;lt;&amp</title>"),
            Some("123456 78 #_&lt;&".into())
        );
        assert_eq!(
            page("<title>&#0; &#xD800; &#99999999999; &#; &#x; &apos &unknown; & x</title>"),
            Some("\u{FFFD} \u{FFFD} \u{FFFD} &#; &#x; &apos &unknown; & x".into())
        );
        assert_eq!(
            page(
                "<title>abc&eacute;1234 &Eacute;t&eacute &copy;&nbsp;&fjlig; &#150; &#x9D;</title>"
            ),
            Some("abcé1234 Été © fj – \u{9D}".into())
        );
        // The longest name is read, and some without their `;`, even
        // before more letters.
        assert_eq!(
            page(
                "<title>&ltimes; &ltx &notin; &notit; &amp123; &CounterClockwiseContourIntegral;</title>"
            ),
            Some("⋉ <x ∉ ¬it; &123; ∳".into())
        );
        let long = page(&format!("<title>{}</title>", "é".repeat(70))).unwrap();
        assert_eq!(long, "é".repeat(70));
        let shown = short(&long);
        assert_eq!(shown.chars().count(), TITLE_CHARS + 1);
        assert!(shown.ends_with('…'));
        assert_eq!(short("Vite"), "Vite");
    }

    #[test]
    fn knows_one_page_by_any_loopback_name() {
        assert_eq!(loopback_port("http://localhost:5173/"), Some(5173));
        assert_eq!(loopback_port("http://[::1]/"), Some(80));
        assert_eq!(loopback_port("http://[::ffff:127.0.0.1]:3000/"), Some(3000));
        assert_eq!(loopback_port("http://192.168.1.2:5173/"), None);
        assert!(same_page(
            "http://localhost:5173/",
            "http://127.0.0.1:5173/"
        ));
        assert!(same_page(
            "http://[::ffff:127.0.0.1]:80/a?b",
            "http://localhost/a?b"
        ));
        // Each may be another server on the same port.
        for (a, b) in [
            ("http://[::1]:5173/", "http://localhost:5173/"),
            ("http://[::1]:5173/", "http://127.0.0.1:5173/"),
            ("http://127.0.0.2:5173/", "http://127.0.0.1:5173/"),
            ("http://[::ffff:127.0.0.2]:5173/", "http://localhost:5173/"),
        ] {
            assert!(!same_page(a, b), "{a} {b}");
        }
        assert!(!same_page(
            "http://localhost:5173/a",
            "http://localhost:5173/b"
        ));
        assert!(!same_page(
            "http://localhost:5173/",
            "http://localhost:5174/"
        ));
        assert!(!same_page(
            "http://localhost:5173/",
            "https://localhost:5173/"
        ));
        assert!(same_page(
            "http://192.168.1.2:80/",
            "http://192.168.1.2:80/"
        ));
        assert!(!same_page("http://192.168.1.2/", "http://localhost/"));
    }
}

/// HTTP implementation with proxying and redirects disabled. Only a numeric
/// private address (or explicitly loopback-resolved localhost) is contacted.
#[derive(Debug)]
pub struct HttpPreviewChecker {
    client: reqwest::Client,
}
impl HttpPreviewChecker {
    pub fn new() -> Result<Self, ProtocolError> {
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .resolve_to_addrs(
                "localhost",
                &[
                    std::net::SocketAddr::from(([127, 0, 0, 1], 80)),
                    std::net::SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, 80)),
                ],
            )
            .connect_timeout(std::time::Duration::from_secs(2))
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .map_err(|_| {
                ProtocolError::new(
                    ErrorCode::Internal,
                    "Preview HTTP client initialization failed",
                )
            })?;
        Ok(Self { client })
    }
}
#[async_trait]
impl PreviewChecker for HttpPreviewChecker {
    /// Any HTTP answer means a server is listening; even an error page comes
    /// from a running app. HEAD is tried first because it is cheap, but many
    /// development servers refuse it (Python's `http.server` answers 501), so
    /// an error answer to HEAD is settled by a one-byte GET instead.
    async fn check(&self, url: &url::Url) -> Result<bool, ProtocolError> {
        let url = validate(url.as_str())?;
        let Ok(response) = self.client.head(url.clone()).send().await else {
            return Ok(false);
        };
        if response.status().is_client_error() || response.status().is_server_error() {
            let answered = self
                .client
                .get(url)
                .header(reqwest::header::RANGE, "bytes=0-0")
                .send()
                .await;
            return Ok(answered.is_ok());
        }
        Ok(true)
    }

    async fn page(&self, url: &url::Url) -> Result<PageAnswer, ProtocolError> {
        use reqwest::header::{ACCEPT, CONTENT_TYPE, LOCATION};
        let url = validate(url.as_str())?;
        let Ok(mut response) = self
            .client
            .get(url.clone())
            .header(ACCEPT, "text/html")
            .timeout(PAGE_WAIT)
            .send()
            .await
        else {
            return Ok(PageAnswer::Silent);
        };
        let header = |name| {
            response
                .headers()
                .get(name)
                .and_then(|v: &reqwest::header::HeaderValue| v.to_str().ok())
        };
        let html = header(CONTENT_TYPE)
            .is_some_and(|t| t.trim_start().to_ascii_lowercase().starts_with("text/html"));
        let within = response.status().is_redirection()
            && header(LOCATION)
                .and_then(|to| url.join(to).ok())
                .is_some_and(|to| {
                    to.host() == url.host()
                        && to.port_or_known_default() == url.port_or_known_default()
                });
        if within {
            return Ok(PageAnswer::Page(None));
        }
        // A dev server can fail a page while it is still starting.
        if response.status().is_server_error() {
            return Ok(PageAnswer::Silent);
        }
        if !(html && response.status().is_success()) {
            return Ok(PageAnswer::Other);
        }
        let mut body = Vec::new();
        while body.len() < TITLE_WITHIN {
            match response.chunk().await {
                Ok(Some(chunk)) => {
                    let room = TITLE_WITHIN - body.len();
                    body.extend_from_slice(&chunk[..chunk.len().min(room)]);
                }
                _ => break,
            }
        }
        Ok(PageAnswer::Page(title(&body)))
    }
}

/// The port of a page on this computer, when `raw` is one.
pub fn loopback_port(raw: &str) -> Option<u16> {
    let url = url::Url::parse(raw).ok()?;
    let local = match url.host()? {
        url::Host::Domain(name) => name == "localhost",
        url::Host::Ipv4(ip) => ip.is_loopback(),
        url::Host::Ipv6(ip) => {
            ip.is_loopback() || ip.to_ipv4_mapped().is_some_and(|ip| ip.is_loopback())
        }
    };
    local.then(|| url.port_or_known_default()).flatten()
}

/// The address a page on this computer is opened at: `localhost` at
/// 127.0.0.1, as forwarding opens it, and an IPv4 address written as IPv6 at
/// that IPv4 address.
fn loopback_address(url: &url::Url) -> Option<std::net::IpAddr> {
    use std::net::Ipv4Addr;
    match url.host()? {
        url::Host::Domain(name) => (name == "localhost").then_some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        url::Host::Ipv4(ip) => Some(IpAddr::V4(ip)),
        url::Host::Ipv6(ip) => Some(ip.to_ipv4_mapped().map_or(IpAddr::V6(ip), IpAddr::V4)),
    }
}

/// Whether two addresses name the same page. On this computer `localhost`
/// is 127.0.0.1, but `[::1]` and each other 127.* address may be another
/// server on the same port.
pub fn same_page(a: &str, b: &str) -> bool {
    match (url::Url::parse(a), url::Url::parse(b)) {
        (Ok(x), Ok(y)) if loopback_port(a).is_some() && loopback_port(b).is_some() => {
            x.scheme() == y.scheme()
                && loopback_address(&x) == loopback_address(&y)
                && loopback_port(a) == loopback_port(b)
                && x.path() == y.path()
                && x.query() == y.query()
        }
        _ => a == b,
    }
}
