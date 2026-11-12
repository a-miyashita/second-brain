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

/// Prefix of a body-only input hash (ADR-0017).
pub const BODY_HASH_PREFIX: &str = "b2:";

/// Hash of a summary input for `llm_*` summaries: the body alone (ADR-0017). The
/// model, provider, profile and prompt version are not part of it, so switching
/// any of them never makes a summary stale.
pub fn body_hash(body: &str) -> String {
    format!("{BODY_HASH_PREFIX}{}", sha256_parts([body.as_bytes()]))
}

/// The hash recorded for source-native summaries (prompt version, model and
/// body; unchanged since ADR-0005). `llm_*` summaries use [`body_hash`].
pub fn summary_input_hash(prompt_version: &str, model: &str, body: &str) -> String {
    sha256_parts([prompt_version.as_bytes(), model.as_bytes(), body.as_bytes()])
}

/// Case-insensitive glob match. `*` matches any run of characters except `/`,
/// `**` any run including `/`, and `?` one character except `/`. A pattern
/// without `/` is also tried against the last path segment by
/// [`glob_match_path`].
pub fn glob_match(pattern: &str, text: &str) -> bool {
    fn go(p: &[char], t: &[char]) -> bool {
        match p.first() {
            None => t.is_empty(),
            Some('*') => {
                let double = p.get(1) == Some(&'*');
                let rest = &p[if double { 2 } else { 1 }..];
                // `**/` also matches nothing at all ("**/a" matches "a").
                if double && rest.first() == Some(&'/') && go(&rest[1..], t) {
                    return true;
                }
                (0..=t.len()).any(|i| (double || !t[..i].contains(&'/')) && go(rest, &t[i..]))
            }
            Some('?') => t.first().is_some_and(|c| *c != '/') && go(&p[1..], &t[1..]),
            Some(c) => t.first() == Some(c) && go(&p[1..], &t[1..]),
        }
    }
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    go(&p, &t)
}

/// [`glob_match`] on a path, or on its last segment when the pattern has no `/`.
pub fn glob_match_path(pattern: &str, path: &str) -> bool {
    glob_match(pattern, path)
        || (!pattern.contains('/') && glob_match(pattern, path.rsplit('/').next().unwrap_or(path)))
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
    #[test]
    fn globs() {
        use super::{glob_match, glob_match_path};
        assert!(glob_match("*.sharepoint.com", "contoso-my.SharePoint.com"));
        assert!(!glob_match("*.sharepoint.com", "sharepoint.com"));
        assert!(glob_match("~$*", "~$draft.docx"));
        assert!(glob_match("_archive/**", "_archive/2020/a.docx"));
        assert!(!glob_match("_archive/*", "_archive/2020/a.docx"));
        assert!(glob_match("**/old/*.pdf", "old/a.pdf"));
        assert!(glob_match("**/old/*.pdf", "x/y/old/a.pdf"));
        assert!(glob_match("a?c", "abc") && !glob_match("a?c", "a/c"));
        assert!(glob_match_path("*.docx", "minutes/2026/A.DOCX"));
        assert!(!glob_match_path("minutes/*.docx", "minutes/2026/a.docx"));
        assert!(glob_match_path("minutes/**", "minutes/2026/a.docx"));
    }

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
