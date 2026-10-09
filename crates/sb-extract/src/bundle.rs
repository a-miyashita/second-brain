//! Turn the bytes of a document into the raw bundle of an ingested entry
//! (ADR-0014): the extracted text is the raw data, the original bytes are kept
//! only on request.

use chrono::{DateTime, SecondsFormat, Utc};
use second_brain_kernel::document::{IngestHint, IngestSettings, keys};
use second_brain_kernel::source::SourceError;
use second_brain_kernel::{RawBundle, RawMode, RawObject, RawRole};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::{EXTRACTOR, ExtractError, ExtractInput, ExtractLimits, Extracted, extract_bounded};

/// Everything `build_bundle` needs from a source's fetch.
pub struct BundleInput<'a> {
    /// The bytes to extract text from.
    pub bytes: &'a [u8],
    /// The bytes as obtained, when `bytes` was prepared first (a cleaned export).
    /// They are what `--keep-original` stores and what the hash covers.
    pub original: Option<&'a [u8]>,
    pub media_type: Option<&'a str>,
    pub file_name: Option<&'a str>,
    pub hint: &'a IngestHint,
    pub settings: &'a IngestSettings,
    /// Used as the title when the document has none (a file or page name).
    pub fallback_title: Option<String>,
    pub source_created: Option<DateTime<Utc>>,
    pub source_modified: Option<DateTime<Utc>>,
    /// Source-specific metadata (`drive_file_id`, `final_url`, ...).
    pub extra_meta: Map<String, Value>,
    pub fetch_state: Option<Value>,
}

pub enum DocBundle {
    Bundle(RawBundle),
    /// Nothing to store: an unsupported type, or no text.
    NotApplicable(String),
}

fn rfc3339(t: DateTime<Utc>) -> Value {
    json!(t.to_rfc3339_opts(SecondsFormat::Secs, true))
}

/// The lowercase hex SHA-256 of a byte slice.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// The extension of the stored original: 1 to 8 ASCII alphanumerics, else `bin`.
fn original_ext(file_name: Option<&str>) -> String {
    file_name
        .and_then(|n| n.rsplit_once('.'))
        .map(|(_, e)| e.to_ascii_lowercase())
        .filter(|e| (1..=8).contains(&e.len()) && e.chars().all(|c| c.is_ascii_alphanumeric()))
        .unwrap_or_else(|| "bin".into())
}

fn limits(s: &IngestSettings) -> ExtractLimits {
    ExtractLimits {
        max_text_chars: s.max_text_chars,
        timeout: std::time::Duration::from_secs(s.extract_timeout_secs.max(1)),
        ..Default::default()
    }
}

/// Extract the text of `input` and build the bundle. `Replace` mode; the raw
/// data is the `extracted_text` object, plus `primary` with `keep_original`.
pub fn build_bundle(input: BundleInput<'_>) -> Result<DocBundle, SourceError> {
    if input.bytes.len() as u64 > input.settings.max_file_bytes {
        return Err(SourceError::Rejected(format!(
            "the file is larger than ingest.max_file_bytes ({} bytes)",
            input.settings.max_file_bytes
        )));
    }
    let ex: Extracted = match extract_bounded(
        &ExtractInput {
            bytes: input.bytes,
            media_type: input.media_type,
            file_name: input.file_name,
        },
        &limits(input.settings),
    ) {
        Ok(e) => e,
        Err(ExtractError::Unsupported(what)) => {
            return Ok(DocBundle::NotApplicable(format!(
                "unsupported file type: {what}"
            )));
        }
        Err(ExtractError::Empty) => {
            return Ok(DocBundle::NotApplicable(no_text_message(&input)));
        }
        Err(e) => return Err(SourceError::Rejected(e.to_string())),
    };
    if ex.text.trim().chars().count() < input.settings.min_text_chars {
        return Ok(DocBundle::NotApplicable(no_text_message(&input)));
    }
    let mut meta = input.extra_meta;
    for (k, v) in input.hint.metadata() {
        meta.insert(k, v);
    }
    let original = input.original.unwrap_or(input.bytes);
    meta.insert(keys::ORIGINAL_SHA256.into(), json!(sha256_hex(original)));
    meta.insert(keys::ORIGINAL_SIZE.into(), json!(original.len()));
    meta.insert("extractor".into(), json!(EXTRACTOR));
    meta.insert("text_truncated".into(), json!(ex.stats.truncated));
    if let Some(t) = &ex.title {
        meta.insert(keys::DOC_TITLE.into(), json!(t));
    }
    if let Some(t) = ex.created {
        meta.insert(keys::DOC_CREATED.into(), rfc3339(t));
    }
    if let Some(t) = ex.modified {
        meta.insert(keys::DOC_MODIFIED.into(), rfc3339(t));
    }
    if let Some(t) = input.source_created {
        meta.insert(keys::SOURCE_CREATED.into(), rfc3339(t));
    }
    if let Some(t) = input.source_modified {
        meta.insert(keys::SOURCE_MODIFIED.into(), rfc3339(t));
    }
    if let Some(t) = input.fallback_title.filter(|t| !t.trim().is_empty()) {
        meta.insert(keys::TITLE_FALLBACK.into(), json!(t));
    }
    if let Some(n) = ex.stats.page_count {
        meta.insert("page_count".into(), json!(n));
    }
    if let Some(n) = ex.stats.slide_count {
        meta.insert("slide_count".into(), json!(n));
    }
    if let Some(n) = &ex.stats.sheet_names {
        meta.insert("sheet_names".into(), json!(n));
    }
    if !ex.warnings.is_empty() {
        meta.insert("extract_warnings".into(), json!(ex.warnings));
    }
    for (k, v) in &ex.meta {
        meta.insert(k.clone(), json!(v));
    }
    let mut objects = vec![RawObject {
        role: RawRole::ExtractedText,
        media_type: "text/markdown".into(),
        ext: "md".into(),
        bytes: ex.text.into_bytes(),
    }];
    if input.hint.keep_original || input.settings.keep_original {
        objects.push(RawObject {
            role: RawRole::Primary,
            media_type: input
                .media_type
                .map(|m| m.split(';').next().unwrap_or(m).trim().to_string())
                .filter(|m| !m.is_empty())
                .unwrap_or_else(|| "application/octet-stream".into()),
            ext: original_ext(input.file_name),
            bytes: original.to_vec(),
        });
    }
    Ok(DocBundle::Bundle(RawBundle {
        mode: RawMode::Replace,
        objects,
        fetch_state: input.fetch_state,
        metadata: Value::Object(meta),
    }))
}

