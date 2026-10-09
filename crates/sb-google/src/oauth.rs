//! OAuth 2.0 for installed apps: authorization code with PKCE and a loopback
//! redirect on 127.0.0.1 (ADR-0006, accounts-and-auth.md).

use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::Rng;
use second_brain_kernel::Secret;
use second_brain_kernel::source::SourceError;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

pub const SCOPE_OPENID: &str = "openid";
pub const SCOPE_EMAIL: &str = "email";
pub const SCOPE_DRIVE_READONLY: &str = "https://www.googleapis.com/auth/drive.readonly";
pub const SCOPE_CALENDAR_READONLY: &str = "https://www.googleapis.com/auth/calendar.readonly";
pub const SCOPE_MEET_READONLY: &str = "https://www.googleapis.com/auth/meetings.space.readonly";
pub const SCOPE_GMAIL_READONLY: &str = "https://www.googleapis.com/auth/gmail.readonly";

/// Scopes for a set of features (`meet`, `docs`, `gmail`), plus the base scopes.
pub fn scopes_for(features: &[String], meet_api: bool) -> Vec<&'static str> {
    let mut s = vec![SCOPE_OPENID, SCOPE_EMAIL];
    let mut add = |x: &'static str| {
        if !s.contains(&x) {
            s.push(x);
        }
    };
    for f in features {
        match f.as_str() {
            "meet" => {
                add(SCOPE_DRIVE_READONLY);
                add(SCOPE_CALENDAR_READONLY);
                if meet_api {
                    add(SCOPE_MEET_READONLY);
                }
            }
            "docs" => add(SCOPE_DRIVE_READONLY),
            "gmail" => add(SCOPE_GMAIL_READONLY),
            _ => {}
        }
    }
    s
}

/// Errors of the OAuth flow.
#[derive(Debug, thiserror::Error)]
pub enum OAuthError {
    #[error("invalid OAuth client JSON: {0}")]
    InvalidClient(String),
    #[error("authorization was denied or failed: {0}")]
    Denied(String),
    #[error("the refresh token is invalid or revoked (invalid_grant)")]
    InvalidGrant,
    #[error("network error: {0}")]
    Network(String),
    #[error("token endpoint error: {0}")]
    Token(String),
    #[error("timed out waiting for the browser redirect")]
    Timeout,
}

impl From<OAuthError> for SourceError {
    fn from(e: OAuthError) -> Self {
        match e {
            OAuthError::InvalidGrant | OAuthError::Denied(_) | OAuthError::InvalidClient(_) => {
                SourceError::Auth(e.to_string())
            }
            OAuthError::Network(m) => SourceError::Network(m),
            other => SourceError::Api(other.to_string()),
        }
    }
}

/// A Desktop-app OAuth client.
#[derive(Debug, Clone)]
pub struct OAuthClient {
    pub client_id: String,
    client_secret: Secret,
    pub auth_uri: String,
    pub token_uri: String,
}

#[derive(Deserialize)]
struct ClientJson {
    client_id: String,
    client_secret: String,
    #[serde(default)]
    auth_uri: Option<String>,
    #[serde(default)]
    token_uri: Option<String>,
}

impl OAuthClient {
    /// Parse a client JSON as downloaded from Google Cloud (`{"installed": {...}}`).
    pub fn from_json(json: &str) -> Result<Self, OAuthError> {
        let v: Value =
            serde_json::from_str(json).map_err(|e| OAuthError::InvalidClient(e.to_string()))?;
        let inner = v
            .get("installed")
            .or_else(|| v.get("web"))
            .cloned()
            .unwrap_or(v);
        let c: ClientJson =
            serde_json::from_value(inner).map_err(|e| OAuthError::InvalidClient(e.to_string()))?;
        Ok(OAuthClient {
            client_id: c.client_id,
            client_secret: Secret::new(c.client_secret),
            auth_uri: c
                .auth_uri
                .unwrap_or_else(|| "https://accounts.google.com/o/oauth2/auth".into()),
            token_uri: c
                .token_uri
                .unwrap_or_else(|| "https://oauth2.googleapis.com/token".into()),
        })
    }

