//! Pure, deterministic normalization of stored Slack messages
//! (source-slack.md "Normalization").

use std::collections::{BTreeMap, HashSet};
use std::sync::LazyLock;

use chrono::{DateTime, TimeZone, Utc};
use chrono_tz::Tz;
use regex::Regex;
use sb_core::source::SourceError;
use sb_core::util::truncate_chars;
use sb_core::{
    Language, NormalizeCtx, NormalizeInput, NormalizeOutcome, Normalized, PromptKind, SectionDraft,
    SectionKind, SectionOrigin, SourceKind, SummaryInput,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// One user in the directory cache.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UserInfo {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub real_name: String,
    #[serde(default)]
    pub is_bot: bool,
    #[serde(default)]
    pub deleted: bool,
    /// Harvested from a Slack Connect message, not from `users.list`.
    #[serde(default)]
    pub external: bool,
}

impl UserInfo {
    /// Display name: real name, then display name, then handle.
    pub fn label(&self) -> Option<&str> {
        [&self.real_name, &self.display_name, &self.name]
            .into_iter()
            .map(|s| s.trim())
            .find(|s| !s.is_empty())
    }

    /// Build from a `users.list` member.
    pub fn from_member(m: &Value) -> Self {
        let s = |p: &str| {
            m.pointer(p)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        UserInfo {
            name: s("/name"),
            display_name: s("/profile/display_name"),
            real_name: {
                let r = s("/profile/real_name");
                if r.is_empty() { s("/real_name") } else { r }
            },
            is_bot: m.get("is_bot").and_then(Value::as_bool).unwrap_or(false),
            deleted: m.get("deleted").and_then(Value::as_bool).unwrap_or(false),
            external: false,
        }
    }

    /// Build from a `user_profile` object embedded in a message.
    pub fn from_profile(p: &Value) -> Option<Self> {
        let s = |k: &str| {
            p.get(k)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let u = UserInfo {
            name: s("name"),
            display_name: s("display_name"),
            real_name: {
                let r = s("real_name");
                if r.is_empty() { s("first_name") } else { r }
            },
            is_bot: false,
            deleted: false,
            external: true,
        };
        u.label().is_some().then_some(u)
    }
}

/// The user directory: Slack user ID → info.
pub type Directory = BTreeMap<String, UserInfo>;

/// Read the directory from a normalize snapshot (`{"users": {...}}`).
pub fn directory_from_snapshot(snapshot: &Value) -> Directory {
    snapshot
        .get("users")
        .and_then(|u| serde_json::from_value(u.clone()).ok())
        .unwrap_or_default()
}

fn name_of(dir: &Directory, id: &str) -> String {
    dir.get(id)
        .and_then(|u| u.label())
        .map(str::to_string)
        .unwrap_or_else(|| id.to_string())
}

static RE_ANGLE: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // A constant pattern; covered by tests.
    Regex::new(r"<([^<>]+)>").unwrap()
});
static RE_URL: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // A constant pattern; covered by tests.
    Regex::new(r"https?://\S+").unwrap()
});
static RE_EDGE_RAW_MENTION: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // A constant pattern; covered by tests.
    Regex::new(r"^(?:\s*<[@!][^>]*>)+|(?:\s*<[@!][^>]*>)+\s*$").unwrap()
});
static RE_EDGE_MENTION: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // A constant pattern; covered by tests.
    Regex::new(r"^(?:@\S+\s*)+|(?:\s*@\S+)+$").unwrap()
});

