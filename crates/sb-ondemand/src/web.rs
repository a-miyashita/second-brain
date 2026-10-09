//! `web.page`: one fetch of an `http(s)` page (source-documents.md).
//!
//! The fetch is hardened for the case where an agent chooses the URL: only
//! `http` and `https`, no cookies or credentials, a size and time limit, and
//! no connection to a non-public address (the guard runs in the DNS resolver,
//! so it covers every redirect hop and DNS rebinding).

use std::error::Error as StdError;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::header::{self, HeaderMap, HeaderValue};
use second_brain_extract::bundle::{BundleInput, DocBundle, build_bundle, sha256_hex};
use second_brain_kernel::document::{IngestHint, IngestSettings, normalize_document};
use second_brain_kernel::source::{Source, SourceError, SyncHost};
use second_brain_kernel::{
    AccountCtx, AccountKind, FetchOutcome, FetchRequest, FetchedEntry, NormalizeCtx,
    NormalizeInput, NormalizeOutcome, SourceKind, SourceRef, SyncOptions,
};
use serde_json::{Map, Value, json};
use url::Url;

const ACCEPT: &str =
    "text/html,application/xhtml+xml,application/pdf,text/plain,text/markdown;q=0.9,*/*;q=0.1";
const MAX_ATTEMPTS: u32 = 3;
const BLOCKED: &str = "non-public address";
const LOGIN_HINT: &str =
    "the page needs a login or refuses automated access; save it as a file and ingest that";

/// Whether an address is a public, routable one.
pub fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => is_public_v4(v),
        IpAddr::V6(v) => is_public_v6(v),
    }
}

fn is_public_v4(v: Ipv4Addr) -> bool {
    let o = v.octets();
    !(v.is_loopback()
        || v.is_private()
        || v.is_link_local()
        || v.is_unspecified()
        || v.is_broadcast()
        || v.is_multicast()
        || v.is_documentation()
        // 100.64.0.0/10 carrier-grade NAT
        || (o[0] == 100 && (64..=127).contains(&o[1]))
        // 192.0.0.0/24 IETF protocol assignments
        || (o[0] == 192 && o[1] == 0 && o[2] == 0)
        // 198.18.0.0/15 benchmarking
        || (o[0] == 198 && (o[1] == 18 || o[1] == 19))
        // 240.0.0.0/4 reserved
        || o[0] >= 240
        // 0.0.0.0/8
        || o[0] == 0)
}

fn is_public_v6(v: Ipv6Addr) -> bool {
    if let Some(v4) = v.to_ipv4_mapped() {
        return is_public_v4(v4);
    }
    let seg = v.segments();
    !(v.is_loopback()
        || v.is_unspecified()
        || v.is_multicast()
        // fc00::/7 unique local
        || (seg[0] & 0xfe00) == 0xfc00
        // fe80::/10 link-local and fec0::/10 site-local
        || (seg[0] & 0xffc0) == 0xfe80
        || (seg[0] & 0xffc0) == 0xfec0
        // 64:ff9b::/96 and 64:ff9b:1::/48 NAT64 (may reach private IPv4 hosts)
        || (seg[0] == 0x64 && seg[1] == 0xff9b && (seg[2..6].iter().all(|s| *s == 0) || seg[2] == 1))
        // ::a.b.c.d IPv4-compatible (deprecated)
        || seg[..6].iter().all(|s| *s == 0)
        // 2001::/32 Teredo
        || (seg[0] == 0x2001 && seg[1] == 0)
        // 2002::/16 6to4: the embedded IPv4 address decides
        || (seg[0] == 0x2002
            && !is_public_v4(Ipv4Addr::new(
                (seg[1] >> 8) as u8,
                seg[1] as u8,
                (seg[2] >> 8) as u8,
                seg[2] as u8,
            ))))
}

/// Whether the environment configures an HTTP proxy.
fn proxy_in_env() -> bool {
    [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ]
    .iter()
    .any(|k| std::env::var(k).is_ok_and(|v| !v.trim().is_empty()))
}

