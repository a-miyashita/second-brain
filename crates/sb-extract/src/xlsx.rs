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
        let mut a = crate::zipx::Archive::open(input.bytes, limits)?;
        check_dimensions(&mut a, limits)?;
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

/// The declared size of a sheet in cells, from `<dimension ref="A1:C9"/>`.
fn declared_cells(xml: &str) -> Option<u64> {
    let start = xml.find("<dimension")?;
    let rest = &xml[start..];
    let end = rest.find('>')?;
    let tag = &rest[..end];
    let r = tag.split("ref=\"").nth(1)?.split('"').next()?;
    let (a, b) = r.split_once(':').unwrap_or((r, r));
    let split = |s: &str| {
        let col: String = s.chars().take_while(char::is_ascii_alphabetic).collect();
        let row: String = s.chars().skip(col.len()).collect();
        let c = col
            .to_ascii_uppercase()
            .bytes()
            .fold(0u64, |n, ch| n * 26 + u64::from(ch - b'A' + 1));
        Some((c, row.parse::<u64>().ok()?))
    };
    let (c1, r1) = split(a)?;
    let (c2, r2) = split(b)?;
    Some(c2.abs_diff(c1).saturating_add(1) * r2.abs_diff(r1).saturating_add(1))
}

/// A sparse sheet with a huge declared range would be materialized in full by the
/// reader, so the declared sizes are checked first.
fn check_dimensions(
    a: &mut crate::zipx::Archive<'_>,
    limits: &ExtractLimits,
) -> Result<(), ExtractError> {
    let mut total = 0u64;
    for name in a.names() {
        if !(name.starts_with("xl/worksheets/") && name.ends_with(".xml")) {
            continue;
        }
        if let Some(bytes) = a.read(&name)? {
            let head = &bytes[..bytes.len().min(4096)];
            if let Some(n) = declared_cells(&String::from_utf8_lossy(head)) {
                total = total.saturating_add(n);
                if total > limits.max_cells as u64 {
                    return Err(ExtractError::TooLarge(format!(
                        "a sheet declares more than {} cells",
                        limits.max_cells
                    )));
                }
            }
        }
    }
    Ok(())
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
