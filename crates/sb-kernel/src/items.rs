//! Structured items extracted with a summary (ADR-0024, items.md).
//!
//! Everything here is pure: no I/O and no clock. Callers pass `now` where time
//! matters.

use std::collections::BTreeSet;

use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use unicode_normalization::UnicodeNormalization;

use crate::{SectionDraft, SectionKind, SectionOrigin, SourceKind};

/// Prompt versions whose output carries items (ADR-0024). Summaries made with
/// any other version have none.
pub const ITEM_PROMPT_VERSIONS: &[&str] = &[
    "conversation-summary/v2",
    "meeting-summary/v2",
    "document-summary/v4",
];

/// Whether a summary written with this prompt version has items.
pub fn prompt_extracts_items(version: &str) -> bool {
    ITEM_PROMPT_VERSIONS.contains(&version)
}

/// Shortest quote (in normalized characters) that can back a completion claim.
/// A bare "done" or "ok" proves nothing about which task was finished.
pub const MIN_COMPLETION_EVIDENCE_CHARS: usize = 6;

/// Longest evidence quote that is kept, in characters.
pub const MAX_EVIDENCE_CHARS: usize = 200;
/// Most actors kept per item.
pub const MAX_ACTORS: usize = 10;

/// What an item is (items.md "Item kinds").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    Decision,
    Action,
    Resolved,
    Question,
    Accomplishment,
}

impl ItemKind {
    pub const ALL: [ItemKind; 5] = [
        ItemKind::Decision,
        ItemKind::Action,
        ItemKind::Resolved,
        ItemKind::Question,
        ItemKind::Accomplishment,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ItemKind::Decision => "decision",
            ItemKind::Action => "action",
            ItemKind::Resolved => "resolved",
            ItemKind::Question => "question",
            ItemKind::Accomplishment => "accomplishment",
        }
    }

    /// Whether an item of this kind can close another one (candidate pairs).
    pub fn is_closure_like(self) -> bool {
        matches!(
            self,
            ItemKind::Resolved | ItemKind::Accomplishment | ItemKind::Decision
        )
    }
}

impl std::str::FromStr for ItemKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "decision" => Ok(ItemKind::Decision),
            "action" => Ok(ItemKind::Action),
            "resolved" => Ok(ItemKind::Resolved),
            "question" => Ok(ItemKind::Question),
            "accomplishment" => Ok(ItemKind::Accomplishment),
            other => Err(format!("unknown item kind {other:?}")),
        }
    }
}

/// The own status of an item (`action`: open, done, dropped; `question`: open, answered).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemStatus {
    Open,
    Done,
    Dropped,
    Answered,
}

impl ItemStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ItemStatus::Open => "open",
            ItemStatus::Done => "done",
            ItemStatus::Dropped => "dropped",
            ItemStatus::Answered => "answered",
        }
    }
}

impl std::str::FromStr for ItemStatus {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "open" => Ok(ItemStatus::Open),
            "done" => Ok(ItemStatus::Done),
            "dropped" => Ok(ItemStatus::Dropped),
            "answered" => Ok(ItemStatus::Answered),
            other => Err(format!("unknown item status {other:?}")),
        }
    }
}

/// How sure the extraction is, tied to what the input says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Low,
    Medium,
    High,
}

impl Confidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Confidence::Low => "low",
            Confidence::Medium => "medium",
            Confidence::High => "high",
        }
    }
}

impl std::str::FromStr for Confidence {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "low" => Ok(Confidence::Low),
            "medium" => Ok(Confidence::Medium),
            "high" => Ok(Confidence::High),
            other => Err(format!("unknown confidence {other:?}")),
        }
    }
}

/// The status of an item after links are applied (items.md "Effective status").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectiveStatus {
    Open,
    Done,
    Dropped,
    Answered,
    Superseded,
}

impl EffectiveStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            EffectiveStatus::Open => "open",
            EffectiveStatus::Done => "done",
            EffectiveStatus::Dropped => "dropped",
            EffectiveStatus::Answered => "answered",
            EffectiveStatus::Superseded => "superseded",
        }
    }
}

impl std::str::FromStr for EffectiveStatus {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "open" => Ok(EffectiveStatus::Open),
            "done" => Ok(EffectiveStatus::Done),
            "dropped" => Ok(EffectiveStatus::Dropped),
            "answered" => Ok(EffectiveStatus::Answered),
            "superseded" => Ok(EffectiveStatus::Superseded),
            other => Err(format!("unknown status {other:?}")),
        }
    }
}

/// An item as the model returned it, before validation. All fields are free
/// text, because the model is not trusted.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ItemDraft {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub requester: Option<String>,
    #[serde(default)]
    pub due: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub actors: Vec<String>,
    #[serde(default)]
    pub confidence: String,
    #[serde(default)]
    pub evidence: Option<String>,
}

impl ItemDraft {
    /// Read a draft from a JSON object, tolerating wrong types.
    pub fn from_value(v: &Value) -> Option<ItemDraft> {
        let o = v.as_object()?;
        let s = |k: &str| -> Option<String> {
            match o.get(k) {
                Some(Value::String(s)) if !s.trim().is_empty() => Some(s.trim().to_string()),
                _ => None,
            }
        };
        let actors = match o.get("actors") {
            Some(Value::Array(a)) => a
                .iter()
                .filter_map(|x| x.as_str().map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .collect(),
            Some(Value::String(x)) if !x.trim().is_empty() => vec![x.trim().to_string()],
            _ => Vec::new(),
        };
        Some(ItemDraft {
            kind: s("kind").unwrap_or_default(),
            text: s("text").unwrap_or_default(),
            owner: s("owner"),
            requester: s("requester"),
            due: s("due"),
            status: s("status"),
            actors,
            confidence: s("confidence").unwrap_or_default(),
            evidence: s("evidence"),
        })
    }
}

/// A validated item, ready to be stored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewItem {
    pub ordinal: i64,
    pub kind: ItemKind,
    pub text: String,
    pub status: Option<ItemStatus>,
    pub owner: Option<String>,
    pub requester: Option<String>,
    /// `YYYY-MM-DD`.
    pub due: Option<String>,
    pub actors: Vec<String>,
    pub occurred_at: Option<DateTime<Utc>>,
    pub confidence: Confidence,
    pub evidence: Option<String>,
    pub scope_key: String,
}

