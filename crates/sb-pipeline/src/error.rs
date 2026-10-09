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
}

impl From<serde_json::Error> for PipelineError {
    fn from(e: serde_json::Error) -> Self {
        PipelineError::Invalid(e.to_string())
    }
}