    /// Override the endpoints (tests).
    pub fn with_endpoints(mut self, auth_uri: String, token_uri: String) -> Self {
        self.auth_uri = auth_uri;
        self.token_uri = token_uri;
        self
    }
}

/// Tokens returned by a code exchange.
#[derive(Debug, Clone)]
pub struct Tokens {
    pub refresh_token: Option<Secret>,
    pub access_token: Secret,
    pub expires_in: u64,
    /// `email` claim of the ID token.
    pub email: Option<String>,
    pub scopes: Vec<String>,
}

fn random_string(len: usize) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";
    let mut rng = rand::rng();
    (0..len)
        .map(|_| CHARS[rng.random_range(0..CHARS.len())] as char)
        .collect()
}

/// S256 code challenge of a verifier.
pub fn code_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// Read the `email` claim from an ID token. The token comes straight from
/// Google's token endpoint over TLS, so its signature is not re-verified.
pub fn id_token_email(id_token: &str) -> Option<String> {
    let payload = id_token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    let v: Value = serde_json::from_slice(&bytes).ok()?;
    v.get("email").and_then(Value::as_str).map(str::to_string)
}

/// A consent flow in progress: the URL to open and the loopback listener.
pub struct PendingAuth {
    pub url: String,
    listener: TcpListener,
    verifier: String,
    state: String,
    redirect_uri: String,
    client: OAuthClient,
}

/// Start a consent flow: bind the loopback listener and build the URL.
pub async fn begin(
    client: &OAuthClient,
    scopes: &[&str],
    login_hint: Option<&str>,
) -> Result<PendingAuth, OAuthError> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| OAuthError::Network(format!("loopback listener: {e}")))?;
    let port = listener
        .local_addr()
        .map_err(|e| OAuthError::Network(e.to_string()))?
        .port();
    let redirect_uri = format!("http://127.0.0.1:{port}");
    let verifier = random_string(64);
    let state = random_string(24);
    let mut url =
        url::Url::parse(&client.auth_uri).map_err(|e| OAuthError::InvalidClient(e.to_string()))?;
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("client_id", &client.client_id)
            .append_pair("redirect_uri", &redirect_uri)
            .append_pair("response_type", "code")
            .append_pair("scope", &scopes.join(" "))
            .append_pair("code_challenge", &code_challenge(&verifier))
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", &state)
            .append_pair("access_type", "offline")
            .append_pair("prompt", "consent")
            .append_pair("include_granted_scopes", "true");
        if let Some(h) = login_hint {
            q.append_pair("login_hint", h);
        }
    }
    Ok(PendingAuth {
        url: url.to_string(),
        listener,
        verifier,
        state,
        redirect_uri,
        client: client.clone(),
    })
}

const DONE_PAGE: &str = "<!doctype html><meta charset=utf-8><title>second-brain</title>\
<p>Authorization complete. You can close this tab and return to the terminal.</p>";

