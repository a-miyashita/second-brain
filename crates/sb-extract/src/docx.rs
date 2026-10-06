//! docx: headings, paragraphs, lists, tables and footnotes as Markdown.

use crate::zipx::{Archive, Ev, attr, core_props, events};
use crate::{ExtractError, ExtractInput, ExtractLimits, Extracted, md_table};

pub(crate) fn extract(
    input: &ExtractInput<'_>,
    limits: &ExtractLimits,
) -> Result<Extracted, ExtractError> {
    let mut a = Archive::open(input.bytes, limits)?;
    let doc = a
        .read("word/document.xml")?
        .ok_or_else(|| ExtractError::Corrupt("word/document.xml is missing".into()))?;
    let mut text = render(&events(&doc)?);
    if let Some(fx) = a.read("word/footnotes.xml")? {
        let notes = footnotes(&events(&fx)?);
        if !notes.is_empty() {
            text = format!("{}\n\n---\n\n", text.trim_end());
            text.push_str(&notes);
        }
    }
    let (title, created, modified) = core_props(&mut a);
    Ok(Extracted {
        title,
        text,
        created,
        modified,
        ..Default::default()
    })
}

#[derive(Default)]
struct Para {
    text: String,
    style: Option<String>,
    list: bool,
    level: usize,
}

/// Paragraph and table state machine over `w:` events.
fn render(evs: &[Ev]) -> String {
    let mut out = String::new();
    let mut para = Para::default();
    let mut in_t = false;
    // Table state: rows of cells, each cell a vector of paragraph texts.
    let mut tables: Vec<Vec<Vec<String>>> = Vec::new();
    let mut cell_stack: Vec<String> = Vec::new();
    let mut row_stack: Vec<Vec<String>> = Vec::new();
    let mut in_numpr = false;
    for ev in evs {
        match ev {
            Ev::Start(n, at) | Ev::Empty(n, at) => match n.as_str() {
                "tbl" => tables.push(Vec::new()),
                "tr" => row_stack.push(Vec::new()),
                "tc" => cell_stack.push(String::new()),
                "p" => para = Para::default(),
                "pStyle" => para.style = attr(at, "val").map(str::to_string),
                "numPr" => in_numpr = true,
                "ilvl" if in_numpr => {
                    para.level = attr(at, "val").and_then(|v| v.parse().ok()).unwrap_or(0)
                }
                "numId" if in_numpr => para.list = attr(at, "val") != Some("0"),
                "t" => in_t = true,
                "tab" => para.text.push('\t'),
                "br" | "cr" => para.text.push('\n'),
                _ => {}
            },
            Ev::Text(t) if in_t => para.text.push_str(t),
            Ev::Text(_) => {}
            Ev::End(n) => match n.as_str() {
                "t" => in_t = false,
                "numPr" => in_numpr = false,
                "p" => {
                    let line = format_para(&para);
                    if let Some(cell) = cell_stack.last_mut() {
                        if !line.trim().is_empty() {
                            if !cell.is_empty() {
                                cell.push_str("<br>");
                            }
                            cell.push_str(line.trim());
                        }
                    } else if !line.trim().is_empty() {
                        out.push_str(&line);
                        out.push_str("\n\n");
                    }
                    para = Para::default();
                }
                "tc" => {
                    let c = cell_stack.pop().unwrap_or_default();
                    if let Some(r) = row_stack.last_mut() {
                        r.push(c);
                    }
                }
                "tr" => {
                    let r = row_stack.pop().unwrap_or_default();
                    if let Some(t) = tables.last_mut() {
                        t.push(r);
                    }
                }
                "tbl" => {
                    let t = tables.pop().unwrap_or_default();
                    // The cells already hold `<br>`, which `md_cell` must not escape again.
                    let table = md_table(&t).replace("\\<br>", "<br>");
                    if let Some(cell) = cell_stack.last_mut() {
                        // A nested table is flattened into the cell.
                        if !cell.is_empty() {
                            cell.push_str("<br>");
                        }
                        cell.push_str(&table.replace('\n', " "));
                    } else {
                        out.push_str(&table);
                        out.push('\n');
                    }
                }
                _ => {}
            },
        }
    }
    out
}

fn format_para(p: &Para) -> String {
    let text = p.text.trim_end().to_string();
    if text.trim().is_empty() {
        return String::new();
    }
    if let Some(style) = &p.style {
        let s = style.to_ascii_lowercase().replace([' ', '_', '-'], "");
        if s == "title" {
            return format!("# {text}");
        }
        if let Some(n) = s
            .strip_prefix("heading")
            .and_then(|d| d.parse::<usize>().ok())
            && (1..=6).contains(&n)
        {
            return format!("{} {text}", "#".repeat(n));
        }
    }
    if p.list {
        return format!("{}- {text}", "  ".repeat(p.level.min(6)));
    }
    text
}

/// Footnote paragraphs as `[^n]: text` lines. Separator notes (ids below 1) are skipped.
fn footnotes(evs: &[Ev]) -> String {
    let mut out = Vec::new();
    let mut id: Option<i64> = None;
    let mut cur = String::new();
    let mut in_t = false;
    for ev in evs {
        match ev {
            Ev::Start(n, at) if n == "footnote" => {
                id = attr(at, "id").and_then(|v| v.parse().ok());
                cur.clear();
            }
            Ev::Start(n, _) if n == "t" => in_t = true,
            Ev::Text(t) if in_t => cur.push_str(t),
            Ev::End(n) if n == "t" => in_t = false,
            Ev::End(n) if n == "footnote" => {
                if let Some(i) = id.take()
                    && i >= 1
                    && !cur.trim().is_empty()
                {
                    out.push(format!("[^{i}]: {}", cur.trim()));
                }
            }
            _ => {}
        }
    }
    out.join("\n")
}
