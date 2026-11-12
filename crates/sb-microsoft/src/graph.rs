//! A thin Microsoft Graph client for drive items (ADR-0021).
//!
//! The access token goes only to the Graph base URL. A download is a redirect to
//! a short-lived pre-authenticated URL; it is requested without credentials and,
//! unless tests ask otherwise, over `https` only.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use second_brain_kernel::source::SourceError;
use serde_json::Value;

use crate::oauth::TokenProvider;

pub const GRAPH_API: &str = "https://graph.microsoft.com/v1.0";

const MAX_ATTEMPTS: u32 = 5;
const MAX_RETRY_WAIT: Duration = Duration::from_secs(120);
const ITEM_FIELDS: &str = "id,name,size,eTag,cTag,createdDateTime,lastModifiedDateTime,webUrl,\
file,folder,package,deleted,remoteItem,parentReference,createdBy,lastModifiedBy";

/// The result of a lookup that can legitimately find nothing.
#[derive(Debug, Clone, PartialEq)]
pub enum ItemLookup {
    Found(Value),
    /// Graph answered 400, 403 or 404.
    Missing {
        status: u16,
        message: String,
    },
}

/// The sharing ID of a URL: `u!` and the unpadded base64url of the URL.
pub fn share_id(url: &str) -> String {
    format!("u!{}", URL_SAFE_NO_PAD.encode(url.as_bytes()))
}

/// Why a Graph call failed. `Missing` is an answer (400, 403 or 404) that a caller may
/// treat as "no such item"; everything else is a `SourceError`.
#[derive(Debug)]
enum GraphError {
    Missing { status: u16, message: String },
    Source(SourceError),
}

impl From<SourceError> for GraphError {
    fn from(e: SourceError) -> Self {
        GraphError::Source(e)
    }
}

impl From<crate::oauth::OAuthError> for GraphError {
    fn from(e: crate::oauth::OAuthError) -> Self {
        GraphError::Source(e.into())
    }
}

impl From<GraphError> for SourceError {
    fn from(e: GraphError) -> Self {
        match e {
            GraphError::Source(e) => e,
            GraphError::Missing { status, message } => {
                SourceError::Api(format!("Microsoft Graph {status}: {message}"))
            }
        }
    }
}

/// Graph client for one account.
#[derive(Clone)]
pub struct GraphApi {
    http: reqwest::Client,
    download_http: reqwest::Client,
    tokens: Arc<TokenProvider>,
    base: String,
    https_only: bool,
}