impl PendingAuth {
    /// Wait for the browser redirect, then exchange the code for tokens.
    pub async fn finish(self, timeout: Duration) -> Result<Tokens, OAuthError> {
        let deadline = Instant::now() + timeout;
        let code = loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(OAuthError::Timeout);
            }
            let (mut sock, _) = tokio::time::timeout(remaining, self.listener.accept())
                .await
                .map_err(|_| OAuthError::Timeout)?
                .map_err(|e| OAuthError::Network(e.to_string()))?;
            let mut buf = vec![0u8; 8192];
            let n = sock
                .read(&mut buf)
                .await
                .map_err(|e| OAuthError::Network(e.to_string()))?;
            let req = String::from_utf8_lossy(&buf[..n]);
            let target = req
                .lines()
                .next()
                .and_then(|l| l.split_whitespace().nth(1))
                .unwrap_or("/");
            let parsed = url::Url::parse(&format!("http://127.0.0.1{target}")).ok();
            let get = |k: &str| {
                parsed.as_ref().and_then(|u| {
                    u.query_pairs()
                        .find(|(key, _)| key == k)
                        .map(|(_, v)| v.to_string())
                })
            };
            let (status, body) = match (get("code"), get("error")) {
                (Some(_), _) | (_, Some(_)) => ("200 OK", DONE_PAGE),
                _ => ("404 Not Found", "not found"),
            };
            let _ = sock
                .write_all(
                    format!(
                        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await;
            if let Some(err) = get("error") {
                return Err(OAuthError::Denied(err));
            }
            if let Some(code) = get("code") {
                if get("state").as_deref() != Some(self.state.as_str()) {
                    return Err(OAuthError::Denied("state mismatch".into()));
                }
                break code;
            }
            // Favicon or other requests: keep waiting.
        };
        exchange_code(&self.client, &code, &self.verifier, &self.redirect_uri).await
    }
}

fn http() -> Result<reqwest::Client, OAuthError> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| OAuthError::Network(e.to_string()))
}

async fn token_request(client: &OAuthClient, form: &[(&str, &str)]) -> Result<Value, OAuthError> {
    let resp = http()?
        .post(&client.token_uri)
        .form(form)
        .send()
        .await
        .map_err(|e| OAuthError::Network(e.to_string()))?;
    let status = resp.status();
    let v: Value = resp
        .json()
        .await
        .map_err(|e| OAuthError::Token(e.to_string()))?;
    if status.is_success() {
        return Ok(v);
    }
    let err = v.get("error").and_then(Value::as_str).unwrap_or("unknown");
    if err == "invalid_grant" {
        return Err(OAuthError::InvalidGrant);
    }
    let desc = v
        .get("error_description")
        .and_then(Value::as_str)
        .unwrap_or("");
    Err(OAuthError::Token(format!("{status}: {err} {desc}")))
}

fn tokens_from(v: &Value) -> Result<Tokens, OAuthError> {
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    Ok(Tokens {
        refresh_token: s("refresh_token").map(Secret::new),
        access_token: Secret::new(
            s("access_token").ok_or_else(|| OAuthError::Token("no access_token".into()))?,
        ),
        expires_in: v.get("expires_in").and_then(Value::as_u64).unwrap_or(3600),
        email: s("id_token").as_deref().and_then(id_token_email),
        scopes: s("scope")
            .map(|x| x.split_whitespace().map(str::to_string).collect())
            .unwrap_or_default(),
    })
}

/// Exchange an authorization code.
pub async fn exchange_code(
    client: &OAuthClient,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Result<Tokens, OAuthError> {
    let v = token_request(
        client,
        &[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("code_verifier", verifier),
            ("redirect_uri", redirect_uri),
            ("client_id", &client.client_id),
            ("client_secret", client.client_secret.expose()),
        ],
    )
    .await?;
    tokens_from(&v)
}

/// Get a fresh access token from a refresh token.
pub async fn refresh(client: &OAuthClient, refresh_token: &Secret) -> Result<Tokens, OAuthError> {
    let v = token_request(
        client,
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token.expose()),
            ("client_id", &client.client_id),
            ("client_secret", client.client_secret.expose()),
        ],
    )
    .await?;
    tokens_from(&v)
}

/// Access tokens refreshed on demand and cached in memory only.
pub struct TokenProvider {
    client: OAuthClient,
    refresh_token: Secret,
    cached: Mutex<Option<(Secret, Instant)>>,
}

impl TokenProvider {
    pub fn new(client: OAuthClient, refresh_token: Secret) -> Self {
        TokenProvider {
            client,
            refresh_token,
            cached: Mutex::new(None),
        }
    }

