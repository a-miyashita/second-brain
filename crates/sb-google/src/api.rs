//! Thin Drive v3 and Calendar v3 REST clients (ADR-0006).

use std::sync::Arc;
use std::time::Duration;

use sb_core::source::SourceError;
use serde_json::Value;

use crate::oauth::TokenProvider;

pub const DRIVE_API: &str = "https://www.googleapis.com/drive/v3";
pub const CALENDAR_API: &str = "https://www.googleapis.com/calendar/v3";
pub const GOOGLE_DOC_MIME: &str = "application/vnd.google-apps.document";
pub const FOLDER_MIME: &str = "application/vnd.google-apps.folder";

const MAX_ATTEMPTS: u32 = 5;

/// Google REST client for one account.
#[derive(Clone)]
pub struct GoogleApi {
    http: reqwest::Client,
    tokens: Arc<TokenProvider>,
    pub drive_base: String,
    pub calendar_base: String,
}

impl GoogleApi {
    pub fn new(tokens: Arc<TokenProvider>) -> Result<Self, SourceError> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("second-brain/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|e| SourceError::Network(e.to_string()))?;
        Ok(GoogleApi {
            http,
            tokens,
            drive_base: DRIVE_API.into(),
            calendar_base: CALENDAR_API.into(),
        })
    }

    /// Point both APIs at a mock server (tests).
    pub fn with_base(mut self, base: &str) -> Self {
        self.drive_base = format!("{base}/drive/v3");
        self.calendar_base = format!("{base}/calendar/v3");
        self
    }

    /// GET with retries on 429/5xx and rate-limit 403s. `Ok(None)` on 404.
    async fn get(
        &self,
        url: &str,
        query: &[(&str, String)],
    ) -> Result<Option<reqwest::Response>, SourceError> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            let token = self.tokens.access_token().await?;
            let resp = self
                .http
                .get(url)
                .bearer_auth(token.expose())
                .query(query)
                .send()
                .await
                .map_err(|e| SourceError::Network(e.to_string()))?;
            let status = resp.status();
            if status.is_success() {
                return Ok(Some(resp));
            }
            if status.as_u16() == 404 {
                return Ok(None);
            }
            let body = resp.text().await.unwrap_or_default();
            let rate_limited = status.as_u16() == 429
                || (status.as_u16() == 403
                    && (body.contains("rateLimitExceeded")
                        || body.contains("userRateLimitExceeded")));
            if (rate_limited || status.is_server_error()) && attempt < MAX_ATTEMPTS {
                tokio::time::sleep(Duration::from_secs(2u64.pow(attempt).min(60))).await;
                continue;
            }
            let msg: String = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|v| {
                    v.pointer("/error/message")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .unwrap_or(body)
                .chars()
                .take(300)
                .collect();
            return Err(match status.as_u16() {
                401 => SourceError::Auth(format!("Google API: {msg}")),
                429 => SourceError::RateLimited(msg),
                403 if rate_limited => SourceError::RateLimited(msg),
                _ => SourceError::Api(format!("Google API {status}: {msg}")),
            });
        }
    }

    async fn get_json(
        &self,
        url: &str,
        query: &[(&str, String)],
    ) -> Result<Option<Value>, SourceError> {
        match self.get(url, query).await? {
            Some(r) => Ok(Some(
                r.json()
                    .await
                    .map_err(|e| SourceError::Parse(e.to_string()))?,
            )),
            None => Ok(None),
        }
    }

    /// Drive file metadata; `None` if not found or not accessible.
    pub async fn file(&self, id: &str) -> Result<Option<Value>, SourceError> {
        self.get_json(
            &format!("{}/files/{id}", self.drive_base),
            &[
                (
                    "fields",
                    "id,name,mimeType,createdTime,modifiedTime,parents,webViewLink".into(),
                ),
                ("supportsAllDrives", "true".into()),
            ],
        )
        .await
    }

    /// Export a Google Doc; `None` if not found.
    pub async fn export(&self, id: &str, mime: &str) -> Result<Option<Vec<u8>>, SourceError> {
        match self
            .get(
                &format!("{}/files/{id}/export", self.drive_base),
                &[("mimeType", mime.to_string())],
            )
            .await?
        {
            Some(r) => Ok(Some(
                r.bytes()
                    .await
                    .map_err(|e| SourceError::Network(e.to_string()))?
                    .to_vec(),
            )),
            None => Ok(None),
        }
    }

    /// One page of `files.list`.
    pub async fn list(
        &self,
        q: &str,
        order_by: &str,
        page_token: Option<&str>,
    ) -> Result<(Vec<Value>, Option<String>), SourceError> {
        let mut query = vec![
            ("q", q.to_string()),
            ("orderBy", order_by.to_string()),
            ("pageSize", "100".to_string()),
            (
                "fields",
                "nextPageToken,files(id,name,mimeType,createdTime,modifiedTime,parents)"
                    .to_string(),
            ),
            ("supportsAllDrives", "true".to_string()),
            ("includeItemsFromAllDrives", "true".to_string()),
        ];
        if let Some(t) = page_token {
            query.push(("pageToken", t.to_string()));
        }
        let v = self
            .get_json(&format!("{}/files", self.drive_base), &query)
            .await?
            .unwrap_or(Value::Null);
        let files = v
            .get("files")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let next = v
            .get("nextPageToken")
            .and_then(Value::as_str)
            .map(str::to_string);
        Ok((files, next))
    }

    /// One page of primary-calendar events in `[time_min, time_max)`.
    pub async fn events(
        &self,
        time_min: &str,
        time_max: &str,
        page_token: Option<&str>,
    ) -> Result<(Vec<Value>, Option<String>), SourceError> {
        let mut query = vec![
            ("timeMin", time_min.to_string()),
            ("timeMax", time_max.to_string()),
            ("singleEvents", "true".to_string()),
            ("orderBy", "startTime".to_string()),
            ("maxResults", "250".to_string()),
        ];
        if let Some(t) = page_token {
            query.push(("pageToken", t.to_string()));
        }
        let v = self
            .get_json(
                &format!("{}/calendars/primary/events", self.calendar_base),
                &query,
            )
            .await?
            .unwrap_or(Value::Null);
        let items = v
            .get("items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let next = v
            .get("nextPageToken")
            .and_then(Value::as_str)
            .map(str::to_string);
        Ok((items, next))
    }
}

/// The Drive file ID in a Google Docs URL (`/document/d/<id>/...`).
pub fn doc_id_from_url(url: &str) -> Option<String> {
    let rest = url.split("/d/").nth(1)?;
    let id: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    (!id.is_empty()).then_some(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doc_ids() {
        assert_eq!(
            doc_id_from_url("https://docs.google.com/document/d/1AbC-_9/edit?usp=x").as_deref(),
            Some("1AbC-_9")
        );
        assert_eq!(doc_id_from_url("https://example.test/"), None);
    }
}