/// A stored item with its row and entry ids.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StoredItem {
    pub id: i64,
    pub entry_id: i64,
    #[serde(flatten)]
    pub item: NewItem,
}

/// What validation did (stored in `summaries.usage.items`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemCounts {
    pub kept: u32,
    pub downgraded: u32,
    pub dropped: u32,
}

/// What validation needs to know about the entry.
#[derive(Debug, Clone)]
pub struct ItemCtx<'a> {
    pub source_kind: SourceKind,
    pub source_id: &'a str,
    pub title: &'a str,
    pub metadata: &'a Value,
    pub source_created_at: Option<DateTime<Utc>>,
    pub source_updated_at: Option<DateTime<Utc>>,
    /// IANA time zone of the rendered Slack times (`slack.day_timezone`).
    pub timezone: &'a str,
    pub max_items: usize,
}

/// Normalize text for matching: NFKC, lowercase, Markdown emphasis marks removed,
/// runs of whitespace collapsed.
pub fn normalize_for_match(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = false;
    for c in s.nfkc().flat_map(char::to_lowercase) {
        if matches!(c, '*' | '_' | '`' | '~') {
            continue;
        }
        if c.is_whitespace() {
            space = true;
            continue;
        }
        if space && !out.is_empty() {
            out.push(' ');
        }
        space = false;
        out.push(c);
    }
    out
}

/// Whether `quote` occurs in `body` (both normalized the same way).
pub fn verify_evidence(body: &str, quote: &str) -> bool {
    let q = normalize_for_match(quote);
    !q.is_empty() && normalize_for_match(body).contains(&q)
}

fn clip_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn clean(o: &Option<String>) -> Option<String> {
    o.as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Validate the items a model returned (items.md "Validation").
pub fn validate_items(
    drafts: &[ItemDraft],
    body: &str,
    ctx: &ItemCtx<'_>,
) -> (Vec<NewItem>, ItemCounts) {
    let norm_body = normalize_for_match(body);
    let scope = scope_key(ctx);
    let mut counts = ItemCounts::default();
    let mut kept: Vec<NewItem> = Vec::new();
    for d in drafts {
        let Ok(kind) = d.kind.parse::<ItemKind>() else {
            counts.dropped += 1;
            continue;
        };
        let text = d.text.trim();
        if text.is_empty() {
            counts.dropped += 1;
            continue;
        }
        let mut status = match kind {
            ItemKind::Action => match d.status.as_deref() {
                None => Some(ItemStatus::Open),
                Some(s) => match s.parse::<ItemStatus>() {
                    Ok(x @ (ItemStatus::Open | ItemStatus::Done | ItemStatus::Dropped)) => Some(x),
                    _ => {
                        counts.dropped += 1;
                        continue;
                    }
                },
            },
            ItemKind::Question => match d.status.as_deref() {
                None => Some(ItemStatus::Open),
                Some(s) => match s.parse::<ItemStatus>() {
                    Ok(x @ (ItemStatus::Open | ItemStatus::Answered)) => Some(x),
                    _ => {
                        counts.dropped += 1;
                        continue;
                    }
                },
            },
            _ => None,
        };
        let mut confidence = d
            .confidence
            .parse::<Confidence>()
            .unwrap_or(Confidence::Low);
        let quote = clean(&d.evidence).map(|q| clip_chars(&q, MAX_EVIDENCE_CHARS));
        let completion_claim = matches!(kind, ItemKind::Resolved | ItemKind::Accomplishment)
            || matches!(
                status,
                Some(ItemStatus::Done | ItemStatus::Dropped | ItemStatus::Answered)
            );
        let verified = quote.as_deref().is_some_and(|q| {
            let nq = normalize_for_match(q);
            !nq.is_empty()
                && (!completion_claim || nq.chars().count() >= MIN_COMPLETION_EVIDENCE_CHARS)
                && norm_body.contains(&nq)
        });
        let mut downgraded = false;
        if !verified {
            match kind {
                ItemKind::Resolved | ItemKind::Accomplishment => {
                    counts.dropped += 1;
                    continue;
                }
                ItemKind::Action
                    if matches!(status, Some(ItemStatus::Done | ItemStatus::Dropped)) =>
                {
                    status = Some(ItemStatus::Open);
                    confidence = Confidence::Low;
                    downgraded = true;
                }
                ItemKind::Question if status == Some(ItemStatus::Answered) => {
                    status = Some(ItemStatus::Open);
                    confidence = Confidence::Low;
                    downgraded = true;
                }
                _ => {
                    if confidence != Confidence::Low {
                        confidence = Confidence::Low;
                        downgraded = true;
                    }
                }
            }
        }
        let due = if kind == ItemKind::Action {
            d.due
                .as_deref()
                .and_then(|s| NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok())
                .map(|x| x.format("%Y-%m-%d").to_string())
        } else {
            None
        };
        let mut actors: Vec<String> = Vec::new();
        for a in &d.actors {
            let a = a.trim();
            if !a.is_empty() && !actors.iter().any(|x| x == a) && actors.len() < MAX_ACTORS {
                actors.push(a.to_string());
            }
        }
        let evidence = if verified { quote } else { None };
        let occurred_at = occurred_at(ctx, body, evidence.as_deref());
        if downgraded {
            counts.downgraded += 1;
        }
        counts.kept += 1;
        kept.push(NewItem {
            ordinal: 0,
            kind,
            text: text.to_string(),
            status,
            owner: if matches!(kind, ItemKind::Action | ItemKind::Question) {
                clean(&d.owner)
            } else {
                None
            },
            requester: if kind == ItemKind::Action {
                clean(&d.requester)
            } else {
                None
            },
            due,
            actors,
            occurred_at,
            confidence,
            evidence,
            scope_key: scope.clone(),
        });
    }
    // Cap: keep the most confident, in the original order.
    if kept.len() > ctx.max_items {
        let mut idx: Vec<usize> = (0..kept.len()).collect();
        idx.sort_by_key(|&i| (std::cmp::Reverse(kept[i].confidence), i));
        let keep: BTreeSet<usize> = idx.into_iter().take(ctx.max_items).collect();
        let dropped = kept.len() - keep.len();
        counts.kept = counts.kept.saturating_sub(dropped as u32);
        counts.dropped += dropped as u32;
        kept = kept
            .into_iter()
            .enumerate()
            .filter(|(i, _)| keep.contains(i))
            .map(|(_, x)| x)
            .collect();
    }
    for (i, it) in kept.iter_mut().enumerate() {
        it.ordinal = i as i64;
    }
    (kept, counts)
}

/// The scope that groups related items (items.md "`scope_key`").
pub fn scope_key(ctx: &ItemCtx<'_>) -> String {
    match ctx.source_kind {
        SourceKind::SlackThread | SourceKind::SlackDay => {
            let id = ctx
                .metadata
                .get("channel_id")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| {
                    ctx.source_id
                        .split(':')
                        .next()
                        .unwrap_or_default()
                        .to_string()
                });
            format!("slack:{id}")
        }
        SourceKind::GoogleMeet => {
            match ctx
                .metadata
                .get("recurring_event_id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                Some(id) => format!("meet:{id}"),
                None => format!("meet:title:{}", normalize_meeting_title(ctx.title)),
            }
        }
        k => format!("{}:{}", k.as_str(), ctx.source_id),
    }
}

