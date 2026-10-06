//! HTML main-content extraction and Markdown rendering (extract.md).

use std::collections::HashMap;

use ego_tree::NodeId;
use regex::Regex;
use scraper::node::Node;
use scraper::{ElementRef, Html, Selector};

use crate::text::decode;
use crate::{ExtractError, ExtractInput, ExtractLimits, Extracted, md_table, parse_time};

const SKIP: &[&str] = &[
    "script", "style", "noscript", "template", "iframe", "svg", "form", "nav", "footer", "aside",
    "head", "button", "select", "textarea", "canvas", "object", "embed", "dialog",
];
const SKIP_ROLES: &[&str] = &["navigation", "banner", "contentinfo", "complementary"];
const BLOCKS: &[&str] = &[
    "p",
    "div",
    "section",
    "article",
    "main",
    "figure",
    "figcaption",
    "details",
    "summary",
    "dl",
    "dd",
    "dt",
    "address",
    "fieldset",
    "header",
    "body",
    "center",
    "td",
    "th",
    "tr",
    "tbody",
    "thead",
    "tfoot",
    "caption",
    "html",
];
const MAX_DEPTH: usize = 256;

pub(crate) fn extract(
    input: &ExtractInput<'_>,
    _limits: &ExtractLimits,
) -> Result<Extracted, ExtractError> {
    let (src, warn) = decode(input.bytes, input.media_type, true);
    let doc = Html::parse_document(&src);
    let root = main_content(&doc);
    let mut w = Writer::default();
    w.walk(root, 0);
    w.flush();
    let mut out = Extracted {
        text: w.out,
        warnings: warn.into_iter().collect(),
        ..Default::default()
    };
    out.title = title(&doc);
    let (created, modified) = dates(&doc, &src, root);
    out.created = created;
    out.modified = modified;
    if let Some(s) = meta(&doc, &["og:site_name"]) {
        out.meta.insert("site_name".into(), s);
    }
    if let Some(c) = canonical(&doc) {
        out.meta.insert("canonical_url".into(), c);
    }
    Ok(out)
}

fn sel(s: &str) -> Selector {
    // The selectors in this file are constants, so a failure is a programming error
    // that the tests below would catch.
    Selector::parse(s).unwrap_or_else(|_| Selector::parse("*").unwrap_or_else(|_| unreachable!()))
}

fn skipped(el: &ElementRef<'_>) -> bool {
    let n = el.value().name();
    if SKIP.contains(&n) {
        return true;
    }
    if el.value().attr("hidden").is_some() || el.value().attr("aria-hidden") == Some("true") {
        return true;
    }
    if let Some(r) = el.value().attr("role")
        && SKIP_ROLES.contains(&r)
    {
        return true;
    }
    if n == "header" {
        // A header is page chrome unless it sits inside the article.
        return !el
            .ancestors()
            .filter_map(ElementRef::wrap)
            .any(|a| matches!(a.value().name(), "article" | "main"));
    }
    false
}

/// Length of the visible text of an element, and the part inside links.
fn text_len(el: ElementRef<'_>) -> (usize, usize) {
    fn go(el: ElementRef<'_>, in_link: bool, acc: &mut (usize, usize), depth: usize) {
        if depth > MAX_DEPTH || skipped(&el) {
            return;
        }
        let link = in_link || el.value().name() == "a";
        for c in el.children() {
            match c.value() {
                Node::Text(t) => {
                    let n = t
                        .split_whitespace()
                        .map(|w| w.chars().count())
                        .sum::<usize>();
                    acc.0 += n;
                    if link {
                        acc.1 += n;
                    }
                }
                Node::Element(_) => {
                    if let Some(e) = ElementRef::wrap(c) {
                        go(e, link, acc, depth + 1);
                    }
                }
                _ => {}
            }
        }
    }
    let mut acc = (0, 0);
    go(el, false, &mut acc, 0);
    acc
}

