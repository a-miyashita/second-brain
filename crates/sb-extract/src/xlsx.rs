//! xlsx / xls / ods: one table per visible sheet.

use std::io::Cursor;

use calamine::{Data, Reader, SheetVisible, open_workbook_auto_from_rs};

use crate::{ExtractError, ExtractInput, ExtractLimits, Extracted, md_table};

pub(crate) fn extract(
    input: &ExtractInput<'_>,
    limits: &ExtractLimits,
) -> Result<Extracted, ExtractError> {
    if input.bytes.len() as u64 > limits.max_decompressed_bytes {
        return Err(ExtractError::TooLarge("spreadsheet file".into()));
    }
    // A zip container is checked against the decompression limits first.
    if input.bytes.starts_with(b"PK") {
        crate::zipx::Archive::open(input.bytes, limits)?;
    }
    let mut wb = open_workbook_auto_from_rs(Cursor::new(input.bytes))
        .map_err(|e| ExtractError::Corrupt(e.to_string()))?;
    let visible: Vec<String> = wb
        .sheets_metadata()
        .iter()
        .filter(|s| s.visible == SheetVisible::Visible)
        .map(|s| s.name.clone())
        .collect();
    let mut text = String::new();
    let mut names = Vec::new();
    let mut cells = 0usize;
    for name in visible {
        let range = wb
            .worksheet_range(&name)
            .map_err(|e| ExtractError::Corrupt(e.to_string()))?;
        let (h, w) = range.get_size();
        cells = cells.saturating_add(h.saturating_mul(w));
        if cells > limits.max_cells {
            return Err(ExtractError::TooLarge(format!(
                "more than {} cells",
                limits.max_cells
            )));
        }
        let rows: Vec<Vec<String>> = range
            .rows()
            .map(|r| r.iter().map(cell).collect::<Vec<_>>())
            .filter(|r| r.iter().any(|c| !c.trim().is_empty()))
            .collect();
        names.push(name.clone());
        if rows.is_empty() {
            continue;
        }
        text.push_str(&format!("## Sheet: {name}\n\n"));
        text.push_str(&md_table(&rows));
        text.push('\n');
    }
    let mut r = Extracted {
        text,
        ..Default::default()
    };
    r.stats.sheet_names = Some(names);
    Ok(r)
}

fn cell(d: &Data) -> String {
    match d {
        Data::Empty => String::new(),
        Data::String(s) => s.clone(),
        Data::Float(f) if f.fract() == 0.0 && f.abs() < 1e15 => format!("{}", *f as i64),
        Data::Float(f) => f.to_string(),
        Data::Int(i) => i.to_string(),
        Data::Bool(b) => b.to_string(),
        Data::DateTime(dt) => match dt.as_datetime() {
            Some(t) if t.time() == chrono::NaiveTime::MIN => t.format("%Y-%m-%d").to_string(),
            Some(t) => t.format("%Y-%m-%d %H:%M:%S").to_string(),
            None => dt.as_f64().to_string(),
        },
        Data::DateTimeIso(s) | Data::DurationIso(s) => s.clone(),
        Data::Error(e) => format!("#{e:?}"),
    }
}
