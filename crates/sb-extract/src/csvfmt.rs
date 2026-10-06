//! CSV and TSV as a Markdown table.

use crate::text::decode;
use crate::{ExtractError, ExtractInput, ExtractLimits, Extracted, md_table};

pub(crate) fn extract(
    input: &ExtractInput<'_>,
    limits: &ExtractLimits,
    delimiter: u8,
) -> Result<Extracted, ExtractError> {
    let (text, warn) = decode(input.bytes, input.media_type, false);
    let mut rdr = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(false)
        .flexible(true)
        .from_reader(text.as_bytes());
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut cells = 0usize;
    for rec in rdr.records() {
        let rec = rec.map_err(|e| ExtractError::Corrupt(e.to_string()))?;
        cells += rec.len();
        if cells > limits.max_cells {
            return Err(ExtractError::TooLarge(format!(
                "more than {} cells",
                limits.max_cells
            )));
        }
        if rec.iter().all(|c| c.trim().is_empty()) {
            continue;
        }
        rows.push(rec.iter().map(str::to_string).collect());
    }
    Ok(Extracted {
        text: md_table(&rows),
        warnings: warn.into_iter().collect(),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(b: &[u8], d: u8) -> Result<Extracted, ExtractError> {
        extract(
            &ExtractInput {
                bytes: b,
                media_type: None,
                file_name: None,
            },
            &ExtractLimits::default(),
            d,
        )
    }

    #[test]
    fn quoted_fields_and_newlines() {
        let r = run(b"name,note\n\"Smith, J\",\"line1\nline2\"\n,\n", b',').unwrap();
        assert_eq!(
            r.text,
            "| name | note |\n| --- | --- |\n| Smith, J | line1<br>line2 |\n"
        );
    }

    #[test]
    fn tsv_and_ragged_rows() {
        let r = run(b"a\tb\tc\n1\t2\n", b'\t').unwrap();
        assert!(r.text.contains("| 1 | 2 |  |"));
    }

    #[test]
    fn cell_limit() {
        let limits = ExtractLimits {
            max_cells: 3,
            ..Default::default()
        };
        let r = extract(
            &ExtractInput {
                bytes: b"a,b\nc,d\n",
                media_type: None,
                file_name: None,
            },
            &limits,
            b',',
        );
        assert!(matches!(r, Err(ExtractError::TooLarge(_))));
    }
}