fn main_content(doc: &Html) -> ElementRef<'_> {
    let body = doc
        .select(&sel("body"))
        .next()
        .unwrap_or_else(|| doc.root_element());
    let body_len = text_len(body).0;
    let mut best: Option<(usize, ElementRef<'_>)> = None;
    for e in doc.select(&sel("article, main, [role=main]")) {
        if skipped(&e) {
            continue;
        }
        let l = text_len(e).0;
        if best.as_ref().is_none_or(|(b, _)| l > *b) {
            best = Some((l, e));
        }
    }
    if let Some((l, e)) = best
        && (l >= 500 || l * 4 >= body_len)
    {
        return e;
    }
    // Readability-style scoring of the parents of paragraphs.
    let mut scores: HashMap<NodeId, f64> = HashMap::new();
    for p in doc.select(&sel("p, li, pre, blockquote")) {
        if p.ancestors()
            .filter_map(ElementRef::wrap)
            .any(|a| skipped(&a))
            || skipped(&p)
        {
            continue;
        }
        let (l, link) = text_len(p);
        if l < 25 {
            continue;
        }
        let s = 1.0 + (l as f64 / 100.0).min(3.0) - (link as f64 / l as f64);
        let mut anc = p.ancestors().filter_map(ElementRef::wrap);
        if let Some(parent) = anc.next() {
            *scores.entry(parent.id()).or_default() += s;
            if let Some(grand) = anc.next() {
                *scores.entry(grand.id()).or_default() += s / 2.0;
            }
        }
    }
    let mut ranked: Vec<(NodeId, f64)> = scores.into_iter().collect();
    // Highest score first; node ids break ties so the result is deterministic.
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    for (id, _) in ranked {
        if let Some(e) = doc.tree.get(id).and_then(ElementRef::wrap) {
            let (l, link) = text_len(e);
            if l > 0 && (link as f64 / l as f64) < 0.5 {
                return e;
            }
        }
    }
    body
}

fn meta(doc: &Html, names: &[&str]) -> Option<String> {
    for n in names {
        let q = format!("meta[property=\"{n}\"], meta[name=\"{n}\"], meta[itemprop=\"{n}\"]");
        if let Some(c) = doc
            .select(&sel(&q))
            .filter_map(|m| m.value().attr("content"))
            .map(str::trim)
            .find(|c| !c.is_empty())
        {
            return Some(c.to_string());
        }
    }
    None
}

fn canonical(doc: &Html) -> Option<String> {
    doc.select(&sel("link[rel=\"canonical\"]"))
        .filter_map(|l| l.value().attr("href"))
        .map(str::trim)
        .find(|h| h.starts_with("http"))
        .map(str::to_string)
}

fn title(doc: &Html) -> Option<String> {
    if let Some(t) = meta(doc, &["og:title"]) {
        return Some(t);
    }
    if let Some(t) = doc.select(&sel("title")).next() {
        let t: String = t.text().collect();
        if !t.trim().is_empty() {
            return Some(t);
        }
    }
    doc.select(&sel("h1")).next().map(|h| h.text().collect())
}