fn is_date_token(t: &str) -> bool {
    t.chars().any(|c| c.is_ascii_digit())
        && t.chars()
            .all(|c| c.is_ascii_digit() || "-/年月日:.時分jstutc".contains(c))
}

/// Meeting title without a "(recurring)" suffix or a trailing date, for series keys.
pub fn normalize_meeting_title(title: &str) -> String {
    let n = normalize_for_match(title);
    let mut tokens: Vec<&str> = n.split(' ').filter(|t| !t.is_empty()).collect();
    loop {
        match tokens.last() {
            Some(t) if is_date_token(t) => {
                tokens.pop();
            }
            Some(&"(recurring)") | Some(&"(定期)") | Some(&"recurring") => {
                tokens.pop();
            }
            _ => break,
        }
    }
    tokens.join(" ")
}

/// A rendered message line is `<stamp> <speaker>: <body>`; require a short speaker
/// label so that a stamp-like continuation line of a message does not start a block.
fn has_speaker(rest: &str) -> bool {
    rest.find(": ")
        .is_some_and(|i| i > 0 && i <= 80 && !rest[..i].contains('\n'))
}

fn parse_stamp(line: &str) -> Option<(Option<NaiveDate>, (u32, u32), usize)> {
    // `YYYY-MM-DD HH:MM ` or `HH:MM ` at the start of a message block.
    let b = line.as_bytes();
    let hm = |s: &str| -> Option<(u32, u32)> {
        let (h, m) = s.split_once(':')?;
        if h.len() != 2 || m.len() != 2 {
            return None;
        }
        Some((h.parse().ok()?, m.parse().ok()?))
    };
    if b.len() >= 17
        && line.is_char_boundary(10)
        && let Ok(d) = NaiveDate::parse_from_str(&line[..10], "%Y-%m-%d")
        && line.as_bytes()[10] == b' '
        && line.is_char_boundary(16)
        && let Some(t) = hm(&line[11..16])
        && has_speaker(line[16..].trim_start())
    {
        return Some((Some(d), t, 16));
    }
    if b.len() >= 6
        && line.is_char_boundary(5)
        && let Some(t) = hm(&line[..5])
        && line.as_bytes()[5] == b' '
        && has_speaker(line[5..].trim_start())
    {
        return Some((None, t, 5));
    }
    None
}

fn slack_occurred_at(ctx: &ItemCtx<'_>, body: &str, quote: &str) -> Option<DateTime<Utc>> {
    let nq = normalize_for_match(quote);
    if nq.is_empty() {
        return None;
    }
    let zone: Tz = ctx.timezone.parse().unwrap_or(Tz::UTC);
    // Split into message blocks: a block starts at a line with a time stamp.
    let mut blocks: Vec<(Option<NaiveDate>, (u32, u32), String)> = Vec::new();
    for line in body.lines() {
        if let Some((d, t, off)) = parse_stamp(line) {
            blocks.push((d, t, line[off..].to_string()));
        } else if let Some(last) = blocks.last_mut() {
            last.2.push('\n');
            last.2.push_str(line);
        }
    }
    let (d, (h, m), _) = blocks
        .into_iter()
        .find(|(_, _, text)| normalize_for_match(text).contains(&nq))?;
    let date = d.or_else(|| {
        ctx.source_created_at
            .map(|t| t.with_timezone(&zone).date_naive())
    })?;
    let naive: NaiveDateTime = date.and_hms_opt(h, m, 0)?;
    zone.from_local_datetime(&naive)
        .earliest()
        .map(|t| t.with_timezone(&Utc))
}