/// A DNS resolver that drops non-public addresses.
struct GuardedResolver;

impl Resolve for GuardedResolver {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(async move {
            let found: Vec<SocketAddr> =
                tokio::net::lookup_host((name.as_str(), 0)).await?.collect();
            let ok: Vec<SocketAddr> = found.into_iter().filter(|a| is_public_ip(a.ip())).collect();
            if ok.is_empty() {
                let e: Box<dyn StdError + Send + Sync> =
                    format!("{} resolves only to a {BLOCKED}", name.as_str()).into();
                return Err(e);
            }
            Ok(Box::new(ok.into_iter()) as Addrs)
        })
    }
}

/// `Err` when the URL's host is an IP literal that may not be fetched.
fn check_literal(u: &Url, allow_private: bool) -> Result<(), String> {
    if !matches!(u.scheme(), "http" | "https") {
        return Err(format!("unsupported scheme `{}:`", u.scheme()));
    }
    if !allow_private {
        let ip = match u.host() {
            Some(url::Host::Ipv4(v)) => Some(IpAddr::V4(v)),
            Some(url::Host::Ipv6(v)) => Some(IpAddr::V6(v)),
            _ => None,
        };
        if ip.is_some_and(|ip| !is_public_ip(ip)) {
            return Err(format!("the address is a {BLOCKED}"));
        }
    }
    Ok(())
}

/// The normalized form of a URL, which is the entry's identity: lowercase host
/// (punycode), no fragment, no default port, `/` for an empty path, and without
/// tracking parameters. Other parameters keep their order.
pub fn normalize_url(raw: &str) -> Option<Url> {
    let mut u = Url::parse(raw.trim()).ok()?;
    if !matches!(u.scheme(), "http" | "https") || u.host_str().is_none() {
        return None;
    }
    u.set_fragment(None);
    let _ = u.set_username("");
    let _ = u.set_password(None);
    let kept: Vec<(String, String)> = u
        .query_pairs()
        .filter(|(k, _)| {
            let k = k.to_ascii_lowercase();
            !(k.starts_with("utm_")
                || matches!(k.as_str(), "fbclid" | "gclid" | "mc_cid" | "mc_eid"))
        })
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    if kept.is_empty() {
        u.set_query(None);
    } else {
        u.query_pairs_mut().clear().extend_pairs(kept);
    }
    Some(u)
}

/// A URL for logs: no query string and no credentials.
fn loggable(u: &Url) -> String {
    format!(
        "{}://{}{}",
        u.scheme(),
        u.host_str().unwrap_or(""),
        u.path()
    )
}

fn chain(e: &dyn StdError) -> String {
    let mut s = e.to_string();
    let mut cur = e.source();
    while let Some(c) = cur {
        s.push_str(": ");
        s.push_str(&c.to_string());
        cur = c.source();
    }
    s
}

pub struct WebSource {
    account: AccountCtx,
    settings: IngestSettings,
    client: reqwest::Client,
    retry_delay: Duration,
    /// An HTTP proxy is configured in the environment.
    proxy_env: bool,
}

