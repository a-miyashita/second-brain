//! The Slack source adapter (source-slack.md "Sync algorithm").

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Duration as ChronoDuration, NaiveDate, TimeZone, Utc};
use sb_core::coverage::{
    BackwardPlan, Coverage, TimeRange, covered_since_of, next_cursor, plan_ranges,
    with_covered_since,
};
use sb_core::source::{Source, SourceError, SyncHost};
use sb_core::{
    AccountCtx, AccountKind, CursorUpdate, DiscoveryBatch, FetchOutcome, FetchRequest,
    FetchedEntry, NormalizeCtx, NormalizeInput, NormalizeOutcome, QueueItem, RawBundle, RawMode,
    RawObject, RawRole, Secret, SourceKind, SourceRef, SyncMode, SyncOptions,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::client::{SlackClient, ts_gt};
use crate::render::{self, Directory, UserInfo};

/// Cache key of the user directory.
pub const USERS_CACHE: &str = "slack.users";
/// Cache key of the conversation directory.
pub const CHANNELS_CACHE: &str = "slack.channels";
/// Back-fill window length.
const WINDOW_DAYS: i64 = 7;

/// `mention_scan` setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MentionScan {
    #[default]
    All,
    SearchOnly,
    Off,
}

fn yes() -> bool {
    true
}

/// Account configuration (`accounts.config`).
#[derive(Debug, Clone, Deserialize)]
pub struct SlackConfig {
    #[serde(default)]
    pub full_channels: Vec<String>,
    #[serde(default = "yes")]
    pub include_dms: bool,
    #[serde(default = "yes")]
    pub exclude_bot_dms: bool,
    #[serde(default)]
    pub exclude_channels: Vec<String>,
    #[serde(default)]
    pub mention_scan: MentionScan,
    #[serde(default)]
    pub search_terms: Vec<String>,
    #[serde(default = "d45")]
    pub thread_watch_days: i64,
    #[serde(default = "d7")]
    pub thread_hot_days: i64,
    #[serde(default = "d30")]
    pub dormant_days: i64,
    #[serde(default = "d4")]
    pub fetch_jobs: u64,
    #[serde(default = "d7")]
    pub users_cache_days: i64,
    /// Workspace URL from `auth.test`, used to build permalinks locally.
    #[serde(default)]
    pub team_url: Option<String>,
}

fn d45() -> i64 {
    45
}
fn d30() -> i64 {
    30
}
fn d7() -> i64 {
    7
}
fn d4() -> u64 {
    4
}

/// The default `config` written by `sb account add slack`.
pub fn default_config_json() -> Value {
    json!({
        "full_channels": [],
        "include_dms": true,
        "exclude_bot_dms": true,
        "exclude_channels": [],
        "mention_scan": "all",
        "search_terms": [],
        "thread_watch_days": 45,
        "thread_hot_days": 7,
        "dormant_days": 30,
        "fetch_jobs": 4,
        "users_cache_days": 7
    })
}

/// A conversation and how it is ingested.
#[derive(Debug, Clone, PartialEq)]
struct Conv {
    id: String,
    meta: Value,
    class: Class,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    /// Every message (full channels, DMs).
    Full,
    /// Only threads involving me.
    Involvement,
}

/// The Slack source for one account.
pub struct SlackSource {
    account: AccountCtx,
    config: SlackConfig,
    client: Option<SlackClient>,
    /// The authenticated user's ID (from the account identity `T…:U…`).
    me: Option<String>,
    tz: String,
}

impl SlackSource {
    /// Build the source. Without a token, only `normalize` works.
    pub fn new(
        account: AccountCtx,
        token: Option<Secret>,
        api_base: Option<String>,
        timezone: String,
    ) -> Result<Self, SourceError> {
        let config: SlackConfig = serde_json::from_value(account.config.clone())
            .map_err(|e| SourceError::Parse(format!("Slack account config: {e}")))?;
        let me = account
            .identity
            .as_deref()
            .and_then(|i| i.split_once(':'))
            .map(|(_, u)| u.to_string());
        let client = token.map(|t| SlackClient::new(t, api_base)).transpose()?;
        Ok(SlackSource {
            account,
            config,
            client,
            me,
            tz: timezone,
        })
    }

    fn client(&self) -> Result<&SlackClient, SourceError> {
        self.client
            .as_ref()
            .ok_or_else(|| SourceError::Auth("no Slack token stored for this account".into()))
    }

    fn permalink(&self, channel: &str, ts: &str) -> Option<String> {
        self.config.team_url.as_ref().map(|u| {
            format!(
                "{}/archives/{channel}/p{}",
                u.trim_end_matches('/'),
                ts.replace('.', "")
            )
        })
    }

