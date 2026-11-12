//! Document links in Slack messages and the excerpt around them
//! (docs/specs/link-ingest.md "Finding links", ADR-0022, ADR-0023).
//!
//! Pure: the links are classified by URL alone and nothing is requested.

use std::collections::HashSet;
use std::sync::LazyLock;

use chrono_tz::Tz;
use regex::Regex;
use second_brain_kernel::links::{DiscoveredLink, classify_link};
use serde_json::Value;

use crate::render::{Directory, render_body, speaker, ts_to_time};

/// Replaces a link in the excerpt: a sharing URL can be a credential.
pub const LINK_PLACEHOLDER: &str = "[linked document]";
/// A message is cut at this many characters in the excerpt.
const MAX_MESSAGE_CHARS: usize = 600;
/// Below this many remaining characters a message is left out, not cut.
const MIN_USEFUL_CHARS: usize = 80;

static RE_WRAPPED: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // A constant pattern; covered by tests.
    Regex::new(r"<(https?://[^|>\s]+)(?:\|[^>]*)?>").unwrap()
});
static RE_BARE: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // A constant pattern; covered by tests.
    Regex::new(r"https?://[^\s<>|]+").unwrap()
});

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

fn push_unique(out: &mut Vec<String>, url: String) {
    if !out.contains(&url) {
        out.push(url);
    }
}

fn collect_block_links(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(o) => {
            if o.get("type").and_then(Value::as_str) == Some("link")
                && let Some(u) = o.get("url").and_then(Value::as_str)
            {
                push_unique(out, unescape(u));
            }
            for x in o.values() {
                collect_block_links(x, out);
            }
        }
        Value::Array(a) => a.iter().for_each(|x| collect_block_links(x, out)),
        _ => {}
    }
}

/// Every URL in a message, in order of appearance, without duplicates: mrkdwn
/// `<url|label>` and `<url>`, bare URLs, `link` elements of blocks, and the
/// attachment fields `title_link`, `from_url` and `original_url`.
pub fn message_urls(m: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let text = m.get("text").and_then(Value::as_str).unwrap_or("");
    for c in RE_WRAPPED.captures_iter(text) {
        push_unique(&mut out, unescape(&c[1]));
    }
    let unwrapped = RE_WRAPPED.replace_all(text, " ");
    for b in RE_BARE.find_iter(&unwrapped) {
        let u = b
            .as_str()
            .trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '}', '\'', '"']);
        push_unique(&mut out, unescape(u));
    }
    if let Some(blocks) = m.get("blocks") {
        collect_block_links(blocks, &mut out);
    }
    for a in m
        .get("attachments")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        for k in ["title_link", "from_url", "original_url"] {
            if let Some(u) = a.get(k).and_then(Value::as_str) {
                push_unique(&mut out, unescape(u));
            }
        }
    }
    out
}

fn cut(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
    t.push('…');
    t
}

/// The body of a message for the excerpt: every document link is replaced.
fn excerpt_line(m: &Value, dir: &Directory, urls: &[String]) -> String {
    let mut body = render_body(m, dir);
    for u in urls {
        body = body.replace(u.as_str(), LINK_PLACEHOLDER);
        if let Some(l) = classify_link(u) {
            body = body.replace(l.url.as_str(), LINK_PLACEHOLDER);
        }
    }
    format!(
        "{}: {}",
        speaker(m, dir),
        cut(body.trim(), MAX_MESSAGE_CHARS)
    )
}