impl WebSource {
    pub fn new(account: AccountCtx, settings: IngestSettings) -> Result<Self, SourceError> {
        let allow_private = settings.web_allow_private;
        let max_redirects = settings.web_max_redirects;
        let redirects = reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() > max_redirects {
                return attempt.error("too many redirects");
            }
            match check_literal(attempt.url(), allow_private) {
                Ok(()) => attempt.follow(),
                Err(e) => attempt.error(e),
            }
        });
        let mut headers = HeaderMap::new();
        headers.insert(header::ACCEPT, HeaderValue::from_static(ACCEPT));
        let mut b = reqwest::Client::builder()
            .user_agent(concat!("second-brain/", env!("CARGO_PKG_VERSION")))
            .default_headers(headers)
            .redirect(redirects)
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(settings.web_timeout_secs.max(1)));
        if !allow_private {
            // Behind a proxy the proxy resolves names, so the guard would never run:
            // no proxy is used at all while the guard is on.
            b = b.dns_resolver(Arc::new(GuardedResolver)).no_proxy();
        }
        let client = b
            .build()
            .map_err(|e| SourceError::Network(format!("cannot create the HTTP client: {e}")))?;
        Ok(WebSource {
            account,
            settings,
            client,
            retry_delay: Duration::from_secs(1),
            proxy_env: proxy_in_env(),
        })
    }

    /// Pretend that a proxy is (not) configured (tests).
    pub fn with_proxy_env(mut self, on: bool) -> Self {
        self.proxy_env = on;
        self
    }

    /// Shorten the wait between retries (tests).
    pub fn with_retry_delay(mut self, d: Duration) -> Self {
        self.retry_delay = d;
        self
    }

    /// GET with retries on network errors, 429 and 5xx.
    async fn get(
        &self,
        url: &Url,
        state: Option<&Value>,
    ) -> Result<reqwest::Response, SourceError> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            let mut req = self.client.get(url.clone());
            if let Some(s) = state {
                if let Some(e) = s.get("etag").and_then(Value::as_str) {
                    req = req.header(header::IF_NONE_MATCH, e);
                }
                if let Some(m) = s.get("last_modified").and_then(Value::as_str) {
                    req = req.header(header::IF_MODIFIED_SINCE, m);
                }
            }
            match req.send().await {
                Ok(r) => {
                    let code = r.status().as_u16();
                    if (code == 429 || r.status().is_server_error()) && attempt < MAX_ATTEMPTS {
                        tracing::info!(url = %loggable(url), code, attempt, "retrying");
                        tokio::time::sleep(self.retry_delay * attempt).await;
                        continue;
                    }
                    return Ok(r);
                }
                Err(e) => {
                    let text = chain(&e);
                    if text.contains(BLOCKED) {
                        return Err(SourceError::Rejected(format!(
                            "refusing to fetch a {BLOCKED} (loopback, private or link-local); \
                             set ingest.web.allow_private to allow intranet pages"
                        )));
                    }
                    if e.is_redirect() {
                        return Err(SourceError::Rejected(format!("redirect refused: {text}")));
                    }
                    if e.is_timeout() && attempt >= MAX_ATTEMPTS {
                        return Err(SourceError::Rejected(format!(
                            "the page did not answer within {} s",
                            self.settings.web_timeout_secs
                        )));
                    }
                    if attempt >= MAX_ATTEMPTS {
                        return Err(SourceError::Network(text));
                    }
                    tracing::info!(url = %loggable(url), attempt, "network error; retrying");
                    tokio::time::sleep(self.retry_delay * attempt).await;
                }
            }
        }
    }

    /// Read the body, stopping above the size limit.
    async fn body(&self, mut resp: reqwest::Response) -> Result<Vec<u8>, SourceError> {
        let max = self.settings.max_file_bytes;
        let too_large = || {
            SourceError::Rejected(format!(
                "the page is larger than ingest.max_file_bytes ({max} bytes)"
            ))
        };
        if resp.content_length().is_some_and(|l| l > max) {
            return Err(too_large());
        }
        let mut buf = Vec::new();
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| SourceError::Network(chain(&e)))?
        {
            buf.extend_from_slice(&chunk);
            if buf.len() as u64 > max {
                return Err(too_large());
            }
        }
        Ok(buf)
    }
}

fn http_time(h: Option<&HeaderValue>) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc2822(h?.to_str().ok()?)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

fn last_segment(u: &Url) -> String {
    u.path_segments()
        .and_then(|mut s| s.rfind(|p| !p.is_empty()))
        .map(|s| {
            url::form_urlencoded::parse(s.as_bytes())
                .next()
                .map(|(k, _)| k.into_owned())
                .unwrap_or_else(|| s.to_string())
        })
        .unwrap_or_default()
}