/// When an item happened (items.md "`occurred_at`").
pub fn occurred_at(ctx: &ItemCtx<'_>, body: &str, evidence: Option<&str>) -> Option<DateTime<Utc>> {
    match ctx.source_kind {
        SourceKind::SlackThread | SourceKind::SlackDay => evidence
            .and_then(|q| slack_occurred_at(ctx, body, q))
            .or(ctx.source_updated_at)
            .or(ctx.source_created_at),
        SourceKind::GoogleMeet => ctx.source_created_at.or(ctx.source_updated_at),
        _ => ctx.source_updated_at.or(ctx.source_created_at),
    }
}

/// Render the `decisions` and `action_items` sections from validated items.
pub fn render_sections(items: &[NewItem]) -> Vec<SectionDraft> {
    let mut out = Vec::new();
    let mut decisions = Vec::new();
    let mut actions = Vec::new();
    for it in items {
        match it.kind {
            ItemKind::Decision => decisions.push(format!("- {}", it.text)),
            ItemKind::Action => {
                let prefix = match it.status {
                    Some(ItemStatus::Done) => "[done] ",
                    Some(ItemStatus::Dropped) => "[dropped] ",
                    _ => "",
                };
                actions.push(format!("- {prefix}{}", it.text));
            }
            _ => {}
        }
    }
    for (kind, bullets) in [
        (SectionKind::Decisions, decisions),
        (SectionKind::ActionItems, actions),
    ] {
        if !bullets.is_empty() {
            out.push(SectionDraft {
                kind,
                origin: SectionOrigin::Generated,
                text: bullets.join("\n"),
            });
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Aliases ("mine")

/// Why a reference matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchBasis {
    Email,
    Name,
}

/// Which field of an item matched the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MineField {
    Owner,
    Requester,
    Actor,
}

/// A match of an item against the aliases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct MineMatch {
    #[serde(rename = "match")]
    pub field: MineField,
    pub basis: MatchBasis,
}

/// Normalize a person's name for comparison: NFKC, lowercase, punctuation and
/// whitespace runs collapsed to single spaces.
pub fn normalize_name(s: &str) -> String {
    let mut out = String::new();
    let mut space = false;
    for c in s.nfkc().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() || c == '@' {
            if space && !out.is_empty() {
                out.push(' ');
            }
            space = false;
            out.push(c);
        } else {
            space = true;
        }
    }
    out
}

/// The names and emails of the user, across all accounts.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AliasSet {
    names: Vec<String>,
    emails: Vec<String>,
}

fn token_set(s: &str) -> BTreeSet<&str> {
    s.split(' ').filter(|t| !t.is_empty()).collect()
}

/// Whether `haystack` holds `email` as a whole address, not inside a longer one
/// (`jo@x.com` is not in `kajo@x.com` or `jo@x.com.evil`).
fn contains_email(haystack: &str, email: &str) -> bool {
    let mut from = 0;
    while let Some(pos) = haystack[from..].find(email) {
        let start = from + pos;
        let end = start + email.len();
        let before = haystack[..start].chars().next_back();
        let mut after = haystack[end..].chars();
        let next = after.next();
        let next2 = after.next();
        let word = |c: char| c.is_alphanumeric() || matches!(c, '.' | '_' | '%' | '+' | '-');
        let ok_before = before.is_none_or(|c| !word(c));
        let ok_after = match next {
            None => true,
            Some(c) if c.is_alphanumeric() || matches!(c, '-' | '_') => false,
            Some('.') => !next2.is_some_and(|c| c.is_alphanumeric()),
            Some(_) => true,
        };
        if ok_before && ok_after {
            return true;
        }
        from = start + email.len().max(1);
        if from >= haystack.len() {
            break;
        }
    }
    false
}

impl AliasSet {
    pub fn new<S: AsRef<str>>(names: &[S], emails: &[S]) -> Self {
        let mut n: Vec<String> = Vec::new();
        for x in names {
            let k = normalize_name(x.as_ref());
            if !k.is_empty() && !n.contains(&k) {
                n.push(k);
            }
        }
        let mut e: Vec<String> = Vec::new();
        for x in emails {
            let k = x.as_ref().trim().to_lowercase();
            if !k.is_empty() && !e.contains(&k) {
                e.push(k);
            }
        }
        AliasSet {
            names: n,
            emails: e,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty() && self.emails.is_empty()
    }

    /// Whether a reference written in an input is the user (accounts-and-auth.md "Matching").
    pub fn matches(&self, reference: &str) -> Option<MatchBasis> {
        let lower = reference.to_lowercase();
        if self.emails.iter().any(|e| contains_email(&lower, e)) {
            return Some(MatchBasis::Email);
        }
        let r = normalize_name(reference);
        if r.is_empty() {
            return None;
        }
        let r_compact: String = r.chars().filter(|c| *c != ' ').collect();
        let r_tokens = token_set(&r);
        for a in &self.names {
            if *a == r {
                return Some(MatchBasis::Name);
            }
            let a_compact: String = a.chars().filter(|c| *c != ' ').collect();
            if !a_compact.is_empty() && a_compact == r_compact {
                return Some(MatchBasis::Name);
            }
            let a_tokens = token_set(a);
            if a_tokens.len() >= 2 && a_tokens == r_tokens {
                return Some(MatchBasis::Name);
            }
        }
        None
    }

    /// The first field of an item that is the user (`--mine`).
    pub fn item_is_mine(&self, it: &NewItem) -> Option<MineMatch> {
        match it.kind {
            ItemKind::Action | ItemKind::Question => {
                let b = it.owner.as_deref().and_then(|o| self.matches(o))?;
                Some(MineMatch {
                    field: MineField::Owner,
                    basis: b,
                })
            }
            _ => it
                .actors
                .iter()
                .find_map(|a| self.matches(a))
                .map(|b| MineMatch {
                    field: MineField::Actor,
                    basis: b,
                }),
        }
    }

    /// `--asked-by-me`: the requester is the user.
    pub fn item_asked_by_me(&self, it: &NewItem) -> Option<MineMatch> {
        let b = it.requester.as_deref().and_then(|o| self.matches(o))?;
        Some(MineMatch {
            field: MineField::Requester,
            basis: b,
        })
    }
}

// ---------------------------------------------------------------------------
// Effective status, staleness and candidate pairs

/// An incoming `resolves` or `supersedes` link, as the status computation needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IncomingLink {
    pub relation: LinkRelation,
    pub confidence: Confidence,
    /// `occurred_at` of the item the link comes from.
    pub from_occurred_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkRelation {
    Resolves,
    Supersedes,
    SameAs,
}

impl LinkRelation {
    pub fn as_str(self) -> &'static str {
        match self {
            LinkRelation::Resolves => "resolves",
            LinkRelation::Supersedes => "supersedes",
            LinkRelation::SameAs => "same_as",
        }
    }
}

