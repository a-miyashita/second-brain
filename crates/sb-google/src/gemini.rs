//! Deterministic, heading-based parser for Gemini "Take notes for me"
//! documents exported as Markdown (source-google-meet.md "Normalization").

use std::sync::LazyLock;

use regex::Regex;
use sb_core::{SectionDraft, SectionKind, SectionOrigin};
use serde::Serialize;

macro_rules! re {
    ($name:ident, $pat:expr) => {
        static $name: LazyLock<Regex> = LazyLock::new(|| {
            #[allow(clippy::unwrap_used)] // A constant pattern; covered by tests.
            Regex::new($pat).unwrap()
        });
    };
}

re!(RE_TRANSCRIPT_HEADING, r"^#\s+(?:\*\*)?\s*(?:📖|🎞)");
re!(RE_ANCHOR, r"\[(\d{1,2}:\d{2}(?::\d{2})?)\]\(#[^)]*\)");
re!(RE_ESCAPE, r"\\([\[\]\-_*#.()!+])");
re!(RE_BLANKS, r"\n{3,}");
re!(RE_MAILTO, r"\[([^\]]+)\]\(mailto:([^)\s]+)\)");
re!(RE_LINK_URL, r"\((https?://[^)\s]+)\)");
re!(
    RE_AUTO_TITLE,
    r"(?:に開始した会議|^会議\s*[0-9０-９]{4}\s*年|^Meeting started|^Meeting\s+[0-9]{4}[/-][0-9]{1,2}[/-][0-9]{1,2})"
);
re!(
    RE_TRAILING_DATE,
    r"\s*[-–—]?\s*[0-9０-９]{4}\s*[/／年.\-][0-9０-９]{1,2}.*$"
);
re!(RE_RECURRING, r"(?i)\s*\((?:recurring|定期)\)\s*$");

/// A participant parsed from the "Invited" line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Person {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

/// Parsed notes.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedNotes {
    /// Title from the `##` heading (may be auto-generated).
    pub title: Option<String>,
    pub title_is_auto: bool,
    pub recurring_in_title: bool,
    pub sections: Vec<SectionDraft>,
    pub participants: Vec<Person>,
    pub absentees: Vec<Person>,
    pub calendar_url: Option<String>,
    pub transcript_url: Option<String>,
    /// The notes part (before the embedded transcript), cleaned.
    pub notes_text: String,
    /// The embedded transcript, cleaned.
    pub transcript: Option<String>,
}

/// Mechanical cleanup applied to every section and the transcript.
pub fn clean(text: &str) -> String {
    let t = RE_ANCHOR.replace_all(text, "$1");
    let t = RE_ESCAPE.replace_all(&t, "$1");
    let t = t.replace("\r\n", "\n");
    let t = RE_BLANKS.replace_all(&t, "\n\n");
    t.trim().to_string()
}

fn heading_text(line: &str) -> Option<(usize, String)> {
    let trimmed = line.trim_start();
    let level = trimmed.chars().take_while(|c| *c == '#').count();
    if level == 0 || !trimmed[level..].starts_with(' ') {
        return None;
    }
    let text = trimmed[level..].trim().trim_matches('*').trim().to_string();
    Some((level, text))
}

fn section_kind(label: &str) -> Option<SectionKind> {
    let l = RE_ESCAPE.replace_all(label, "$1").trim().to_string();
    match l.as_str() {
        "概要" | "Summary" => Some(SectionKind::Overview),
        "決定事項" | "Decisions" => Some(SectionKind::Decisions),
        "次のステップ" | "推奨される次のステップ" | "Suggested next steps" | "Next steps" => {
            Some(SectionKind::ActionItems)
        }
        "詳細" | "Details" => Some(SectionKind::Details),
        _ => None,
    }
}

fn is_survey_line(line: &str) -> bool {
    let t = line.trim().trim_start_matches(['*', '_', ' ']);
    t.starts_with("これらのメモ")
        || t.starts_with("How did we do")
        || t.starts_with("How is the quality")
}

/// Parse participants from the "Invited" line. Struck-through links are absentees.
fn parse_invited(line: &str) -> (Vec<Person>, Vec<Person>) {
    let mut present = Vec::new();
    let mut absent = Vec::new();
    for (i, part) in line.split("~~").enumerate() {
        let struck = i % 2 == 1;
        for c in RE_MAILTO.captures_iter(part) {
            let p = Person {
                name: clean(&c[1]),
                email: Some(c[2].to_string()),
            };
            if struck {
                absent.push(p)
            } else {
                present.push(p)
            }
        }
    }
    (present, absent)
}

