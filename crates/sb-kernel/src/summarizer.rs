//! The summarizer abstraction (ADR-0005).

use async_trait::async_trait;

use crate::model::{Generator, SummaryInput, SummaryOutput, Usage};

/// Errors raised by summarizers.
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    /// The provider could not be reached (e.g. a local server is down).
    #[error("unreachable: {0}")]
    Unreachable(String),
    #[error("timed out: {0}")]
    Timeout(String),
    /// Missing or rejected credentials.
    #[error("authentication failed: {0}")]
    Auth(String),
    #[error("rate limited: {0}")]
    RateLimited(String),
    /// The provider returned an error.
    #[error("provider error: {0}")]
    Provider(String),
    /// The output was not valid JSON of the expected shape, even after a repair
    /// attempt.
    #[error("bad output: {0}")]
    BadOutput(String),
    #[error("configuration error: {0}")]
    Config(String),
    #[error("cancelled")]
    Cancelled,
}

impl LlmError {
    /// Stable issue code for this error.
    pub fn issue_code(&self) -> &'static str {
        match self {
            LlmError::Unreachable(_) => "llm.local_unreachable",
            LlmError::BadOutput(_) => "llm.bad_output",
            LlmError::Auth(_) => "llm.auth",
            LlmError::Config(_) => "llm.config",
            _ => "llm.failed",
        }
    }
}

/// Produces structured summaries from summary inputs.
#[async_trait]
pub trait Summarizer: Send + Sync {
    /// The generator recorded for summaries produced for `input`.
    fn generator(&self, input: &SummaryInput) -> Generator;

    /// Summarize, adding the usage of **every** call made to `usage`, including
    /// the calls of attempts that end in an error (billed, but unusable). The
    /// usage ledger needs those (ADR-0013).
    async fn summarize_tracked(
        &self,
        input: &SummaryInput,
        usage: &mut Usage,
    ) -> Result<SummaryOutput, LlmError>;

    /// Summarize; the total usage is in the returned `SummaryOutput::usage`.
    async fn summarize(&self, input: &SummaryInput) -> Result<SummaryOutput, LlmError> {
        let mut usage = Usage::default();
        let mut out = self.summarize_tracked(input, &mut usage).await?;
        out.usage = usage;
        Ok(out)
    }
}