fn download_client(https_only: bool) -> Result<reqwest::Client, SourceError> {
    // This client never sends credentials; the policy only keeps a redirect
    // chain short and on https.
    reqwest::Client::builder()
        .user_agent(concat!("second-brain/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(300))
        .redirect(reqwest::redirect::Policy::custom(move |a| {
            if a.previous().len() < 3 && (!https_only || a.url().scheme() == "https") {
                a.follow()
            } else {
                a.stop()
            }
        }))
        .build()
        .map_err(|e| SourceError::Network(e.without_url().to_string()))
}

impl GraphApi {
    pub fn new(tokens: Arc<TokenProvider>) -> Result<Self, SourceError> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("second-brain/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(120))
            // The download redirect is handled by hand (see `download`).
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| SourceError::Network(e.without_url().to_string()))?;
        Ok(GraphApi {
            http,
            download_http: download_client(true)?,
            tokens,
            base: GRAPH_API.into(),
            https_only: true,
        })
    }

    /// Point the client at a mock server (tests).
    pub fn with_base(mut self, base: &str) -> Self {
        self.base = base.trim_end_matches('/').to_string();
        self
    }

    /// Allow `http` download URLs (tests with a local mock server).
    pub fn allow_http_downloads(mut self) -> Result<Self, SourceError> {
        self.https_only = false;
        self.download_http = download_client(false)?;
        Ok(self)
    }

    fn url(&self, segments: &[&str]) -> Result<url::Url, SourceError> {
        let mut u = url::Url::parse(&self.base).map_err(|e| SourceError::Api(e.to_string()))?;
        u.path_segments_mut()
            .map_err(|_| SourceError::Api("invalid Graph base URL".into()))?
            .pop_if_empty()
            .extend(segments);
        Ok(u)
    }

    /// An authorized GET without following redirects. Retries on 429 and 5xx
    /// (honoring `Retry-After`), and once on 401 with a fresh token.
    async fn get(&self, url: &url::Url) -> Result<reqwest::Response, GraphError> {
        let mut attempt = 0;
        let mut refreshed = false;
        loop {
            attempt += 1;
            let token = self.tokens.access_token().await?;
            let resp = match self
                .http
                .get(url.clone())
                .bearer_auth(token.expose())
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    if attempt >= MAX_ATTEMPTS {
                        return Err(SourceError::Network(e.without_url().to_string()).into());
                    }
                    tracing::info!(attempt, "Graph network error; retrying");
                    tokio::time::sleep(Duration::from_secs(2u64.pow(attempt).min(60))).await;
                    continue;
                }
            };
            let status = resp.status();
            if status.is_success() || status.is_redirection() {
                return Ok(resp);
            }
            if status.as_u16() == 401 && !refreshed {
                refreshed = true;
                self.tokens.invalidate().await;
                continue;
            }
            if (status.as_u16() == 429 || status.is_server_error()) && attempt < MAX_ATTEMPTS {
                let wait = resp
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.trim().parse::<u64>().ok())
                    .map(Duration::from_secs)
                    .unwrap_or_else(|| Duration::from_secs(2u64.pow(attempt).min(60)))
                    .min(MAX_RETRY_WAIT);
                tracing::info!(attempt, status = status.as_u16(), "Graph asked to back off");
                tokio::time::sleep(wait).await;
                continue;
            }
            if status.as_u16() == 401 {
                // A 401 right after a successful refresh is not about the stored
                // credential, so it must not put the account into `needs_reauth`.
                return Err(SourceError::Api(
                    "Microsoft Graph rejected a freshly issued token (HTTP 401)".into(),
                )
                .into());
            }
            return Err(error_from(resp).await);
        }
    }

    async fn lookup(&self, url: &url::Url) -> Result<ItemLookup, SourceError> {
        match self.get(url).await {
            Ok(r) => Ok(ItemLookup::Found(
                r.json()
                    .await
                    .map_err(|e| SourceError::Parse(e.to_string()))?,
            )),
            Err(GraphError::Missing { status, message }) => {
                Ok(ItemLookup::Missing { status, message })
            }
            Err(GraphError::Source(e)) => Err(e),
        }
    }

    /// The drive item behind a SharePoint or OneDrive URL (a sharing link or a
    /// direct document URL).
    pub async fn shares_item(&self, url: &str) -> Result<ItemLookup, SourceError> {
        let mut u = self.url(&["shares", &share_id(url), "driveItem"])?;
        u.query_pairs_mut().append_pair("$select", ITEM_FIELDS);
        self.lookup(&u).await
    }

    /// A drive item by drive and item ID.
    pub async fn item(&self, drive_id: &str, item_id: &str) -> Result<ItemLookup, SourceError> {
        let mut u = self.url(&["drives", drive_id, "items", item_id])?;
        u.query_pairs_mut().append_pair("$select", ITEM_FIELDS);
        self.lookup(&u).await
    }

    /// One page of the children of a folder. `next` is the `@odata.nextLink` of
    /// the previous page; it is followed only on the Graph host, so the token
    /// cannot be sent anywhere else.
    pub async fn children_page(
        &self,
        drive_id: &str,
        item_id: &str,
        next: Option<&str>,
    ) -> Result<(Vec<Value>, Option<String>), SourceError> {
        let u = match next {
            Some(link) => {
                let u = url::Url::parse(link)
                    .map_err(|_| SourceError::Api("the next page link is not a URL".into()))?;
                let base =
                    url::Url::parse(&self.base).map_err(|e| SourceError::Api(e.to_string()))?;
                if u.scheme() != base.scheme()
                    || u.host_str() != base.host_str()
                    || u.port_or_known_default() != base.port_or_known_default()
                {
                    return Err(SourceError::Api(
                        "the next page link points to another host; not following it".into(),
                    ));
                }
                u
            }
            None => {
                let mut u = self.url(&["drives", drive_id, "items", item_id, "children"])?;
                u.query_pairs_mut()
                    .append_pair("$top", "200")
                    .append_pair("$select", ITEM_FIELDS);
                u
            }
        };
        let v: Value = match self.get(&u).await {
            Ok(r) => r
                .json()
                .await
                .map_err(|e| SourceError::Parse(e.to_string()))?,
            Err(GraphError::Missing { status, message }) => {
                return Err(SourceError::Api(format!(
                    "Microsoft Graph {status} listing a folder: {message}"
                )));
            }
            Err(GraphError::Source(e)) => return Err(e),
        };
        let items = v
            .get("value")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let next = v
            .get("@odata.nextLink")
            .and_then(Value::as_str)
            .map(str::to_string);
        Ok((items, next))
    }

    /// The content of a file. `None` if it is not found. Graph answers with a
    /// redirect to a pre-authenticated URL, which is fetched without the token.
    pub async fn download(
        &self,
        drive_id: &str,
        item_id: &str,
        max_bytes: u64,
    ) -> Result<Option<Vec<u8>>, SourceError> {
        let too_large = || {
            SourceError::Rejected(format!(
                "the file is larger than ingest.max_file_bytes ({max_bytes} bytes)"
            ))
        };
        let u = self.url(&["drives", drive_id, "items", item_id, "content"])?;
        let first = match self.get(&u).await {
            Ok(r) => r,
            Err(GraphError::Missing { status: 404, .. }) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let mut resp = if first.status().is_redirection() {
            let location = first
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| SourceError::Api("the download redirect has no Location".into()))?;
            let target = url::Url::parse(location)
                .map_err(|_| SourceError::Api("the download redirect is not a URL".into()))?;
            if self.https_only && target.scheme() != "https" {
                return Err(SourceError::Rejected(
                    "the download URL is not https; refusing to fetch it".into(),
                ));
            }
            let r = self
                .download_http
                .get(target)
                .send()
                .await
                .map_err(|e| SourceError::Network(e.without_url().to_string()))?;
            if r.status().as_u16() == 404 {
                return Ok(None);
            }
            if !r.status().is_success() {
                return Err(SourceError::Api(format!(
                    "download failed with HTTP {}",
                    r.status()
                )));
            }
            r
        } else {
            first
        };
        if resp.content_length().is_some_and(|l| l > max_bytes) {
            return Err(too_large());
        }
        // The size can be missing or change: read in chunks.
        let mut buf = Vec::new();
        while let Some(c) = resp
            .chunk()
            .await
            .map_err(|e| SourceError::Network(e.without_url().to_string()))?
        {
            buf.extend_from_slice(&c);
            if buf.len() as u64 > max_bytes {
                return Err(too_large());
            }
        }
        Ok(Some(buf))
    }
}

