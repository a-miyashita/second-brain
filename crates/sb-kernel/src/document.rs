//! Shared parts of the document sources (`google.doc`, `web.page`, `local.file`):
//! ingest options, settings and the pure normalization of stored text
//! (docs/specs/source-documents.md, ADR-0014).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::kinds::{RawRole, SectionKind, SectionOrigin};
use crate::model::{
    NormalizeOutcome, Normalized, PromptKind, RawSegment, SectionDraft, SourceRef, SummaryInput,
};
use crate::source::SourceError;

/// Fetch-metadata keys written by `fetch` and read by `normalize`.
pub mod keys {
    pub const CONTEXT: &str = "context";
    pub const TITLE_OVERRIDE: &str = "title_override";
    pub const DATE_OVERRIDE: &str = "date_override";
    pub const TITLE_FALLBACK: &str = "title_fallback";
    pub const DOC_TITLE: &str = "doc_title";
    pub const DOC_CREATED: &str = "doc_created";
    pub const DOC_MODIFIED: &str = "doc_modified";
    pub const SOURCE_CREATED: &str = "source_created";
    pub const SOURCE_MODIFIED: &str = "source_modified";
    pub const ORIGINAL_SHA256: &str = "original_sha256";
    pub const ORIGINAL_SIZE: &str = "original_size";
}

/// What `sb ingest` asks of a fetch. Travels in `FetchRequest::hint`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct IngestHint {
    /// The locator as the user gave it.
    pub locator: String,
    pub context: Option<String>,
    pub title: Option<String>,
    pub date: Option<DateTime<Utc>>,
    pub keep_original: bool,
}

impl IngestHint {
    pub fn from_value(v: &Value) -> IngestHint {
        serde_json::from_value(v.clone()).unwrap_or_default()
    }

