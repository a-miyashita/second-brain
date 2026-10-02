//! The prompt-driven summarizer shared by every backend: prompt selection,
//! map-reduce for long inputs and one repair attempt for invalid JSON.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use sb_core::summarizer::{LlmError, Summarizer};
use sb_core::{Generator, GeneratorKind, SummaryInput, SummaryOutput, Usage};

use crate::backend::{Backend, CompletionRequest, Role};
use crate::prompts;

/// A summarizer built from a profile and a backend.
pub struct LlmSummarizer {
    pub kind: GeneratorKind,
    pub provider: String,
    pub model: String,
    pub max_input_chars: usize,
    pub max_output_tokens: u32,
    pub request_timeout: Duration,
    /// Timeout used for one retry after a timeout (local servers may be
    /// reloading the model).
    pub retry_timeout: Option<Duration>,
    /// `summary.language`: `auto`, `ja` or `en`.
    pub language: String,
    pub backend: Arc<dyn Backend>,
}

impl LlmSummarizer {
    async fn call(
        &self,
        system: &str,
        user: String,
        usage: &mut Usage,
    ) -> Result<String, LlmError> {
        let mut req = CompletionRequest {
            system: system.to_string(),
            messages: vec![(Role::User, user)],
            max_tokens: self.max_output_tokens,
            timeout: self.request_timeout,
        };
        let c = match self.backend.complete(&req).await {
            Err(LlmError::Timeout(t)) if self.retry_timeout.is_some() => {
                tracing::warn!(
                    "LLM request timed out ({t}); retrying once with the warm-up timeout"
                );
                req.timeout = self.retry_timeout.unwrap_or(self.request_timeout);
                self.backend.complete(&req).await?
            }
            other => other?,
        };
        usage.add(&c.usage);
        Ok(c.text)
    }

    /// A structured call retried from scratch when the output stays invalid.
    async fn structured(
        &self,
        system: &str,
        user: String,
        want_details: bool,
        usage: &mut Usage,
    ) -> Result<SummaryOutput, LlmError> {
        let mut attempt = 1;
        loop {
            match self
                .structured_once(system, user.clone(), want_details, usage)
                .await
            {
                Err(LlmError::BadOutput(e)) if attempt < BAD_OUTPUT_ATTEMPTS => {
                    tracing::warn!(attempt, error = %e, "bad summarizer output; retrying");
                    attempt += 1;
                }
                r => return r,
            }
        }
    }

    /// One structured call with a single repair attempt.
    async fn structured_once(
        &self,
        system: &str,
        user: String,
        want_details: bool,
        usage: &mut Usage,
    ) -> Result<SummaryOutput, LlmError> {
        let first = self.call(system, user.clone(), usage).await?;
        match prompts::parse_output(&first, want_details) {
            Ok(o) => Ok(o),
            Err(e) => {
                tracing::debug!(error = %e, "invalid summarizer output; asking for a repair");
                let req = CompletionRequest {
                    system: system.to_string(),
                    messages: vec![
                        (Role::User, user),
                        (Role::Assistant, first),
                        (Role::User, prompts::REPAIR_PROMPT.to_string()),
                    ],
                    max_tokens: self.max_output_tokens,
                    timeout: self.request_timeout,
                };
                let c = self.backend.complete(&req).await?;
                usage.add(&c.usage);
                prompts::parse_output(&c.text, want_details).map_err(LlmError::BadOutput)
            }
        }
    }
}

/// Fresh generations attempted (each with one repair) before giving up.
const BAD_OUTPUT_ATTEMPTS: u32 = 3;

#[async_trait]
impl Summarizer for LlmSummarizer {
    fn generator(&self, input: &SummaryInput) -> Generator {
        Generator {
            kind: self.kind,
            provider: self.provider.clone(),
            model: self.model.clone(),
            prompt_version: Some(prompts::prompt_version(input.prompt).to_string()),
        }
    }