/// An error response of a Graph call.
async fn error_from(resp: reqwest::Response) -> GraphError {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    let v: Option<Value> = serde_json::from_str(&body).ok();
    let msg: String = v
        .as_ref()
        .and_then(|v| {
            let code = v
                .pointer("/error/code")
                .and_then(Value::as_str)
                .unwrap_or("");
            let m = v.pointer("/error/message").and_then(Value::as_str)?;
            Some(if code.is_empty() {
                m.to_string()
            } else {
                format!("{code}: {m}")
            })
        })
        .unwrap_or(body)
        .chars()
        .take(300)
        .collect();
    match status.as_u16() {
        401 => SourceError::Auth(format!("Microsoft Graph: {msg}")).into(),
        429 => SourceError::RateLimited(msg).into(),
        s @ (400 | 403 | 404) => GraphError::Missing {
            status: s,
            message: msg,
        },
        _ => SourceError::Api(format!("Microsoft Graph {status}: {msg}")).into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn share_ids() {
        // The example of the Graph documentation.
        assert_eq!(
            share_id(
                "https://onedrive.live.com/redir?resid=1231244193912!12&authKey=1201919!12&ithint=file%2cxlsx"
            ),
            "u!aHR0cHM6Ly9vbmVkcml2ZS5saXZlLmNvbS9yZWRpcj9yZXNpZD0xMjMxMjQ0MTkzOTEyITEyJmF1dGhLZXk9MTIwMTkxOSExMiZpdGhpbnQ9ZmlsZSUyY3hsc3g"
        );
    }
}