impl std::str::FromStr for LinkRelation {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "resolves" => Ok(LinkRelation::Resolves),
            "supersedes" => Ok(LinkRelation::Supersedes),
            "same_as" => Ok(LinkRelation::SameAs),
            o => Err(format!("unknown link relation {o:?}")),
        }
    }
}

/// The effective status of an item (items.md "Effective status"). `None` for
/// kinds without a status.
pub fn effective_status(
    it: &NewItem,
    incoming: &[IncomingLink],
    link_min: Confidence,
) -> Option<EffectiveStatus> {
    let own = match it.status? {
        ItemStatus::Open => EffectiveStatus::Open,
        ItemStatus::Done => EffectiveStatus::Done,
        ItemStatus::Dropped => EffectiveStatus::Dropped,
        ItemStatus::Answered => EffectiveStatus::Answered,
    };
    if own != EffectiveStatus::Open {
        return Some(own);
    }
    let later = |l: &IncomingLink| match (l.from_occurred_at, it.occurred_at) {
        (Some(f), Some(t)) => f > t,
        _ => true,
    };
    let strong = |l: &&IncomingLink| l.confidence >= link_min && later(l);
    if incoming
        .iter()
        .filter(strong)
        .any(|l| l.relation == LinkRelation::Supersedes)
    {
        return Some(EffectiveStatus::Superseded);
    }
    if incoming
        .iter()
        .filter(strong)
        .any(|l| l.relation == LinkRelation::Resolves)
    {
        return Some(match it.kind {
            ItemKind::Question => EffectiveStatus::Answered,
            _ => EffectiveStatus::Done,
        });
    }
    Some(own)
}

/// Whether an effectively open item is stale: nothing newer was recorded in its
/// scope for `stale_days`. Never changes the status.
pub fn is_stale(
    effective: Option<EffectiveStatus>,
    item_at: Option<DateTime<Utc>>,
    scope_latest: Option<DateTime<Utc>>,
    stale_days: i64,
    now: DateTime<Utc>,
) -> bool {
    if effective != Some(EffectiveStatus::Open) {
        return false;
    }
    let newest = match (item_at, scope_latest) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    };
    newest.is_some_and(|t| now - t > Duration::days(stale_days))
}

fn trigrams(s: &str) -> BTreeSet<String> {
    let n: Vec<char> = normalize_for_match(s).chars().collect();
    if n.is_empty() {
        return BTreeSet::new();
    }
    if n.len() < 3 {
        return BTreeSet::from([n.iter().collect()]);
    }
    n.windows(3).map(|w| w.iter().collect()).collect()
}

/// Jaccard similarity of the character trigrams of two texts (0.0 to 1.0).
pub fn trigram_similarity(a: &str, b: &str) -> f64 {
    let (x, y) = (trigrams(a), trigrams(b));
    if x.is_empty() || y.is_empty() {
        return 0.0;
    }
    let inter = x.intersection(&y).count() as f64;
    let union = x.union(&y).count() as f64;
    inter / union
}

/// A possible resolution of an open item.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// Index into the pool passed to `candidates_for`.
    pub index: usize,
    pub similarity: f64,
    pub same_scope: bool,
    pub shared_person: bool,
}

fn people(it: &NewItem) -> Vec<String> {
    let mut v: Vec<String> = it.actors.iter().map(|a| normalize_name(a)).collect();
    for x in [&it.owner, &it.requester].into_iter().flatten() {
        v.push(normalize_name(x));
    }
    v.retain(|s| !s.is_empty());
    v
}

