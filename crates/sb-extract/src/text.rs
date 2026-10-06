//! Decoding and normalization of text, and the plain-text / Markdown extractor.

use encoding_rs::{Encoding, UTF_8};
use regex::Regex;

use crate::{ExtractError, ExtractInput, ExtractLimits, Extracted};

/// Decode bytes of a text-like format. The order is: a BOM, a charset label
/// (media type parameter or an HTML `<meta>`), strict UTF-8, then detection
/// (Shift_JIS, EUC-JP, ISO-2022-JP, Windows-1252, ...). Returns the text and,
/// when the encoding is not UTF-8, a warning.
pub(crate) fn decode(
    bytes: &[u8],
    media_type: Option<&str>,
    sniff_html: bool,
) -> (String, Option<String>) {
    if let Some((enc, bom_len)) = Encoding::for_bom(bytes) {
        let (s, _) = enc.decode_without_bom_handling(&bytes[bom_len..]);
        let w = (enc != UTF_8).then(|| format!("decoded as {}", enc.name()));
        return (s.into_owned(), w);
    }
    let label = media_type
        .and_then(charset_param)
        .or_else(|| sniff_html.then(|| sniff_meta_charset(bytes)).flatten());
    if let Some(enc) = label.and_then(|l| Encoding::for_label(l.as_bytes())) {
        let (s, _) = enc.decode_without_bom_handling(bytes);
        let w = (enc != UTF_8).then(|| format!("decoded as {}", enc.name()));
        return (s.into_owned(), w);
    }
    if let Ok(s) = std::str::from_utf8(bytes) {
        return (s.to_string(), None);
    }
    let mut det = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Allow);
    det.feed(bytes, true);
    let enc = det.guess(Some(b"jp"), chardetng::Utf8Detection::Allow);
    let (s, _) = enc.decode_without_bom_handling(bytes);
    (
        s.into_owned(),
        Some(format!("decoded as {} (detected)", enc.name())),
    )
}

fn charset_param(media_type: &str) -> Option<String> {
    let lower = media_type.to_ascii_lowercase();
    let idx = lower.find("charset=")?;
    let v = &lower[idx + 8..];
    let v = v.split(';').next()?.trim().trim_matches(['"', '\'']);
    (!v.is_empty()).then(|| v.to_string())
}

fn sniff_meta_charset(bytes: &[u8]) -> Option<String> {
    let head = &bytes[..bytes.len().min(4096)];
    let head: String = head
        .iter()
        .map(|&b| if b < 0x80 { b as char } else { ' ' })
        .collect();
    let re = Regex::new(r#"(?i)charset\s*=\s*["']?\s*([A-Za-z0-9_\-]+)"#).ok()?;
    re.captures(&head).map(|c| c[1].to_string())
}

/// `\n` line endings, no BOM or control characters, trailing spaces removed and
/// runs of three or more blank lines collapsed to two.
pub(crate) fn normalize(s: &str) -> String {
    let s = s
        .trim_start_matches('\u{feff}')
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let mut out = String::with_capacity(s.len());
    let mut blank = 0usize;
    for line in s.split('\n') {
        let line: String = line
            .chars()
            .filter(|c| !c.is_control() || *c == '\t')
            .collect();
        let line = line.trim_end();
        if line.is_empty() {
            blank += 1;
            if blank > 2 {
                continue;
            }
        } else {
            blank = 0;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim_matches('\n').to_string()
}

pub(crate) fn extract_text(
    input: &ExtractInput<'_>,
    _limits: &ExtractLimits,
) -> Result<Extracted, ExtractError> {
    let (text, warn) = decode(input.bytes, input.media_type, false);
    let title = text
        .lines()
        .find(|l| l.trim_start().starts_with("# "))
        .map(|l| l.trim_start().trim_start_matches('#').trim().to_string());
    Ok(Extracted {
        title,
        text,
        warnings: warn.into_iter().collect(),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_shift_jis_and_euc_jp() {
        let (sjis, _, _) =
            encoding_rs::SHIFT_JIS.encode("議事録: 決定事項を確認した。次回は来週。");
        let (s, w) = decode(&sjis, None, false);
        assert!(s.contains("決定事項"));
        assert!(w.is_some());
        let (euc, _, _) = encoding_rs::EUC_JP.encode("議事録: 決定事項を確認した。次回は来週。");
        let (s, _) = decode(&euc, None, false);
        assert!(s.contains("決定事項"));
    }

    #[test]
    fn honours_labels() {
        let (sjis, _, _) = encoding_rs::SHIFT_JIS.encode("日本語");
        let (s, _) = decode(&sjis, Some("text/plain; charset=Shift_JIS"), false);
        assert_eq!(s, "日本語");
        let html = [b"<meta charset=\"shift_jis\">".as_slice(), &sjis].concat();
        let (s, _) = decode(&html, None, true);
        assert!(s.ends_with("日本語"));
    }

    #[test]
    fn utf8_is_preferred_and_silent() {
        let (s, w) = decode("あいう".as_bytes(), None, false);
        assert_eq!(s, "あいう");
        assert!(w.is_none());
    }

    #[test]
    fn markdown_title() {
        let r = extract_text(
            &ExtractInput {
                bytes: b"intro\n# The Title\nbody",
                media_type: None,
                file_name: None,
            },
            &ExtractLimits::default(),
        )
        .unwrap();
        assert_eq!(r.title.as_deref(), Some("The Title"));
    }
}
