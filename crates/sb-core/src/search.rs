//! The pluggable search backend (ADR-0004).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::kinds::{SectionKind, SourceKind};
use crate::model::AccountId;

/// Default number of hits.
pub const DEFAULT_LIMIT: u32 = 8;
/// Maximum number of hits.
pub const MAX_LIMIT: u32 = 50;

/// Search mode. Only full-text exists in the MVP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    #[default]
    Auto,
    FullText,
    Vector,
    Hybrid,
}

/// A search query. Terms are ANDed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchQuery {
    pub terms: Vec<String>,
    pub sections: Vec<SectionKind>,
    pub source_kinds: Vec<SourceKind>,
    pub accounts: Vec<AccountId>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub limit: u32,
    pub mode: SearchMode,
    /// Return every matching section instead of the best one per entry.
    pub all_sections: bool,
}

impl Default for SearchQuery {
    fn default() -> Self {
        SearchQuery {
            terms: Vec::new(),
            sections: Vec::new(),
            source_kinds: Vec::new(),
            accounts: Vec::new(),
            since: None,
            until: None,
            limit: DEFAULT_LIMIT,
            mode: SearchMode::Auto,
            all_sections: false,
        }
    }
}

impl SearchQuery {
    /// The limit clamped to `1..=MAX_LIMIT`.
    pub fn effective_limit(&self) -> u32 {
        self.limit.clamp(1, MAX_LIMIT)
    }
}

/// One search hit: an entry and one of its sections.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hit {
    pub entry_id: i64,
    pub section: SectionKind,
    pub score: f64,
    pub snippet: String,
}

/// What a backend can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    pub full_text: bool,
    pub vector: bool,
    pub hybrid: bool,
}

/// Errors raised by search backends.
#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error("invalid query: {0}")]
    InvalidQuery(String),
    #[error("unsupported mode: {0:?}")]
    UnsupportedMode(SearchMode),
    #[error("backend error: {0}")]
    Backend(String),
}

/// A search backend. Backends own their tables and files, are told when an
/// entry's sections change, and can always be rebuilt from the catalog.
pub trait SearchBackend {
    fn name(&self) -> &'static str;
    fn capabilities(&self) -> Capabilities;
    /// (Re-)index all sections of an entry.
    fn index(&self, entry_id: i64) -> Result<(), SearchError>;
    /// Remove an entry from the index.
    fn remove(&self, entry_id: i64) -> Result<(), SearchError>;
    /// Rebuild the whole index from the catalog. Returns the number of rows indexed.
    fn rebuild(&self) -> Result<u64, SearchError>;
    fn search(&self, query: &SearchQuery) -> Result<Vec<Hit>, SearchError>;
}