/// The links of a stored conversation and an excerpt for each (first message
/// that holds a link wins). `messages` are in order; for a thread the first
/// one is the parent. `label` is the conversation label of the entry title.
pub fn discover(
    messages: &[Value],
    dir: &Directory,
    label: &str,
    is_thread: bool,
    zone: &Tz,
    max_chars: usize,
) -> Vec<DiscoveredLink> {
    let all_urls: Vec<Vec<String>> = messages.iter().map(message_urls).collect();
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for (i, urls) in all_urls.iter().enumerate() {
        for u in urls {
            let Some(link) = classify_link(u) else {
                continue;
            };
            if !seen.insert(link.key.clone()) {
                continue;
            }
            let excerpt = excerpt(
                messages, &all_urls, i, dir, label, is_thread, zone, max_chars,
            );
            let message_ts = messages[i]
                .get("ts")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            out.push(DiscoveredLink {
                link,
                message_ts,
                excerpt,
            });
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn excerpt(
    messages: &[Value],
    all_urls: &[Vec<String>],
    i: usize,
    dir: &Directory,
    label: &str,
    is_thread: bool,
    zone: &Tz,
    max_chars: usize,
) -> String {
    let date = messages[i]
        .get("ts")
        .and_then(Value::as_str)
        .and_then(ts_to_time)
        .map(|t| t.with_timezone(zone).format("%Y-%m-%d").to_string())
        .unwrap_or_default();
    let header = format!("{label}, {date}");
    let mut budget = max_chars.saturating_sub(header.chars().count() + 1);
    // Priority: the link message, the thread's first message, then the nearest
    // messages before the link, then the one after.
    let mut order: Vec<usize> = vec![i];
    if is_thread && i != 0 {
        order.push(0);
    }
    for j in [i.checked_sub(1), i.checked_sub(2), Some(i + 1)]
        .into_iter()
        .flatten()
    {
        if j < messages.len() && !order.contains(&j) {
            order.push(j);
        }
    }
    let mut chosen: Vec<(usize, String)> = Vec::new();
    for j in order {
        let line = excerpt_line(&messages[j], dir, &all_urls[j]);
        let need = line.chars().count() + 1;
        if need <= budget {
            budget -= need;
            chosen.push((j, line));
        } else if budget >= MIN_USEFUL_CHARS {
            chosen.push((j, cut(&line, budget.saturating_sub(1))));
            budget = 0;
        }
    }
    chosen.sort_by_key(|(j, _)| *j);
    let mut s = header;
    for (_, l) in chosen {
        s.push('\n');
        s.push_str(&l);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const DOC: &str = "https://docs.google.com/document/d/AAA/edit";
    const SP: &str = "https://contoso.sharepoint.com/:w:/s/team/Eabc?e=xyz";

    fn dir() -> Directory {
        Directory::new()
    }

    fn tz() -> Tz {
        "Asia/Tokyo".parse().unwrap()
    }

    #[test]
    fn urls_are_found_in_every_place_slack_puts_them() {
        let m = json!({
            "text": format!("see <{DOC}|the plan> and <https://example.com/a?x=1&amp;y=2> or https://bare.example/z. Also <{DOC}>"),
            "blocks": [{"type": "rich_text", "elements": [{"type": "rich_text_section", "elements": [
                {"type": "link", "url": SP},
                {"type": "text", "text": "x"}]}]}],
            "attachments": [{"title_link": "https://t.example/t", "from_url": DOC, "original_url": "https://o.example/o"}]
        });
        assert_eq!(
            message_urls(&m),
            vec![
                DOC,
                "https://example.com/a?x=1&y=2",
                "https://bare.example/z",
                SP,
                "https://t.example/t",
                "https://o.example/o",
            ]
        );
        assert!(message_urls(&json!({"text": "no links <@U1>"})).is_empty());
    }

    #[test]
    fn only_supported_links_are_discovered_once_per_document() {
        let messages = vec![
            json!({"ts": "1727740800.000100", "user": "U1", "text": format!("plan: <{DOC}|doc>")}),
            json!({"ts": "1727740860.000200", "user": "U2", "text": format!("same doc again https://drive.google.com/open?id=AAA and <{SP}> and https://example.com/page and https://x.slack.com/archives/C1/p1")}),
        ];
        let links = discover(&messages, &dir(), "#dev", true, &tz(), 1200);
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].link.key, "google:AAA");
        assert_eq!(links[0].message_ts, "1727740800.000100");
        assert_eq!(links[1].link.host, "contoso.sharepoint.com");
        assert_eq!(links[1].message_ts, "1727740860.000200");
    }

    #[test]
    fn the_excerpt_has_a_header_the_link_message_and_no_urls_of_documents() {
        let messages = vec![
            json!({"ts": "1727740800.000100", "user": "U1", "text": "Should we change the export format?"}),
            json!({"ts": "1727740860.000200", "user": "U2", "text": "Let's look at this first"}),
            json!({"ts": "1727740920.000300", "user": "U1", "text": format!("Draft: <{DOC}|Export design>")}),
            json!({"ts": "1727740980.000400", "user": "U2", "text": "Thanks"}),
        ];
        let links = discover(&messages, &dir(), "#dev", true, &tz(), 1200);
        let e = &links[0].excerpt;
        // 1727740920 = 2024-10-01T00:02:00Z, 09:02 in Tokyo.
        assert!(e.starts_with("#dev, 2024-10-01\n"), "{e}");
        assert!(
            e.contains(&format!("Export design ({LINK_PLACEHOLDER})")),
            "{e}"
        );
        assert!(!e.contains("docs.google.com"), "{e}");
        // The thread's first message, the two before, and the one after.
        for needle in ["Should we change", "Let's look", "Thanks"] {
            assert!(e.contains(needle), "{e}");
        }
        // Chronological order.
        assert!(e.find("Should we").unwrap() < e.find("Draft").unwrap());
        assert!(e.find("Draft").unwrap() < e.find("Thanks").unwrap());
    }

    #[test]
    fn a_small_limit_keeps_the_link_message_and_the_parent_first() {
        let long = "x".repeat(300);
        let messages = vec![
            json!({"ts": "1727740800.000100", "user": "U1", "text": "Parent question"}),
            json!({"ts": "1727740860.000200", "user": "U2", "text": long}),
            json!({"ts": "1727740920.000300", "user": "U1", "text": format!("<{DOC}>")}),
        ];
        let e = &discover(&messages, &dir(), "#dev", true, &tz(), 120)[0].excerpt;
        assert!(e.chars().count() <= 120, "{} {e}", e.chars().count());
        assert!(e.contains(LINK_PLACEHOLDER));
        assert!(e.contains("Parent question"));
        assert!(!e.contains("xxxx"));
    }

    #[test]
    fn a_day_entry_has_no_parent_and_the_excerpt_is_deterministic() {
        let messages = vec![
            json!({"ts": "1727740800.000100", "user": "U1", "text": "first"}),
            json!({"ts": "1727740860.000200", "user": "U1", "text": "second"}),
            json!({"ts": "1727740920.000300", "user": "U1", "text": "third"}),
            json!({"ts": "1727740980.000400", "user": "U1", "text": format!("<{DOC}>")}),
        ];
        let a = discover(&messages, &dir(), "#dev", false, &tz(), 1200);
        let b = discover(&messages, &dir(), "#dev", false, &tz(), 1200);
        assert_eq!(a, b);
        assert!(!a[0].excerpt.contains("first"), "{}", a[0].excerpt);
        assert!(a[0].excerpt.contains("second") && a[0].excerpt.contains("third"));
    }

    #[test]
    fn other_document_links_in_context_messages_are_hidden_too() {
        let messages = vec![
            json!({"ts": "1727740800.000100", "user": "U1", "text": format!("older <{SP}>")}),
            json!({"ts": "1727740860.000200", "user": "U1", "text": format!("<{DOC}>")}),
        ];
        let links = discover(&messages, &dir(), "#dev", false, &tz(), 1200);
        for l in &links {
            assert!(!l.excerpt.contains("sharepoint.com"), "{}", l.excerpt);
            assert!(!l.excerpt.contains("docs.google.com"), "{}", l.excerpt);
        }
    }
}
