//! PDF: the text layer, page by page. No OCR.

use crate::{ExtractError, ExtractInput, ExtractLimits, Extracted};

pub(crate) fn extract(
    input: &ExtractInput<'_>,
    limits: &ExtractLimits,
) -> Result<Extracted, ExtractError> {
    let pages = pdf_extract::extract_text_from_mem_by_pages(input.bytes).map_err(|e| {
        let m = e.to_string();
        if m.to_ascii_lowercase().contains("encrypt") || m.to_ascii_lowercase().contains("password")
        {
            ExtractError::Encrypted
        } else {
            ExtractError::Corrupt(m)
        }
    })?;
    if pages.len() > limits.max_pages {
        return Err(ExtractError::TooLarge(format!(
            "{} pages (limit {})",
            pages.len(),
            limits.max_pages
        )));
    }
    let empty = pages.iter().filter(|p| p.trim().is_empty()).count();
    let mut r = Extracted {
        text: pages
            .iter()
            .map(|p| p.trim())
            .filter(|p| !p.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n"),
        ..Default::default()
    };
    r.stats.page_count = Some(pages.len());
    if empty > 0 && empty < pages.len() {
        r.warnings.push(format!("{empty} pages had no text layer"));
    }
    if r.text.trim().is_empty() {
        r.warnings.push("no text layer (scanned document?)".into());
    }
    Ok(r)
}
