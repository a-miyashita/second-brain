//! OAuth 2.0 for installed apps: authorization code with PKCE and a loopback
//! redirect (ADR-0006, ADR-0021, accounts-and-auth.md).
//!
//! Provider crates (`sb-google`, `sb-microsoft`) keep their scopes, client
//! formats and token lifecycle. This crate holds only what is the same for
//! both: PKCE, the loopback listener, and the token endpoint requests.

use std::future::poll_fn;
use std::task::Poll;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::Rng;
use second_brain_kernel::Secret;
use second_brain_kernel::source::SourceError;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Errors of the OAuth flow.
#[derive(Debug, thiserror::Error)]
pub enum OAuthError {
    #[error("invalid OAuth client: {0}")]
    InvalidClient(String),
    #[error("authorization was denied or failed: {0}")]
    Denied(String),
    #[error("the refresh token is invalid or revoked (invalid_grant)")]
    InvalidGrant,
    /// The provider wants an interactive sign-in or consent again
    /// (`interaction_required`, `consent_required`, `login_required`).
    #[error("signing in again is required: {0}")]
    InteractionRequired(String),
    #[error("network error: {0}")]
    Network(String),
    #[error("token endpoint error: {0}")]
    Token(String),
    #[error("timed out waiting for the browser redirect")]
    Timeout,
}

impl OAuthError {
    /// Whether the account has to sign in again (`needs_reauth`).
    pub fn needs_reauth(&self) -> bool {
        matches!(
            self,
            OAuthError::InvalidGrant | OAuthError::InteractionRequired(_)
        )
    }
}

impl From<OAuthError> for SourceError {
    fn from(e: OAuthError) -> Self {
        match e {
            OAuthError::InvalidGrant
            | OAuthError::InteractionRequired(_)
            | OAuthError::Denied(_)
            | OAuthError::InvalidClient(_) => SourceError::Auth(e.to_string()),
            OAuthError::Network(m) => SourceError::Network(m),
            other => SourceError::Api(other.to_string()),
        }
    }
}

/// An OAuth client and the endpoints it talks to. A public client has no secret.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub client_id: String,
    pub client_secret: Option<Secret>,
    pub auth_uri: String,
    pub token_uri: String,
}

/// Tokens returned by a token endpoint.
#[derive(Debug, Clone)]
pub struct Tokens {
    pub refresh_token: Option<Secret>,
    pub access_token: Secret,
    pub expires_in: u64,
    /// The raw ID token (a JWT), if the provider returned one.
    pub id_token: Option<String>,
    pub scopes: Vec<String>,
}

impl Tokens {
    /// The claims of the ID token.
    pub fn id_claims(&self) -> Option<Value> {
        self.id_token.as_deref().and_then(id_token_claims)
    }
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

/// The claims of an ID token. The token comes straight from the provider's token
/// endpoint over TLS, so its signature is not re-verified.
pub fn id_token_claims(id_token: &str) -> Option<Value> {
    let payload = id_token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// A consent flow in progress: the URL to open and the loopback listeners.
pub struct PendingAuth {
    pub url: String,
    listeners: Vec<TcpListener>,
    verifier: String,
    state: String,
    redirect_uri: String,
    client: ClientConfig,
}

/// How the redirect and the authorization URL are built.
#[derive(Debug, Clone, Copy)]
pub struct FlowOptions<'a> {
    /// Host in the redirect URI: `127.0.0.1`, or `localhost`. For `localhost` the
    /// IPv6 loopback is listened on too, because a browser may resolve the name
    /// to `::1`.
    pub redirect_host: &'a str,
    /// Extra query parameters of the authorization URL (`prompt`, `login_hint`, ...).
    pub extra_params: &'a [(&'a str, &'a str)],
}

/// Start a consent flow: bind the loopback listeners and build the URL.
pub async fn begin(
    client: &ClientConfig,
    scopes: &[&str],
    opts: &FlowOptions<'_>,
) -> Result<PendingAuth, OAuthError> {
    let first = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| OAuthError::Network(format!("loopback listener: {e}")))?;
    let port = first
        .local_addr()
        .map_err(|e| OAuthError::Network(e.to_string()))?
        .port();
    let mut listeners = vec![first];
    if opts.redirect_host == "localhost"
        && let Ok(v6) = TcpListener::bind(("::1", port)).await
    {
        listeners.push(v6);
    }
    let redirect_uri = format!("http://{}:{port}", opts.redirect_host);
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
            .append_pair("state", &state);
        for (k, v) in opts.extra_params {
            q.append_pair(k, v);
        }
    }
    Ok(PendingAuth {
        url: url.to_string(),
        listeners,
        verifier,
        state,
        redirect_uri,
        client: client.clone(),
    })
}

