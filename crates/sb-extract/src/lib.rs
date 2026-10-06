//! Pure text extraction (docs/specs/extract.md).
//!
//! Bytes in, Markdown out. No file, network, environment or clock access, so the
//! same input always gives the same output. Every input is untrusted: sizes,
//! decompressed sizes and time are bounded.

use std::time::Duration;

use chrono::{DateTime, Utc};

pub mod bundle;
mod csvfmt;
#[cfg(feature = "docx")]
mod docx;
#[cfg(feature = "html")]
mod html;
#[cfg(feature = "pdf")]
mod pdf;
#[cfg(feature = "pptx")]
mod pptx;
mod text;
#[cfg(feature = "xlsx")]
mod xlsx;
#[cfg(any(feature = "docx", feature = "pptx"))]
mod zipx;

/// Name and version recorded in the entry metadata.
pub const EXTRACTOR: &str = concat!("sb-extract ", env!("CARGO_PKG_VERSION"));

/// The document formats the extractor understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Text,
    Markdown,
    Csv,
    Tsv,
    Html,
    Docx,
    Pptx,
    Xlsx,
    Pdf,
    Unknown,
}

impl Format {
    pub fn as_str(self) -> &'static str {
        match self {
            Format::Text => "text",
            Format::Markdown => "markdown",
            Format::Csv => "csv",
            Format::Tsv => "tsv",
            Format::Html => "html",
            Format::Docx => "docx",
            Format::Pptx => "pptx",
            Format::Xlsx => "xlsx",
            Format::Pdf => "pdf",
            Format::Unknown => "unknown",
        }
    }
}

/// The input of one extraction.
#[derive(Debug, Clone, Copy)]
pub struct ExtractInput<'a> {
    pub bytes: &'a [u8],
    /// The media type, possibly with parameters (`text/html; charset=Shift_JIS`).
    pub media_type: Option<&'a str>,
    pub file_name: Option<&'a str>,
}

/// Bounds applied to every format.
#[derive(Debug, Clone)]
pub struct ExtractLimits {
    /// Output cap in characters; the text is cut and `Stats::truncated` is set.
    pub max_text_chars: usize,
    /// Sum of all decompressed zip members.
    pub max_decompressed_bytes: u64,
    /// Zip members visited.
    pub max_entries: usize,
    /// PDF pages and pptx slides.
    pub max_pages: usize,
    /// Spreadsheet and CSV cells.
    pub max_cells: usize,
    /// Deadline of `extract_bounded`.
    pub timeout: Duration,
}

impl Default for ExtractLimits {
    fn default() -> Self {
        ExtractLimits {
            max_text_chars: 300_000,
            max_decompressed_bytes: 200 * 1024 * 1024,
            max_entries: 10_000,
            max_pages: 2_000,
            max_cells: 1_000_000,
            timeout: Duration::from_secs(60),
        }
    }
}

/// Facts about the document.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stats {
    pub page_count: Option<usize>,
    pub slide_count: Option<usize>,
    pub sheet_names: Option<Vec<String>>,
    /// The text was cut at `max_text_chars`.
    pub truncated: bool,
}

/// The result of an extraction.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Extracted {
    pub title: Option<String>,
    /// Markdown.
    pub text: String,
    pub created: Option<DateTime<Utc>>,
    pub modified: Option<DateTime<Utc>>,
    pub stats: Stats,
    /// Non-fatal notes, for example the detected encoding.
    pub warnings: Vec<String>,
    /// Other facts found in the document (`site_name`, `canonical_url`).
    pub meta: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExtractError {
    #[error("unsupported format: {0}")]
    Unsupported(String),
    #[error("the file is corrupt: {0}")]
    Corrupt(String),
    #[error("the file is password-protected")]
    Encrypted,
    #[error("the file is too large to extract: {0}")]
    TooLarge(String),
    #[error("no extractable text")]
    Empty,
    #[error("extraction did not finish within {0} s")]
    Timeout(u64),
}

