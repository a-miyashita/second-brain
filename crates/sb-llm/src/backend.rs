//! Completion backends: HTTP APIs and CLI subprocesses.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use reqwest::StatusCode;
use second_brain_kernel::summarizer::LlmError;
use second_brain_kernel::{Secret, Usage};
use serde_json::{Value, json};

/// A message role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

/// A completion request.
#[derive(Debug, Clone)]
pub struct CompletionRequest {
    pub system: String,
    pub messages: Vec<(Role, String)>,
    pub max_tokens: u32,
    pub timeout: Duration,
}

/// A completion.
#[derive(Debug, Clone, PartialEq)]
pub struct Completion {
    pub text: String,
    pub usage: Usage,
}

/// Something that turns a prompt into text.
#[async_trait]
pub trait Backend: Send + Sync {
    async fn complete(&self, req: &CompletionRequest) -> Result<Completion, LlmError>;
}

const MAX_HTTP_ATTEMPTS: u32 = 3;

fn http_client() -> Result<reqwest::Client, LlmError> {
    reqwest::Client::builder()
        .user_agent(concat!("second-brain/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| LlmError::Config(e.to_string()))
}

fn map_send_error(e: reqwest::Error) -> LlmError {
    if e.is_timeout() {
        LlmError::Timeout(e.to_string())
    } else if e.is_connect() {
        LlmError::Unreachable(e.to_string())
    } else {
        LlmError::Provider(e.to_string())
    }
}

/// Send a JSON POST with retries on 429, 5xx and transport errors.
async fn post_json(
    client: &reqwest::Client,
    url: &str,
    headers: &[(&str, String)],
    body: &Value,
    timeout: Duration,
) -> Result<Value, LlmError> {
    let mut attempt = 0;
    loop {
        attempt += 1;
        let mut rb = client.post(url).timeout(timeout).json(body);
        for (k, v) in headers {
            rb = rb.header(*k, v);
        }
        let resp = match rb.send().await {
            Ok(r) => r,
            Err(e) => {
                let err = map_send_error(e);
                if attempt < MAX_HTTP_ATTEMPTS
                    && !matches!(err, LlmError::Timeout(_) | LlmError::Unreachable(_))
                {
                    tokio::time::sleep(Duration::from_secs(2u64.pow(attempt))).await;
                    continue;
                }
                return Err(err);
            }
        };
        let status = resp.status();
        let retry_after = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok());
        let text = resp.text().await.map_err(map_send_error)?;
        if status.is_success() {
            return serde_json::from_str(&text)
                .map_err(|e| LlmError::Provider(format!("invalid response JSON: {e}")));
        }
        let msg = error_message(&text).unwrap_or_else(|| status.to_string());
        match status {
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => return Err(LlmError::Auth(msg)),
            StatusCode::TOO_MANY_REQUESTS => {
                if attempt < MAX_HTTP_ATTEMPTS {
                    tokio::time::sleep(Duration::from_secs(
                        retry_after.unwrap_or(5 * attempt as u64).min(120),
                    ))
                    .await;
                    continue;
                }
                return Err(LlmError::RateLimited(msg));
            }
            s if s.is_server_error() || s.as_u16() == 529 => {
                if attempt < MAX_HTTP_ATTEMPTS {
                    tokio::time::sleep(Duration::from_secs(
                        retry_after.unwrap_or(2u64.pow(attempt)).min(120),
                    ))
                    .await;
                    continue;
                }
                return Err(LlmError::Provider(format!("{status}: {msg}")));
            }
            _ => return Err(LlmError::Provider(format!("{status}: {msg}"))),
        }
    }
}

fn error_message(body: &str) -> Option<String> {
    let v: Value = serde_json::from_str(body).ok()?;
    v.pointer("/error/message")
        .or_else(|| v.get("message"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Anthropic Messages API.
pub struct AnthropicBackend {
    client: reqwest::Client,
    base_url: String,
    api_key: Secret,
    model: String,
}

impl AnthropicBackend {
    pub fn new(base_url: Option<String>, api_key: Secret, model: String) -> Result<Self, LlmError> {
        Ok(AnthropicBackend {
            client: http_client()?,
            base_url: base_url.unwrap_or_else(|| "https://api.anthropic.com".into()),
            api_key,
            model,
        })
    }
}

#[async_trait]
impl Backend for AnthropicBackend {
    async fn complete(&self, req: &CompletionRequest) -> Result<Completion, LlmError> {
        let started = Instant::now();
        let messages: Vec<Value> = req
            .messages
            .iter()
            .map(|(r, c)| {
                json!({"role": if *r == Role::User { "user" } else { "assistant" }, "content": c})
            })
            .collect();
        let body = json!({
            "model": self.model,
            "max_tokens": req.max_tokens,
            "system": req.system,
            "messages": messages,
        });
        let url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));
        let v = post_json(
            &self.client,
            &url,
            &[
                ("x-api-key", self.api_key.expose().to_string()),
                ("anthropic-version", "2023-06-01".to_string()),
            ],
            &body,
            req.timeout,
        )
        .await?;
        if v.get("stop_reason").and_then(Value::as_str) == Some("refusal") {
            return Err(LlmError::Provider(
                "the model declined the request (refusal)".into(),
            ));
        }
        let text: String = v
            .get("content")
            .and_then(Value::as_array)
            .map(|blocks| {
                blocks
                    .iter()
                    .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                    .filter_map(|b| b.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default();
        Ok(Completion {
            text,
            usage: Usage {
                input_tokens: v
                    .pointer("/usage/input_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                output_tokens: v
                    .pointer("/usage/output_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                duration_ms: started.elapsed().as_millis() as u64,
                cost_usd: None,
                calls: 1,
                model: None,
            },
        })
    }
}

/// OpenAI Chat Completions, also used for OpenAI-compatible local servers.
pub struct OpenAiBackend {
    client: reqwest::Client,
    base_url: String,
    api_key: Option<Secret>,
    model: String,
    /// `max_completion_tokens` (OpenAI) vs `max_tokens` (compatible servers).
    official: bool,
    keep_alive: Option<String>,
}

impl OpenAiBackend {
    pub fn new(
        base_url: Option<String>,
        api_key: Option<Secret>,
        model: String,
        official: bool,
        keep_alive: Option<String>,
    ) -> Result<Self, LlmError> {
        Ok(OpenAiBackend {
            client: http_client()?,
            base_url: base_url.unwrap_or_else(|| "https://api.openai.com/v1".into()),
            api_key,
            model,
            official,
            keep_alive,
        })
    }

    /// `GET <base_url>/models` with a short timeout (reachability check).
    pub async fn probe(&self, timeout: Duration) -> Result<(), LlmError> {
        let url = format!("{}/models", self.base_url.trim_end_matches('/'));
        let mut rb = self.client.get(&url).timeout(timeout);
        if let Some(k) = &self.api_key {
            rb = rb.bearer_auth(k.expose());
        }
        // A probe that gets no answer in time means the server is not reachable.
        // (Windows does not refuse a closed port at once; it retries the SYN until
        // the timeout, so a refused connection surfaces as a timeout there.)
        let resp = rb.send().await.map_err(|e| match map_send_error(e) {
            LlmError::Timeout(m) => LlmError::Unreachable(m),
            other => other,
        })?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(LlmError::Unreachable(format!(
                "GET {url}: {}",
                resp.status()
            )))
        }
    }
}

#[async_trait]
impl Backend for OpenAiBackend {
    async fn complete(&self, req: &CompletionRequest) -> Result<Completion, LlmError> {
        let started = Instant::now();
        let mut messages = vec![json!({"role": "system", "content": req.system})];
        messages.extend(req.messages.iter().map(|(r, c)| {
            json!({"role": if *r == Role::User { "user" } else { "assistant" }, "content": c})
        }));
        let mut body = json!({"model": self.model, "messages": messages});
        let tokens_key = if self.official {
            "max_completion_tokens"
        } else {
            "max_tokens"
        };
        body[tokens_key] = json!(req.max_tokens);
        if let Some(k) = &self.keep_alive {
            body["keep_alive"] = json!(k);
        }
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let headers: Vec<(&str, String)> = self
            .api_key
            .iter()
            .map(|k| ("authorization", format!("Bearer {}", k.expose())))
            .collect();
        let v = post_json(&self.client, &url, &headers, &body, req.timeout).await?;
        let text = v
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        Ok(Completion {
            text,
            usage: Usage {
                input_tokens: v
                    .pointer("/usage/prompt_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                output_tokens: v
                    .pointer("/usage/completion_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                duration_ms: started.elapsed().as_millis() as u64,
                cost_usd: None,
                calls: 1,
                model: None,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn req() -> CompletionRequest {
        CompletionRequest {
            system: "sys".into(),
            messages: vec![(Role::User, "hello".into())],
            max_tokens: 100,
            timeout: Duration::from_secs(5),
        }
    }

    #[tokio::test]
    async fn anthropic_request_shape() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(header("x-api-key", "k-123"))
            .and(header("anthropic-version", "2023-06-01"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "content": [{"type": "text", "text": "{\"overview\":\"o\"}"}],
                "stop_reason": "end_turn",
                "usage": {"input_tokens": 12, "output_tokens": 3}
            })))
            .expect(1)
            .mount(&server)
            .await;
        let b =
            AnthropicBackend::new(Some(server.uri()), Secret::new("k-123"), "m".into()).unwrap();
        let c = b.complete(&req()).await.unwrap();
        assert_eq!(c.text, "{\"overview\":\"o\"}");
        assert_eq!(c.usage.input_tokens, 12);
        let body: Value =
            serde_json::from_slice(&server.received_requests().await.unwrap()[0].body).unwrap();
        assert_eq!(body["system"], "sys");
        assert_eq!(body["messages"][0]["role"], "user");
    }

    #[tokio::test]
    async fn anthropic_auth_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(401)
                    .set_body_json(json!({"error": {"message": "invalid x-api-key"}})),
            )
            .mount(&server)
            .await;
        let b = AnthropicBackend::new(Some(server.uri()), Secret::new("bad"), "m".into()).unwrap();
        assert!(
            matches!(b.complete(&req()).await, Err(LlmError::Auth(m)) if m.contains("invalid"))
        );
    }

    #[tokio::test]
    async fn openai_compatible_shape_and_probe() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{"message": {"content": "{\"overview\":\"x\"}"}}],
                "usage": {"prompt_tokens": 7, "completion_tokens": 2}
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": []})))
            .mount(&server)
            .await;
        let b = OpenAiBackend::new(
            Some(format!("{}/v1", server.uri())),
            None,
            "local".into(),
            false,
            Some("30m".into()),
        )
        .unwrap();
        b.probe(Duration::from_secs(2)).await.unwrap();
        let c = b.complete(&req()).await.unwrap();
        assert_eq!(c.usage.output_tokens, 2);
        let reqs = server.received_requests().await.unwrap();
        let body: Value = serde_json::from_slice(&reqs.last().unwrap().body).unwrap();
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["max_tokens"], 100);
        assert_eq!(body["keep_alive"], "30m");
    }

    #[tokio::test]
    async fn unreachable_server() {
        let b = OpenAiBackend::new(
            Some("http://127.0.0.1:9/v1".into()),
            None,
            "m".into(),
            false,
            None,
        )
        .unwrap();
        assert!(matches!(
            b.probe(Duration::from_secs(2)).await,
            Err(LlmError::Unreachable(_))
        ));
    }
}
