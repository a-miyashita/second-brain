//! Domain data types.

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::kinds::{
    AccountKind, GeneratorKind, Language, RawRole, SectionKind, SectionOrigin, SourceKind,
};

/// Error for invalid account IDs.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "invalid account id {0:?}: use 1-40 lowercase letters, digits, '-' or '_', starting with a letter or digit"
)]
pub struct InvalidAccountId(pub String);

/// User-chosen, immutable account slug (ADR-0007).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AccountId(String);

impl AccountId {
    pub fn new(s: impl Into<String>) -> Result<Self, InvalidAccountId> {
        let s = s.into();
        let valid = !s.is_empty()
            && s.len() <= 40
            && s.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
            && s.chars()
                .next()
                .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
        if valid {
            Ok(AccountId(s))
        } else {
            Err(InvalidAccountId(s))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for AccountId {
    type Error = InvalidAccountId;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        AccountId::new(s)
    }
}

impl From<AccountId> for String {
    fn from(a: AccountId) -> String {
        a.0
    }
}

impl fmt::Display for AccountId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A credential value. It has no `Display` and its `Debug` is redacted, so it
/// cannot end up in logs by accident.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(s: impl Into<String>) -> Self {
        Secret(s.into())
    }

    /// Access the secret value. Call sites should pass it straight to the
    /// consumer (an HTTP header, a subprocess environment) and nowhere else.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// A masked rendering, safe to print: keeps a short prefix only.
    pub fn masked(&self) -> String {
        let prefix: String = self.0.chars().take(4).collect();
        if self.0.chars().count() <= 8 {
            "****".to_string()
        } else {
            format!("{prefix}****")
        }
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(****)")
    }
}

/// The account context given to sources.
#[derive(Debug, Clone)]
pub struct AccountCtx {
    pub id: AccountId,
    pub kind: AccountKind,
    pub label: String,
    pub identity: Option<String>,
    /// Source settings, e.g. Slack `full_channels`.
    pub config: Value,
}

/// Natural key of an entry plus the canonical link and timestamps.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceRef {
    pub account_id: AccountId,
    pub source_kind: SourceKind,
    pub source_id: String,
    pub source_url: Option<String>,
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
}

/// One raw object produced by a fetch.
#[derive(Debug, Clone, PartialEq)]
pub struct RawObject {
    pub role: RawRole,
    pub media_type: String,
    /// File extension without the dot, e.g. `jsonl`, `md`.
    pub ext: String,
    pub bytes: Vec<u8>,
}

/// Whether a fetched bundle replaces the stored raw data or appends a segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawMode {
    /// Delete all segments and write segment 0.
    Replace,
    /// Add a new segment per role, holding only the new part (ADR-0012).
    Append,
}

/// Raw content with source metadata, as returned by `Source::fetch`.
#[derive(Debug, Clone, PartialEq)]
pub struct RawBundle {
    pub mode: RawMode,
    pub objects: Vec<RawObject>,
    /// New source-defined incremental state, e.g. `{"last_ts": ".."}`.
    pub fetch_state: Option<Value>,
    /// Source metadata known at fetch time (e.g. a calendar event title). It
    /// is stored in the entry's metadata and given back to `normalize`.
    pub metadata: Value,
}

/// A stored raw segment, as given to `normalize`.
#[derive(Debug, Clone, PartialEq)]
pub struct RawSegment {
    pub role: RawRole,
    pub seq: i64,
    pub media_type: String,
    pub bytes: Vec<u8>,
}

/// A section produced by normalization or summarization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SectionDraft {
    pub kind: SectionKind,
    pub origin: SectionOrigin,
    pub text: String,
}

/// Which embedded prompt a summary input uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptKind {
    Conversation,
    Meeting,
    Document,
}