/// Render Slack mrkdwn `text`: resolve mentions and links, unescape entities.
pub fn render_text(text: &str, dir: &Directory) -> String {
    let replaced = RE_ANGLE.replace_all(text, |c: &regex::Captures<'_>| {
        let inner = &c[1];
        let (target, label) = match inner.split_once('|') {
            Some((t, l)) => (t, Some(l)),
            None => (inner, None),
        };
        if let Some(id) = target.strip_prefix('@') {
            return format!(
                "@{}",
                label
                    .map(str::to_string)
                    .unwrap_or_else(|| name_of(dir, id))
            );
        }
        if let Some(id) = target.strip_prefix('#') {
            return format!("#{}", label.unwrap_or(id));
        }
        if let Some(special) = target.strip_prefix('!') {
            let word = special.split('^').next().unwrap_or(special);
            return match (word, label) {
                (_, Some(l)) => format!("@{}", l.trim_start_matches('@')),
                ("here" | "channel" | "everyone", None) => format!("@{word}"),
                (w, None) => format!("@{w}"),
            };
        }
        if let Some(addr) = target.strip_prefix("mailto:") {
            return label.unwrap_or(addr).to_string();
        }
        match label {
            Some(l) if l != target => format!("{l} ({target})"),
            _ => target.to_string(),
        }
    });
    replaced
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// Collect text from Block Kit blocks (section, header, rich_text, context,
/// including elements, fields and accessory). Images and buttons are skipped;
/// duplicate fragments are removed.
pub fn render_blocks(blocks: &Value, dir: &Directory) -> String {
    let mut out: Vec<String> = Vec::new();
    fn text_obj(v: &Value, dir: &Directory) -> Option<String> {
        v.get("text")
            .and_then(Value::as_str)
            .map(|t| render_text(t, dir))
    }
    fn rich(el: &Value, dir: &Directory, buf: &mut String) {
        match el.get("type").and_then(Value::as_str).unwrap_or("") {
            "text" => buf.push_str(el.get("text").and_then(Value::as_str).unwrap_or("")),
            "user" => {
                let id = el.get("user_id").and_then(Value::as_str).unwrap_or("");
                buf.push('@');
                buf.push_str(&name_of(dir, id));
            }
            "channel" => {
                buf.push('@');
                buf.push_str(
                    el.get("channel_id")
                        .and_then(Value::as_str)
                        .unwrap_or("channel"),
                );
            }
            "broadcast" => {
                buf.push('@');
                buf.push_str(el.get("range").and_then(Value::as_str).unwrap_or("here"));
            }
            "usergroup" => buf.push_str("@group"),
            "link" => {
                let url = el.get("url").and_then(Value::as_str).unwrap_or("");
                match el.get("text").and_then(Value::as_str) {
                    Some(t) if !t.is_empty() && t != url => buf.push_str(&format!("{t} ({url})")),
                    _ => buf.push_str(url),
                }
            }
            "emoji" => {
                if let Some(u) = el.get("unicode").and_then(Value::as_str) {
                    let s: String = u
                        .split('-')
                        .filter_map(|h| u32::from_str_radix(h, 16).ok())
                        .filter_map(char::from_u32)
                        .collect();
                    buf.push_str(&s);
                } else if let Some(n) = el.get("name").and_then(Value::as_str) {
                    buf.push_str(&format!(":{n}:"));
                }
            }
            "rich_text_section" | "rich_text_preformatted" | "rich_text_quote" => {
                for e in el
                    .get("elements")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    rich(e, dir, buf);
                }
                buf.push('\n');
            }
            "rich_text_list" => {
                for item in el
                    .get("elements")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    buf.push_str("- ");
                    let mut inner = String::new();
                    for e in item
                        .get("elements")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                    {
                        rich(e, dir, &mut inner);
                    }
                    if inner.is_empty() {
                        rich(item, dir, &mut inner);
                    }
                    buf.push_str(inner.trim_end());
                    buf.push('\n');
                }
            }
            _ => {}
        }
    }
    fn walk(b: &Value, dir: &Directory, out: &mut Vec<String>) {
        match b.get("type").and_then(Value::as_str).unwrap_or("") {
            "section" | "header" => {
                if let Some(t) = b.get("text").and_then(|t| text_obj(t, dir)) {
                    out.push(t);
                }
                for f in b
                    .get("fields")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if let Some(t) = text_obj(f, dir) {
                        out.push(t);
                    }
                }
                if let Some(acc) = b.get("accessory") {
                    walk(acc, dir, out);
                }
            }
            "context" => {
                let parts: Vec<String> = b
                    .get("elements")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|e| text_obj(e, dir))
                    .collect();
                if !parts.is_empty() {
                    out.push(parts.join(" "));
                }
            }
            "rich_text" => {
                let mut buf = String::new();
                for e in b
                    .get("elements")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    rich(e, dir, &mut buf);
                }
                let t = buf.trim_end().to_string();
                if !t.is_empty() {
                    out.push(t);
                }
            }
            // Images, buttons, dividers and other interactive elements are skipped.
            _ => {}
        }
    }
    for b in blocks.as_array().into_iter().flatten() {
        walk(b, dir, &mut out);
    }
    let mut seen = HashSet::new();
    out.into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && seen.insert(s.clone()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Render one message body: text (or blocks), legacy attachments and files.
pub fn render_body(m: &Value, dir: &Directory) -> String {
    let text = m.get("text").and_then(Value::as_str).unwrap_or("");
    let mut body = if text.trim().is_empty() {
        m.get("blocks")
            .map(|b| render_blocks(b, dir))
            .unwrap_or_default()
    } else {
        render_text(text, dir)
    };
    for a in m
        .get("attachments")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let title = a.get("title").and_then(Value::as_str).unwrap_or("").trim();
        let atext = a
            .get("text")
            .and_then(Value::as_str)
            .map(|t| render_text(t, dir))
            .unwrap_or_default();
        let line = match (title.is_empty(), atext.trim().is_empty()) {
            (false, false) => format!("{title} / {}", atext.trim()),
            (false, true) => title.to_string(),
            (true, false) => atext.trim().to_string(),
            (true, true) => a
                .get("fallback")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string(),
        };
        if !line.is_empty() {
            for l in line.lines() {
                body.push_str(&format!("\n> {l}"));
            }
        }
    }
    for f in m
        .get("files")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let name = f
            .get("name")
            .or_else(|| f.get("title"))
            .and_then(Value::as_str)
            .unwrap_or("file");
        body.push_str(&format!("\n[attachment: {name}]"));
    }
    body.trim().to_string()
}