const DONE_PAGE: &str = "<!doctype html><meta charset=utf-8><title>second-brain</title>\
<p>Authorization complete. You can close this tab and return to the terminal.</p>";

async fn accept_any(listeners: &[TcpListener]) -> std::io::Result<TcpStream> {
    poll_fn(|cx| {
        for l in listeners {
            if let Poll::Ready(r) = l.poll_accept(cx) {
                return Poll::Ready(r.map(|(s, _)| s));
            }
        }
        Poll::Pending
    })
    .await
}

impl PendingAuth {
    /// Wait for the browser redirect, then exchange the code for tokens.
    pub async fn finish(self, timeout: Duration) -> Result<Tokens, OAuthError> {
        let deadline = Instant::now() + timeout;
        let code = loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(OAuthError::Timeout);
            }
            let mut sock = tokio::time::timeout(remaining, accept_any(&self.listeners))
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
                return Err(OAuthError::Denied(match get("error_description") {
                    Some(d) if !d.is_empty() => format!("{err}: {d}"),
                    _ => err,
                }));
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

async fn token_request(client: &ClientConfig, form: &[(&str, &str)]) -> Result<Value, OAuthError> {
    let mut form: Vec<(&str, &str)> = form.to_vec();
    form.push(("client_id", &client.client_id));
    if let Some(secret) = &client.client_secret {
        form.push(("client_secret", secret.expose()));
    }
    let resp = http()?
        .post(&client.token_uri)
        .form(&form)
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
    let desc = v
        .get("error_description")
        .and_then(Value::as_str)
        .unwrap_or("");
    match err {
        "invalid_grant" => Err(OAuthError::InvalidGrant),
        "interaction_required" | "consent_required" | "login_required" => {
            Err(OAuthError::InteractionRequired(format!("{err} {desc}")))
        }
        _ => Err(OAuthError::Token(format!("{status}: {err} {desc}"))),
    }
}

fn tokens_from(v: &Value) -> Result<Tokens, OAuthError> {
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    Ok(Tokens {
        refresh_token: s("refresh_token").map(Secret::new),
        access_token: Secret::new(
            s("access_token").ok_or_else(|| OAuthError::Token("no access_token".into()))?,
        ),
        expires_in: v.get("expires_in").and_then(Value::as_u64).unwrap_or(3600),
        id_token: s("id_token"),
        scopes: s("scope")
            .map(|x| x.split_whitespace().map(str::to_string).collect())
            .unwrap_or_default(),
    })
}

/// Exchange an authorization code.
pub async fn exchange_code(
    client: &ClientConfig,
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
        ],
    )
    .await?;
    tokens_from(&v)
}