/// Input to a summarizer (ADR-0005).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SummaryInput {
    pub source_kind: SourceKind,
    pub prompt: PromptKind,
    pub title: String,
    pub date: Option<DateTime<Utc>>,
    /// User-provided context (e.g. why a document was added).
    pub context: Option<String>,
    pub body: String,
    /// Number of messages for conversations; used by thresholds.
    pub message_count: Option<usize>,
    /// Whether the summarizer should produce a `details` section.
    pub want_details: bool,
}

/// Token usage and timing of a summarizer call.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub duration_ms: u64,
    /// Cost reported by the provider, when it reports one (e.g. Claude CLI).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    /// Number of LLM calls (more than one for map-reduce).
    pub calls: u32,
}

impl Usage {
    pub fn add(&mut self, other: &Usage) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.duration_ms += other.duration_ms;
        self.calls += other.calls;
        self.cost_usd = match (self.cost_usd, other.cost_usd) {
            (Some(a), Some(b)) => Some(a + b),
            (a, b) => a.or(b),
        };
    }
}

/// Structured summarizer output.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SummaryOutput {
    pub overview: String,
    #[serde(default)]
    pub decisions: Vec<String>,
    #[serde(default)]
    pub action_items: Vec<String>,
    #[serde(default)]
    pub details: Option<String>,
    #[serde(skip)]
    pub usage: Usage,
}

impl SummaryOutput {
    /// Render the output to `generated` sections. Empty lists produce no section.
    pub fn to_sections(&self, want_details: bool) -> Vec<SectionDraft> {
        let mut out = Vec::new();
        let overview = self.overview.trim();
        if !overview.is_empty() {
            out.push(SectionDraft {
                kind: SectionKind::Overview,
                origin: SectionOrigin::Generated,
                text: overview.to_string(),
            });
        }
        for (kind, items) in [
            (SectionKind::Decisions, &self.decisions),
            (SectionKind::ActionItems, &self.action_items),
        ] {
            let bullets: Vec<String> = items
                .iter()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(|s| format!("- {}", s.trim_start_matches("- ")))
                .collect();
            if !bullets.is_empty() {
                out.push(SectionDraft {
                    kind,
                    origin: SectionOrigin::Generated,
                    text: bullets.join("\n"),
                });
            }
        }
        if want_details
            && let Some(d) = self.details.as_deref().map(str::trim)
            && !d.is_empty()
        {
            out.push(SectionDraft {
                kind: SectionKind::Details,
                origin: SectionOrigin::Generated,
                text: d.to_string(),
            });
        }
        out
    }
}

/// Who generated the current summary of an entry (ADR-0005).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Generator {
    pub kind: GeneratorKind,
    pub provider: String,
    pub model: String,
    pub prompt_version: Option<String>,
}

impl Generator {
    /// The generator recorded for Gemini Meet notes.
    pub fn gemini_meet_notes() -> Self {
        Generator {
            kind: GeneratorKind::SourceNative,
            provider: "google".into(),
            model: "gemini-meet-notes".into(),
            prompt_version: None,
        }
    }
}

/// Context passed to `Source::normalize`. It holds everything normalize may
/// need beyond the raw data, so that normalize stays pure.
#[derive(Debug, Clone)]
pub struct NormalizeCtx {
    pub account: AccountCtx,
    pub language: Language,
    /// IANA time zone name for local dates (e.g. Slack day entries).
    pub timezone: String,
    /// Source-specific snapshot, e.g. the Slack user directory.
    pub snapshot: Value,
}

/// Input to `Source::normalize`.
#[derive(Debug, Clone)]
pub struct NormalizeInput {
    pub source_ref: SourceRef,
    /// Metadata stored at fetch time (from `RawBundle::metadata`).
    pub fetch_metadata: Value,
    /// All raw segments, ordered by role then seq.
    pub segments: Vec<RawSegment>,
}

/// Result of normalization.
#[derive(Debug, Clone, PartialEq)]
pub struct Normalized {
    pub title: String,
    pub source_url: Option<String>,
    pub source_created_at: Option<DateTime<Utc>>,
    pub source_updated_at: Option<DateTime<Utc>>,
    pub metadata: Value,
    /// Extracted sections, and native generated sections if any.
    pub sections: Vec<SectionDraft>,
    pub summary_input: Option<SummaryInput>,
    /// Set when `sections` contains a source-native summary.
    pub native_summary: Option<Generator>,
}

