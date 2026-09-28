//! A thin Slack Web API client (user token).

use std::time::Duration;

use sb_core::Secret;
use sb_core::source::SourceError;
use serde_json::Value;

/// Default API base.
pub const SLACK_API: &str = "https://slack.com/api";

const MAX_ATTEMPTS: u32 = 5;

/// Result of `auth.test`.
#[derive(Debug, Clone, PartialEq)]
pub struct AuthInfo {
    pub team_id: String,
    pub user_id: String,
    pub team: String,
    pub user: String,
    /// Workspace URL, e.g. `https://acme.slack.com/`.
    pub url: String,
    /// Granted scopes from the `x-oauth-scopes` header.
    pub scopes: Vec<String>,
}

/// User scopes the Slack app manifest requests.
pub const REQUIRED_SCOPES: &[&str] = &[
    "channels:history",
    "channels:read",
    "groups:history",
    "groups:read",
    "im:history",
    "im:read",
    "mpim:history",
    "mpim:read",
    "users:read",
    "users:read.email",
    "search:read",
    "files:read",
];

/// Slack Web API client.
#[derive(Clone)]
pub struct SlackClient {
    http: reqwest::Client,
    base: String,
    token: Secret,
}

impl std::fmt::Debug for SlackClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SlackClient")
            .field("base", &self.base)
            .finish()
    }
}

fn is_auth_error(code: &str) -> bool {
    matches!(
        code,
        "invalid_auth"
            | "not_authed"
            | "token_revoked"
            | "token_expired"
            | "account_inactive"
            | "no_permission"
            | "missing_scope"
            | "not_allowed_token_type"
    )
}

impl SlackClient {
    pub fn new(token: Secret, base: Option<String>) -> Result<Self, SourceError> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("second-brain/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| SourceError::Network(e.to_string()))?;
        Ok(SlackClient {
            http,
            base: base
                .unwrap_or_else(|| SLACK_API.to_string())
                .trim_end_matches('/')
                .to_string(),
            token,
        })
    }

    /// Call a read method with query parameters. Honours `Retry-After` on
    /// HTTP 429 and maps Slack error codes.
    pub async fn call(
        &self,
        method: &str,
        params: &[(&str, String)],
    ) -> Result<(Value, reqwest::header::HeaderMap), SourceError> {
        let url = format!("{}/{method}", self.base);
        let mut attempt = 0;
        loop {
            attempt += 1;
            let resp = self
                .http
                .get(&url)
                .bearer_auth(self.token.expose())
                .query(params)
                .send()
                .await
                .map_err(|e| SourceError::Network(format!("{method}: {e}")))?;
            let status = resp.status();
            let headers = resp.headers().clone();
            if status.as_u16() == 429 {
                let wait = headers
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(30);
                if attempt >= MAX_ATTEMPTS {
                    return Err(SourceError::RateLimited(format!(
                        "{method}: still rate limited after {attempt} attempts"
                    )));
                }
                tracing::info!(method, wait, "Slack rate limit; waiting");
                tokio::time::sleep(Duration::from_secs(wait.min(300))).await;
                continue;
            }
            if status.is_server_error() {
                if attempt >= MAX_ATTEMPTS {
                    return Err(SourceError::Api(format!("{method}: HTTP {status}")));
                }
                tokio::time::sleep(Duration::from_secs(2u64.pow(attempt))).await;
                continue;
            }
            let v: Value = resp
                .json()
                .await
                .map_err(|e| SourceError::Parse(format!("{method}: {e}")))?;
            if v.get("ok").and_then(Value::as_bool) == Some(true) {
                return Ok((v, headers));
            }
            let code = v
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("unknown_error")
                .to_string();
            if code == "ratelimited" && attempt < MAX_ATTEMPTS {
                tokio::time::sleep(Duration::from_secs(30)).await;
                continue;
            }
            if is_auth_error(&code) {
                return Err(SourceError::Auth(format!("Slack {method}: {code}")));
            }
            return Err(SourceError::Api(format!("Slack {method}: {code}")));
        }
    }