/// Get a fresh access token from a refresh token. The response may carry a new
/// refresh token (Microsoft rotates them); the caller must keep it.
pub async fn refresh(client: &ClientConfig, refresh_token: &Secret) -> Result<Tokens, OAuthError> {
    let v = token_request(
        client,
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token.expose()),
        ],
    )
    .await?;
    tokens_from(&v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client(token_uri: String, secret: Option<&str>) -> ClientConfig {
        ClientConfig {
            client_id: "cid".into(),
            client_secret: secret.map(Secret::new),
            auth_uri: "https://auth.example/authorize".into(),
            token_uri,
        }
    }

    #[test]
    fn pkce_matches_rfc7636() {
        assert_eq!(
            code_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn claims_from_id_token() {
        let payload = URL_SAFE_NO_PAD.encode(br#"{"email":"user@example.test","sub":"1"}"#);
        let c = id_token_claims(&format!("h.{payload}.s")).unwrap();
        assert_eq!(c["email"], "user@example.test");
        assert!(id_token_claims("nonsense").is_none());
    }

    async fn browser_redirect(
        pending: &PendingAuth,
        query: &str,
    ) -> tokio::task::JoinHandle<String> {
        let url = url::Url::parse(&pending.url).unwrap();
        let redirect = url
            .query_pairs()
            .find(|(k, _)| k == "redirect_uri")
            .map(|(_, v)| v.to_string())
            .unwrap();
        let state = url
            .query_pairs()
            .find(|(k, _)| k == "state")
            .map(|(_, v)| v.to_string())
            .unwrap();
        let query = query.replace("{state}", &state);
        tokio::spawn(async move {
            reqwest::get(format!("{redirect}/?{query}"))
                .await
                .unwrap()
                .text()
                .await
                .unwrap()
        })
    }

    #[tokio::test]
    async fn loopback_flow_public_client_on_localhost() {
        let server = MockServer::start().await;
        let payload = URL_SAFE_NO_PAD.encode(br#"{"tid":"t","oid":"o"}"#);
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("grant_type=authorization_code"))
            .and(body_string_contains("code=abc"))
            .and(body_string_contains("code_verifier="))
            .and(body_string_contains("client_id=cid"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "at1", "refresh_token": "rt1", "expires_in": 3599,
                "id_token": format!("h.{payload}.s"), "scope": "openid email"
            })))
            .mount(&server)
            .await;
        let c = client(format!("{}/token", server.uri()), None);
        let opts = FlowOptions {
            redirect_host: "localhost",
            extra_params: &[("prompt", "select_account")],
        };
        let pending = begin(&c, &["openid"], &opts).await.unwrap();
        let url = url::Url::parse(&pending.url).unwrap();
        let q = |k: &str| {
            url.query_pairs()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.to_string())
        };
        assert!(q("redirect_uri").unwrap().starts_with("http://localhost:"));
        assert_eq!(q("code_challenge_method").as_deref(), Some("S256"));
        assert_eq!(q("prompt").as_deref(), Some("select_account"));
        let browser = browser_redirect(&pending, "state={state}&code=abc").await;
        let tokens = pending.finish(Duration::from_secs(10)).await.unwrap();
        assert!(browser.await.unwrap().contains("Authorization complete"));
        assert_eq!(tokens.refresh_token.as_ref().unwrap().expose(), "rt1");
        assert_eq!(tokens.id_claims().unwrap()["oid"], "o");
    }

    #[tokio::test]
    async fn denied_redirect_carries_the_description() {
        let c = client("http://127.0.0.1:1/token".into(), None);
        let opts = FlowOptions {
            redirect_host: "127.0.0.1",
            extra_params: &[],
        };
        let pending = begin(&c, &["openid"], &opts).await.unwrap();
        let browser = browser_redirect(
            &pending,
            "error=access_denied&error_description=admin+needed",
        )
        .await;
        let err = pending.finish(Duration::from_secs(10)).await.unwrap_err();
        let _ = browser.await;
        assert!(matches!(&err, OAuthError::Denied(m) if m == "access_denied: admin needed"));
    }

    #[tokio::test]
    async fn token_errors_are_classified() {
        let server = MockServer::start().await;
        for (token, error) in [
            ("revoked", "invalid_grant"),
            ("stale", "interaction_required"),
            ("other", "server_error"),
        ] {
            Mock::given(method("POST"))
                .and(path("/token"))
                .and(body_string_contains(format!("refresh_token={token}")))
                .and(body_string_contains("client_secret=s"))
                .respond_with(
                    ResponseTemplate::new(400).set_body_json(
                        json!({"error": error, "error_description": "AADSTS0 text"}),
                    ),
                )
                .mount(&server)
                .await;
        }
        let c = client(format!("{}/token", server.uri()), Some("s"));
        let e = refresh(&c, &Secret::new("revoked")).await.unwrap_err();
        assert!(matches!(e, OAuthError::InvalidGrant) && e.needs_reauth());
        let e = refresh(&c, &Secret::new("stale")).await.unwrap_err();
        assert!(matches!(e, OAuthError::InteractionRequired(_)) && e.needs_reauth());
        assert!(matches!(SourceError::from(e), SourceError::Auth(_)));
        let e = refresh(&c, &Secret::new("other")).await.unwrap_err();
        assert!(matches!(e, OAuthError::Token(_)) && !e.needs_reauth());
    }
}