/// The speaker name of a message.
pub fn speaker(m: &Value, dir: &Directory) -> String {
    if let Some(u) = m.get("user").and_then(Value::as_str) {
        if let Some(info) = dir.get(u).and_then(|i| i.label()) {
            return info.to_string();
        }
        if let Some(p) = m.get("user_profile").and_then(UserInfo::from_profile)
            && let Some(l) = p.label()
        {
            return l.to_string();
        }
        return u.to_string();
    }
    m.get("username")
        .or_else(|| m.pointer("/bot_profile/name"))
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string()
}

/// Parse stored JSONL segments into messages, de-duplicated by `ts` (the last
/// occurrence wins) and ordered by `ts`.
pub fn parse_messages(input: &NormalizeInput) -> Result<Vec<Value>, SourceError> {
    let mut by_ts: BTreeMap<(u64, u64), Value> = BTreeMap::new();
    for seg in &input.segments {
        let text =
            std::str::from_utf8(&seg.bytes).map_err(|e| SourceError::Parse(e.to_string()))?;
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let v: Value = serde_json::from_str(line)
                .map_err(|e| SourceError::Parse(format!("raw JSONL: {e}")))?;
            let ts = v
                .get("ts")
                .and_then(Value::as_str)
                .unwrap_or("0")
                .to_string();
            by_ts.insert(ts_parts(&ts), v);
        }
    }
    Ok(by_ts.into_values().collect())
}

fn ts_parts(s: &str) -> (u64, u64) {
    let (sec, frac) = s.split_once('.').unwrap_or((s, "0"));
    let frac = format!("{frac:0<6}");
    (
        sec.parse().unwrap_or(0),
        frac.get(..6).and_then(|f| f.parse().ok()).unwrap_or(0),
    )
}

/// Convert a Slack `ts` to a UTC time.
pub fn ts_to_time(ts: &str) -> Option<DateTime<Utc>> {
    let (sec, micros) = ts_parts(ts);
    Utc.timestamp_opt(sec as i64, (micros * 1000) as u32)
        .single()
}

/// Parse a time zone name, falling back to UTC.
pub fn tz(name: &str) -> Tz {
    name.parse().unwrap_or(Tz::UTC)
}