    async fn summarize(&self, input: &SummaryInput) -> Result<SummaryOutput, LlmError> {
        let system = prompts::system_prompt(input, &self.language);
        let mut usage = Usage::default();
        let chunks = prompts::split_chunks(&input.body, self.max_input_chars);
        let mut out = if chunks.len() == 1 {
            self.structured(
                &system,
                prompts::user_prompt(input, &input.body),
                input.want_details,
                &mut usage,
            )
            .await?
        } else {
            tracing::info!(chunks = chunks.len(), title = %input.title, "long input; summarizing map-reduce style");
            let mut partials = Vec::with_capacity(chunks.len());
            for (i, c) in chunks.iter().enumerate() {
                let user = prompts::chunk_prompt(input, c, i, chunks.len());
                partials.push(
                    self.structured(&system, user, input.want_details, &mut usage)
                        .await?,
                );
            }
            self.structured(
                &system,
                prompts::merge_prompt(input, &partials),
                input.want_details,
                &mut usage,
            )
            .await?
        };
        out.usage = usage;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::Completion;
    use sb_core::{PromptKind, SourceKind};
    use std::sync::Mutex;

    /// Replays canned answers and records requests.
    struct Scripted {
        answers: Mutex<Vec<String>>,
        seen: Mutex<Vec<CompletionRequest>>,
    }

    #[async_trait]
    impl Backend for Scripted {
        async fn complete(&self, req: &CompletionRequest) -> Result<Completion, LlmError> {
            self.seen.lock().unwrap().push(req.clone());
            let text = self.answers.lock().unwrap().remove(0);
            Ok(Completion {
                text,
                usage: Usage {
                    input_tokens: 10,
                    output_tokens: 1,
                    calls: 1,
                    ..Default::default()
                },
            })
        }
    }

    fn summarizer(answers: &[&str], max_chars: usize) -> (Arc<Scripted>, LlmSummarizer) {
        let b = Arc::new(Scripted {
            answers: Mutex::new(answers.iter().map(|s| s.to_string()).collect()),
            seen: Mutex::new(vec![]),
        });
        let s = LlmSummarizer {
            kind: GeneratorKind::LlmApi,
            provider: "anthropic".into(),
            model: "m".into(),
            max_input_chars: max_chars,
            max_output_tokens: 100,
            request_timeout: Duration::from_secs(1),
            retry_timeout: None,
            language: "auto".into(),
            backend: b.clone(),
        };
        (b, s)
    }

    fn input(body: String) -> SummaryInput {
        SummaryInput {
            source_kind: SourceKind::SlackThread,
            prompt: PromptKind::Conversation,
            title: "t".into(),
            date: None,
            context: None,
            body,
            message_count: None,
            want_details: false,
        }
    }

    #[tokio::test]
    async fn repairs_once() {
        let (b, s) = summarizer(
            &["oops", r#"{"overview":"fixed","decisions":["d"]}"#],
            40_000,
        );
        let out = s.summarize(&input("x".into())).await.unwrap();
        assert_eq!(out.overview, "fixed");
        assert_eq!(out.usage.calls, 2);
        let seen = b.seen.lock().unwrap();
        assert_eq!(seen[1].messages.len(), 3);
        assert_eq!(
            s.generator(&input(String::new())).prompt_version.as_deref(),
            Some("conversation-summary/v1")
        );
    }

    #[tokio::test]
    async fn bad_output_after_repair_fails() {
        let (_b, s) = summarizer(&["oops"; 6], 40_000);
        assert!(matches!(
            s.summarize(&input("x".into())).await,
            Err(LlmError::BadOutput(_))
        ));
    }

    #[tokio::test]
    async fn map_reduce_for_long_input() {
        let body: String = (0..300)
            .map(|i| format!("line {i} with some words\n"))
            .collect();
        let ok = r#"{"overview":"part","decisions":[],"action_items":[]}"#;
        let (b, s) = summarizer(&[ok; 20], 2_000);
        let out = s.summarize(&input(body)).await.unwrap();
        let calls = b.seen.lock().unwrap().len();
        assert!(calls >= 4, "several chunks plus a merge, got {calls}");
        assert_eq!(out.usage.calls as usize, calls);
        assert!(
            b.seen.lock().unwrap().last().unwrap().messages[0]
                .1
                .contains("<partial_summaries>")
        );
    }
}
