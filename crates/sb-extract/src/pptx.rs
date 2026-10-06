//! pptx: one block per slide, with the speaker notes.

use crate::zipx::{Archive, Ev, attr, core_props, events};
use crate::{ExtractError, ExtractInput, ExtractLimits, Extracted, md_table};

pub(crate) fn extract(
    input: &ExtractInput<'_>,
    limits: &ExtractLimits,
) -> Result<Extracted, ExtractError> {
    let mut a = Archive::open(input.bytes, limits)?;
    let mut slides: Vec<(u32, String)> = a
        .names()
        .into_iter()
        .filter_map(|n| {
            let num = n
                .strip_prefix("ppt/slides/slide")?
                .strip_suffix(".xml")?
                .parse()
                .ok()?;
            Some((num, n))
        })
        .collect();
    slides.sort();
    if slides.len() > limits.max_pages {
        return Err(ExtractError::TooLarge(format!(
            "{} slides (limit {})",
            slides.len(),
            limits.max_pages
        )));
    }
    let mut text = String::new();
    let count = slides.len();
    for (i, (num, name)) in slides.into_iter().enumerate() {
        let Some(xml) = a.read(&name)? else { continue };
        let slide = parse_shapes(&events(&xml)?);
        let notes = match notes_part(&mut a, num)? {
            Some(n) => parse_shapes(&events(&n)?).body,
            None => String::new(),
        };
        let title = slide.title.trim();
        text.push_str(&format!("## Slide {}", i + 1));
        if !title.is_empty() {
            text.push_str(&format!(": {title}"));
        }
        text.push_str("\n\n");
        if !slide.body.trim().is_empty() {
            text.push_str(slide.body.trim());
            text.push_str("\n\n");
        }
        if !notes.trim().is_empty() {
            text.push_str("Notes:\n\n");
            text.push_str(notes.trim());
            text.push_str("\n\n");
        }
    }
    let (title, created, modified) = core_props(&mut a);
    let mut r = Extracted {
        title,
        text,
        created,
        modified,
        ..Default::default()
    };
    r.stats.slide_count = Some(count);
    Ok(r)
}

/// The notes part of a slide, found through the slide's relationships.
fn notes_part(a: &mut Archive<'_>, num: u32) -> Result<Option<Vec<u8>>, ExtractError> {
    let rels = format!("ppt/slides/_rels/slide{num}.xml.rels");
    let Some(xml) = a.read(&rels)? else {
        return Ok(None);
    };
    for ev in events(&xml)? {
        if let Ev::Empty(n, at) | Ev::Start(n, at) = ev
            && n == "Relationship"
            && attr(&at, "Type").is_some_and(|t| t.ends_with("/notesSlide"))
            && let Some(target) = attr(&at, "Target")
        {
            let name = target.trim_start_matches("../");
            return a.read(&format!("ppt/{name}"));
        }
    }
    Ok(None)
}

#[derive(Default)]
struct Shapes {
    title: String,
    body: String,
}

/// Text of shapes in document order. Title placeholders go to `title`. Slide
/// number, date and footer placeholders, and the slide image of a notes page,
/// are skipped.
fn parse_shapes(evs: &[Ev]) -> Shapes {
    let mut out = Shapes::default();
    let mut ph: Option<String> = None;
    let mut in_sp = 0usize;
    let mut para = String::new();
    let mut shape_text = String::new();
    let mut in_t = false;
    let mut tables: Vec<Vec<Vec<String>>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut cell = String::new();
    let mut in_cell = false;
    for ev in evs {
        match ev {
            Ev::Start(n, at) | Ev::Empty(n, at) => match n.as_str() {
                "sp" => {
                    in_sp += 1;
                    ph = None;
                    shape_text.clear();
                }
                "ph" => ph = Some(attr(at, "type").unwrap_or("body").to_string()),
                "tbl" => tables.push(Vec::new()),
                "tr" => row.clear(),
                "tc" => {
                    in_cell = true;
                    cell.clear();
                }
                "p" => para.clear(),
                "t" => in_t = true,
                "br" => para.push('\n'),
                _ => {}
            },
            Ev::Text(t) if in_t => para.push_str(t),
            Ev::Text(_) => {}
            Ev::End(n) => match n.as_str() {
                "t" => in_t = false,
                "p" => {
                    let p = para.trim();
                    if !p.is_empty() {
                        if in_cell {
                            if !cell.is_empty() {
                                cell.push_str("<br>");
                            }
                            cell.push_str(p);
                        } else {
                            shape_text.push_str(p);
                            shape_text.push('\n');
                        }
                    }
                    para.clear();
                }
                "tc" => {
                    in_cell = false;
                    row.push(std::mem::take(&mut cell));
                }
                "tr" => {
                    if let Some(t) = tables.last_mut() {
                        t.push(std::mem::take(&mut row));
                    }
                }
                "tbl" => {
                    if let Some(t) = tables.pop() {
                        out.body.push_str(&md_table(&t));
                        out.body.push('\n');
                    }
                }
                "sp" => {
                    in_sp = in_sp.saturating_sub(1);
                    let t = shape_text.trim().to_string();
                    match ph.as_deref() {
                        Some("title" | "ctrTitle") => {
                            if out.title.is_empty() {
                                out.title = t.replace('\n', " ");
                            }
                        }
                        Some("sldNum" | "dt" | "ftr" | "sldImg" | "hdr") => {}
                        _ => {
                            if !t.is_empty() {
                                out.body.push_str(&t);
                                out.body.push_str("\n\n");
                            }
                        }
                    }
                    ph = None;
                    shape_text.clear();
                }
                _ => {}
            },
        }
    }
    let _ = in_sp;
    out
}