/// Outcome of normalization.
#[derive(Debug, Clone, PartialEq)]
pub enum NormalizeOutcome {
    Entry(Box<Normalized>),
    /// The raw data is not something this source makes entries from
    /// (e.g. an agenda document attached to a calendar event).
    NotApplicable(String),
}

/// Work discovered by a sync but not yet fetched (ADR-0012).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueueItem {
    pub source_kind: SourceKind,
    pub source_id: String,
    pub reason: String,
    pub hint: Value,
}

/// A request to fetch one item.
#[derive(Debug, Clone, PartialEq)]
pub struct FetchRequest {
    pub source_kind: SourceKind,
    pub source_id: String,
    /// Stored incremental state; `None` for new items or full refetches.
    pub fetch_state: Option<Value>,
    /// Stored fetch metadata of an existing entry.
    pub metadata: Value,
    pub hint: Value,
    /// Force a full refetch (replace all segments).
    pub full: bool,
}

/// An entry with freshly fetched raw data.
#[derive(Debug, Clone, PartialEq)]
pub struct FetchedEntry {
    pub source_ref: SourceRef,
    pub bundle: RawBundle,
}

/// Outcome of fetching one item.
#[derive(Debug, Clone, PartialEq)]
pub enum FetchOutcome {
    Fetched(Box<FetchedEntry>),
    /// Nothing new since the stored fetch state.
    Unchanged,
    /// The item exists but is not something this source makes entries from.
    NotApplicable(String),
    /// The item no longer exists or is not accessible.
    NotFound(String),
}

/// A cursor update in `sync_state`.
#[derive(Debug, Clone, PartialEq)]
pub struct CursorUpdate {
    pub source_kind: SourceKind,
    pub key: String,
    /// `None` deletes the cursor.
    pub value: Option<Value>,
}

/// A unit of discovery committed atomically: entries fetched during discovery,
/// newly queued items and the cursors that cover them (ADR-0012).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DiscoveryBatch {
    pub entries: Vec<FetchedEntry>,
    pub enqueue: Vec<QueueItem>,
    pub cursors: Vec<CursorUpdate>,
}

impl DiscoveryBatch {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty() && self.enqueue.is_empty() && self.cursors.is_empty()
    }
}

/// Sync depth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SyncMode {
    #[default]
    Normal,
    Deep,
}

/// Options for one sync of one source.
#[derive(Debug, Clone, Default)]
pub struct SyncOptions {
    pub mode: SyncMode,
    /// Back-fill from this date instead of the source default.
    pub since: Option<DateTime<Utc>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_id_validation() {
        assert!(AccountId::new("work-google").is_ok());
        assert!(AccountId::new("acme_slack2").is_ok());
        assert!(AccountId::new("").is_err());
        assert!(AccountId::new("Work").is_err());
        assert!(AccountId::new("-x").is_err());
        assert!(AccountId::new("a b").is_err());
    }

    #[test]
    fn secret_is_redacted() {
        let s = Secret::new("xoxp-1234567890");
        assert_eq!(format!("{s:?}"), "Secret(****)");
        assert_eq!(s.masked(), "xoxp****");
        assert_eq!(Secret::new("short").masked(), "****");
    }

    #[test]
    fn summary_output_rendering() {
        let out = SummaryOutput {
            overview: " We met. ".into(),
            decisions: vec!["Alice decided CSV".into(), "  ".into()],
            action_items: vec![],
            details: Some("long".into()),
            usage: Usage::default(),
        };
        let s = out.to_sections(false);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].text, "We met.");
        assert_eq!(s[1].kind, SectionKind::Decisions);
        assert_eq!(s[1].text, "- Alice decided CSV");
        assert_eq!(out.to_sections(true).len(), 3);
    }
}
