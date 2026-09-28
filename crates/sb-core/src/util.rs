//! Small pure helpers shared across crates.

use chrono::{DateTime, SecondsFormat, Utc};
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

/// Hex SHA-256 of bytes.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Hex SHA-256 over several parts, each length-prefixed so that part
/// boundaries matter.
pub fn sha256_parts<'a>(parts: impl IntoIterator<Item = &'a [u8]>) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update((p.len() as u64).to_le_bytes());
        h.update(p);
    }
    hex::encode(h.finalize())
}

/// Hash of the exact summarizer input (summarization.md): prompt version,
/// model and body.
pub fn summary_input_hash(prompt_version: &str, model: &str, body: &str) -> String {
    sha256_parts([prompt_version.as_bytes(), model.as_bytes(), body.as_bytes()])
}

/// NFKC normalization, used for queries and indexed text comparisons.
pub fn nfkc(s: &str) -> String {
    s.nfkc().collect()
}

/// RFC 3339 in UTC with second precision, the catalog's timestamp format.
pub fn ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Parse an RFC 3339 timestamp.
pub fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// Make a source ID filesystem-safe (architecture.md). Short, safe IDs are kept
/// as they are; others get a readable prefix joined with a short hash.
pub fn slugify_source_id(id: &str) -> String {
    let safe: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let unchanged = safe == id && !id.starts_with('.') && !id.is_empty();
    if unchanged && id.len() <= 64 {
        return safe;
    }
    let prefix: String = safe.trim_start_matches('.').chars().take(40).collect();
    let hash = &sha256_hex(id.as_bytes())[..12];
    if prefix.is_empty() {
        hash.to_string()
    } else {
        format!("{prefix}-{hash}")
    }
}

/// Truncate to at most `max` characters, adding an ellipsis when cut.
pub fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_keeps_safe_ids() {
        assert_eq!(slugify_source_id("1AbC_def-9"), "1AbC_def-9");
    }

    #[test]
    fn slug_hashes_unsafe_ids() {
        let s = slugify_source_id("C0123:1727000000.000100");
        assert!(s.starts_with("C0123_1727000000.000100-"));
        assert_ne!(s, slugify_source_id("C0123/1727000000.000100"));
        let long = "x".repeat(100);
        assert!(slugify_source_id(&long).len() < 60);
        assert!(!slugify_source_id("..").starts_with('.'));
    }

    #[test]
    fn hash_parts_are_delimited() {
        assert_ne!(
            sha256_parts([b"ab".as_slice(), b"c".as_slice()]),
            sha256_parts([b"a".as_slice(), b"bc".as_slice()])
        );
    }

    #[test]
    fn nfkc_folds_width() {
        assert_eq!(nfkc("ＣＳＶ１"), "CSV1");
    }

    #[test]
    fn truncation() {
        assert_eq!(truncate_chars("abcdef", 4), "abc…");
        assert_eq!(truncate_chars("abc", 4), "abc");
    }
}