/// The conversation label used in titles.
pub fn conversation_label(meta: &Value, dir: &Directory, lang: Language) -> String {
    let kind = meta
        .get("channel_kind")
        .and_then(Value::as_str)
        .unwrap_or("channel");
    let name = meta
        .get("channel_name")
        .and_then(Value::as_str)
        .unwrap_or("");
    match kind {
        "dm" => {
            let partner = meta
                .get("dm_user")
                .and_then(Value::as_str)
                .map(|u| name_of(dir, u))
                .unwrap_or_else(|| name.to_string());
            format!("DM: {partner}")
        }
        "group_dm" => {
            let names = meta
                .get("members")
                .and_then(Value::as_array)
                .map(|m| {
                    m.iter()
                        .filter_map(Value::as_str)
                        .map(|u| name_of(dir, u))
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| mpim_names(name));
            match lang {
                Language::Ja => format!("グループDM: {names}"),
                Language::En => format!("Group DM: {names}"),
            }
        }
        _ => format!(
            "#{}",
            if name.is_empty() {
                meta.get("channel_id")
                    .and_then(Value::as_str)
                    .unwrap_or("?")
            } else {
                name
            }
        ),
    }
}

/// Handles from an mpim name like `mpdm-alice--bob--carol-1`.
fn mpim_names(name: &str) -> String {
    let core = name.strip_prefix("mpdm-").unwrap_or(name);
    let core = core.rsplit_once('-').map(|(a, _)| a).unwrap_or(core);
    core.split("--").collect::<Vec<_>>().join(", ")
}

/// The first meaningful line of rendered bodies: URLs and leading/trailing
/// mentions removed, at least four characters, truncated to about 46.
pub fn first_meaningful_line(bodies: &[String]) -> Option<String> {
    for body in bodies {
        for line in body.lines() {
            if line.starts_with("> ") || line.starts_with("[attachment:") {
                continue;
            }
            let no_url = RE_URL.replace_all(line, "");
            let trimmed = RE_EDGE_MENTION.replace_all(no_url.trim(), "");
            let t = trimmed.trim();
            if t.chars().count() >= 4 {
                return Some(truncate_chars(t, 46));
            }
        }
    }
    None
}

/// Normalize a `slack.thread` or `slack.day` entry.
pub fn normalize(
    ctx: &NormalizeCtx,
    input: &NormalizeInput,
) -> Result<NormalizeOutcome, SourceError> {
    let kind = input.source_ref.source_kind;
    let mut dir = directory_from_snapshot(&ctx.snapshot);
    let messages = parse_messages(input)?;
    if messages.is_empty() {
        return Ok(NormalizeOutcome::NotApplicable("no messages".into()));
    }
    // Harvest Slack Connect names embedded in messages (local to this call).
    for m in &messages {
        if let (Some(u), Some(p)) = (m.get("user").and_then(Value::as_str), m.get("user_profile"))
            && !dir.contains_key(u)
            && let Some(info) = UserInfo::from_profile(p)
        {
            dir.insert(u.to_string(), info);
        }
    }
    let zone = tz(&ctx.timezone);
    let times: Vec<Option<DateTime<Utc>>> = messages
        .iter()
        .map(|m| m.get("ts").and_then(Value::as_str).and_then(ts_to_time))
        .collect();
    let local_dates: HashSet<String> = times
        .iter()
        .flatten()
        .map(|t| t.with_timezone(&zone).format("%Y-%m-%d").to_string())
        .collect();
    let multi_day = local_dates.len() > 1;
    let mut lines = Vec::with_capacity(messages.len());
    let mut bodies = Vec::with_capacity(messages.len());
    let mut participants: Vec<String> = Vec::new();
    for (m, t) in messages.iter().zip(&times) {
        let body = render_body(m, &dir);
        let who = speaker(m, &dir);
        if !participants.contains(&who) {
            participants.push(who.clone());
        }
        let stamp = t
            .map(|t| {
                let l = t.with_timezone(&zone);
                if multi_day {
                    l.format("%Y-%m-%d %H:%M").to_string()
                } else {
                    l.format("%H:%M").to_string()
                }
            })
            .unwrap_or_default();
        lines.push(format!("{stamp} {who}: {body}"));
        // For the title, mentions at the edges are removed before rendering,
        // so that names containing spaces disappear completely.
        let raw_text = m.get("text").and_then(Value::as_str).unwrap_or("");
        bodies.push(if raw_text.trim().is_empty() {
            body
        } else {
            render_text(&RE_EDGE_RAW_MENTION.replace_all(raw_text, ""), &dir)
        });
    }
    let details = lines.join("\n");
    let meta = &input.fetch_metadata;
    let label = conversation_label(meta, &dir, ctx.language);
    let first = times.iter().flatten().next().copied();
    let last = times.iter().flatten().last().copied();
    let title = match kind {
        SourceKind::SlackDay => {
            let date = input
                .source_ref
                .source_id
                .rsplit(':')
                .next()
                .unwrap_or("")
                .to_string();
            match ctx.language {
                Language::Ja => format!("{label} {date} の会話"),
                Language::En => format!("{label} {date} conversation"),
            }
        }
        _ => match first_meaningful_line(&bodies) {
            Some(line) => format!("{label} {line}"),
            None => format!(
                "{label} {}",
                first
                    .map(|t| t.with_timezone(&zone).format("%Y-%m-%d").to_string())
                    .unwrap_or_default()
            ),
        },
    };
    let thread_ts = match kind {
        SourceKind::SlackThread => input
            .source_ref
            .source_id
            .split_once(':')
            .map(|(_, t)| t.to_string()),
        _ => None,
    };
    let metadata = json!({
        "channel_id": meta.get("channel_id"),
        "channel_name": meta.get("channel_name"),
        "channel_kind": meta.get("channel_kind"),
        "thread_ts": thread_ts,
        "message_count": messages.len(),
        "participants": participants.iter().map(|p| json!({"name": p})).collect::<Vec<_>>(),
    });
    Ok(NormalizeOutcome::Entry(Box::new(Normalized {
        title: title.clone(),
        source_url: meta
            .get("permalink")
            .and_then(Value::as_str)
            .map(str::to_string),
        source_created_at: first,
        source_updated_at: last,
        metadata,
        sections: vec![SectionDraft {
            kind: SectionKind::Details,
            origin: SectionOrigin::Extracted,
            text: details.clone(),
        }],
        summary_input: Some(SummaryInput {
            source_kind: kind,
            prompt: PromptKind::Conversation,
            title,
            date: first,
            context: None,
            body: details,
            message_count: Some(messages.len()),
            want_details: false,
        }),
        native_summary: None,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sb_core::{AccountCtx, AccountId, AccountKind, RawRole, RawSegment, SourceRef};

    fn dir() -> Directory {
        let mut d = Directory::new();
        d.insert(
            "U1".into(),
            UserInfo {
                name: "alice".into(),
                real_name: "Alice Example".into(),
                ..Default::default()
            },
        );
        d.insert(
            "U2".into(),
            UserInfo {
                name: "bob".into(),
                display_name: "Bob".into(),
                ..Default::default()
            },
        );
        d
    }

    #[test]
    fn mrkdwn_rendering() {
        let d = dir();
        assert_eq!(
            render_text("hi <@U1> and <@U9|carol>", &d),
            "hi @Alice Example and @carol"
        );
        assert_eq!(render_text("<!here> see <#C1|dev>", &d), "@here see #dev");
        assert_eq!(
            render_text("<https://x.test/a|spec> &amp; <https://y.test>", &d),
            "spec (https://x.test/a) & https://y.test"
        );
        assert_eq!(
            render_text("a &lt;b&gt; <mailto:a@b.test|a@b.test>", &d),
            "a <b> a@b.test"
        );
        assert_eq!(render_text("<!subteam^S1|@devs> ping", &d), "@devs ping");
    }

    #[test]
    fn blocks_are_walked_when_text_is_empty() {
        let d = dir();
        let m = json!({
            "text": "",
            "blocks": [
                {"type": "header", "text": {"type": "plain_text", "text": "Release notes"}},
                {"type": "section", "text": {"type": "mrkdwn", "text": "Version 2 is out"},
                 "fields": [{"type": "mrkdwn", "text": "Owner: <@U2>"}],
                 "accessory": {"type": "button", "text": {"type": "plain_text", "text": "Open"}}},
                {"type": "image", "image_url": "https://img.test/x.png"},
                {"type": "rich_text", "elements": [{"type": "rich_text_section", "elements": [
                    {"type": "user", "user_id": "U1"}, {"type": "text", "text": " please check "},
                    {"type": "link", "url": "https://doc.test", "text": "the doc"}]}]},
                {"type": "context", "elements": [{"type": "mrkdwn", "text": "Version 2 is out"}]}
            ],
            "attachments": [{"title": "Build", "text": "passed"}, {"fallback": "old style"}],
            "files": [{"name": "plan.pdf"}]
        });
        let body = render_body(&m, &d);
        assert_eq!(
            body,
            "Release notes\nVersion 2 is out\nOwner: @Bob\n@Alice Example please check the doc (https://doc.test)\n> Build / passed\n> old style\n[attachment: plan.pdf]"
        );
    }

    #[test]
    fn title_line_rules() {
        let bodies = vec![
            "@Bob https://x.test".to_string(),
            "@Bob can you review the CSV export plan for the quarterly report? @Alice Example"
                .to_string(),
        ];
        let t = first_meaningful_line(&bodies).unwrap();
        assert_eq!(t, "can you review the CSV export plan for the qu…");
        assert!(t.chars().count() <= 46);
        assert_eq!(mpim_names("mpdm-alice--bob--carol-1"), "alice, bob, carol");
    }

    fn input(kind: SourceKind, id: &str, segments: &[&str], meta: Value) -> NormalizeInput {
        NormalizeInput {
            source_ref: SourceRef {
                account_id: AccountId::new("acme").unwrap(),
                source_kind: kind,
                source_id: id.into(),
                source_url: None,
                created_at: None,
                updated_at: None,
            },
            fetch_metadata: meta,
            segments: segments
                .iter()
                .enumerate()
                .map(|(i, s)| RawSegment {
                    role: RawRole::Primary,
                    seq: i as i64,
                    media_type: "application/x-ndjson".into(),
                    bytes: s.as_bytes().to_vec(),
                })
                .collect(),
        }
    }

    fn ctx(lang: Language) -> NormalizeCtx {
        NormalizeCtx {
            account: AccountCtx {
                id: AccountId::new("acme").unwrap(),
                kind: AccountKind::Slack,
                label: "Acme".into(),
                identity: None,
                config: json!({}),
            },
            language: lang,
            timezone: "Asia/Tokyo".into(),
            snapshot: json!({"users": dir()}),
        }
    }

    #[test]
    fn thread_segments_are_deduplicated() {
        // 1727740800 = 2024-10-01T00:00:00Z = 09:00 JST.
        let seg0 = "{\"ts\":\"1727740800.000100\",\"user\":\"U1\",\"text\":\"Shall we switch the export to CSV?\"}\n{\"ts\":\"1727740860.000200\",\"user\":\"U2\",\"text\":\"Yes\"}\n";
        let seg1 = "{\"ts\":\"1727740800.000100\",\"user\":\"U1\",\"text\":\"Shall we switch the export to CSV? (edited)\"}\n{\"ts\":\"1727740920.000300\",\"user\":\"UX\",\"text\":\"Agreed\",\"user_profile\":{\"real_name\":\"Xavier Partner\"}}\n";
        let meta = json!({"channel_id": "C1", "channel_name": "dev", "channel_kind": "channel", "permalink": "https://acme.slack.test/archives/C1/p1727740800000100"});
        let out = normalize(
            &ctx(Language::En),
            &input(
                SourceKind::SlackThread,
                "C1:1727740800.000100",
                &[seg0, seg1],
                meta,
            ),
        )
        .unwrap();
        let NormalizeOutcome::Entry(n) = out else {
            panic!()
        };
        assert_eq!(n.title, "#dev Shall we switch the export to CSV? (edited)");
        assert_eq!(
            n.sections[0].text,
            "09:00 Alice Example: Shall we switch the export to CSV? (edited)\n09:01 Bob: Yes\n09:02 Xavier Partner: Agreed"
        );
        assert_eq!(n.metadata["message_count"], 3);
        assert_eq!(n.metadata["thread_ts"], "1727740800.000100");
        assert_eq!(n.summary_input.as_ref().unwrap().message_count, Some(3));
        assert_eq!(
            n.source_url.as_deref(),
            Some("https://acme.slack.test/archives/C1/p1727740800000100")
        );
        assert_eq!(
            crate::render::ts_to_time("1727740800.000100")
                .unwrap()
                .timestamp(),
            1727740800
        );
    }

    #[test]
    fn day_titles_and_dm_labels() {
        let seg = "{\"ts\":\"1727740800.000100\",\"user\":\"U2\",\"text\":\"hello\"}\n{\"ts\":\"1727827200.000100\",\"user\":\"U1\",\"text\":\"next day\"}\n";
        let meta = json!({"channel_id": "D1", "channel_kind": "dm", "dm_user": "U2"});
        let NormalizeOutcome::Entry(n) = normalize(
            &ctx(Language::Ja),
            &input(SourceKind::SlackDay, "D1:day:2024-10-01", &[seg], meta),
        )
        .unwrap() else {
            panic!()
        };
        assert_eq!(n.title, "DM: Bob 2024-10-01 の会話");
        assert!(
            n.sections[0]
                .text
                .starts_with("2024-10-01 09:00 Bob: hello"),
            "multi-day conversations include the date"
        );
        let gm = json!({"channel_kind": "group_dm", "channel_name": "mpdm-alice--bob-1"});
        assert_eq!(
            conversation_label(&gm, &dir(), Language::En),
            "Group DM: alice, bob"
        );
    }
}
