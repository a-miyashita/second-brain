#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Golden tests per format. Fixtures are generated here; they contain no real data.

use std::io::Write;

use sb_extract::{ExtractError, ExtractInput, ExtractLimits, extract, extract_bounded};

fn zip_of(parts: &[(&str, &str)]) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut z = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts = zip::write::SimpleFileOptions::default();
        for (name, body) in parts {
            z.start_file(*name, opts).unwrap();
            z.write_all(body.as_bytes()).unwrap();
        }
        z.finish().unwrap();
    }
    buf
}

fn run(bytes: &[u8], name: &str) -> Result<sb_extract::Extracted, ExtractError> {
    extract(
        &ExtractInput {
            bytes,
            media_type: None,
            file_name: Some(name),
        },
        &ExtractLimits::default(),
    )
}

const W: &str = "xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"";

#[test]
fn docx_headings_lists_tables_footnotes_and_tracked_changes() {
    let doc = format!(
        r#"<w:document {W}><w:body>
<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Plan</w:t></w:r></w:p>
<w:p><w:r><w:t>Intro </w:t></w:r><w:ins><w:r><w:t>inserted</w:t></w:r></w:ins><w:del><w:r><w:delText>deleted</w:delText></w:r></w:del><w:r><w:t> &amp; more</w:t></w:r></w:p>
<w:p><w:pPr><w:numPr><w:ilvl w:val="1"/><w:numId w:val="3"/></w:numPr></w:pPr><w:r><w:t>item</w:t></w:r></w:p>
<w:tbl><w:tr><w:tc><w:p><w:r><w:t>Name</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Owner</w:t></w:r></w:p></w:tc></w:tr>
<w:tr><w:tc><w:p><w:r><w:t>Task</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>佐藤</w:t></w:r></w:p></w:tc></w:tr></w:tbl>
<w:p><w:r><w:t>End</w:t></w:r></w:p></w:body></w:document>"#
    );
    let foot = format!(
        r#"<w:footnotes {W}><w:footnote w:id="-1"><w:p><w:r><w:t>sep</w:t></w:r></w:p></w:footnote>
<w:footnote w:id="1"><w:p><w:r><w:t>A note</w:t></w:r></w:p></w:footnote></w:footnotes>"#
    );
    let core = r#"<cp:coreProperties xmlns:cp="x" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="d"><dc:title>Quarterly plan</dc:title><dcterms:created>2026-09-01T00:00:00Z</dcterms:created></cp:coreProperties>"#;
    let bytes = zip_of(&[
        ("word/document.xml", &doc),
        ("word/footnotes.xml", &foot),
        ("docProps/core.xml", core),
    ]);
    let r = run(&bytes, "plan.docx").unwrap();
    assert_eq!(r.title.as_deref(), Some("Quarterly plan"));
    assert_eq!(r.created.unwrap().to_rfc3339(), "2026-09-01T00:00:00+00:00");
    assert_eq!(
        r.text,
        "# Plan\n\nIntro inserted & more\n\n  - item\n\n| Name | Owner |\n| --- | --- |\n| Task | 佐藤 |\n\nEnd\n\n---\n\n[^1]: A note"
    );
}

const A: &str = "xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:p=\"http://schemas.openxmlformats.org/presentationml/2006/main\"";

#[test]
fn pptx_slides_titles_notes_and_skipped_placeholders() {
    let s1 = format!(
        r#"<p:sld {A}><p:cSld><p:spTree>
<p:sp><p:nvSpPr><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>Kickoff</a:t></a:r></a:p></p:txBody></p:sp>
<p:sp><p:txBody><a:p><a:r><a:t>Goal one</a:t></a:r></a:p><a:p><a:r><a:t>Goal two</a:t></a:r></a:p></p:txBody></p:sp>
<p:sp><p:nvSpPr><p:nvPr><p:ph type="sldNum"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>1</a:t></a:r></a:p></p:txBody></p:sp>
</p:spTree></p:cSld></p:sld>"#
    );
    let s2 = format!(
        r#"<p:sld {A}><p:cSld><p:spTree><p:sp><p:txBody><a:p><a:r><a:t>Second</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#
    );
    let notes = format!(
        r#"<p:notes {A}><p:cSld><p:spTree>
<p:sp><p:nvSpPr><p:nvPr><p:ph type="sldImg"/></p:nvPr></p:nvSpPr></p:sp>
<p:sp><p:nvSpPr><p:nvPr><p:ph type="body"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>Say this aloud</a:t></a:r></a:p></p:txBody></p:sp>
</p:spTree></p:cSld></p:notes>"#
    );
    let rels = r#"<Relationships xmlns="r"><Relationship Id="r1" Type="http://x/relationships/notesSlide" Target="../notesSlides/notesSlide1.xml"/></Relationships>"#;
    let bytes = zip_of(&[
        ("ppt/presentation.xml", "<p/>"),
        ("ppt/slides/slide2.xml", &s2),
        ("ppt/slides/slide1.xml", &s1),
        ("ppt/slides/_rels/slide1.xml.rels", rels),
        ("ppt/notesSlides/notesSlide1.xml", &notes),
    ]);
    let r = run(&bytes, "deck.pptx").unwrap();
    assert_eq!(r.stats.slide_count, Some(2));
    assert_eq!(
        r.text,
        "## Slide 1: Kickoff\n\nGoal one\nGoal two\n\nNotes:\n\nSay this aloud\n\n## Slide 2\n\nSecond"
    );
}

