//! Microsoft Entra ID OAuth for a public client: authorization code with PKCE and
//! a `localhost` loopback redirect (ADR-0021, accounts-and-auth.md).
//!
//! Entra rotates refresh tokens: every refresh may return a new one. The
//! `TokenProvider` hands it to a `RefreshSink` (which stores it) *before* it
//! returns the access token for use.

use std::sync::Arc;
use std::time::{Duration, Instant};

use second_brain_kernel::Secret;
use second_brain_oauth::{ClientConfig, FlowOptions};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;

pub use second_brain_oauth::{OAuthError, PendingAuth, Tokens};

/// Scopes of an account: sign-in claims, a refresh token and read access to the
/// files the user can reach in SharePoint and OneDrive (ADR-0021 decision 7).
pub const SCOPES: &[&str] = &[
    "openid",
    "profile",
    "email",
    "offline_access",
    "Files.Read.All",
];

pub const LOGIN_HOST: &str = "https://login.microsoftonline.com";

/// Where a rotated refresh token is stored. An `Err` fails the refresh.
pub type RefreshSink = Arc<dyn Fn(&Secret) -> Result<(), String> + Send + Sync>;

/// A public client application registered by the user.
#[derive(Debug, Clone)]
pub struct MicrosoftClient {
    pub client_id: String,
    pub tenant: String,
    auth_uri: String,
    token_uri: String,
}

fn is_guid(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    parts.len() == 5
        && parts
            .iter()
            .zip([8usize, 4, 4, 4, 12])
            .all(|(p, n)| p.len() == n && p.chars().all(|c| c.is_ascii_hexdigit()))
}

impl MicrosoftClient {
    /// `tenant` is a tenant ID, a verified domain, or `organizations`,
    /// `consumers` or `common`.
    pub fn new(client_id: &str, tenant: &str) -> Result<Self, OAuthError> {
        let client_id = client_id.trim();
        let tenant = tenant.trim();
        if !is_guid(client_id) {
            return Err(OAuthError::InvalidClient(format!(
                "{client_id:?} is not an Application (client) ID; expected a GUID"
            )));
        }
        // The tenant becomes a URL path segment: allow only what a tenant ID or a
        // domain name can contain.
        if tenant.is_empty()
            || !tenant
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
        {
            return Err(OAuthError::InvalidClient(format!(
                "{tenant:?} is not a tenant ID, domain, organizations, consumers or common"
            )));
        }
        Ok(MicrosoftClient {
            client_id: client_id.to_string(),
            tenant: tenant.to_string(),
            auth_uri: format!("{LOGIN_HOST}/{tenant}/oauth2/v2.0/authorize"),
            token_uri: format!("{LOGIN_HOST}/{tenant}/oauth2/v2.0/token"),
        })
    }

    /// Override the endpoints (tests).
    pub fn with_endpoints(mut self, auth_uri: String, token_uri: String) -> Self {
        self.auth_uri = auth_uri;
        self.token_uri = token_uri;
        self
    }

    fn config(&self) -> ClientConfig {
        ClientConfig {
            client_id: self.client_id.clone(),
            client_secret: None,
            auth_uri: self.auth_uri.clone(),
            token_uri: self.token_uri.clone(),
        }
    }
}

/// The `config` of a Microsoft account (not secret).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MicrosoftConfig {
    /// Application (client) ID of the registered application.
    pub client_id: String,
    #[serde(default = "default_tenant")]
    pub tenant: String,
    /// The scopes that were granted at the last sign-in (informational).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
}

fn default_tenant() -> String {
    "organizations".into()
}

impl MicrosoftConfig {
    pub fn new(client_id: &str, tenant: &str) -> Self {
        MicrosoftConfig {
            client_id: client_id.trim().to_string(),
            tenant: tenant.trim().to_string(),
            scopes: Vec::new(),
        }
    }

    /// Parse the stored account config.
    pub fn from_value(v: &Value) -> Result<Self, OAuthError> {
        serde_json::from_value(v.clone()).map_err(|e| OAuthError::InvalidClient(e.to_string()))
    }

    pub fn to_value(&self) -> Value {
        json!(self)
    }

    /// The OAuth client; fails if the client ID or tenant is malformed.
    pub fn client(&self) -> Result<MicrosoftClient, OAuthError> {
        MicrosoftClient::new(&self.client_id, &self.tenant)
    }
}

/// Who signed in: the tenant and object IDs and the user name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub tid: String,
    pub oid: String,
    pub username: Option<String>,
}

impl Identity {
    /// The account identity stored in the catalog.
    pub fn id(&self) -> String {
        format!("{}:{}", self.tid, self.oid)
    }

    /// From the claims of an ID token.
    pub fn from_tokens(t: &Tokens) -> Option<Identity> {
        let c = t.id_claims()?;
        let s = |k: &str| c.get(k).and_then(|v| v.as_str()).map(str::to_string);
        Some(Identity {
            tid: s("tid")?,
            oid: s("oid")?,
            username: s("preferred_username").or_else(|| s("email")),
        })
    }
}

/// Start a consent flow. The redirect is `http://localhost:<port>`, which Entra
/// accepts for any port when `http://localhost` is registered.
pub async fn begin(
    client: &MicrosoftClient,
    login_hint: Option<&str>,
) -> Result<PendingAuth, OAuthError> {
    let mut extra = vec![("prompt", "select_account")];
    if let Some(h) = login_hint {
        extra.push(("login_hint", h));
    }
    let opts = FlowOptions {
        redirect_host: "localhost",
        extra_params: &extra,
    };
    second_brain_oauth::begin(&client.config(), SCOPES, &opts).await
}

