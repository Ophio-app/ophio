// SPDX-License-Identifier: Apache-2.0
//! Asking GitHub for Øphio's newest release, for the daily check (see
//! `services::ReleaseCheck`).
//!
//! The question goes to GitHub's public "latest release" of [`REPOSITORY`],
//! which leaves out drafts and pre-releases, as a plain HTTPS request: no
//! GitHub account or token, no cookies, and nothing of the person's beyond
//! the minimal `User-Agent` GitHub requires.
use ports::releases::{LatestRelease, ReleaseSource};
use std::time::Duration;

/// The repository whose releases this build follows. A fork that publishes
/// its own releases points this at its repository.
pub const REPOSITORY: &str = "Ophio-app/ophio";
/// GitHub's API, without a trailing slash.
const API: &str = "https://api.github.com";
/// Asking gives up after this; the next check asks again.
const TIMEOUT: Duration = Duration::from_secs(20);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// The most of an answer read. A release's page with its downloads listed
/// is a few tens of kilobytes.
const MOST: usize = 1024 * 1024;

/// Whether this build checks for new versions until the person chooses:
/// public builds do, personal ones, built from the newest source with
/// `OPHIO_PERSONAL_SIGNING=1`, already have everything and don't.
pub const ON_BY_DEFAULT: bool = !crate::phone::PERSONAL_BUILD;

#[derive(Debug)]
pub struct GithubReleases {
    client: reqwest::Client,
    url: String,
    repository: String,
}

impl GithubReleases {
    /// None when no client could be made as it must be, with its name and
    /// timeouts: then the computer doesn't check at all.
    pub fn new() -> Option<Self> {
        Self::at(API, REPOSITORY)
    }
    /// Asks `api` about `repository`'s releases.
    pub fn at(api: &str, repository: &str) -> Option<Self> {
        // A client of its own, so no default header, credential or cookie
        // store set up for other requests comes along.
        let client = reqwest::Client::builder()
            .user_agent("ophio")
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(TIMEOUT)
            .build()
            .ok()?;
        Some(Self {
            client,
            url: format!(
                "{}/repos/{repository}/releases/latest",
                api.trim_end_matches('/')
            ),
            repository: repository.into(),
        })
    }
}

#[derive(serde::Deserialize)]
struct Answer {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}

/// A tag that can go in a link as it is.
fn plain(tag: &str) -> bool {
    !tag.is_empty()
        && tag.len() <= 100
        && (tag.bytes())
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'+'))
}

/// The latest release in GitHub's answer `body`. The release page is made
/// here from the tag, not taken from the answer, so it always leads to this
/// repository's releases on GitHub. A draft, a pre-release or a tag that
/// can't go in a link is no usable answer, as a failure is: what the
/// computer knew stays.
fn read(body: &[u8], repository: &str) -> Result<Option<LatestRelease>, String> {
    let answer: Answer = serde_json::from_slice(body).map_err(|e| format!("not a release: {e}"))?;
    if answer.draft || answer.prerelease || !plain(&answer.tag_name) {
        return Err(format!("not a usable release: {:?}", answer.tag_name));
    }
    Ok(Some(LatestRelease {
        url: format!(
            "https://github.com/{repository}/releases/tag/{}",
            answer.tag_name
        ),
        tag: answer.tag_name,
    }))
}

#[async_trait::async_trait]
impl ReleaseSource for GithubReleases {
    async fn latest(&self) -> Result<Option<LatestRelease>, String> {
        let mut response = self
            .client
            .get(&self.url)
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .send()
            .await
            .map_err(|e| e.without_url().to_string())?;
        // A 404 is a repository with no release yet, or one moved or made
        // private: either way no usable answer, so what was known stays.
        if !response.status().is_success() {
            return Err(format!("GitHub answered {}", response.status()));
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| e.without_url().to_string())?
        {
            if body.len() + chunk.len() > MOST {
                return Err("the answer was too long".into());
            }
            body.extend_from_slice(&chunk);
        }
        read(&body, &self.repository)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn the_latest_release_is_read_and_linked_on_github() {
        let body = br#"{"tag_name":"v0.3.0","draft":false,"prerelease":false,"html_url":"https://example.com/elsewhere","assets":[]}"#;
        assert_eq!(
            read(body, REPOSITORY).unwrap(),
            Some(LatestRelease {
                tag: "v0.3.0".into(),
                url: "https://github.com/Ophio-app/ophio/releases/tag/v0.3.0".into(),
            })
        );
        for body in [
            r#"{"tag_name":"v0.3.0","prerelease":true}"#,
            r#"{"tag_name":"v0.3.0","draft":true}"#,
            r#"{"tag_name":"v0.3.0/../../evil"}"#,
            r#"{"tag_name":""}"#,
        ] {
            assert!(read(body.as_bytes(), REPOSITORY).is_err(), "{body}");
        }
        assert!(read(b"<html>", REPOSITORY).is_err());
    }

    #[test]
    fn personal_builds_dont_check_by_default() {
        assert_eq!(ON_BY_DEFAULT, !cfg!(ophio_personal_signing));
    }

    /// A server on this computer that answers one request with `answer`,
    /// and hands back what was asked.
    fn serve_once(answer: &'static str) -> (String, std::thread::JoinHandle<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let asked = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut buf = [0; 4096];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = stream.read(&mut buf).unwrap();
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..n]);
            }
            stream.write_all(answer.as_bytes()).unwrap();
            String::from_utf8_lossy(&request).into_owned()
        });
        (url, asked)
    }

    #[tokio::test]
    async fn the_question_carries_nothing_but_what_http_needs() {
        let body = r#"{"tag_name":"v9.9.9","draft":false,"prerelease":false}"#;
        let answer: &'static str = Box::leak(
            format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            )
            .into_boxed_str(),
        );
        let (url, asked) = serve_once(answer);
        let source = GithubReleases::at(&url, "someone/fork").unwrap();
        let latest = source.latest().await.unwrap().unwrap();
        assert_eq!(latest.tag, "v9.9.9");
        assert_eq!(
            latest.url,
            "https://github.com/someone/fork/releases/tag/v9.9.9"
        );
        let request = asked.join().unwrap().to_ascii_lowercase();
        assert!(request.starts_with("get /repos/someone/fork/releases/latest http/1.1\r\n"));
        let headers: Vec<&str> = (request.lines().skip(1))
            .filter(|l| !l.is_empty())
            .map(|l| l.split(':').next().unwrap())
            .collect();
        for header in &headers {
            assert!(
                ["host", "user-agent", "accept", "accept-encoding"].contains(header),
                "{header} was sent"
            );
        }
        assert!(request.contains("user-agent: ophio\r\n"));
        assert!(!request.contains("authorization") && !request.contains("cookie"));
    }

    #[tokio::test]
    async fn no_release_a_refusal_or_no_connection_is_no_usable_answer() {
        let (url, asked) =
            serve_once("HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
        assert!(
            GithubReleases::at(&url, REPOSITORY)
                .unwrap()
                .latest()
                .await
                .is_err()
        );
        asked.join().unwrap();
        let (url, asked) =
            serve_once("HTTP/1.1 403 Forbidden\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
        assert!(
            GithubReleases::at(&url, REPOSITORY)
                .unwrap()
                .latest()
                .await
                .is_err()
        );
        asked.join().unwrap();
        // Nothing listening: offline, as far as this can tell.
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", closed.local_addr().unwrap());
        drop(closed);
        assert!(
            GithubReleases::at(&url, REPOSITORY)
                .unwrap()
                .latest()
                .await
                .is_err()
        );
    }
}