/// Up to 3 items of `pool` that might close `target` (items.md "Candidate pairs").
/// `pool` holds `(entry_id, item, effective status)`.
pub fn candidates_for(
    target_entry: i64,
    target: &NewItem,
    pool: &[(i64, &NewItem, Option<EffectiveStatus>)],
    window_days: i64,
    min_similarity: f64,
) -> Vec<Candidate> {
    let Some(t0) = target.occurred_at else {
        return Vec::new();
    };
    let tp = people(target);
    let mut out: Vec<Candidate> = Vec::new();
    for (i, (entry, j, eff)) in pool.iter().enumerate() {
        if *entry == target_entry {
            continue;
        }
        let closure = j.kind.is_closure_like()
            || (j.kind == ItemKind::Action
                && matches!(eff, Some(EffectiveStatus::Done | EffectiveStatus::Dropped)));
        if !closure {
            continue;
        }
        let Some(t1) = j.occurred_at else { continue };
        if t1 <= t0 || t1 - t0 > Duration::days(window_days) {
            continue;
        }
        let same_scope = j.scope_key == target.scope_key;
        let jp = people(j);
        let shared_person = tp.iter().any(|p| jp.contains(p));
        if !same_scope && !shared_person {
            continue;
        }
        let similarity = trigram_similarity(&target.text, &j.text);
        if similarity < min_similarity {
            continue;
        }
        out.push(Candidate {
            index: i,
            similarity,
            same_scope,
            shared_person,
        });
    }
    out.sort_by(|a, b| {
        b.similarity
            .partial_cmp(&a.similarity)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out.truncate(3);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ctx<'a>(kind: SourceKind, meta: &'a Value) -> ItemCtx<'a> {
        ItemCtx {
            source_kind: kind,
            source_id: "C1:1700000000.000100",
            title: "Weekly Sync (recurring) 2026-11-12",
            metadata: meta,
            source_created_at: Some(Utc.with_ymd_and_hms(2026, 11, 12, 0, 0, 0).unwrap()),
            source_updated_at: Some(Utc.with_ymd_and_hms(2026, 11, 12, 6, 0, 0).unwrap()),
            timezone: "Asia/Tokyo",
            max_items: 30,
        }
    }

    fn draft(kind: &str, status: Option<&str>, quote: &str) -> ItemDraft {
        ItemDraft {
            kind: kind.into(),
            text: "something".into(),
            status: status.map(str::to_string),
            confidence: "high".into(),
            evidence: Some(quote.into()),
            owner: Some("Dana".into()),
            ..Default::default()
        }
    }

    #[test]
    fn evidence_is_checked_after_normalization() {
        let body = "09:00 Dana: I'll **update**  the CSV spec\nby Friday";
        assert!(verify_evidence(body, "i'll update the csv spec by friday"));
        assert!(verify_evidence("ＣＳＶ に決定", "csv に決定"));
        assert!(!verify_evidence(body, "I will update the spec"));
        assert!(!verify_evidence(body, "  "));
    }

    #[test]
    fn unverified_completion_is_not_kept_as_completion() {
        let meta = json!({"channel_id": "C1"});
        let c = ctx(SourceKind::SlackThread, &meta);
        let body = "09:00 Dana: I'll fix it\n10:00 Dana: done";
        let drafts = vec![
            draft("action", Some("done"), "Dana: done"),
            draft("action", Some("done"), "never said this"),
            draft("resolved", None, "never said this"),
            draft("accomplishment", None, "10:00 dana: done"),
            draft("decision", None, "not in the body"),
            draft("bogus", None, "x"),
        ];
        let (items, counts) = validate_items(&drafts, body, &c);
        assert_eq!(items.len(), 4);
        assert_eq!(items[0].status, Some(ItemStatus::Done));
        assert_eq!(items[0].confidence, Confidence::High);
        assert_eq!(items[1].status, Some(ItemStatus::Open));
        assert_eq!(items[1].confidence, Confidence::Low);
        assert!(items[1].evidence.is_none());
        assert_eq!(items[2].kind, ItemKind::Accomplishment);
        assert_eq!(items[3].kind, ItemKind::Decision);
        assert_eq!(items[3].confidence, Confidence::Low);
        assert_eq!(
            counts,
            ItemCounts {
                kept: 4,
                downgraded: 2,
                dropped: 2
            }
        );
        assert_eq!(items[3].ordinal, 3);
    }

    #[test]
    fn a_bare_word_cannot_back_a_completion() {
        let meta = json!({});
        let c = ctx(SourceKind::GoogleDoc, &meta);
        let body = "Kei: ok\nDana: done";
        let (items, counts) = validate_items(
            &[
                draft("action", Some("done"), "done"),
                draft("accomplishment", None, "ok"),
                draft("decision", None, "ok"),
            ],
            body,
            &c,
        );
        assert_eq!(items.len(), 2, "the accomplishment is dropped");
        assert_eq!(items[0].status, Some(ItemStatus::Open));
        assert_eq!(items[0].confidence, Confidence::Low);
        assert_eq!(items[1].kind, ItemKind::Decision);
        assert_eq!(
            items[1].confidence,
            Confidence::High,
            "short quotes are fine for a decision"
        );
        assert_eq!(counts.dropped, 1);
    }

    #[test]
    fn invalid_fields_are_repaired_or_dropped() {
        let meta = json!({});
        let c = ctx(SourceKind::GoogleDoc, &meta);
        let mut d = draft("action", Some("open"), "q");
        d.due = Some("next friday".into());
        d.confidence = "certain".into();
        let mut e = draft("action", Some("wontfix"), "q");
        e.text = "x".into();
        let mut f = draft("decision", None, "q");
        f.text = "  ".into();
        let (items, counts) = validate_items(&[d, e, f], "q", &c);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].due, None);
        assert_eq!(items[0].confidence, Confidence::Low);
        assert_eq!(counts.dropped, 2);
        let mut ok = draft("action", None, "q");
        ok.due = Some("2026-11-20".into());
        let (items, _) = validate_items(&[ok], "q", &c);
        assert_eq!(items[0].due.as_deref(), Some("2026-11-20"));
        assert_eq!(items[0].status, Some(ItemStatus::Open));
    }

    #[test]
    fn cap_keeps_the_most_confident_in_order() {
        let meta = json!({});
        let mut c = ctx(SourceKind::GoogleDoc, &meta);
        c.max_items = 2;
        let mut a = draft("decision", None, "q");
        a.confidence = "low".into();
        let b = draft("decision", None, "q");
        let mut d = draft("decision", None, "q");
        d.text = "third".into();
        let (items, counts) = validate_items(&[a, b, d], "q", &c);
        assert_eq!(items.len(), 2);
        assert_eq!(items[1].text, "third");
        assert_eq!(items[0].ordinal, 0);
        assert_eq!(counts.kept, 2);
        assert_eq!(counts.dropped, 1);
    }

    #[test]
    fn scope_keys() {
        let m = json!({"channel_id": "C9"});
        assert_eq!(scope_key(&ctx(SourceKind::SlackDay, &m)), "slack:C9");
        let m = json!({"recurring_event_id": "abc_123"});
        assert_eq!(scope_key(&ctx(SourceKind::GoogleMeet, &m)), "meet:abc_123");
        let m = json!({});
        assert_eq!(
            scope_key(&ctx(SourceKind::GoogleMeet, &m)),
            "meet:title:weekly sync"
        );
        assert_eq!(
            scope_key(&ctx(SourceKind::GoogleDoc, &m)),
            "google.doc:C1:1700000000.000100"
        );
        assert_eq!(
            normalize_meeting_title("Ｄａｉｌｙ Dev Standup (Recurring) 2026年7月27日 09:15"),
            "daily dev standup"
        );
    }

    #[test]
    fn slack_occurred_at_reads_the_message_line() {
        let meta = json!({"channel_id": "C1"});
        let c = ctx(SourceKind::SlackThread, &meta);
        // Tokyo: 2026-11-12 09:00 = 00:00Z, the entry date.
        let body = "09:00 Kei: please check\nthe spec\n13:30 Dana: I'll update it by Friday";
        let t = occurred_at(&c, body, Some("I'll update it by friday")).unwrap();
        assert_eq!(t, Utc.with_ymd_and_hms(2026, 11, 12, 4, 30, 0).unwrap());
        // A quote on a continuation line belongs to its message.
        let t = occurred_at(&c, body, Some("the spec")).unwrap();
        assert_eq!(t, Utc.with_ymd_and_hms(2026, 11, 12, 0, 0, 0).unwrap());
        // A stamp-like line without a speaker belongs to the message above it.
        let body = "09:00 Kei: plan\n10:30 works for me\n13:30 Dana: ok";
        let t = occurred_at(&c, body, Some("works for me")).unwrap();
        assert_eq!(t, Utc.with_ymd_and_hms(2026, 11, 12, 0, 0, 0).unwrap());
        // Multi-day body with date stamps.
        let body = "2026-11-10 23:05 Kei: hi\n2026-11-11 08:00 Dana: done";
        let t = occurred_at(&c, body, Some("dana: done")).unwrap();
        assert_eq!(t, Utc.with_ymd_and_hms(2026, 11, 10, 23, 0, 0).unwrap());
        // No quote: the last update.
        assert_eq!(occurred_at(&c, body, None), c.source_updated_at);
        let m = json!({});
        let meet = ctx(SourceKind::GoogleMeet, &m);
        assert_eq!(occurred_at(&meet, "x", None), meet.source_created_at);
        let doc = ctx(SourceKind::GoogleDoc, &m);
        assert_eq!(occurred_at(&doc, "x", None), doc.source_updated_at);
    }

    #[test]
    fn sections_are_rendered_from_items() {
        let meta = json!({});
        let c = ctx(SourceKind::GoogleDoc, &meta);
        let body = "the team agreed and the work was finished";
        let drafts = vec![
            draft("decision", None, "the team agreed"),
            draft("action", Some("done"), "work was finished"),
            draft("action", Some("open"), "the team agreed"),
            draft("question", None, "the team agreed"),
        ];
        let (items, _) = validate_items(&drafts, body, &c);
        let s = render_sections(&items);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].kind, SectionKind::Decisions);
        assert_eq!(s[1].text, "- [done] something\n- something");
        assert!(render_sections(&[]).is_empty());
    }

    #[test]
    fn alias_matching() {
        let a = AliasSet::new(&["Dana Example", "dana", "山田 太郎"], &["a.m@example.com"]);
        assert_eq!(a.matches("Dana Example"), Some(MatchBasis::Name));
        assert_eq!(a.matches("Example, Dana"), Some(MatchBasis::Name));
        assert_eq!(a.matches("DANA"), Some(MatchBasis::Name));
        assert_eq!(a.matches("山田太郎"), Some(MatchBasis::Name));
        assert_eq!(a.matches("Dana S"), None);
        assert_eq!(a.matches("Akira"), None);
        assert_eq!(a.matches("Example"), None, "a surname alone never matches");
        assert_eq!(
            a.matches("Ask A.M. <A.M@Example.com>"),
            Some(MatchBasis::Email)
        );
        let e = AliasSet::new(&[] as &[&str], &["jo@x.com"]);
        assert_eq!(e.matches("Jo <jo@x.com>"), Some(MatchBasis::Email));
        assert_eq!(e.matches("jo@x.com."), Some(MatchBasis::Email));
        assert_eq!(e.matches("kajo@x.com"), None);
        assert_eq!(e.matches("jo@x.com.evil"), None);
        assert_eq!(e.matches("jo@x.community"), None);
        assert!(AliasSet::default().is_empty());
        assert_eq!(AliasSet::default().matches("anyone"), None);
    }

    #[test]
    fn mine_uses_owner_for_actions_and_actors_for_the_rest() {
        let a = AliasSet::new(&["Dana"], &[] as &[&str]);
        let meta = json!({});
        let c = ctx(SourceKind::GoogleDoc, &meta);
        let mut d = draft("decision", None, "q");
        d.actors = vec!["Dana".into()];
        let mut act = draft("action", None, "q");
        act.owner = Some("Kei".into());
        act.requester = Some("Dana".into());
        let (items, _) = validate_items(&[d, act], "q", &c);
        assert_eq!(a.item_is_mine(&items[0]).unwrap().field, MineField::Actor);
        assert!(a.item_is_mine(&items[1]).is_none());
        assert_eq!(
            a.item_asked_by_me(&items[1]).unwrap().field,
            MineField::Requester
        );
    }

    fn open_action(at: DateTime<Utc>) -> NewItem {
        NewItem {
            ordinal: 0,
            kind: ItemKind::Action,
            text: "update the csv export spec".into(),
            status: Some(ItemStatus::Open),
            owner: Some("Dana".into()),
            requester: None,
            due: None,
            actors: vec![],
            occurred_at: Some(at),
            confidence: Confidence::High,
            evidence: None,
            scope_key: "slack:C1".into(),
        }
    }

    #[test]
    fn effective_status_and_staleness() {
        let t0 = Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap();
        let it = open_action(t0);
        assert_eq!(
            effective_status(&it, &[], Confidence::Medium),
            Some(EffectiveStatus::Open)
        );
        let link = |rel, conf, at| IncomingLink {
            relation: rel,
            confidence: conf,
            from_occurred_at: Some(at),
        };
        let later = t0 + Duration::days(2);
        assert_eq!(
            effective_status(
                &it,
                &[link(LinkRelation::Resolves, Confidence::High, later)],
                Confidence::Medium
            ),
            Some(EffectiveStatus::Done)
        );
        assert_eq!(
            effective_status(
                &it,
                &[link(LinkRelation::Resolves, Confidence::Low, later)],
                Confidence::Medium
            ),
            Some(EffectiveStatus::Open),
            "a weak link does not close"
        );
        assert_eq!(
            effective_status(
                &it,
                &[link(
                    LinkRelation::Resolves,
                    Confidence::High,
                    t0 - Duration::days(1)
                )],
                Confidence::Medium
            ),
            Some(EffectiveStatus::Open),
            "an earlier item cannot close it"
        );
        assert_eq!(
            effective_status(
                &it,
                &[link(LinkRelation::Supersedes, Confidence::High, later)],
                Confidence::Medium
            ),
            Some(EffectiveStatus::Superseded)
        );
        let mut decision = it.clone();
        decision.kind = ItemKind::Decision;
        decision.status = None;
        assert_eq!(effective_status(&decision, &[], Confidence::Medium), None);

        let now = t0 + Duration::days(31);
        assert!(is_stale(
            Some(EffectiveStatus::Open),
            Some(t0),
            Some(t0),
            30,
            now
        ));
        assert!(!is_stale(
            Some(EffectiveStatus::Open),
            Some(t0),
            Some(now - Duration::days(1)),
            30,
            now
        ));
        assert!(!is_stale(
            Some(EffectiveStatus::Done),
            Some(t0),
            Some(t0),
            30,
            now
        ));
    }

    #[test]
    fn trigram_similarity_works_for_japanese_and_english() {
        assert!(trigram_similarity("update the csv spec", "updated the CSV spec") > 0.5);
        assert!(trigram_similarity("CSV仕様書を更新する", "CSV仕様書の更新が完了") > 0.2);
        assert!(trigram_similarity("book a room", "fix the login bug") < 0.1);
        assert_eq!(trigram_similarity("", "x"), 0.0);
    }

    #[test]
    fn candidate_pairs_follow_the_rules() {
        let t0 = Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap();
        let target = open_action(t0);
        let mk = |kind, at, scope: &str, text: &str, who: &str| NewItem {
            kind,
            text: text.into(),
            occurred_at: Some(at),
            scope_key: scope.into(),
            status: None,
            owner: None,
            actors: vec![who.into()],
            ..open_action(at)
        };
        let good = mk(
            ItemKind::Accomplishment,
            t0 + Duration::days(3),
            "slack:C1",
            "updated the csv export spec",
            "Dana",
        );
        let other_scope_other_person = mk(
            ItemKind::Accomplishment,
            t0 + Duration::days(3),
            "slack:C2",
            "updated the csv export spec",
            "Kei",
        );
        let other_scope_same_person = mk(
            ItemKind::Resolved,
            t0 + Duration::days(4),
            "slack:C2",
            "csv export spec updated",
            "dana",
        );
        let too_early = mk(
            ItemKind::Accomplishment,
            t0 - Duration::days(1),
            "slack:C1",
            "updated the csv export spec",
            "Dana",
        );
        let too_late = mk(
            ItemKind::Accomplishment,
            t0 + Duration::days(90),
            "slack:C1",
            "updated the csv export spec",
            "Dana",
        );
        let unrelated = mk(
            ItemKind::Accomplishment,
            t0 + Duration::days(3),
            "slack:C1",
            "booked the offsite venue",
            "Dana",
        );
        let same_entry = good.clone();
        let mut done_action = open_action(t0 + Duration::days(5));
        done_action.status = Some(ItemStatus::Done);
        done_action.text = "update csv export spec: done".into();
        let pool: Vec<(i64, &NewItem, Option<EffectiveStatus>)> = vec![
            (2, &good, None),
            (3, &other_scope_other_person, None),
            (4, &other_scope_same_person, None),
            (5, &too_early, None),
            (6, &too_late, None),
            (7, &unrelated, None),
            (1, &same_entry, None),
            (8, &done_action, Some(EffectiveStatus::Done)),
        ];
        let c = candidates_for(1, &target, &pool, 60, 0.2);
        let idx: Vec<usize> = c.iter().map(|x| x.index).collect();
        assert!(idx.contains(&0), "same scope");
        assert!(idx.contains(&2), "shared person");
        assert!(idx.contains(&7), "a done action closes");
        for bad in [1, 3, 4, 5, 6] {
            assert!(!idx.contains(&bad), "pool item {bad} must not match");
        }
        assert!(c.len() <= 3);
    }
}