    async fn permalink_or_api(&self, channel: &str, ts: &str) -> Option<String> {
        if let Some(p) = self.permalink(channel, ts) {
            return Some(p);
        }
        match self.client().ok()?.permalink(channel, ts).await {
            Ok(p) if !p.is_empty() => Some(p),
            _ => None,
        }
    }

    /// Refresh the user directory when its cache expired. Harvested external
    /// users are kept.
    async fn refresh_users(&self, host: &dyn SyncHost) -> Result<(), SourceError> {
        if host.cache_get(USERS_CACHE)?.is_some() {
            return Ok(());
        }
        let members = self.client()?.users().await?;
        let mut dir: Directory = host
            .cache_get_stale(USERS_CACHE)?
            .and_then(|v| serde_json::from_value::<Directory>(v).ok())
            .unwrap_or_default()
            .into_iter()
            .filter(|(_, u)| u.external)
            .collect();
        for m in &members {
            if let Some(id) = m.get("id").and_then(Value::as_str) {
                dir.insert(id.to_string(), UserInfo::from_member(m));
            }
        }
        host.cache_put(
            USERS_CACHE,
            &serde_json::to_value(&dir).map_err(|e| SourceError::Parse(e.to_string()))?,
            Duration::from_secs(self.config.users_cache_days.max(1) as u64 * 86_400),
        )
    }

    /// Add names of Slack Connect users embedded in messages to the cache.
    fn harvest(&self, host: &dyn SyncHost, messages: &[Value]) -> Result<(), SourceError> {
        let mut dir: Directory = host
            .cache_get_stale(USERS_CACHE)?
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();
        let mut changed = false;
        for m in messages {
            if let (Some(u), Some(p)) =
                (m.get("user").and_then(Value::as_str), m.get("user_profile"))
                && !dir.contains_key(u)
                && let Some(info) = UserInfo::from_profile(p)
            {
                dir.insert(u.to_string(), info);
                changed = true;
            }
        }
        if changed {
            host.cache_put(
                USERS_CACHE,
                &serde_json::to_value(&dir).map_err(|e| SourceError::Parse(e.to_string()))?,
                Duration::from_secs(self.config.users_cache_days.max(1) as u64 * 86_400),
            )?;
        }
        Ok(())
    }

    fn conv_meta(c: &Value) -> Value {
        let b = |k: &str| c.get(k).and_then(Value::as_bool).unwrap_or(false);
        let kind = if b("is_im") {
            "dm"
        } else if b("is_mpim") {
            "group_dm"
        } else if b("is_private") || b("is_group") {
            "private"
        } else {
            "channel"
        };
        let mut meta = json!({
            "channel_id": c.get("id"),
            "channel_name": c.get("name"),
            "channel_kind": kind,
        });
        if let Some(u) = c.get("user").and_then(Value::as_str) {
            meta["dm_user"] = json!(u);
        }
        meta
    }

    /// Metadata of a conversation, from the cache or `conversations.info`.
    async fn channel_meta(&self, host: &dyn SyncHost, channel: &str) -> Result<Value, SourceError> {
        if let Some(m) = host
            .cache_get_stale(CHANNELS_CACHE)?
            .and_then(|c| c.get(channel).cloned())
        {
            return Ok(m);
        }
        let info = self.client()?.conversation_info(channel).await?;
        Ok(Self::conv_meta(&info))
    }

    fn classify(&self, c: &Value, dir: &Directory) -> Option<Class> {
        let meta = Self::conv_meta(c);
        let id = c.get("id").and_then(Value::as_str).unwrap_or("");
        let name = c.get("name").and_then(Value::as_str).unwrap_or("");
        let kind = meta["channel_kind"].as_str().unwrap_or("channel");
        let listed = |list: &[String]| {
            list.iter()
                .any(|x| x.trim_start_matches('#') == name || x == id)
        };
        if listed(&self.config.exclude_channels) {
            return None;
        }
        match kind {
            "dm" | "group_dm" => {
                if !self.config.include_dms {
                    return None;
                }
                if kind == "dm" {
                    let partner = c.get("user").and_then(Value::as_str).unwrap_or("");
                    let info = dir.get(partner);
                    if self.config.exclude_bot_dms
                        && (partner == "USLACKBOT" || info.is_some_and(|u| u.is_bot))
                    {
                        return None;
                    }
                    if let Some(u) = info {
                        let names = [&u.name, &u.display_name, &u.real_name];
                        if self
                            .config
                            .exclude_channels
                            .iter()
                            .any(|x| names.iter().any(|n| !n.is_empty() && *n == x))
                        {
                            return None;
                        }
                    }
                }
                Some(Class::Full)
            }
            _ if listed(&self.config.full_channels) => Some(Class::Full),
            _ => {
                let member = c.get("is_member").and_then(Value::as_bool).unwrap_or(true);
                (member && self.config.mention_scan == MentionScan::All)
                    .then_some(Class::Involvement)
            }
        }
    }