    /// Call a cursor-paginated method and collect the array under `key`.
    pub async fn paginate(
        &self,
        method: &str,
        params: &[(&str, String)],
        key: &str,
        max_pages: usize,
    ) -> Result<Vec<Value>, SourceError> {
        let mut out = Vec::new();
        let mut cursor = String::new();
        for _ in 0..max_pages {
            let mut p: Vec<(&str, String)> = params.to_vec();
            if !cursor.is_empty() {
                p.push(("cursor", cursor.clone()));
            }
            let (v, _) = self.call(method, &p).await?;
            if let Some(items) = v.get(key).and_then(Value::as_array) {
                out.extend(items.iter().cloned());
            }
            cursor = v
                .pointer("/response_metadata/next_cursor")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if cursor.is_empty() {
                break;
            }
        }
        Ok(out)
    }

    pub async fn auth_test(&self) -> Result<AuthInfo, SourceError> {
        let (v, headers) = self.call("auth.test", &[]).await?;
        let s = |k: &str| {
            v.get(k)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let scopes = headers
            .get("x-oauth-scopes")
            .and_then(|h| h.to_str().ok())
            .map(|s| {
                s.split(',')
                    .map(|x| x.trim().to_string())
                    .filter(|x| !x.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        Ok(AuthInfo {
            team_id: s("team_id"),
            user_id: s("user_id"),
            team: s("team"),
            user: s("user"),
            url: s("url"),
            scopes,
        })
    }

    /// All conversations visible to the user (channels, private channels, DMs,
    /// group DMs), excluding archived ones.
    pub async fn conversations(&self) -> Result<Vec<Value>, SourceError> {
        self.paginate(
            "users.conversations",
            &[
                (
                    "types",
                    "public_channel,private_channel,mpim,im".to_string(),
                ),
                ("exclude_archived", "true".to_string()),
                ("limit", "200".to_string()),
            ],
            "channels",
            200,
        )
        .await
    }

    pub async fn conversation_info(&self, channel: &str) -> Result<Value, SourceError> {
        let (v, _) = self
            .call("conversations.info", &[("channel", channel.to_string())])
            .await?;
        Ok(v.get("channel").cloned().unwrap_or(Value::Null))
    }

    /// Messages of a conversation in `(oldest, latest)`, oldest first.
    pub async fn history(
        &self,
        channel: &str,
        oldest: &str,
        latest: &str,
    ) -> Result<Vec<Value>, SourceError> {
        let mut msgs = self
            .paginate(
                "conversations.history",
                &[
                    ("channel", channel.to_string()),
                    ("oldest", oldest.to_string()),
                    ("latest", latest.to_string()),
                    ("inclusive", "false".to_string()),
                    ("limit", "200".to_string()),
                ],
                "messages",
                1000,
            )
            .await?;
        msgs.sort_by(|a, b| ts_key(a).total_cmp(&ts_key(b)));
        Ok(msgs)
    }

    /// Replies of a thread (parent included), oldest first. With `oldest`,
    /// only newer messages are requested (the parent may still be returned).
    pub async fn replies(
        &self,
        channel: &str,
        thread_ts: &str,
        oldest: Option<&str>,
    ) -> Result<Vec<Value>, SourceError> {
        let mut params = vec![
            ("channel", channel.to_string()),
            ("ts", thread_ts.to_string()),
            ("limit", "200".to_string()),
        ];
        if let Some(o) = oldest {
            params.push(("oldest", o.to_string()));
            params.push(("inclusive", "false".to_string()));
        }
        let mut msgs = self
            .paginate("conversations.replies", &params, "messages", 1000)
            .await?;
        msgs.sort_by(|a, b| ts_key(a).total_cmp(&ts_key(b)));
        Ok(msgs)
    }

    pub async fn users(&self) -> Result<Vec<Value>, SourceError> {
        self.paginate(
            "users.list",
            &[("limit", "200".to_string())],
            "members",
            500,
        )
        .await
    }

    /// `search.messages`, all pages up to `max_pages`.
    pub async fn search(&self, query: &str, max_pages: u32) -> Result<Vec<Value>, SourceError> {
        let mut out = Vec::new();
        let mut page = 1;
        loop {
            let (v, _) = self
                .call(
                    "search.messages",
                    &[
                        ("query", query.to_string()),
                        ("count", "100".to_string()),
                        ("sort", "timestamp".to_string()),
                        ("page", page.to_string()),
                    ],
                )
                .await?;
            if let Some(m) = v.pointer("/messages/matches").and_then(Value::as_array) {
                out.extend(m.iter().cloned());
            }
            let pages = v
                .pointer("/messages/paging/pages")
                .and_then(Value::as_u64)
                .unwrap_or(1) as u32;
            if page >= pages || page >= max_pages {
                break;
            }
            page += 1;
        }
        Ok(out)
    }

    pub async fn permalink(&self, channel: &str, ts: &str) -> Result<String, SourceError> {
        let (v, _) = self
            .call(
                "chat.getPermalink",
                &[
                    ("channel", channel.to_string()),
                    ("message_ts", ts.to_string()),
                ],
            )
            .await?;
        Ok(v.get("permalink")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string())
    }
}

/// Numeric value of a message `ts` for ordering.
pub fn ts_key(m: &Value) -> f64 {
    m.get("ts")
        .and_then(Value::as_str)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0)
}

/// Compare two Slack timestamps (`"1727000000.000100"`).
pub fn ts_gt(a: &str, b: &str) -> bool {
    parse_ts_parts(a) > parse_ts_parts(b)
}

fn parse_ts_parts(s: &str) -> (u64, u64) {
    let (sec, frac) = s.split_once('.').unwrap_or((s, "0"));
    let frac = format!("{frac:0<6}");
    (sec.parse().unwrap_or(0), frac[..6].parse().unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn ts_ordering() {
        assert!(ts_gt("1727000000.000200", "1727000000.000100"));
        assert!(ts_gt("1727000001.0", "1727000000.999999"));
        assert!(!ts_gt("1727000000.000100", "1727000000.000100"));
    }

    #[tokio::test]
    async fn auth_test_reads_scopes_and_maps_errors() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/auth.test"))
            .and(header("authorization", "Bearer xoxp-good"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("x-oauth-scopes", "channels:history, users:read")
                    .set_body_json(json!({"ok": true, "team_id": "T1", "user_id": "U1", "team": "Acme", "user": "me", "url": "https://acme.slack.test/"})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/auth.test"))
            .and(header("authorization", "Bearer xoxp-bad"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"ok": false, "error": "invalid_auth"})),
            )
            .mount(&server)
            .await;
        let c = SlackClient::new(Secret::new("xoxp-good"), Some(server.uri())).unwrap();
        let a = c.auth_test().await.unwrap();
        assert_eq!(a.team_id, "T1");
        assert_eq!(a.scopes, vec!["channels:history", "users:read"]);
        let bad = SlackClient::new(Secret::new("xoxp-bad"), Some(server.uri())).unwrap();
        assert!(matches!(bad.auth_test().await, Err(SourceError::Auth(_))));
    }

    #[tokio::test]
    async fn retries_after_429_and_paginates() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/users.list"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/users.list"))
            .and(query_param("cursor", "next"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"ok": true, "members": [{"id": "U2"}]})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/users.list"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"ok": true, "members": [{"id": "U1"}], "response_metadata": {"next_cursor": "next"}}),
            ))
            .mount(&server)
            .await;
        let c = SlackClient::new(Secret::new("t"), Some(server.uri())).unwrap();
        let users = c.users().await.unwrap();
        assert_eq!(users.len(), 2);
    }
}