/// Whether a title was auto-generated because Gemini did not know the meeting name.
pub fn is_auto_title(title: &str) -> bool {
    RE_AUTO_TITLE.is_match(title.trim())
}

/// A folder name with its trailing date part removed.
pub fn strip_trailing_date(name: &str) -> String {
    RE_TRAILING_DATE.replace(name.trim(), "").trim().to_string()
}

/// Strip a "(recurring)" suffix; returns the title and whether it was present.
pub fn strip_recurring(title: &str) -> (String, bool) {
    let stripped = RE_RECURRING.replace(title, "").trim().to_string();
    let had = stripped != title.trim();
    (stripped, had)
}

/// Parse a notes document. Returns `None` when it has none of the known
/// section headings (not a Gemini notes document).
pub fn parse(markdown: &str) -> Option<ParsedNotes> {
    let lines: Vec<&str> = markdown.lines().collect();
    let split = lines
        .iter()
        .position(|l| RE_TRANSCRIPT_HEADING.is_match(l.trim_start()));
    let (notes_lines, transcript) = match split {
        Some(i) => (&lines[..i], Some(clean(&lines[i + 1..].join("\n")))),
        None => (&lines[..], None),
    };
    let first_section = notes_lines.iter().position(|l| {
        heading_text(l)
            .and_then(|(_, t)| section_kind(&t))
            .is_some()
    })?;

    let header = &notes_lines[..first_section];
    let mut title = None;
    let mut participants = Vec::new();
    let mut absentees = Vec::new();
    let mut calendar_url = None;
    let mut transcript_url = None;
    for l in header {
        if title.is_none()
            && let Some((2, t)) = heading_text(l)
        {
            title = Some(clean(&t).trim_matches('*').trim().to_string());
            continue;
        }
        if l.contains("招待済み") || l.trim_start().starts_with("Invited") {
            let (p, a) = parse_invited(l);
            participants.extend(p);
            absentees.extend(a);
            continue;
        }
        for c in RE_LINK_URL.captures_iter(l) {
            let url = c[1].to_string();
            if url.contains("calendar.google.com") && calendar_url.is_none() {
                calendar_url = Some(url);
            } else if url.contains("docs.google.com") && transcript_url.is_none() {
                transcript_url = Some(url);
            }
        }
    }

    let mut sections: Vec<SectionDraft> = Vec::new();
    let mut current: Option<(SectionKind, Vec<String>)> = None;
    let flush = |cur: &mut Option<(SectionKind, Vec<String>)>, out: &mut Vec<SectionDraft>| {
        if let Some((kind, body)) = cur.take() {
            let text = clean(&body.join("\n"));
            if !text.is_empty() && !out.iter().any(|s| s.kind == kind) {
                out.push(SectionDraft {
                    kind,
                    origin: SectionOrigin::Generated,
                    text,
                });
            }
        }
    };
    for l in &notes_lines[first_section..] {
        if let Some((level, t)) = heading_text(l) {
            if let Some(kind) = section_kind(&t) {
                flush(&mut current, &mut sections);
                current = Some((kind, Vec::new()));
                continue;
            }
            if level == 1 {
                // A top-level heading that is not a known section ends the
                // current section.
                flush(&mut current, &mut sections);
                continue;
            }
        }
        if is_survey_line(l) {
            continue;
        }
        if let Some((_, body)) = current.as_mut() {
            // Demote `##` sub-headings inside sections.
            let line = match l.strip_prefix("## ") {
                Some(rest) => format!("### {rest}"),
                None => l.to_string(),
            };
            body.push(line);
        }
    }
    flush(&mut current, &mut sections);

    let (title, recurring_in_title) = match title {
        Some(t) => {
            let (t, r) = strip_recurring(&t);
            (Some(t), r)
        }
        None => (None, false),
    };
    Some(ParsedNotes {
        title_is_auto: title.as_deref().is_none_or(is_auto_title),
        title,
        recurring_in_title,
        sections,
        participants,
        absentees,
        calendar_url,
        transcript_url,
        notes_text: clean(&notes_lines.join("\n")),
        transcript: transcript.filter(|t| !t.is_empty()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic notes document following the observed export format.
    pub const NOTES_JA: &str = "# 📝 メモ\n\n2026年9月3日\n\n## **Daily Dev Standup (recurring)**\n\n\
招待済み [Alice Example](mailto:alice@example.test) ~~[Bob Example](mailto:bob@example.test)~~ [Carol Example](mailto:carol@example.test)\n\n\
添付ファイル [Daily Dev Standup](https://calendar.google.com/calendar/event?eid=abc123)\n\n\
会議の録音 [文字起こし](https://docs.google.com/document/d/TRANSCRIPT_ID/edit?usp=meet_tnfm_calendar)\n\n\
### **概要**\n\nチームは CSV エクスポートの形式について議論した\\.\n\n\n\n\
### **決定事項**\n\n## **調整済み**\n\n* CSV 形式で進めることに決定した\n\n\
### **次のステップ**\n\n* \\[Alice Example\\] 仕様書を更新する\n\n\
### **詳細**\n\n* **CSV の検討**: Alice が形式を提案した ([00:12:34](#heading=h.x1))\n\n\
*これらのメモはいかがでしたか？ [簡単なアンケート](https://example.test/survey)にご協力ください。*\n\n\
# **📖 文字起こし**\n\n2026年9月3日\n\n### 00:00:00\n\n**Alice Example:** おはようございます\\.\n";

    #[test]
    fn parses_sections_participants_and_transcript() {
        let p = parse(NOTES_JA).unwrap();
        assert_eq!(p.title.as_deref(), Some("Daily Dev Standup"));
        assert!(p.recurring_in_title);
        assert!(!p.title_is_auto);
        let kinds: Vec<SectionKind> = p.sections.iter().map(|s| s.kind).collect();
        assert_eq!(
            kinds,
            vec![
                SectionKind::Overview,
                SectionKind::Decisions,
                SectionKind::ActionItems,
                SectionKind::Details
            ]
        );
        assert_eq!(
            p.sections[0].text,
            "チームは CSV エクスポートの形式について議論した."
        );
        assert_eq!(
            p.sections[1].text,
            "### **調整済み**\n\n* CSV 形式で進めることに決定した"
        );
        assert_eq!(p.sections[2].text, "* [Alice Example] 仕様書を更新する");
        assert_eq!(
            p.sections[3].text,
            "* **CSV の検討**: Alice が形式を提案した (00:12:34)"
        );
        assert_eq!(
            p.participants
                .iter()
                .map(|x| x.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Alice Example", "Carol Example"]
        );
        assert_eq!(p.absentees[0].email.as_deref(), Some("bob@example.test"));
        assert_eq!(
            p.calendar_url.as_deref(),
            Some("https://calendar.google.com/calendar/event?eid=abc123")
        );
        assert!(p.transcript_url.unwrap().contains("TRANSCRIPT_ID"));
        let t = p.transcript.unwrap();
        assert!(t.contains("**Alice Example:** おはようございます."));
        assert!(!p.notes_text.contains("文字起こし\n\n2026"));
    }

    #[test]
    fn english_headings_and_not_applicable() {
        let en = "## Weekly sync\n\nInvited [Dana](mailto:dana@example.test)\n\n### Summary\n\nShort.\n\n### Suggested next steps\n\n* Dana will file the ticket\n\nHow did we do with these notes?\n";
        let p = parse(en).unwrap();
        assert_eq!(p.sections.len(), 2);
        assert_eq!(p.sections[1].kind, SectionKind::ActionItems);
        assert_eq!(p.sections[1].text, "* Dana will file the ticket");
        assert!(parse("# Agenda\n\n- item one\n- item two\n").is_none());
    }

    #[test]
    fn auto_titles_and_folder_names() {
        assert!(is_auto_title("2026／07／27 09：15 JST に開始した会議"));
        assert!(is_auto_title("会議 2026年7月27日 09:15 JST"));
        assert!(is_auto_title("Meeting started 2026/07/27 09:15 JST"));
        assert!(!is_auto_title("Daily Dev Standup"));
        assert_eq!(
            strip_trailing_date("Daily Dev Standup - 2026/07/27 09:15 JST"),
            "Daily Dev Standup"
        );
        assert_eq!(strip_trailing_date("定例会 2026年7月27日"), "定例会");
        assert_eq!(
            strip_recurring("Standup (Recurring)"),
            ("Standup".to_string(), true)
        );
    }
}