    /// Whether a parent message involves me (history scan rule).
    fn involves_me(&self, m: &Value) -> bool {
        let text = m.get("text").and_then(Value::as_str).unwrap_or("");
        if ["<!channel>", "<!here>", "<!everyone>"]
            .iter()
            .any(|b| text.contains(b))
        {
            return true;
        }
        match &self.me {
            Some(me) => {
                m.get("user").and_then(Value::as_str) == Some(me.as_str())
                    || text.contains(&format!("<@{me}"))
            }
            None => false,
        }
    }

    fn local_date(&self, ts: &str) -> String {
        render::ts_to_time(ts)
            .map(|t| {
                t.with_timezone(&render::tz(&self.tz))
                    .format("%Y-%m-%d")
                    .to_string()
            })
            .unwrap_or_default()
    }

    fn day_bounds(&self, date: &str) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
        let d = NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
        let zone = render::tz(&self.tz);
        let start = zone
            .from_local_datetime(&d.and_hms_opt(0, 0, 0)?)
            .earliest()?;
        let end = zone
            .from_local_datetime(&(d + ChronoDuration::days(1)).and_hms_opt(0, 0, 0)?)
            .earliest()?;
        Some((start.with_timezone(&Utc), end.with_timezone(&Utc)))
    }

    fn entry(
        &self,
        kind: SourceKind,
        id: String,
        created: Option<DateTime<Utc>>,
        url: Option<String>,
        bundle: RawBundle,
    ) -> FetchedEntry {
        FetchedEntry {
            source_ref: SourceRef {
                account_id: self.account.id.clone(),
                source_kind: kind,
                source_id: id,
                source_url: url,
                created_at: created,
                updated_at: None,
            },
            bundle,
        }
    }

    fn jsonl(messages: &[Value]) -> Vec<u8> {
        let mut s = String::new();
        for m in messages {
            s.push_str(&m.to_string());
            s.push('\n');
        }
        s.into_bytes()
    }

    fn max_ts(messages: &[Value], floor: Option<&str>) -> Option<String> {
        let mut best: Option<String> = floor.map(str::to_string);
        for m in messages {
            if let Some(ts) = m.get("ts").and_then(Value::as_str)
                && best.as_deref().is_none_or(|b| ts_gt(ts, b))
            {
                best = Some(ts.to_string());
            }
        }
        best
    }

    /// Messages that belong to day entries: not thread parents with replies,
    /// not thread replies, not join/leave noise.
    fn is_day_message(m: &Value) -> bool {
        let replies = m.get("reply_count").and_then(Value::as_u64).unwrap_or(0);
        let ts = m.get("ts").and_then(Value::as_str);
        let thread_ts = m.get("thread_ts").and_then(Value::as_str);
        let subtype = m.get("subtype").and_then(Value::as_str).unwrap_or("");
        replies == 0
            && (thread_ts.is_none() || thread_ts == ts)
            && !matches!(
                subtype,
                "channel_join"
                    | "channel_leave"
                    | "thread_broadcast"
                    | "group_join"
                    | "group_leave"
            )
    }

    /// Process one history window of a conversation into a discovery batch.
    fn window_batch(
        &self,
        host: &dyn SyncHost,
        conv: &Conv,
        messages: &[Value],
        prev_activity: Option<&str>,
        kinds: &[SourceKind],
    ) -> Result<(DiscoveryBatch, Option<String>), SourceError> {
        let mut batch = DiscoveryBatch::default();
        let want = |k: SourceKind| kinds.is_empty() || kinds.contains(&k);
        // Day entries (full conversations only).
        if conv.class == Class::Full && want(SourceKind::SlackDay) {
            let mut days: BTreeMap<String, Vec<Value>> = BTreeMap::new();
            for m in messages.iter().filter(|m| Self::is_day_message(m)) {
                let ts = m.get("ts").and_then(Value::as_str).unwrap_or("");
                days.entry(self.local_date(ts)).or_default().push(m.clone());
            }
            for (date, msgs) in days {
                let id = format!("{}:day:{date}", conv.id);
                let state = host.fetch_state(SourceKind::SlackDay, &id)?;
                let last = state
                    .as_ref()
                    .and_then(|s| s.get("last_ts"))
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let new: Vec<Value> = msgs
                    .into_iter()
                    .filter(|m| {
                        let ts = m.get("ts").and_then(Value::as_str).unwrap_or("");
                        last.as_deref().is_none_or(|l| ts_gt(ts, l))
                    })
                    .collect();
                if new.is_empty() {
                    continue;
                }
                let first_ts = new[0]
                    .get("ts")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let mut meta = conv.meta.clone();
                if last.is_none()
                    && let Some(p) = self.permalink(&conv.id, &first_ts)
                {
                    meta["permalink"] = json!(p);
                }
                let fetch_state = json!({"last_ts": Self::max_ts(&new, last.as_deref())});
                let created = self.day_bounds(&date).map(|(s, _)| s);
                batch.entries.push(
                    self.entry(
                        SourceKind::SlackDay,
                        id,
                        created,
                        meta.get("permalink")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        RawBundle {
                            mode: RawMode::Append,
                            objects: vec![RawObject {
                                role: RawRole::Primary,
                                media_type: "application/x-ndjson".into(),
                                ext: "jsonl".into(),
                                bytes: Self::jsonl(&new),
                            }],
                            fetch_state: Some(fetch_state),
                            metadata: meta,
                        },
                    ),
                );
            }
        }
        // Threads.
        let mut activity = prev_activity.map(str::to_string);
        for m in messages {
            let ts = m.get("ts").and_then(Value::as_str).unwrap_or("");
            if activity.as_deref().is_none_or(|a| ts_gt(ts, a)) {
                activity = Some(ts.to_string());
            }
            let replies = m.get("reply_count").and_then(Value::as_u64).unwrap_or(0);
            let is_parent = m
                .get("thread_ts")
                .and_then(Value::as_str)
                .is_none_or(|t| t == ts);
            if !is_parent {
                continue;
            }
            let wanted = want(SourceKind::SlackThread)
                && match conv.class {
                    Class::Full => replies > 0,
                    Class::Involvement => self.involves_me(m),
                };
            if !wanted {
                continue;
            }
            let id = format!("{}:{ts}", conv.id);
            let latest = m
                .get("latest_reply")
                .and_then(Value::as_str)
                .unwrap_or(ts)
                .to_string();
            let stored = host.fetch_state(SourceKind::SlackThread, &id)?;
            let known = stored
                .as_ref()
                .and_then(|s| s.get("last_ts"))
                .and_then(Value::as_str);
            let exists = host.entry_exists(SourceKind::SlackThread, &id)?;
            let needed = !exists || known.is_none_or(|k| ts_gt(&latest, k));
            if needed {
                batch.enqueue.push(QueueItem {
                    source_kind: SourceKind::SlackThread,
                    source_id: id.clone(),
                    reason: if exists { "new_replies" } else { "new_thread" }.into(),
                    hint: json!({"latest_reply": latest}),
                });
            }
            batch.cursors.push(CursorUpdate {
                source_kind: SourceKind::SlackThread,
                key: format!("watch:{id}"),
                value: Some(json!({"last_activity": latest})),
            });
        }
        Ok((batch, activity))
    }

    /// The local midnight at or before `t` in `slack.day_timezone`.
    fn local_midnight(&self, t: DateTime<Utc>) -> DateTime<Utc> {
        let date = t
            .with_timezone(&render::tz(&self.tz))
            .format("%Y-%m-%d")
            .to_string();
        self.day_bounds(&date).map_or(t, |(s, _)| s)
    }

    /// The local midnight `days` calendar days before the one at or before `t`
    /// (a calendar shift, so a DST change cannot break the alignment).
    fn midnight_before(&self, t: DateTime<Utc>, days: i64) -> DateTime<Utc> {
        let local = t.with_timezone(&render::tz(&self.tz)).date_naive();
        self.day_bounds(
            &(local - ChronoDuration::days(days))
                .format("%Y-%m-%d")
                .to_string(),
        )
        .map_or(t - ChronoDuration::days(days), |(s, _)| s)
    }

    fn ts_of(t: DateTime<Utc>) -> String {
        format!("{}.{:06}", t.timestamp(), t.timestamp_subsec_micros())
    }

    /// Scan one conversation: the forward step from its cursor, then the
    /// backward range requested with `--since` (ADR-0016). Returns the backward
    /// range that was fetched, for the involvement search.
    async fn scan_conversation(
        &self,
        host: &dyn SyncHost,
        conv: &Conv,
        opts: &SyncOptions,
    ) -> Result<Option<TimeRange>, SourceError> {
        let key = format!("conv:{}", conv.id);
        let run_start = opts.run_start(host.now());
        let initial_start = self.local_midnight(opts.initial_start(run_start));
        let cursor = host.cursor(SourceKind::SlackThread, &key)?;
        let last_activity = cursor
            .as_ref()
            .and_then(|c| c.get("last_activity"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let cov = cursor.as_ref().map(|c| Coverage {
            since: covered_since_of(c),
            until: c
                .get("oldest")
                .and_then(Value::as_str)
                .and_then(render::ts_to_time)
                .unwrap_or(run_start),
        });
        let mut plan = plan_ranges(
            cov,
            run_start,
            initial_start,
            opts.since.map(|t| self.local_midnight(t)),
            opts.until.map(|t| self.local_midnight(t)),
        );
        // Dormant conversations are only scanned forward in deep runs (full
        // channels excepted); the deep run continues from the stored cursor.
        if opts.mode == SyncMode::Normal
            && cursor.is_some()
            && !self.is_full_channel(conv)
            && let Some(a) = last_activity.as_deref().and_then(render::ts_to_time)
            && run_start - a > ChronoDuration::days(self.config.dormant_days)
        {
            plan.forward = None;
        }
        if let Some(fw) = plan.forward {
            self.scan_forward(
                host,
                conv,
                opts,
                fw,
                run_start,
                cov,
                initial_start,
                last_activity,
            )
            .await?;
        }
        let Some(bw) = plan.backward else {
            return Ok(None);
        };
        self.scan_backward(host, conv, opts, bw).await?;
        Ok(Some(bw.range))
    }

    #[allow(clippy::too_many_arguments)]
    async fn scan_forward(
        &self,
        host: &dyn SyncHost,
        conv: &Conv,
        opts: &SyncOptions,
        range: TimeRange,
        run_start: DateTime<Utc>,
        cov: Option<Coverage>,
        initial_start: DateTime<Utc>,
        mut activity: Option<String>,
    ) -> Result<(), SourceError> {
        // Without a cursor the first window defines the covered start.
        let covered_since = cov.map_or(Some(initial_start), |c| c.since);
        let mut w_start = range.from;
        while w_start < range.to {
            if host.is_cancelled() {
                return Err(SourceError::Cancelled);
            }
            let w_end = (w_start + ChronoDuration::days(WINDOW_DAYS)).min(range.to);
            let msgs = self
                .client()?
                .history(&conv.id, &Self::ts_of(w_start), &Self::ts_of(w_end))
                .await?;
            self.harvest(host, &msgs)?;
            let (mut batch, act) =
                self.window_batch(host, conv, &msgs, activity.as_deref(), &opts.kinds)?;
            activity = act;
            // The last window ends at the run start; the stored cursor stays a
            // little behind it to absorb clock skew (ADR-0016).
            let cursor_at = if w_end >= range.to {
                next_cursor(w_start, run_start, opts.overlap())
            } else {
                w_end
            };
            let mut value = json!({"oldest": Self::ts_of(cursor_at), "last_activity": activity});
            if let Some(s) = covered_since {
                value = with_covered_since(&value, s);
            }
            batch.cursors.push(CursorUpdate {
                source_kind: SourceKind::SlackThread,
                key: format!("conv:{}", conv.id),
                value: Some(value),
            });
            host.commit(batch)?;
            w_start = w_end;
        }
        Ok(())
    }

    /// Fetch a backward range newest window first, so that every committed window
    /// extends the contiguous coverage (ADR-0016).
    async fn scan_backward(
        &self,
        host: &dyn SyncHost,
        conv: &Conv,
        opts: &SyncOptions,
        plan: BackwardPlan,
    ) -> Result<(), SourceError> {
        let key = format!("conv:{}", conv.id);
        let mut w_end = plan.range.to;
        while w_end > plan.range.from {
            if host.is_cancelled() {
                return Err(SourceError::Cancelled);
            }
            let w_start = self
                .midnight_before(w_end, WINDOW_DAYS)
                .max(plan.range.from);
            let msgs = self
                .client()?
                .history(&conv.id, &Self::ts_of(w_start), &Self::ts_of(w_end))
                .await?;
            self.harvest(host, &msgs)?;
            // The conversation's activity marker belongs to the forward cursor.
            let (mut batch, _) = self.window_batch(host, conv, &msgs, None, &opts.kinds)?;
            if plan.record
                && let Some(cur) = host.cursor(SourceKind::SlackThread, &key)?
            {
                // Only a window that touches the covered interval extends it,
                // and never by moving the start forward.
                let known = covered_since_of(&cur);
                if known.is_none_or(|s| w_end >= s && w_start < s) {
                    batch.cursors.push(CursorUpdate {
                        source_kind: SourceKind::SlackThread,
                        key: key.clone(),
                        value: Some(with_covered_since(&cur, w_start)),
                    });
                }
            }
            host.commit(batch)?;
            w_end = w_start;
        }
        Ok(())
    }

    fn is_full_channel(&self, conv: &Conv) -> bool {
        let name = conv
            .meta
            .get("channel_name")
            .and_then(Value::as_str)
            .unwrap_or("");
        self.config
            .full_channels
            .iter()
            .any(|x| x.trim_start_matches('#') == name || *x == conv.id)
    }

    /// Enqueue watched threads by age; forget threads older than the watch window.
    fn enqueue_watched(&self, host: &dyn SyncHost, mode: SyncMode) -> Result<(), SourceError> {
        let now = host.now();
        let horizon = match mode {
            SyncMode::Normal => self.config.thread_hot_days,
            SyncMode::Deep => self.config.thread_watch_days,
        };
        let mut batch = DiscoveryBatch::default();
        for (key, v) in host.cursors(SourceKind::SlackThread, "watch:")? {
            let id = key.trim_start_matches("watch:").to_string();
            let Some(last) = v
                .get("last_activity")
                .and_then(Value::as_str)
                .and_then(render::ts_to_time)
            else {
                continue;
            };
            let age = now - last;
            if age > ChronoDuration::days(self.config.thread_watch_days) {
                batch.cursors.push(CursorUpdate {
                    source_kind: SourceKind::SlackThread,
                    key,
                    value: None,
                });
            } else if age <= ChronoDuration::days(horizon)
                && host.entry_exists(SourceKind::SlackThread, &id)?
            {
                batch.enqueue.push(QueueItem {
                    source_kind: SourceKind::SlackThread,
                    source_id: id,
                    reason: "watched".into(),
                    hint: json!({}),
                });
            }
        }
        if !batch.is_empty() {
            host.commit(batch)?;
        }
        Ok(())
    }

    /// Find threads with my replies through `search.messages`.
    async fn search_involvement(
        &self,
        host: &dyn SyncHost,
        convs: &BTreeMap<String, Conv>,
        excluded: &BTreeSet<String>,
        opts: &SyncOptions,
        range: Option<TimeRange>,
    ) -> Result<(), SourceError> {
        let Some(me) = &self.me else { return Ok(()) };
        // Recent activity by default; a backward range searches that range
        // instead (`after:` and `before:` are exclusive day bounds).
        let (after, before) = match range {
            Some(r) => (r.from, Some(r.to)),
            None => {
                let days = match opts.mode {
                    SyncMode::Normal => self.config.thread_hot_days,
                    SyncMode::Deep => self.config.thread_watch_days,
                };
                (
                    opts.run_start(host.now()) - ChronoDuration::days(days),
                    None,
                )
            }
        };
        let after = (after - ChronoDuration::days(1))
            .format("%Y-%m-%d")
            .to_string();
        let bound = match before {
            Some(b) => format!(
                " after:{after} before:{}",
                (b + ChronoDuration::days(1)).format("%Y-%m-%d")
            ),
            None => format!(" after:{after}"),
        };
        let mut queries = vec![format!("from:<@{me}>{bound}"), format!("<@{me}>{bound}")];
        for t in &self.config.search_terms {
            queries.push(format!("\"{}\"{bound}", t.replace('"', "")));
        }
        let mut batch = DiscoveryBatch::default();
        let mut seen = BTreeSet::new();
        for q in queries {
            if host.is_cancelled() {
                return Err(SourceError::Cancelled);
            }
            for m in self.client()?.search(&q, 10).await? {
                let Some(ch) = m.pointer("/channel/id").and_then(Value::as_str) else {
                    continue;
                };
                if excluded.contains(ch) {
                    continue;
                }
                let ts = m.get("ts").and_then(Value::as_str).unwrap_or("");
                let permalink = m.get("permalink").and_then(Value::as_str).unwrap_or("");
                let thread_ts = url::Url::parse(permalink).ok().and_then(|u| {
                    u.query_pairs()
                        .find(|(k, _)| k == "thread_ts")
                        .map(|(_, v)| v.to_string())
                });
                let root = match (thread_ts, convs.get(ch).map(|c| c.class)) {
                    (Some(t), _) => t,
                    // A top-level message in an involvement-only conversation.
                    (None, Some(Class::Involvement)) | (None, None) if !ts.is_empty() => {
                        ts.to_string()
                    }
                    _ => continue,
                };
                let id = format!("{ch}:{root}");
                if !seen.insert(id.clone()) {
                    continue;
                }
                let known = host.fetch_state(SourceKind::SlackThread, &id)?;
                let fresh = known
                    .as_ref()
                    .and_then(|s| s.get("last_ts"))
                    .and_then(Value::as_str)
                    .is_some_and(|k| !ts_gt(ts, k));
                if fresh {
                    continue;
                }
                batch.enqueue.push(QueueItem {
                    source_kind: SourceKind::SlackThread,
                    source_id: id,
                    reason: "mention".into(),
                    hint: json!({"latest_reply": ts}),
                });
            }
        }
        if !batch.is_empty() {
            host.commit(batch)?;
        }
        Ok(())
    }

    async fn fetch_thread(
        &self,
        host: &dyn SyncHost,
        req: &FetchRequest,
    ) -> Result<FetchOutcome, SourceError> {
        let Some((channel, thread_ts)) = req.source_id.split_once(':') else {
            return Err(SourceError::Parse(format!(
                "bad thread id {:?}",
                req.source_id
            )));
        };
        let last = if req.full {
            None
        } else {
            req.fetch_state
                .as_ref()
                .and_then(|s| s.get("last_ts"))
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        let msgs = match self
            .client()?
            .replies(channel, thread_ts, last.as_deref())
            .await
        {
            Ok(m) => m,
            Err(SourceError::Api(e))
                if e.contains("thread_not_found") || e.contains("channel_not_found") =>
            {
                return Ok(FetchOutcome::NotFound(e));
            }
            Err(e) => return Err(e),
        };
        let new: Vec<Value> = msgs
            .into_iter()
            .filter(|m| {
                let ts = m.get("ts").and_then(Value::as_str).unwrap_or("");
                last.as_deref().is_none_or(|l| ts_gt(ts, l))
            })
            .collect();
        if new.is_empty() {
            return Ok(if last.is_some() {
                FetchOutcome::Unchanged
            } else {
                FetchOutcome::NotFound("thread has no messages".into())
            });
        }
        self.harvest(host, &new)?;
        let mut meta = self.channel_meta(host, channel).await?;
        let permalink = if last.is_none() {
            self.permalink_or_api(channel, thread_ts).await
        } else {
            req.metadata
                .get("permalink")
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        if let Some(p) = &permalink {
            meta["permalink"] = json!(p);
        }
        let reply_count = new
            .iter()
            .find(|m| m.get("ts").and_then(Value::as_str) == Some(thread_ts))
            .and_then(|m| m.get("reply_count"))
            .cloned()
            .or_else(|| {
                req.fetch_state
                    .as_ref()
                    .and_then(|s| s.get("reply_count"))
                    .cloned()
            });
        let fetch_state =
            json!({"last_ts": Self::max_ts(&new, last.as_deref()), "reply_count": reply_count});
        Ok(FetchOutcome::Fetched(Box::new(self.entry(
            SourceKind::SlackThread,
            req.source_id.clone(),
            render::ts_to_time(thread_ts),
            permalink,
            RawBundle {
                mode: if last.is_some() {
                    RawMode::Append
                } else {
                    RawMode::Replace
                },
                objects: vec![RawObject {
                    role: RawRole::Primary,
                    media_type: "application/x-ndjson".into(),
                    ext: "jsonl".into(),
                    bytes: Self::jsonl(&new),
                }],
                fetch_state: Some(fetch_state),
                metadata: meta,
            },
        ))))
    }

    async fn fetch_day(
        &self,
        host: &dyn SyncHost,
        req: &FetchRequest,
    ) -> Result<FetchOutcome, SourceError> {
        let mut parts = req.source_id.splitn(3, ':');
        let (Some(channel), Some("day"), Some(date)) = (parts.next(), parts.next(), parts.next())
        else {
            return Err(SourceError::Parse(format!(
                "bad day id {:?}",
                req.source_id
            )));
        };
        let (start, end) = self
            .day_bounds(date)
            .ok_or_else(|| SourceError::Parse(format!("bad date in {:?}", req.source_id)))?;
        let last = if req.full {
            None
        } else {
            req.fetch_state
                .as_ref()
                .and_then(|s| s.get("last_ts"))
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        let msgs = self
            .client()?
            .history(channel, &Self::ts_of(start), &Self::ts_of(end))
            .await?;
        let new: Vec<Value> = msgs
            .into_iter()
            .filter(Self::is_day_message)
            .filter(|m| {
                let ts = m.get("ts").and_then(Value::as_str).unwrap_or("");
                self.local_date(ts) == date && last.as_deref().is_none_or(|l| ts_gt(ts, l))
            })
            .collect();
        if new.is_empty() {
            return Ok(if last.is_some() {
                FetchOutcome::Unchanged
            } else {
                FetchOutcome::NotFound("no messages on that day".into())
            });
        }
        self.harvest(host, &new)?;
        let mut meta = self.channel_meta(host, channel).await?;
        let first_ts = new[0]
            .get("ts")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let permalink = match req.metadata.get("permalink").and_then(Value::as_str) {
            Some(p) => Some(p.to_string()),
            None => self.permalink_or_api(channel, &first_ts).await,
        };
        if let Some(p) = &permalink {
            meta["permalink"] = json!(p);
        }
        Ok(FetchOutcome::Fetched(Box::new(self.entry(
            SourceKind::SlackDay,
            req.source_id.clone(),
            Some(start),
            permalink,
            RawBundle {
                mode: if last.is_some() {
                    RawMode::Append
                } else {
                    RawMode::Replace
                },
                objects: vec![RawObject {
                    role: RawRole::Primary,
                    media_type: "application/x-ndjson".into(),
                    ext: "jsonl".into(),
                    bytes: Self::jsonl(&new),
                }],
                fetch_state: Some(json!({"last_ts": Self::max_ts(&new, last.as_deref())})),
                metadata: meta,
            },
        ))))
    }
}

#[async_trait]
impl Source for SlackSource {
    fn kinds(&self) -> &'static [SourceKind] {
        &[SourceKind::SlackThread, SourceKind::SlackDay]
    }

    fn account_kind(&self) -> AccountKind {
        AccountKind::Slack
    }

    async fn sync(&self, host: &dyn SyncHost, opts: &SyncOptions) -> Result<(), SourceError> {
        self.refresh_users(host).await?;
        let dir: Directory = host
            .cache_get_stale(USERS_CACHE)?
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();
        let raw_convs = self.client()?.conversations().await?;
        let mut channel_cache = serde_json::Map::new();
        let mut convs: BTreeMap<String, Conv> = BTreeMap::new();
        let mut excluded = BTreeSet::new();
        for c in &raw_convs {
            let Some(id) = c.get("id").and_then(Value::as_str) else {
                continue;
            };
            let meta = Self::conv_meta(c);
            channel_cache.insert(id.to_string(), meta.clone());
            match self.classify(c, &dir) {
                Some(class) => {
                    convs.insert(
                        id.to_string(),
                        Conv {
                            id: id.to_string(),
                            meta,
                            class,
                        },
                    );
                }
                None => {
                    excluded.insert(id.to_string());
                }
            }
        }
        host.cache_put(
            CHANNELS_CACHE,
            &Value::Object(channel_cache),
            Duration::from_secs(86_400),
        )?;
        let wants_threads = opts.kinds.is_empty() || opts.kinds.contains(&SourceKind::SlackThread);
        // The union of the backward ranges fetched by the conversations.
        let mut backward: Option<TimeRange> = None;
        for conv in convs.values() {
            if host.is_cancelled() {
                return Err(SourceError::Cancelled);
            }
            if let Some(r) = self.scan_conversation(host, conv, opts).await? {
                backward = Some(backward.map_or(r, |b| TimeRange {
                    from: b.from.min(r.from),
                    to: b.to.max(r.to),
                }));
            }
        }
        if wants_threads {
            self.enqueue_watched(host, opts.mode)?;
            if self.config.mention_scan != MentionScan::Off {
                self.search_involvement(host, &convs, &excluded, opts, None)
                    .await?;
                if backward.is_some() {
                    self.search_involvement(host, &convs, &excluded, opts, backward)
                        .await?;
                }
            }
        }
        Ok(())
    }

    async fn fetch(
        &self,
        host: &dyn SyncHost,
        req: &FetchRequest,
    ) -> Result<FetchOutcome, SourceError> {
        match req.source_kind {
            SourceKind::SlackThread => self.fetch_thread(host, req).await,
            SourceKind::SlackDay => self.fetch_day(host, req).await,
            k => Err(SourceError::Unsupported(format!(
                "{k} is not a Slack source kind"
            ))),
        }
    }

    fn load_snapshot(&self, host: &dyn SyncHost) -> Result<Value, SourceError> {
        Ok(json!({"users": host.cache_get_stale(USERS_CACHE)?.unwrap_or(json!({}))}))
    }

    fn normalize(
        &self,
        ctx: &NormalizeCtx,
        input: &NormalizeInput,
    ) -> Result<NormalizeOutcome, SourceError> {
        render::normalize(ctx, input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults() {
        let c: SlackConfig = serde_json::from_value(json!({})).unwrap();
        assert!(c.include_dms);
        assert_eq!(c.dormant_days, 30);
        assert_eq!(c.mention_scan, MentionScan::All);
        let d: SlackConfig = serde_json::from_value(default_config_json()).unwrap();
        assert_eq!(d.thread_watch_days, 45);
    }

    #[test]
    fn day_message_rules() {
        assert!(SlackSource::is_day_message(&json!({"ts": "1.0"})));
        assert!(!SlackSource::is_day_message(
            &json!({"ts": "1.0", "thread_ts": "1.0", "reply_count": 2})
        ));
        assert!(!SlackSource::is_day_message(
            &json!({"ts": "2.0", "thread_ts": "1.0"})
        ));
        assert!(!SlackSource::is_day_message(
            &json!({"ts": "2.0", "subtype": "channel_join"})
        ));
    }
}
