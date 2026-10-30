// SPDX-License-Identifier: Apache-2.0
//! Ophio's own OAuth sign-in, for services without a tool of their own: the
//! provider's page opens in the computer's browser and answers to a one-time
//! address on this computer only (RFC 8252, with PKCE).
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use sha2::Digest;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use zeroize::Zeroizing;

/// The app the owner registered with the provider. Google calls a desktop
/// app's secret not secret, but it is kept in the keyring all the same.
#[derive(Clone, PartialEq, Eq)]
pub struct Client {
    pub id: String,
    pub secret: Option<String>,
}
impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}
impl Client {
    /// Reads Google's download (`{"installed": {...}}` or `{"web": {...}}`)
    /// or a bare `{"client_id", "client_secret"}`.
    pub fn parse(text: &str) -> Result<Self, String> {
        #[derive(Deserialize)]
        struct Fields {
            client_id: String,
            client_secret: Option<String>,
        }
        #[derive(Deserialize)]
        struct Download {
            installed: Option<Fields>,
            web: Option<Fields>,
        }
        let invalid = || {
            "That is not an OAuth client's JSON. Download it again from the provider.".to_owned()
        };
        let value: serde_json::Value = serde_json::from_str(text.trim()).map_err(|_| invalid())?;
        let fields = match serde_json::from_value::<Download>(value.clone()) {
            Ok(Download {
                installed: Some(f), ..
            })
            | Ok(Download { web: Some(f), .. }) => f,
            _ => serde_json::from_value::<Fields>(value).map_err(|_| invalid())?,
        };
        let id = fields.client_id.trim().to_owned();
        if id.is_empty() || id.len() > 256 || id.chars().any(char::is_whitespace) {
            return Err(invalid());
        }
        let secret = fields
            .client_secret
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());
        // A secret masking could miss might show in a task's output.
        if secret
            .as_deref()
            .is_some_and(|s| !ports::accounts::maskable(s))
        {
            return Err(
                "Ophio can't hide this client's secret in what tasks show: it is \
                        shorter than 8 characters or has spaces or quotes."
                    .to_owned(),
            );
        }
        Ok(Self { id, secret })
    }
    /// How it is kept in the vault.
    pub fn to_json(&self) -> Zeroizing<String> {
        Zeroizing::new(
            serde_json::json!({"client_id": self.id, "client_secret": self.secret}).to_string(),
        )
    }
}

/// What the token endpoint gave.
pub struct Tokens {
    pub access: Zeroizing<String>,
    pub expires_at: Instant,
    /// Sent on the first sign-in, and sometimes on refresh.
    pub refresh: Option<Zeroizing<String>>,
    /// The `email` of the ID token, when the scopes asked for one.
    pub identity: Option<String>,
}
impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tokens")
            .field("identity", &self.identity)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum OauthError {
    /// The provider no longer accepts the sign-in.
    #[error("{0}")]
    Refused(String),
    #[error("{0}")]
    Failed(String),
}

#[derive(Deserialize)]
struct TokenAnswer {
    access_token: Option<String>,
    expires_in: Option<u64>,
    refresh_token: Option<String>,
    id_token: Option<String>,
    error: Option<String>,
}

/// The most a token endpoint's answer may be. Its tokens, an ID token
/// included, fit many times over.
const MAX_ANSWER: usize = 64 * 1024;

/// The whole answer, read no further than [`MAX_ANSWER`]: `None` when it is
/// longer, or ends in an error.
async fn answer_body(mut response: reqwest::Response) -> Option<Zeroizing<Vec<u8>>> {
    if response
        .content_length()
        .is_some_and(|n| n > MAX_ANSWER as u64)
    {
        return None;
    }
    // Room for all of it from the start, so tokens are never left behind in
    // a buffer that grew.
    let mut body = Zeroizing::new(Vec::with_capacity(MAX_ANSWER));
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) if body.len() + chunk.len() <= MAX_ANSWER => {
                body.extend_from_slice(&chunk)
            }
            Ok(None) => return Some(body),
            _ => return None,
        }
    }
}