    /// The overrides as fetch metadata. Only given values are included, so a
    /// later ingest without them keeps the stored ones.
    pub fn metadata(&self) -> serde_json::Map<String, Value> {
        let mut m = serde_json::Map::new();
        if let Some(c) = self.context.as_deref().filter(|c| !c.trim().is_empty()) {
            m.insert(keys::CONTEXT.into(), json!(c.trim()));
        }
        if let Some(t) = self.title.as_deref().filter(|t| !t.trim().is_empty()) {
            m.insert(keys::TITLE_OVERRIDE.into(), json!(t.trim()));
        }
        if let Some(d) = self.date {
            m.insert(
                keys::DATE_OVERRIDE.into(),
                json!(d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
            );
        }
        m
    }
}

/// Settings of `sb ingest` (`ingest.*`, docs/specs/ingest.md).
#[derive(Debug, Clone, PartialEq)]
pub struct IngestSettings {
    pub max_file_bytes: u64,
    pub max_text_chars: usize,
    pub min_text_chars: usize,
    pub extract_timeout_secs: u64,
    pub keep_original: bool,
    pub web_timeout_secs: u64,
    pub web_max_redirects: usize,
    pub web_allow_private: bool,
    pub local_deny: Vec<String>,
}

impl Default for IngestSettings {
    fn default() -> Self {
        IngestSettings {
            max_file_bytes: 50 * 1024 * 1024,
            max_text_chars: 300_000,
            min_text_chars: 20,
            extract_timeout_secs: 60,
            keep_original: false,
            web_timeout_secs: 30,
            web_max_redirects: 5,
            web_allow_private: false,
            local_deny: Vec::new(),
        }
    }
}

fn meta_str<'a>(m: &'a Value, key: &str) -> Option<&'a str> {
    m.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn meta_time(m: &Value, key: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(meta_str(m, key)?)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// Normalize a stored document: the `extracted_text` segment and the fetch
/// metadata become the `details` and `background` sections and the summary input.
/// Pure and deterministic.
pub fn normalize_document(
    source_ref: &SourceRef,
    fetch_metadata: &Value,
    segments: &[RawSegment],
) -> Result<NormalizeOutcome, SourceError> {
    let seg = segments
        .iter()
        .find(|s| s.role == RawRole::ExtractedText && s.seq == 0)
        .ok_or_else(|| SourceError::Parse("the extracted text is missing".into()))?;
    let text = String::from_utf8_lossy(&seg.bytes).into_owned();
    if text.trim().is_empty() {
        return Ok(NormalizeOutcome::NotApplicable(
            "no extractable text".into(),
        ));
    }
    let title = meta_str(fetch_metadata, keys::TITLE_OVERRIDE)
        .or_else(|| meta_str(fetch_metadata, keys::DOC_TITLE))
        .or_else(|| meta_str(fetch_metadata, keys::TITLE_FALLBACK))
        .unwrap_or(&source_ref.source_id)
        .to_string();
    let created = meta_time(fetch_metadata, keys::DATE_OVERRIDE)
        .or_else(|| meta_time(fetch_metadata, keys::SOURCE_CREATED))
        .or_else(|| meta_time(fetch_metadata, keys::DOC_CREATED))
        .or(source_ref.created_at);
    let updated = meta_time(fetch_metadata, keys::SOURCE_MODIFIED)
        .or_else(|| meta_time(fetch_metadata, keys::DOC_MODIFIED))
        .or(source_ref.updated_at);
    let context = meta_str(fetch_metadata, keys::CONTEXT).map(str::to_string);
    let mut sections = Vec::new();
    if let Some(c) = &context {
        sections.push(SectionDraft {
            kind: SectionKind::Background,
            origin: SectionOrigin::User,
            text: c.clone(),
        });
    }
    sections.push(SectionDraft {
        kind: SectionKind::Details,
        origin: SectionOrigin::Extracted,
        text: text.clone(),
    });
    Ok(NormalizeOutcome::Entry(Box::new(Normalized {
        title: title.clone(),
        source_url: source_ref.source_url.clone(),
        source_created_at: created,
        source_updated_at: updated,
        metadata: json!({}),
        sections,
        summary_input: Some(SummaryInput {
            source_kind: source_ref.source_kind,
            prompt: PromptKind::Document,
            title,
            date: created,
            context,
            body: text,
            message_count: None,
            want_details: false,
        }),
        native_summary: None,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kinds::SourceKind;
    use crate::model::AccountId;

    fn sref() -> SourceRef {
        SourceRef {
            account_id: AccountId::new("local").unwrap(),
            source_kind: SourceKind::LocalFile,
            source_id: "file:///a/b.md".into(),
            source_url: Some("file:///a/b.md".into()),
            created_at: None,
            updated_at: None,
        }
    }

    fn seg(text: &str) -> Vec<RawSegment> {
        vec![RawSegment {
            role: RawRole::ExtractedText,
            seq: 0,
            media_type: "text/markdown".into(),
            bytes: text.as_bytes().to_vec(),
        }]
    }

    fn entry(o: NormalizeOutcome) -> Normalized {
        match o {
            NormalizeOutcome::Entry(n) => *n,
            NormalizeOutcome::NotApplicable(w) => panic!("not applicable: {w}"),
        }
    }

    #[test]
    fn overrides_win_and_context_becomes_background() {
        let meta = json!({
            "title_override": "Mine", "doc_title": "Doc", "title_fallback": "b.md",
            "date_override": "2026-01-02T03:04:05Z", "source_created": "2025-01-01T00:00:00Z",
            "context": "  for the Q3 case  "
        });
        let n = entry(normalize_document(&sref(), &meta, &seg("body text")).unwrap());
        assert_eq!(n.title, "Mine");
        assert_eq!(
            n.source_created_at.unwrap().to_rfc3339(),
            "2026-01-02T03:04:05+00:00"
        );
        assert_eq!(n.sections.len(), 2);
        assert_eq!(n.sections[0].kind, SectionKind::Background);
        assert_eq!(n.sections[0].text, "for the Q3 case");
        assert_eq!(
            n.summary_input.unwrap().context.as_deref(),
            Some("for the Q3 case")
        );
    }

    #[test]
    fn title_falls_back_in_order() {
        let n = entry(
            normalize_document(
                &sref(),
                &json!({"doc_title":"Doc","title_fallback":"f"}),
                &seg("x"),
            )
            .unwrap(),
        );
        assert_eq!(n.title, "Doc");
        let n =
            entry(normalize_document(&sref(), &json!({"title_fallback":"f"}), &seg("x")).unwrap());
        assert_eq!(n.title, "f");
        let n = entry(normalize_document(&sref(), &json!({}), &seg("x")).unwrap());
        assert_eq!(n.title, "file:///a/b.md");
    }

    #[test]
    fn missing_or_empty_text() {
        assert!(normalize_document(&sref(), &json!({}), &[]).is_err());
        assert!(matches!(
            normalize_document(&sref(), &json!({}), &seg("  \n")).unwrap(),
            NormalizeOutcome::NotApplicable(_)
        ));
    }

    #[test]
    fn a_partial_hint_parses() {
        let h = IngestHint::from_value(&json!({"keep_original": true}));
        assert!(h.keep_original);
        assert_eq!(h.locator, "");
    }

    #[test]
    fn hint_metadata_only_contains_given_values() {
        let h = IngestHint {
            context: Some("why".into()),
            ..Default::default()
        };
        let m = h.metadata();
        assert_eq!(m.len(), 1);
        assert_eq!(m["context"], "why");
        assert_eq!(IngestHint::from_value(&Value::Null), IngestHint::default());
    }
}