struct State {
    refresh_token: Secret,
    cached: Option<(Secret, Instant)>,
}

/// Access tokens refreshed on demand and cached in memory only. Refreshes are
/// serialized, and a rotated refresh token is stored before it is used.
pub struct TokenProvider {
    client: MicrosoftClient,
    sink: RefreshSink,
    state: Mutex<State>,
}

impl TokenProvider {
    pub fn new(client: MicrosoftClient, refresh_token: Secret, sink: RefreshSink) -> Self {
        TokenProvider {
            client,
            sink,
            state: Mutex::new(State {
                refresh_token,
                cached: None,
            }),
        }
    }

    pub async fn access_token(&self) -> Result<Secret, OAuthError> {
        let mut st = self.state.lock().await;
        if let Some((t, exp)) = &st.cached
            && Instant::now() + Duration::from_secs(60) < *exp
        {
            return Ok(t.clone());
        }
        let t = second_brain_oauth::refresh(&self.client.config(), &st.refresh_token).await?;
        if let Some(new) = &t.refresh_token
            && new.expose() != st.refresh_token.expose()
        {
            (self.sink)(new).map_err(|e| {
                OAuthError::Token(format!("could not store the new refresh token: {e}"))
            })?;
            st.refresh_token = new.clone();
        }
        st.cached = Some((
            t.access_token.clone(),
            Instant::now() + Duration::from_secs(t.expires_in),
        ));
        Ok(t.access_token)
    }

    /// Forget the cached access token (after a 401).
    pub async fn invalidate(&self) {
        self.state.lock().await.cached = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use serde_json::json;
    use std::sync::Mutex as StdMutex;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const CID: &str = "11111111-2222-3333-4444-555555555555";

    #[test]
    fn client_validation() {
        let c = MicrosoftClient::new(CID, "organizations").unwrap();
        assert_eq!(
            c.auth_uri,
            "https://login.microsoftonline.com/organizations/oauth2/v2.0/authorize"
        );
        assert!(MicrosoftClient::new("not-a-guid", "common").is_err());
        assert!(MicrosoftClient::new(CID, "x/../y").is_err());
        assert!(MicrosoftClient::new(CID, "contoso.onmicrosoft.com").is_ok());
        assert!(MicrosoftClient::new(CID, "").is_err());
    }

    #[test]
    fn identity_from_claims() {
        let payload = URL_SAFE_NO_PAD
            .encode(br#"{"tid":"T1","oid":"O1","preferred_username":"a@example.test"}"#);
        let t = Tokens {
            refresh_token: None,
            access_token: Secret::new("a"),
            expires_in: 1,
            id_token: Some(format!("h.{payload}.s")),
            scopes: vec![],
        };
        let id = Identity::from_tokens(&t).unwrap();
        assert_eq!(id.id(), "T1:O1");
        assert_eq!(id.username.as_deref(), Some("a@example.test"));
    }

    fn provider(server: &MockServer, sink: RefreshSink, refresh: &str) -> TokenProvider {
        let client = MicrosoftClient::new(CID, "common").unwrap().with_endpoints(
            "https://login.example/authorize".into(),
            format!("{}/token", server.uri()),
        );
        TokenProvider::new(client, Secret::new(refresh), sink)
    }

    #[tokio::test]
    async fn rotated_refresh_token_is_stored_before_use() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("refresh_token=rt0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "at1", "refresh_token": "rt1", "expires_in": 3600
            })))
            .expect(1)
            .mount(&server)
            .await;
        let stored = Arc::new(StdMutex::new(Vec::<String>::new()));
        let s2 = stored.clone();
        let sink: RefreshSink = Arc::new(move |t: &Secret| {
            s2.lock().unwrap().push(t.expose().to_string());
            Ok(())
        });
        let p = provider(&server, sink, "rt0");
        assert_eq!(p.access_token().await.unwrap().expose(), "at1");
        // Cached: no second token request (expect(1) above).
        assert_eq!(p.access_token().await.unwrap().expose(), "at1");
        assert_eq!(*stored.lock().unwrap(), vec!["rt1".to_string()]);
    }

    #[tokio::test]
    async fn failed_store_fails_the_refresh() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "at1", "refresh_token": "rt1", "expires_in": 3600
            })))
            .mount(&server)
            .await;
        let sink: RefreshSink = Arc::new(|_| Err("disk full".into()));
        let p = provider(&server, sink, "rt0");
        let err = p.access_token().await.unwrap_err();
        assert!(matches!(&err, OAuthError::Token(m) if m.contains("disk full")));
    }

    #[tokio::test]
    async fn unchanged_refresh_token_is_not_stored_again() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "at1", "refresh_token": "rt0", "expires_in": 3600
            })))
            .mount(&server)
            .await;
        let sink: RefreshSink = Arc::new(|_| Err("must not be called".into()));
        let p = provider(&server, sink, "rt0");
        assert_eq!(p.access_token().await.unwrap().expose(), "at1");
    }

    #[tokio::test]
    async fn reauth_errors_surface() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(
                json!({"error": "invalid_grant", "error_description": "AADSTS70008 expired"}),
            ))
            .mount(&server)
            .await;
        let sink: RefreshSink = Arc::new(|_| Ok(()));
        let p = provider(&server, sink, "rt0");
        assert!(p.access_token().await.unwrap_err().needs_reauth());
    }
}
