//! Notification sinks (ADR-0011).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::kinds::Severity;

/// An issue as seen by notification sinks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IssueNotice {
    pub code: String,
    pub severity: Severity,
    pub account_id: Option<String>,
    pub message: String,
    pub first_seen_at: DateTime<Utc>,
}

/// Errors raised by notifiers.
#[derive(Debug, thiserror::Error)]
#[error("notification failed: {0}")]
pub struct NotifyError(pub String);

/// A notification sink. `doctor` is the baseline and needs no implementation.
pub trait Notifier: Send + Sync {
    fn name(&self) -> &'static str;
    fn notify(&self, issue: &IssueNotice) -> Result<(), NotifyError>;
}