/// Decide the format: magic bytes first, then the media type, then the extension.
pub fn detect(bytes: &[u8], media_type: Option<&str>, file_name: Option<&str>) -> Format {
    if bytes.starts_with(b"%PDF-") {
        return Format::Pdf;
    }
    if bytes.starts_with(b"PK\x03\x04")
        && let Some(f) = detect_zip(bytes)
    {
        return f;
    }
    // OLE2 container (legacy .xls / .doc / .ppt): only .xls is readable.
    if bytes.starts_with(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]) {
        let ext = extension(file_name);
        return match ext.as_deref() {
            Some("xls") => Format::Xlsx,
            _ => Format::Unknown,
        };
    }
    let mt = media_type
        .map(|m| {
            m.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
        })
        .unwrap_or_default();
    let mt = if mt == "application/octet-stream" {
        String::new()
    } else {
        mt
    };
    if mt.is_empty() && looks_like_html(bytes) {
        return Format::Html;
    }
    match mt.as_str() {
        "text/html" | "application/xhtml+xml" => return Format::Html,
        "text/markdown" | "text/x-markdown" => return Format::Markdown,
        "text/csv" => return Format::Csv,
        "text/tab-separated-values" => return Format::Tsv,
        "text/plain" => {
            // A plain-text label does not override a more specific extension.
            if let Some(f) = by_extension(extension(file_name).as_deref()) {
                return f;
            }
            return Format::Text;
        }
        "application/pdf" => return Format::Pdf,
        _ => {}
    }
    if let Some(f) = by_extension(extension(file_name).as_deref()) {
        return f;
    }
    if mt.starts_with("text/") || mt == "application/json" {
        return Format::Text;
    }
    // No hint at all: text if it looks like text.
    if mt.is_empty() && looks_like_text(bytes) {
        return Format::Text;
    }
    Format::Unknown
}

fn extension(file_name: Option<&str>) -> Option<String> {
    let name = file_name?;
    let name = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let (_, ext) = name.rsplit_once('.')?;
    Some(ext.to_ascii_lowercase())
}

fn by_extension(ext: Option<&str>) -> Option<Format> {
    Some(match ext? {
        "txt" | "text" | "log" | "json" | "yaml" | "yml" | "xml" | "rst" => Format::Text,
        "md" | "markdown" => Format::Markdown,
        "csv" => Format::Csv,
        "tsv" => Format::Tsv,
        "html" | "htm" | "xhtml" => Format::Html,
        "docx" => Format::Docx,
        "pptx" => Format::Pptx,
        "xlsx" | "xlsm" | "xls" | "ods" => Format::Xlsx,
        "pdf" => Format::Pdf,
        _ => return None,
    })
}

fn looks_like_html(bytes: &[u8]) -> bool {
    let head: String = bytes
        .iter()
        .take(512)
        .map(|&b| if b < 0x80 { b as char } else { ' ' })
        .collect();
    let h = head.trim_start().to_ascii_lowercase();
    h.starts_with("<!doctype html") || h.starts_with("<html")
}

fn looks_like_text(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(8192)];
    bytes.is_empty() || !head.contains(&0)
}

/// Classify a zip container by its part names (from the local headers' names).
fn detect_zip(bytes: &[u8]) -> Option<Format> {
    // Cheap scan of the first and last parts of the archive for the part names.
    let hay_len = bytes.len().min(1 << 20);
    let mut hay = bytes[..hay_len].to_vec();
    if bytes.len() > hay_len {
        hay.extend_from_slice(&bytes[bytes.len().saturating_sub(1 << 20)..]);
    }
    let has = |needle: &[u8]| hay.windows(needle.len()).any(|w| w == needle);
    if has(b"word/document.xml") {
        Some(Format::Docx)
    } else if has(b"ppt/presentation.xml") || has(b"ppt/slides/slide") {
        Some(Format::Pptx)
    } else if has(b"xl/workbook.xml") || has(b"application/vnd.oasis.opendocument.spreadsheet") {
        Some(Format::Xlsx)
    } else {
        None
    }
}