fn no_text_message(input: &BundleInput<'_>) -> String {
    let html = crate::detect(input.bytes, input.media_type, input.file_name) == crate::Format::Html;
    if html {
        "no extractable text; the page may need JavaScript".into()
    } else {
        "no extractable text (a scanned document has no text layer)".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input<'a>(
        bytes: &'a [u8],
        name: &'a str,
        hint: &'a IngestHint,
        settings: &'a IngestSettings,
    ) -> BundleInput<'a> {
        BundleInput {
            bytes,
            original: None,
            media_type: None,
            file_name: Some(name),
            hint,
            settings,
            fallback_title: Some(name.into()),
            source_created: None,
            source_modified: None,
            extra_meta: Map::new(),
            fetch_state: None,
        }
    }

    #[test]
    fn text_is_the_raw_data_and_the_original_is_dropped_by_default() {
        let hint = IngestHint {
            context: Some("why".into()),
            ..Default::default()
        };
        let s = IngestSettings::default();
        let DocBundle::Bundle(b) = build_bundle(input(
            b"# Title\n\nSome body text for the test.",
            "n.md",
            &hint,
            &s,
        ))
        .unwrap() else {
            panic!("expected a bundle")
        };
        assert_eq!(b.objects.len(), 1);
        assert_eq!(b.objects[0].role, RawRole::ExtractedText);
        assert_eq!(b.metadata["doc_title"], "Title");
        assert_eq!(b.metadata["context"], "why");
        assert_eq!(b.metadata["original_size"], 37);
        assert_eq!(
            b.metadata["original_sha256"].as_str().map(str::len),
            Some(64)
        );
        assert_eq!(b.metadata["title_fallback"], "n.md");
    }

    #[test]
    fn keep_original_adds_a_primary_object() {
        let hint = IngestHint {
            keep_original: true,
            ..Default::default()
        };
        let s = IngestSettings::default();
        let DocBundle::Bundle(b) =
            build_bundle(input(b"plain text that is long enough", "x.TXT", &hint, &s)).unwrap()
        else {
            panic!("expected a bundle")
        };
        assert_eq!(b.objects.len(), 2);
        assert_eq!(b.objects[1].role, RawRole::Primary);
        assert_eq!(b.objects[1].ext, "txt");
        assert_eq!(b.objects[1].bytes, b"plain text that is long enough");
    }

    #[test]
    fn short_unsupported_and_oversized_inputs() {
        let hint = IngestHint::default();
        let s = IngestSettings::default();
        assert!(matches!(
            build_bundle(input(b"tiny", "a.txt", &hint, &s)).unwrap(),
            DocBundle::NotApplicable(m) if m.contains("no extractable text")
        ));
        assert!(matches!(
            build_bundle(input(b"\x00\x01\x02\x03 binary", "a.bin", &hint, &s)).unwrap(),
            DocBundle::NotApplicable(m) if m.contains("unsupported")
        ));
        let small = IngestSettings {
            max_file_bytes: 4,
            ..Default::default()
        };
        assert!(build_bundle(input(b"more than four bytes", "a.txt", &hint, &small)).is_err());
    }
}