async fn token_request(
    http: &reqwest::Client,
    token_url: &str,
    form: &[(&str, &str)],
) -> Result<Tokens, OauthError> {
    let response = http
        .post(token_url)
        .header("accept", "application/json")
        .form(form)
        .send()
        .await
        .map_err(|e| {
            OauthError::Failed(format!(
                "The provider could not be reached: {}",
                e.without_url()
            ))
        })?;
    let status = response.status();
    let body = answer_body(response).await.ok_or_else(|| {
        OauthError::Failed(format!(
            "The provider's answer ({status}) was longer than Ophio reads, or stopped part way."
        ))
    })?;
    let answer: TokenAnswer = serde_json::from_slice(&body).map_err(|_| {
        OauthError::Failed(format!("The provider answered {status} without tokens."))
    })?;
    if let Some(error) = answer.error {
        // invalid_grant: revoked, expired or the password changed.
        return Err(if error == "invalid_grant" {
            OauthError::Refused(
                "The provider no longer accepts this sign-in. Sign in again.".into(),
            )
        } else if error == "invalid_client" || error == "unauthorized_client" {
            OauthError::Refused("The provider refused the OAuth client. Set it again.".into())
        } else {
            OauthError::Failed(if defined(&error) {
                format!("The provider answered {error}.")
            } else {
                format!("The provider refused the request ({status}).")
            })
        });
    }
    let access = answer.access_token.ok_or_else(|| {
        OauthError::Failed(format!("The provider answered {status} without tokens."))
    })?;
    // A token masking could miss might show in a task's output.
    if !ports::accounts::maskable(&access)
        || answer
            .refresh_token
            .as_deref()
            .is_some_and(|r| !ports::accounts::maskable(r))
    {
        return Err(OauthError::Failed(
            "The provider sent a token Ophio can't hide in what tasks show, so it wasn't kept."
                .into(),
        ));
    }
    Ok(Tokens {
        access: Zeroizing::new(access),
        expires_at: Instant::now()
            + Duration::from_secs(answer.expires_in.unwrap_or(3600).min(86_400)),
        refresh: answer.refresh_token.map(Zeroizing::new),
        identity: answer.id_token.as_deref().and_then(email),
    })
}

/// A fresh access token for a refresh token.
pub async fn refresh(
    http: &reqwest::Client,
    token_url: &str,
    client: &Client,
    refresh: &str,
) -> Result<Tokens, OauthError> {
    let mut form = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh),
        ("client_id", client.id.as_str()),
    ];
    if let Some(secret) = &client.secret {
        form.push(("client_secret", secret));
    }
    token_request(http, token_url, &form).await
}

/// Ends the sign-in at the provider.
pub async fn revoke(http: &reqwest::Client, revoke_url: &str, token: &str) -> Result<(), String> {
    let response = http
        .post(revoke_url)
        .form(&[("token", token)])
        .send()
        .await
        .map_err(|e| format!("The provider could not be reached: {}", e.without_url()))?;
    // 400 means it was already revoked or expired.
    if response.status().is_success() || response.status() == reqwest::StatusCode::BAD_REQUEST {
        Ok(())
    } else {
        Err(format!("The provider answered {}.", response.status()))
    }
}

/// An error code OAuth defines. Only these are shown: anything else a
/// provider says could carry what it was sent.
fn defined(error: &str) -> bool {
    matches!(
        error,
        "invalid_request"
            | "invalid_scope"
            | "unsupported_grant_type"
            | "unsupported_response_type"
            | "unauthorized_client"
            | "server_error"
            | "temporarily_unavailable"
    )
}

/// The `email` claim of an ID token received straight from the token
/// endpoint over TLS, which OpenID Connect lets a client use unverified.
pub fn email(id_token: &str) -> Option<String> {
    let payload = id_token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    claims
        .get("email")
        .and_then(|e| e.as_str())
        .map(str::to_owned)
}

fn random(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).expect("the system's random source failed");
    URL_SAFE_NO_PAD.encode(buf)
}

/// One sign-in in progress: the page to open and the address it answers to.
pub struct Pending {
    listener: TcpListener,
    redirect_uri: String,
    state: String,
    verifier: Zeroizing<String>,
    pub url: String,
}
impl std::fmt::Debug for Pending {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pending")
            .field("redirect_uri", &self.redirect_uri)
            .finish_non_exhaustive()
    }
}

pub async fn begin(
    authorize_url: &str,
    scopes: &[String],
    client: &Client,
) -> Result<Pending, String> {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|e| format!("Ophio could not listen for the provider's answer: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("Ophio could not listen for the provider's answer: {e}"))?
        .port();
    let redirect_uri = format!("http://127.0.0.1:{port}/");
    let state = random(24);
    let verifier = Zeroizing::new(random(48));
    let challenge = URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(verifier.as_bytes()));
    let mut url = url::Url::parse(authorize_url)
        .map_err(|_| "The sign-in address is not valid.".to_owned())?;
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &client.id)
        .append_pair("redirect_uri", &redirect_uri)
        .append_pair("scope", &scopes.join(" "))
        .append_pair("state", &state)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        // A refresh token, and a new one each time the owner signs in.
        .append_pair("access_type", "offline")
        .append_pair("prompt", "consent");
    Ok(Pending {
        listener,
        redirect_uri,
        state,
        verifier,
        url: url.into(),
    })
}

const DONE_PAGE: &str = "<!doctype html><meta charset=utf-8><title>Ophio</title>\
<body style=\"font-family:system-ui;margin:3rem\"><h1>Signed in</h1>\
<p>You can close this tab and go back to Ophio.</p>";
const FAILED_PAGE: &str = "<!doctype html><meta charset=utf-8><title>Ophio</title>\
<body style=\"font-family:system-ui;margin:3rem\"><h1>Not signed in</h1>\
<p>You can close this tab. Ophio shows what happened.</p>";

