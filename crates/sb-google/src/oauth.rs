//! Google OAuth 2.0 for installed apps (ADR-0006, accounts-and-auth.md): the
//! Google scopes, the client JSON and the token provider. The generic flow
//! (PKCE, loopback redirect, token requests) is in `second-brain-oauth`.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use second_brain_kernel::Secret;
use second_brain_oauth::{ClientConfig, FlowOptions};
use serde::Deserialize;
use serde_json::Value;

pub use second_brain_oauth::{OAuthError, code_challenge};

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

/// Tokens returned by a code exchange or a refresh.
#[derive(Debug, Clone)]
pub struct Tokens {
    pub refresh_token: Option<Secret>,
    pub access_token: Secret,
    pub expires_in: u64,
    /// `email` claim of the ID token.
    pub email: Option<String>,
    pub scopes: Vec<String>,
}

impl From<second_brain_oauth::Tokens> for Tokens {
    fn from(t: second_brain_oauth::Tokens) -> Self {
        let email = t.id_token.as_deref().and_then(id_token_email);
        Tokens {
            refresh_token: t.refresh_token,
            access_token: t.access_token,
            expires_in: t.expires_in,
            email,
            scopes: t.scopes,
        }
    }
}

/// Read the `email` claim from an ID token (see `second_brain_oauth::id_token_claims`).
pub fn id_token_email(id_token: &str) -> Option<String> {
    second_brain_oauth::id_token_claims(id_token)?
        .get("email")
        .and_then(Value::as_str)
        .map(str::to_string)
}

impl OAuthClient {
    fn config(&self) -> ClientConfig {
        ClientConfig {
            client_id: self.client_id.clone(),
            client_secret: Some(self.client_secret.clone()),
            auth_uri: self.auth_uri.clone(),
            token_uri: self.token_uri.clone(),
        }
    }
}

/// A consent flow in progress: the URL to open and the loopback listener.
pub struct PendingAuth {
    pub url: String,
    inner: second_brain_oauth::PendingAuth,
}

/// Start a consent flow: bind the loopback listener and build the URL.
pub async fn begin(
    client: &OAuthClient,
    scopes: &[&str],
    login_hint: Option<&str>,
) -> Result<PendingAuth, OAuthError> {
    let mut extra = vec![
        ("access_type", "offline"),
        ("prompt", "consent"),
        ("include_granted_scopes", "true"),
    ];
    if let Some(h) = login_hint {
        extra.push(("login_hint", h));
    }
    let opts = FlowOptions {
        redirect_host: "127.0.0.1",
        extra_params: &extra,
    };
    let inner = second_brain_oauth::begin(&client.config(), scopes, &opts).await?;
    Ok(PendingAuth {
        url: inner.url.clone(),
        inner,
    })
}

impl PendingAuth {
    /// Wait for the browser redirect, then exchange the code for tokens.
    pub async fn finish(self, timeout: Duration) -> Result<Tokens, OAuthError> {
        Ok(self.inner.finish(timeout).await?.into())
    }
}

/// Get a fresh access token from a refresh token.
pub async fn refresh(client: &OAuthClient, refresh_token: &Secret) -> Result<Tokens, OAuthError> {
    Ok(second_brain_oauth::refresh(&client.config(), refresh_token)
        .await?
        .into())
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
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use second_brain_kernel::source::SourceError;
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