    pub async fn access_token(&self) -> Result<Secret, OAuthError> {
        if let Ok(g) = self.cached.lock()
            && let Some((t, exp)) = g.as_ref()
            && Instant::now() + Duration::from_secs(60) < *exp
        {
            return Ok(t.clone());
        }
        let t = refresh(&self.client, &self.refresh_token).await?;
        if let Ok(mut g) = self.cached.lock() {
            *g = Some((
                t.access_token.clone(),
                Instant::now() + Duration::from_secs(t.expires_in),
            ));
        }
        Ok(t.access_token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const CLIENT: &str = r#"{"installed":{"client_id":"cid.apps.example","client_secret":"csecret","auth_uri":"https://accounts.example/o/oauth2/auth","token_uri":"https://oauth2.example/token","redirect_uris":["http://localhost"]}}"#;

    #[test]
    fn scopes_and_client_parsing() {
        let s = scopes_for(&["meet".into(), "docs".into()], false);
        assert_eq!(
            s,
            vec![
                SCOPE_OPENID,
                SCOPE_EMAIL,
                SCOPE_DRIVE_READONLY,
                SCOPE_CALENDAR_READONLY
            ]
        );
        let c = OAuthClient::from_json(CLIENT).unwrap();
        assert_eq!(c.client_id, "cid.apps.example");
        assert!(OAuthClient::from_json("{}").is_err());
        // RFC 7636 appendix B example.
        assert_eq!(
            code_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn email_from_id_token() {
        let payload = URL_SAFE_NO_PAD.encode(br#"{"email":"user@example.test","sub":"1"}"#);
        assert_eq!(
            id_token_email(&format!("h.{payload}.s")).as_deref(),
            Some("user@example.test")
        );
    }

    #[tokio::test]
    async fn loopback_flow_and_refresh() {
        let server = MockServer::start().await;
        let payload = URL_SAFE_NO_PAD.encode(br#"{"email":"user@example.test"}"#);
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("grant_type=authorization_code"))
            .and(body_string_contains("code=abc"))
            .and(body_string_contains("code_verifier="))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "at1", "refresh_token": "rt1", "expires_in": 3599,
                "id_token": format!("h.{payload}.s"), "scope": "openid email"
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("refresh_token=revoked"))
            .respond_with(
                ResponseTemplate::new(400).set_body_json(json!({"error": "invalid_grant"})),
            )
            .mount(&server)
            .await;
        let client = OAuthClient::from_json(CLIENT).unwrap().with_endpoints(
            "https://accounts.example/auth".into(),
            format!("{}/token", server.uri()),
        );
        let pending = begin(&client, &[SCOPE_OPENID], None).await.unwrap();
        let url = url::Url::parse(&pending.url).unwrap();
        let get = |k: &str| {
            url.query_pairs()
                .find(|(q, _)| q == k)
                .map(|(_, v)| v.to_string())
                .unwrap()
        };
        let redirect = get("redirect_uri");
        let state = get("state");
        assert_eq!(get("code_challenge_method"), "S256");
        // Simulate the browser redirect.
        let browser = tokio::spawn(async move {
            reqwest::get(format!("{redirect}/?state={state}&code=abc"))
                .await
                .unwrap()
                .text()
                .await
                .unwrap()
        });
        let tokens = pending.finish(Duration::from_secs(10)).await.unwrap();
        assert!(browser.await.unwrap().contains("Authorization complete"));
        assert_eq!(tokens.email.as_deref(), Some("user@example.test"));
        assert_eq!(tokens.refresh_token.unwrap().expose(), "rt1");
        let err = refresh(&client, &Secret::new("revoked")).await.unwrap_err();
        assert!(matches!(err, OAuthError::InvalidGrant));
        assert!(matches!(SourceError::from(err), SourceError::Auth(_)));
    }
}