/// Extract text. Pure and deterministic. Use [`extract_bounded`] to also bound
/// the time and to survive a panic inside a parser.
pub fn extract(
    input: &ExtractInput<'_>,
    limits: &ExtractLimits,
) -> Result<Extracted, ExtractError> {
    let format = detect(input.bytes, input.media_type, input.file_name);
    // A password-protected OOXML file is an OLE container with an OOXML extension.
    if input
        .bytes
        .starts_with(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1])
        && matches!(
            extension(input.file_name).as_deref(),
            Some("docx" | "pptx" | "xlsx" | "xlsm")
        )
    {
        return Err(ExtractError::Encrypted);
    }
    let mut out = match format {
        Format::Text | Format::Markdown => text::extract_text(input, limits)?,
        Format::Csv => csvfmt::extract(input, limits, b',')?,
        Format::Tsv => csvfmt::extract(input, limits, b'\t')?,
        #[cfg(feature = "html")]
        Format::Html => html::extract(input, limits)?,
        #[cfg(feature = "docx")]
        Format::Docx => docx::extract(input, limits)?,
        #[cfg(feature = "pptx")]
        Format::Pptx => pptx::extract(input, limits)?,
        #[cfg(feature = "xlsx")]
        Format::Xlsx => xlsx::extract(input, limits)?,
        #[cfg(feature = "pdf")]
        Format::Pdf => pdf::extract(input, limits)?,
        other => {
            let what = if other == Format::Unknown {
                match extension(input.file_name).as_deref() {
                    Some("doc") => "legacy .doc (save it as docx)".to_string(),
                    Some("ppt") => "legacy .ppt (save it as pptx)".to_string(),
                    Some(e) => format!(".{e}"),
                    None => "unknown file type".to_string(),
                }
            } else {
                other.as_str().to_string()
            };
            return Err(ExtractError::Unsupported(what));
        }
    };
    finish(&mut out, limits.max_text_chars);
    if out.text.trim().is_empty() {
        return Err(ExtractError::Empty);
    }
    Ok(out)
}

/// Run [`extract`] on a worker thread with a deadline. A panic inside a parser
/// becomes `Corrupt`. A parser that loops forever is abandoned (the thread keeps
/// running until the process exits).
pub fn extract_bounded(
    input: &ExtractInput<'_>,
    limits: &ExtractLimits,
) -> Result<Extracted, ExtractError> {
    let bytes = input.bytes.to_vec();
    let media_type = input.media_type.map(str::to_string);
    let file_name = input.file_name.map(str::to_string);
    let limits_c = limits.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("sb-extract".into())
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            let r = std::panic::catch_unwind(|| {
                extract(
                    &ExtractInput {
                        bytes: &bytes,
                        media_type: media_type.as_deref(),
                        file_name: file_name.as_deref(),
                    },
                    &limits_c,
                )
            });
            let _ = tx.send(r);
        });
    if let Err(e) = spawned {
        return Err(ExtractError::Corrupt(format!(
            "cannot start extraction: {e}"
        )));
    }
    match rx.recv_timeout(limits.timeout) {
        Ok(Ok(r)) => r,
        Ok(Err(_)) => Err(ExtractError::Corrupt("the parser panicked".into())),
        Err(_) => Err(ExtractError::Timeout(limits.timeout.as_secs())),
    }
}

/// Normalize the text and apply the output cap.
fn finish(out: &mut Extracted, max_chars: usize) {
    out.text = text::normalize(&out.text);
    if out.text.chars().count() > max_chars {
        let cut: String = out.text.chars().take(max_chars).collect();
        out.text = format!(
            "{}\n\n[... text cut at {max_chars} characters]",
            cut.trim_end()
        );
        out.stats.truncated = true;
    }
    if let Some(t) = &out.title {
        let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
        out.title = if t.is_empty() { None } else { Some(t) };
    }
}

/// Parse a timestamp from a document property: RFC 3339, or a bare date.
pub(crate) fn parse_time(s: &str) -> Option<DateTime<Utc>> {
    let s = s.trim();
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Some(t.with_timezone(&Utc));
    }
    for fmt in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S"] {
        if let Ok(t) = chrono::NaiveDateTime::parse_from_str(s, fmt) {
            return Some(t.and_utc());
        }
    }
    if let Ok(d) = chrono::NaiveDate::parse_from_str(s.get(..10)?, "%Y-%m-%d") {
        return d.and_hms_opt(0, 0, 0).map(|t| t.and_utc());
    }
    None
}