#[async_trait]
impl Source for WebSource {
    fn kinds(&self) -> &'static [SourceKind] {
        &[SourceKind::WebPage]
    }

    fn account_kind(&self) -> AccountKind {
        AccountKind::Web
    }

    fn supports_sync(&self) -> bool {
        false
    }

    fn resolve(&self, locator: &str) -> Option<SourceRef> {
        let u = normalize_url(locator)?;
        Some(SourceRef {
            account_id: self.account.id.clone(),
            source_kind: SourceKind::WebPage,
            source_id: u.to_string(),
            source_url: Some(u.to_string()),
            created_at: None,
            updated_at: None,
        })
    }

    async fn sync(&self, _host: &dyn SyncHost, _opts: &SyncOptions) -> Result<(), SourceError> {
        Err(SourceError::Unsupported("web pages have no sync".into()))
    }

    async fn fetch(
        &self,
        _host: &dyn SyncHost,
        req: &FetchRequest,
    ) -> Result<FetchOutcome, SourceError> {
        let url = Url::parse(&req.source_id)
            .map_err(|e| SourceError::Parse(format!("not a URL: {e}")))?;
        check_literal(&url, self.settings.web_allow_private).map_err(|e| {
            SourceError::Rejected(format!(
                "refusing to fetch: {e}; set ingest.web.allow_private to allow intranet pages"
            ))
        })?;
        if self.proxy_env && !self.settings.web_allow_private {
            return Err(SourceError::Rejected(
                "an HTTP proxy is configured, and the guard against non-public addresses cannot \
                 check names that a proxy resolves; set ingest.web.allow_private to fetch through \
                 the proxy, or save the page as a file and ingest that"
                    .into(),
            ));
        }
        let state = if req.full {
            None
        } else {
            req.fetch_state.as_ref()
        };
        let resp = self.get(&url, state).await?;
        let status = resp.status();
        let final_url = resp.url().clone();
        tracing::debug!(url = %loggable(&url), status = status.as_u16(), "fetched");
        if status.as_u16() == 304 && state.is_some() {
            return Ok(FetchOutcome::Unchanged);
        }
        let code = status.as_u16();
        if code == 401 || code == 403 {
            return Err(SourceError::Rejected(format!("HTTP {code}: {LOGIN_HINT}")));
        }
        if code == 404 || code == 410 {
            return Ok(FetchOutcome::NotFound(format!("HTTP {code}")));
        }
        if !status.is_success() {
            return Err(SourceError::Rejected(format!("HTTP {code}")));
        }
        if final_url
            .host_str()
            .is_some_and(|h| h.starts_with("accounts.google."))
        {
            return Err(SourceError::Rejected(format!(
                "redirected to a Google sign-in page: {LOGIN_HINT}"
            )));
        }
        let content_type = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let etag = resp
            .headers()
            .get(header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let last_modified_raw = resp
            .headers()
            .get(header::LAST_MODIFIED)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let modified = http_time(resp.headers().get(header::LAST_MODIFIED));
        let bytes = self.body(resp).await?;
        let sha = sha256_hex(&bytes);
        if !req.full
            && req
                .fetch_state
                .as_ref()
                .and_then(|s| s.get("sha256"))
                .and_then(Value::as_str)
                == Some(&sha)
        {
            return Ok(FetchOutcome::Unchanged);
        }
        let hint = IngestHint::from_value(&req.hint);
        let name = last_segment(&final_url);
        let mut extra = Map::new();
        // The locator as the user gave it; a refetch keeps the stored one.
        if !hint.locator.is_empty() {
            extra.insert("fetched_url".into(), json!(hint.locator));
        }
        extra.insert("final_url".into(), json!(final_url.as_str()));
        if let Some(c) = &content_type {
            extra.insert("content_type".into(), json!(c));
        }
        if let Some(m) = &last_modified_raw {
            extra.insert("http_last_modified".into(), json!(m));
        }
        if let Some(e) = &etag {
            extra.insert("http_etag".into(), json!(e));
        }
        let fallback = if name.is_empty() {
            url.host_str().unwrap_or("").to_string()
        } else {
            name.clone()
        };
        let bundle = build_bundle(BundleInput {
            bytes: &bytes,
            original: None,
            media_type: content_type.as_deref(),
            file_name: if name.is_empty() { None } else { Some(&name) },
            hint: &hint,
            settings: &self.settings,
            fallback_title: Some(fallback),
            source_created: None,
            source_modified: modified,
            extra_meta: extra,
            fetch_state: Some(json!({
                "etag": etag,
                "last_modified": last_modified_raw,
                "sha256": sha,
            })),
        })?;
        match bundle {
            DocBundle::NotApplicable(why) => Ok(FetchOutcome::NotApplicable(why)),
            DocBundle::Bundle(bundle) => Ok(FetchOutcome::Fetched(Box::new(FetchedEntry {
                source_ref: SourceRef {
                    account_id: self.account.id.clone(),
                    source_kind: SourceKind::WebPage,
                    source_id: req.source_id.clone(),
                    source_url: Some(
                        normalize_url(final_url.as_str())
                            .unwrap_or(final_url)
                            .to_string(),
                    ),
                    created_at: None,
                    updated_at: modified,
                },
                bundle,
            }))),
        }
    }

    fn normalize(
        &self,
        _ctx: &NormalizeCtx,
        input: &NormalizeInput,
    ) -> Result<NormalizeOutcome, SourceError> {
        normalize_document(&input.source_ref, &input.fetch_metadata, &input.segments)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> bool {
        is_public_ip(s.parse().unwrap())
    }

    #[test]
    fn address_classes() {
        for private in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "224.0.0.1",
            "198.18.0.1",
            "255.255.255.255",
            "::1",
            "::",
            "fe80::1",
            "fc00::1",
            "fd12:3456::1",
            "ff02::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "64:ff9b::a00:1",
            "64:ff9b:1::1",
            "::7f00:1",
            "2001:0:4136:e378:8000:63bf:3fff:fdd2",
            "2002:7f00:1::1",
            "2002:0a00:1::1",
        ] {
            assert!(!ip(private), "{private} must be refused");
        }
        for public in [
            "8.8.8.8",
            "1.1.1.1",
            "93.184.216.34",
            "2606:4700:4700::1111",
            "::ffff:8.8.8.8",
            "2002:0808:0808::1",
        ] {
            assert!(ip(public), "{public} must be allowed");
        }
    }

    #[test]
    fn literal_checks() {
        let u = |s: &str| Url::parse(s).unwrap();
        assert!(check_literal(&u("http://169.254.169.254/latest/meta-data"), false).is_err());
        assert!(check_literal(&u("http://[::1]:8080/"), false).is_err());
        assert!(check_literal(&u("http://127.0.0.1/"), true).is_ok());
        assert!(check_literal(&u("https://example.com/"), false).is_ok());
        assert!(check_literal(&u("ftp://example.com/"), false).is_err());
    }

    #[test]
    fn url_normalization() {
        let n = |s: &str| normalize_url(s).map(|u| u.to_string());
        assert_eq!(
            n("HTTPS://Example.COM:443/a/b?utm_source=x&id=7&fbclid=y#frag").as_deref(),
            Some("https://example.com/a/b?id=7")
        );
        assert_eq!(
            n("http://example.com").as_deref(),
            Some("http://example.com/")
        );
        assert_eq!(
            n("http://example.com:80/x?b=2&a=1").as_deref(),
            Some("http://example.com/x?b=2&a=1")
        );
        assert_eq!(
            n("https://user:pw@example.com/p").as_deref(),
            Some("https://example.com/p")
        );
        assert_eq!(
            n("https://例え.jp/").as_deref(),
            Some("https://xn--r8jz45g.jp/")
        );
        assert_eq!(n("ftp://example.com/"), None);
        assert_eq!(n("not a url"), None);
    }

    #[test]
    fn logging_hides_the_query() {
        let u = Url::parse("https://example.com/a?token=secret").unwrap();
        assert_eq!(loggable(&u), "https://example.com/a");
    }
}