fn dates(
    doc: &Html,
    src: &str,
    root: ElementRef<'_>,
) -> (
    Option<chrono::DateTime<chrono::Utc>>,
    Option<chrono::DateTime<chrono::Utc>>,
) {
    let mut created = meta(doc, &["article:published_time", "datePublished", "date"])
        .and_then(|s| parse_time(&s));
    let mut modified =
        meta(doc, &["article:modified_time", "dateModified"]).and_then(|s| parse_time(&s));
    if created.is_none() || modified.is_none() {
        let ld = |key: &str| {
            Regex::new(&format!(r#""{key}"\s*:\s*"([^"]+)""#))
                .ok()
                .and_then(|re| re.captures(src).and_then(|c| parse_time(&c[1])))
        };
        created = created.or_else(|| ld("datePublished"));
        modified = modified.or_else(|| ld("dateModified"));
    }
    if created.is_none() {
        created = root
            .select(&sel("time[datetime]"))
            .filter_map(|t| t.value().attr("datetime"))
            .find_map(parse_time);
    }
    (created, modified)
}

#[derive(Default)]
struct Writer {
    out: String,
    cur: String,
    pending_prefix: Option<String>,
    lists: Vec<(bool, usize)>,
    quote: usize,
}

impl Writer {
    fn push_text(&mut self, t: &str) {
        let mut last_space = self.cur.is_empty() || self.cur.ends_with([' ', '\n']);
        for ch in t.chars() {
            if ch.is_whitespace() {
                if !last_space {
                    self.cur.push(' ');
                    last_space = true;
                }
            } else {
                self.cur.push(ch);
                last_space = false;
            }
        }
    }

    fn flush(&mut self) {
        let text = self.cur.trim().to_string();
        self.cur.clear();
        let prefix = self.pending_prefix.take();
        if text.is_empty() {
            return;
        }
        let q = "> ".repeat(self.quote);
        for (i, line) in text.split('\n').enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            self.out.push_str(&q);
            if i == 0
                && let Some(p) = &prefix
            {
                self.out.push_str(p);
            }
            self.out.push_str(line);
            self.out.push('\n');
        }
        if prefix.is_none() {
            self.out.push('\n');
        }
    }

    fn block_text(&mut self, text: &str) {
        self.flush();
        for l in text.lines() {
            self.out.push_str(&"> ".repeat(self.quote));
            self.out.push_str(l);
            self.out.push('\n');
        }
        self.out.push('\n');
    }

    fn walk(&mut self, el: ElementRef<'_>, depth: usize) {
        if depth > MAX_DEPTH || skipped(&el) {
            return;
        }
        let name = el.value().name();
        match name {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.flush();
                self.children(el, depth);
                let n = name[1..].parse::<usize>().unwrap_or(1);
                self.pending_prefix = Some(format!("{} ", "#".repeat(n)));
                self.flush();
                self.out.push('\n');
            }
            "br" => self.cur.push('\n'),
            "hr" => {
                self.flush();
                self.out.push_str("---\n\n");
            }
            "ul" | "ol" => {
                self.flush();
                self.lists.push((name == "ol", 0));
                self.children(el, depth);
                self.flush();
                self.lists.pop();
                if self.lists.is_empty() {
                    self.out.push('\n');
                }
            }
            "li" => {
                self.flush();
                let indent = "  ".repeat(self.lists.len().saturating_sub(1).min(6));
                let marker = match self.lists.last_mut() {
                    Some((true, n)) => {
                        *n += 1;
                        format!("{n}. ")
                    }
                    _ => "- ".to_string(),
                };
                self.pending_prefix = Some(format!("{indent}{marker}"));
                self.children(el, depth);
                self.flush();
            }
            "pre" => {
                let t: String = el.text().collect();
                self.block_text(&format!("```\n{}\n```", t.trim_matches('\n')));
            }
            "blockquote" => {
                self.flush();
                self.quote += 1;
                self.children(el, depth);
                self.flush();
                self.quote -= 1;
            }
            "table" => self.table(el, depth),
            "a" => {
                let mut inner = String::new();
                for t in el.text() {
                    inner.push_str(t);
                }
                let inner = inner.split_whitespace().collect::<Vec<_>>().join(" ");
                match el.value().attr("href") {
                    Some(h)
                        if !inner.is_empty()
                            && (h.starts_with("http://") || h.starts_with("https://")) =>
                    {
                        self.push_text(&format!("[{inner}]({h})"));
                    }
                    _ => self.push_text(&inner),
                }
            }
            "img" => {
                if let Some(alt) = el.value().attr("alt")
                    && !alt.trim().is_empty()
                {
                    self.push_text(alt);
                }
            }
            n if BLOCKS.contains(&n) => {
                self.flush();
                self.children(el, depth);
                self.flush();
            }
            _ => self.children(el, depth),
        }
    }

    fn children(&mut self, el: ElementRef<'_>, depth: usize) {
        for c in el.children() {
            match c.value() {
                Node::Text(t) => self.push_text(t),
                Node::Element(_) => {
                    if let Some(e) = ElementRef::wrap(c) {
                        self.walk(e, depth + 1);
                    }
                }
                _ => {}
            }
        }
    }

    /// A data table becomes a Markdown table. A layout table (one that holds
    /// blocks, lists or other tables) is walked as plain containers.
    fn table(&mut self, el: ElementRef<'_>, depth: usize) {
        let layout = el
            .descendants()
            .skip(1)
            .filter_map(ElementRef::wrap)
            .any(|d| {
                matches!(
                    d.value().name(),
                    "table"
                        | "p"
                        | "div"
                        | "ul"
                        | "ol"
                        | "h1"
                        | "h2"
                        | "h3"
                        | "h4"
                        | "h5"
                        | "h6"
                        | "pre"
                )
            });
        if layout {
            self.flush();
            self.children(el, depth);
            self.flush();
            return;
        }
        let mut rows: Vec<Vec<String>> = Vec::new();
        for tr in el
            .descendants()
            .filter_map(ElementRef::wrap)
            .filter(|e| e.value().name() == "tr")
        {
            let row: Vec<String> = tr
                .children()
                .filter_map(ElementRef::wrap)
                .filter(|c| matches!(c.value().name(), "td" | "th"))
                .map(|c| {
                    c.text()
                        .collect::<String>()
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .collect();
            if row.iter().any(|c| !c.is_empty()) {
                rows.push(row);
            }
        }
        self.flush();
        if !rows.is_empty() {
            self.out.push_str(&md_table(&rows));
            self.out.push('\n');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(html: &str) -> Extracted {
        extract(
            &ExtractInput {
                bytes: html.as_bytes(),
                media_type: Some("text/html"),
                file_name: None,
            },
            &ExtractLimits::default(),
        )
        .unwrap()
    }

    #[test]
    fn selectors_are_valid() {
        for s in [
            "body",
            "article, main, [role=main]",
            "p, li, pre, blockquote",
            "title",
            "h1",
            "time[datetime]",
            "link[rel=\"canonical\"]",
        ] {
            assert!(Selector::parse(s).is_ok(), "{s}");
        }
    }

    #[test]
    fn article_page_drops_chrome() {
        let r = run(r#"<html><head><title>T - Site</title>
            <meta property="og:title" content="Real Title">
            <meta property="article:published_time" content="2026-09-01T10:00:00+09:00">
            <script>var x=1;</script></head>
            <body><nav><a href="/a">Home</a><a href="/b">About</a></nav>
            <article><h1>Heading</h1><p>First paragraph with <a href="https://e.test/x">a link</a>.</p>
            <ul><li>one</li><li>two<ul><li>nested</li></ul></li></ul>
            <pre>code
  here</pre></article><footer>copyright</footer></body></html>"#);
        assert_eq!(r.title.as_deref(), Some("Real Title"));
        assert!(
            r.text
                .starts_with("# Heading\n\nFirst paragraph with [a link](https://e.test/x).")
        );
        assert!(r.text.contains("- one\n- two\n  - nested"));
        assert!(r.text.contains("```\ncode\n  here\n```"));
        assert!(!r.text.contains("Home"));
        assert!(!r.text.contains("copyright"));
        assert!(!r.text.contains("var x"));
        assert_eq!(
            r.created.map(|d| d.to_rfc3339()),
            Some("2026-09-01T01:00:00+00:00".into())
        );
    }

    #[test]
    fn scores_a_page_without_article() {
        let r = run(
            r#"<body><div id="menu"><a href="/1">One</a> <a href="/2">Two</a></div>
            <div id="content"><p>This is the long first paragraph of the page body, with enough text to count.</p>
            <p>And a second paragraph that is also long enough to be part of the main content here.</p></div>
            <div id="side"><a href="/x">related link</a></div></body>"#,
        );
        assert!(r.text.contains("long first paragraph"));
        assert!(r.text.contains("second paragraph"));
        assert!(!r.text.contains("related link"));
    }

    #[test]
    fn data_table_and_layout_table() {
        let r = run(
            "<body><article><table><tr><th>a</th><th>b</th></tr><tr><td>1</td><td>2</td></tr></table>\
                     <p>after the table there is a paragraph of text that is long enough</p></article></body>",
        );
        assert!(r.text.contains("| a | b |\n| --- | --- |\n| 1 | 2 |"));
    }

    #[test]
    fn javascript_only_page_is_empty_after_finish() {
        let r = extract(
            &ExtractInput {
                bytes: b"<html><body><div id=root></div><script>render()</script></body></html>",
                media_type: Some("text/html"),
                file_name: None,
            },
            &ExtractLimits::default(),
        )
        .unwrap();
        assert!(r.text.trim().is_empty());
    }

    #[test]
    fn deep_nesting_does_not_overflow() {
        let html = format!(
            "<body>{}x{}</body>",
            "<div>".repeat(5000),
            "</div>".repeat(5000)
        );
        let _ = run(&html);
    }
}