impl Pending {
    /// Waits for the provider to send the browser back, then trades the
    /// code for tokens.
    pub async fn finish(
        self,
        http: &reqwest::Client,
        token_url: &str,
        client: &Client,
    ) -> Result<Tokens, String> {
        let code = loop {
            let (mut stream, _) =
                self.listener.accept().await.map_err(|e| {
                    format!("Ophio stopped listening for the provider's answer: {e}")
                })?;
            let Some(target) = read_target(&mut stream).await else {
                continue;
            };
            let Ok(url) = url::Url::parse(&format!("http://127.0.0.1{target}")) else {
                continue;
            };
            let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
            // Anything else the browser asks for (a favicon) or an answer to
            // some other sign-in is not this one's.
            if url.path() != "/" || query.get("state") != Some(&self.state) {
                respond(&mut stream, "404 Not Found", "").await;
                continue;
            }
            if let Some(error) = query.get("error") {
                respond(&mut stream, "200 OK", FAILED_PAGE).await;
                return Err(if error == "access_denied" {
                    "Access was not allowed on the provider's page.".into()
                } else if defined(error) {
                    format!("The provider answered {error}.")
                } else {
                    "The provider refused the sign-in.".into()
                });
            }
            let Some(code) = query.get("code") else {
                respond(&mut stream, "200 OK", FAILED_PAGE).await;
                return Err("The provider sent no sign-in code.".into());
            };
            respond(&mut stream, "200 OK", DONE_PAGE).await;
            break Zeroizing::new(code.clone());
        };
        let mut form = vec![
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", self.redirect_uri.as_str()),
            ("client_id", client.id.as_str()),
            ("code_verifier", self.verifier.as_str()),
        ];
        if let Some(secret) = &client.secret {
            form.push(("client_secret", secret));
        }
        let tokens = token_request(http, token_url, &form)
            .await
            .map_err(|e| e.to_string())?;
        if tokens.refresh.is_none() {
            return Err("The provider did not allow Ophio to stay signed in.".into());
        }
        Ok(tokens)
    }
}

/// The request target of a browser's `GET`, read within a few seconds.
async fn read_target(stream: &mut tokio::net::TcpStream) -> Option<String> {
    let mut buf = Vec::with_capacity(1024);
    let read = async {
        let mut chunk = [0u8; 1024];
        while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < 16 * 1024 {
            let n = stream.read(&mut chunk).await.ok()?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        Some(())
    };
    tokio::time::timeout(Duration::from_secs(5), read)
        .await
        .ok()??;
    let head = String::from_utf8_lossy(&buf);
    let mut words = head.lines().next()?.split(' ');
    (words.next()? == "GET").then_some(())?;
    let target = words.next()?;
    target.starts_with('/').then(|| target.to_owned())
}

async fn respond(stream: &mut tokio::net::TcpStream, status: &str, body: &str) {
    let reply = format!(
        "HTTP/1.1 {status}\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\n\
cache-control: no-store\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(reply.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// The file Google's libraries read through `GOOGLE_APPLICATION_CREDENTIALS`.
pub fn google_authorized_user(client: &Client, refresh: &str) -> Zeroizing<String> {
    Zeroizing::new(
        serde_json::json!({
            "type": "authorized_user",
            "client_id": client.id,
            "client_secret": client.secret,
            "refresh_token": refresh,
        })
        .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_google_downloads_and_bare_clients() {
        let installed = r#"{"installed":{"client_id":"1-a.apps.googleusercontent.com","client_secret":"GOCSPX-s3cr3t","redirect_uris":["http://localhost"]}}"#;
        assert_eq!(
            Client::parse(installed).unwrap(),
            Client {
                id: "1-a.apps.googleusercontent.com".into(),
                secret: Some("GOCSPX-s3cr3t".into())
            }
        );
        assert_eq!(Client::parse(r#"{"client_id":"x"}"#).unwrap().secret, None);
        assert!(Client::parse("sk-not-json").is_err());
        assert!(Client::parse(r#"{"installed":{}}"#).is_err());
        assert!(!format!("{:?}", Client::parse(installed).unwrap()).contains("s3cr3t"));
        // A secret masking would miss is refused, not kept.
        for secret in ["s", "with space1", "quote\"d12"] {
            let json = serde_json::json!({"client_id": "x", "client_secret": secret});
            assert!(Client::parse(&json.to_string()).is_err(), "{secret}");
        }
    }

    #[test]
    fn the_identity_is_the_id_tokens_email() {
        let payload = URL_SAFE_NO_PAD.encode(br#"{"email":"owner@example.com","sub":"1"}"#);
        assert_eq!(
            email(&format!("h.{payload}.sig")).as_deref(),
            Some("owner@example.com")
        );
        assert_eq!(email("not-a-token"), None);
    }
}