#[test]
fn xlsx_visible_sheets_as_tables() {
    let wb = r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets>
<sheet name="Budget" sheetId="1" r:id="rId1"/><sheet name="Hidden" sheetId="2" state="hidden" r:id="rId2"/></sheets></workbook>"#;
    let rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet2.xml"/></Relationships>"#;
    let s1 = r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>
<row r="1"><c r="A1" t="inlineStr"><is><t>Item</t></is></c><c r="B1" t="inlineStr"><is><t>Cost</t></is></c></row>
<row r="2"><c r="A2" t="inlineStr"><is><t>Licence</t></is></c><c r="B2"><v>1200</v></c></row></sheetData></worksheet>"#;
    let s2 = r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>secret</t></is></c></row></sheetData></worksheet>"#;
    let ct = r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/></Types>"#;
    let root_rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#;
    let bytes = zip_of(&[
        ("[Content_Types].xml", ct),
        ("_rels/.rels", root_rels),
        ("xl/workbook.xml", wb),
        ("xl/_rels/workbook.xml.rels", rels),
        ("xl/worksheets/sheet1.xml", s1),
        ("xl/worksheets/sheet2.xml", s2),
    ]);
    let r = run(&bytes, "budget.xlsx").unwrap();
    assert_eq!(
        r.text,
        "## Sheet: Budget\n\n| Item | Cost |\n| --- | --- |\n| Licence | 1200 |"
    );
    assert_eq!(r.stats.sheet_names, Some(vec!["Budget".to_string()]));
}

/// A one-page PDF with Helvetica text, built with correct xref offsets.
fn pdf_with(text: &str) -> Vec<u8> {
    let content = format!("BT /F1 18 Tf 72 700 Td ({text}) Tj ET");
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_string(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_string(),
    ];
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offs = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offs.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offs {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objs.len() + 1
        )
        .as_bytes(),
    );
    out
}

#[test]
fn pdf_text_layer() {
    let r = run(&pdf_with("Hello Budget Review"), "a.pdf").unwrap();
    assert!(r.text.contains("Hello Budget Review"), "{:?}", r.text);
    assert_eq!(r.stats.page_count, Some(1));
}

#[test]
fn damaged_pdf_is_an_error_not_a_panic() {
    let mut b = pdf_with("Hello");
    b.truncate(b.len() / 2);
    let r = extract_bounded(
        &ExtractInput {
            bytes: &b,
            media_type: None,
            file_name: Some("a.pdf"),
        },
        &ExtractLimits::default(),
    );
    assert!(r.is_err(), "{r:?}");
    let g = extract_bounded(
        &ExtractInput {
            bytes: b"%PDF-1.4 garbage garbage",
            media_type: None,
            file_name: None,
        },
        &ExtractLimits::default(),
    );
    assert!(g.is_err());
}

#[test]
fn zip_bomb_and_member_limits() {
    let big = "0".repeat(5 * 1024 * 1024);
    let bytes = zip_of(&[("word/document.xml", &big)]);
    let limits = ExtractLimits {
        max_decompressed_bytes: 1024 * 1024,
        ..Default::default()
    };
    let r = extract(
        &ExtractInput {
            bytes: &bytes,
            media_type: None,
            file_name: Some("a.docx"),
        },
        &limits,
    );
    assert!(matches!(r, Err(ExtractError::TooLarge(_))), "{r:?}");

    let many: Vec<(String, String)> = (0..20).map(|i| (format!("f{i}.txt"), "x".into())).collect();
    let mut parts: Vec<(&str, &str)> = many.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    parts.push(("word/document.xml", "<w:document/>"));
    let bytes = zip_of(&parts);
    let limits = ExtractLimits {
        max_entries: 5,
        ..Default::default()
    };
    let r = extract(
        &ExtractInput {
            bytes: &bytes,
            media_type: None,
            file_name: Some("a.docx"),
        },
        &limits,
    );
    assert!(matches!(r, Err(ExtractError::TooLarge(_))), "{r:?}");
}

#[test]
fn corrupt_zip_and_empty_document() {
    assert!(matches!(
        run(b"PK\x03\x04word/document.xml garbage", "a.docx"),
        Err(ExtractError::Corrupt(_))
    ));
    let bytes = zip_of(&[(
        "word/document.xml",
        &format!("<w:document {W}><w:body/></w:document>"),
    )]);
    assert_eq!(run(&bytes, "a.docx"), Err(ExtractError::Empty));
}

#[test]
fn bounded_extraction_times_out_by_deadline() {
    // A deadline of zero cannot be met by any real extraction.
    let limits = ExtractLimits {
        timeout: std::time::Duration::from_nanos(1),
        ..Default::default()
    };
    let big = "word ".repeat(2_000_000);
    let r = extract_bounded(
        &ExtractInput {
            bytes: big.as_bytes(),
            media_type: None,
            file_name: Some("a.txt"),
        },
        &limits,
    );
    assert!(matches!(r, Err(ExtractError::Timeout(_))), "{r:?}");
}
