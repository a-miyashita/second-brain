use second_brain_kernel::source::SourceError;
use second_brain_kernel::summarizer::LlmError;
use second_brain_store::StoreError;

/// Errors raised by the pipeline.
#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Source(#[from] SourceError),
    #[error(transparent)]
    Llm(#[from] LlmError),
    #[error("{0}")]
    Invalid(String),
    #[error("another sync is running")]
    Locked,
    /// `--mine` was asked but no alias of the user is known (accounts-and-auth.md).
    #[error(
        "no alias of yours is known, so \"mine\" cannot be decided; add extra_aliases to an account (sb config edit --account <id>)"
    )]
    AliasesEmpty,
}

impl From<serde_json::Error> for PipelineError {
    fn from(e: serde_json::Error) -> Self {
        PipelineError::Invalid(e.to_string())
    }
}