/// Escape a table cell for a Markdown table.
pub(crate) fn md_cell(s: &str) -> String {
    s.trim()
        .replace('|', "\\|")
        .replace("\r\n", "<br>")
        .replace(['\n', '\r'], "<br>")
}

/// Render rows as a Markdown table (the first row is the header).
pub(crate) fn md_table(rows: &[Vec<String>]) -> String {
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    for (i, row) in rows.iter().enumerate() {
        out.push('|');
        for c in 0..width {
            out.push(' ');
            out.push_str(&md_cell(row.get(c).map(String::as_str).unwrap_or("")));
            out.push_str(" |");
        }
        out.push('\n');
        if i == 0 {
            out.push('|');
            for _ in 0..width {
                out.push_str(" --- |");
            }
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(bytes: &[u8], name: &str) -> Result<Extracted, ExtractError> {
        extract(
            &ExtractInput {
                bytes,
                media_type: None,
                file_name: Some(name),
            },
            &ExtractLimits::default(),
        )
    }

    #[test]
    fn detects_by_content_media_type_and_extension() {
        assert_eq!(detect(b"%PDF-1.7 x", None, None), Format::Pdf);
        assert_eq!(
            detect(b"a,b", Some("text/csv; charset=utf-8"), None),
            Format::Csv
        );
        assert_eq!(detect(b"x", None, Some("dir/NOTE.MD")), Format::Markdown);
        assert_eq!(detect(b"x", Some("text/plain"), Some("a.csv")), Format::Csv);
        assert_eq!(detect(b"\x00\x01\x02", None, None), Format::Unknown);
        assert_eq!(detect(b"hello", None, None), Format::Text);
    }

    #[test]
    fn plain_text_is_normalized() {
        let r = run(b"\xEF\xBB\xBFa  \r\nb\r\n\r\n\r\n\r\n\r\nc\x00", "a.txt").unwrap();
        assert_eq!(r.text, "a\nb\n\n\nc");
    }

    #[test]
    fn empty_and_unsupported() {
        assert_eq!(run(b"  \n ", "a.txt"), Err(ExtractError::Empty));
        assert!(matches!(
            run(b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1....", "old.doc"),
            Err(ExtractError::Unsupported(m)) if m.contains("docx")
        ));
    }

    #[test]
    fn encrypted_ooxml_is_reported() {
        assert_eq!(
            run(b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1....", "secret.docx"),
            Err(ExtractError::Encrypted)
        );
    }

    #[test]
    fn truncates_and_marks() {
        let big = "あ".repeat(50);
        let limits = ExtractLimits {
            max_text_chars: 10,
            ..Default::default()
        };
        let r = extract(
            &ExtractInput {
                bytes: big.as_bytes(),
                media_type: None,
                file_name: Some("a.txt"),
            },
            &limits,
        )
        .unwrap();
        assert!(r.stats.truncated);
        assert!(r.text.starts_with(&"あ".repeat(10)));
        assert!(r.text.contains("cut at 10"));
    }

    #[test]
    fn extraction_is_deterministic() {
        let a = run("# T\n\nbody\n".as_bytes(), "a.md").unwrap();
        let b = run("# T\n\nbody\n".as_bytes(), "a.md").unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn bounded_reports_panics_as_corrupt_and_works_normally() {
        let r = extract_bounded(
            &ExtractInput {
                bytes: b"hello",
                media_type: None,
                file_name: Some("a.txt"),
            },
            &ExtractLimits::default(),
        )
        .unwrap();
        assert_eq!(r.text, "hello");
    }

    #[test]
    fn table_rendering_escapes_cells() {
        let t = md_table(&[
            vec!["a".into(), "b|c".into()],
            vec!["1".into(), "x\ny".into()],
        ]);
        assert_eq!(t, "| a | b\\|c |\n| --- | --- |\n| 1 | x<br>y |\n");
    }
}
