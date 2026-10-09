//! The `google.meet` source (source-google-meet.md).

use async_trait::async_trait;
use chrono::{DateTime, Duration as ChronoDuration, SecondsFormat, Utc};
use second_brain_kernel::coverage::{
    Coverage, TimeRange, covered_since_of, next_cursor, plan_ranges, with_covered_since,
};
use second_brain_kernel::source::{Source, SourceError, SyncHost};
use second_brain_kernel::util::parse_ts;
use second_brain_kernel::{
    AccountCtx, AccountKind, CursorUpdate, DiscoveryBatch, FetchOutcome, FetchRequest,
    FetchedEntry, Generator, NormalizeCtx, NormalizeInput, NormalizeOutcome, Normalized,
    PromptKind, QueueItem, RawBundle, RawMode, RawObject, RawRole, SourceKind, SourceRef,
    SummaryInput, SyncOptions,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::api::{FOLDER_MIME, GOOGLE_DOC_MIME, GoogleApi, doc_id_from_url};
use crate::gemini;

/// Name of the Drive folder where Meet stores notes.
pub const MEET_FOLDER_NAME: &str = "Meet Recordings";

fn default_strategies() -> Vec<String> {
    vec!["calendar".into(), "drive".into()]
}
fn d3() -> i64 {
    3
}

/// Per-account configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct MeetConfig {
    #[serde(default)]
    pub features: Vec<String>,
    /// Ordered discovery strategies (`calendar`, `drive`).
    #[serde(default = "default_strategies")]
    pub meet_strategies: Vec<String>,
    /// Days the Calendar window reaches behind its cursor, because notes are
    /// attached to an event after it ends (ADR-0016).
    #[serde(default = "d3")]
    pub calendar_overlap_days: i64,
    /// The "Meet Recordings" folder, when detection by name is not wanted.
    #[serde(default)]
    pub meet_folder_id: Option<String>,
}

/// The default `config` written by `sb account add google`.
pub fn default_config_json(features: &[String]) -> Value {
    json!({
        "features": features,
        "meet_strategies": ["calendar", "drive"],
        "calendar_overlap_days": 3,
    })
}

/// The Google Meet notes source for one account.
pub struct MeetSource {
    account: AccountCtx,
    config: MeetConfig,
    api: Option<GoogleApi>,
}

fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

impl MeetSource {
    /// Build the source. Without an API client only `normalize` works.
    pub fn new(account: AccountCtx, api: Option<GoogleApi>) -> Result<Self, SourceError> {
        let config: MeetConfig = serde_json::from_value(account.config.clone())
            .map_err(|e| SourceError::Parse(format!("Google account config: {e}")))?;
        Ok(MeetSource {
            account,
            config,
            api,
        })
    }

    fn api(&self) -> Result<&GoogleApi, SourceError> {
        self.api.as_ref().ok_or_else(|| {
            SourceError::Auth("no Google credentials stored for this account".into())
        })
    }

    pub fn meet_enabled(&self) -> bool {
        self.config.features.is_empty() || self.config.features.iter().any(|f| f == "meet")
    }

    /// Calendar discovery: the forward step from the stored cursor, then the
    /// backward range requested with `--since` (ADR-0016).
    async fn sync_calendar(
        &self,
        host: &dyn SyncHost,
        opts: &SyncOptions,
    ) -> Result<(), SourceError> {
        let run_start = opts.run_start(host.now());
        let initial_start = opts.initial_start(run_start);
        let cursor = host.cursor(SourceKind::GoogleMeet, "calendar")?;
        let cov = cursor.as_ref().and_then(|c| {
            Some(Coverage {
                since: covered_since_of(c),
                until: c
                    .get("last_time_max")
                    .and_then(Value::as_str)
                    .and_then(parse_ts)?,
            })
        });
        let plan = plan_ranges(cov, run_start, initial_start, opts.since, opts.until);
        if let Some(fw) = plan.forward {
            // Notes are attached after an event ends: look back past the cursor.
            let min = if cov.is_some() {
                fw.from - ChronoDuration::days(self.config.calendar_overlap_days)
            } else {
                fw.from
            };
            let mut value = json!({
                "last_time_max": rfc3339(next_cursor(fw.from, run_start, opts.overlap())),
            });
            if let Some(s) = cov.map_or(Some(initial_start), |c| c.since) {
                value = with_covered_since(&value, s);
            }
            let max = run_start + ChronoDuration::hours(1);
            self.list_events(host, min, max, Some(value)).await?;
        }
        if let Some(bw) = plan.backward {
            // The forward step may have just created the cursor.
            let current = host.cursor(SourceKind::GoogleMeet, "calendar")?;
            let value = bw
                .record
                .then(|| current.and_then(|c| extended(&c, bw.range)))
                .flatten();
            self.list_events(host, bw.range.from, bw.range.to, value)
                .await?;
        }
        Ok(())
    }

    /// List events in `[min, max)` and enqueue the notes attached to them. The
    /// cursor `final_cursor` is committed together with the last page only, so
    /// it never runs ahead of undiscovered events.
    async fn list_events(
        &self,
        host: &dyn SyncHost,
        min: DateTime<Utc>,
        max: DateTime<Utc>,
        final_cursor: Option<Value>,
    ) -> Result<(), SourceError> {
        let mut page: Option<String> = None;
        loop {
            if host.is_cancelled() {
                return Err(SourceError::Cancelled);
            }
            let (items, next) = self
                .api()?
                .events(&rfc3339(min), &rfc3339(max), page.as_deref())
                .await?;
            let mut batch = DiscoveryBatch::default();
            for ev in &items {
                if ev.get("status").and_then(Value::as_str) == Some("cancelled") {
                    continue;
                }
                let declined = ev
                    .get("attendees")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .any(|a| {
                        a.get("self").and_then(Value::as_bool) == Some(true)
                            && a.get("responseStatus").and_then(Value::as_str) == Some("declined")
                    });
                if declined {
                    continue;
                }
                let attendees: Vec<Value> = ev
                    .get("attendees")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter(|a| a.get("responseStatus").and_then(Value::as_str) != Some("declined"))
                    .filter(|a| a.get("resource").and_then(Value::as_bool) != Some(true))
                    .map(|a| {
                        let email = a.get("email").and_then(Value::as_str);
                        let name = a
                            .get("displayName")
                            .and_then(Value::as_str)
                            .or(email)
                            .unwrap_or("");
                        json!({"name": name, "email": email})
                    })
                    .collect();
                for att in ev
                    .get("attachments")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if att.get("mimeType").and_then(Value::as_str) != Some(GOOGLE_DOC_MIME) {
                        continue;
                    }
                    let Some(id) = att
                        .get("fileId")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .or_else(|| {
                            att.get("fileUrl")
                                .and_then(Value::as_str)
                                .and_then(doc_id_from_url)
                        })
                    else {
                        continue;
                    };
                    batch.enqueue.push(QueueItem {
                        source_kind: SourceKind::GoogleMeet,
                        source_id: id,
                        reason: "calendar_attachment".into(),
                        hint: json!({
                            "event_title": ev.get("summary"),
                            "event_start": ev.pointer("/start/dateTime").or_else(|| ev.pointer("/start/date")),
                            "recurring": ev.get("recurringEventId").is_some(),
                            "event_url": ev.get("htmlLink"),
                            "attendees": attendees,
                        }),
                    });
                }
            }
            if next.is_none()
                && let Some(v) = &final_cursor
            {
                batch.cursors.push(CursorUpdate {
                    source_kind: SourceKind::GoogleMeet,
                    key: "calendar".into(),
                    value: Some(v.clone()),
                });
            }
            host.commit(batch)?;
            match next {
                Some(n) => page = Some(n),
                None => return Ok(()),
            }
        }
    }

    async fn meet_folder(&self, host: &dyn SyncHost) -> Result<Option<String>, SourceError> {
        if let Some(id) = &self.config.meet_folder_id {
            return Ok(Some(id.clone()));
        }
        if let Some(v) = host.cursor(SourceKind::GoogleMeet, "drive_folder")?
            && let Some(id) = v.get("id").and_then(Value::as_str)
        {
            return Ok(Some(id.to_string()));
        }
        let q = format!(
            "name = '{MEET_FOLDER_NAME}' and mimeType = '{FOLDER_MIME}' and 'me' in owners and trashed = false"
        );
        let (files, _) = self.api()?.list(&q, "createdTime", None).await?;
        let id = files
            .first()
            .and_then(|f| f.get("id"))
            .and_then(Value::as_str)
            .map(str::to_string);
        if let Some(id) = &id {
            host.commit(DiscoveryBatch {
                cursors: vec![CursorUpdate {
                    source_kind: SourceKind::GoogleMeet,
                    key: "drive_folder".into(),
                    value: Some(json!({"id": id})),
                }],
                ..Default::default()
            })?;
        }
        Ok(id)
    }

    async fn sync_drive(&self, host: &dyn SyncHost, opts: &SyncOptions) -> Result<(), SourceError> {
        let Some(folder) = self.meet_folder(host).await? else {
            tracing::info!(account = %self.account.id, "no \"{MEET_FOLDER_NAME}\" folder found; skipping the drive strategy");
            return Ok(());
        };
        let run_start = opts.run_start(host.now());
        let initial_start = opts.initial_start(run_start);
        let cursor = host.cursor(SourceKind::GoogleMeet, "drive")?;
        let cov = cursor.as_ref().and_then(|c| {
            Some(Coverage {
                since: covered_since_of(c),
                until: c
                    .get("modified_after")
                    .and_then(Value::as_str)
                    .and_then(parse_ts)?,
            })
        });
        let plan = plan_ranges(cov, run_start, initial_start, opts.since, opts.until);
        if let Some(fw) = plan.forward {
            let covered_since = cov.map_or(Some(initial_start), |c| c.since);
            self.drive_forward(host, &folder, fw.from, run_start, opts, covered_since)
                .await?;
        }
        if let Some(bw) = plan.backward {
            let q = format!(
                "'{folder}' in parents and mimeType = '{GOOGLE_DOC_MIME}' and trashed = false and modifiedTime > '{}' and modifiedTime <= '{}'",
                rfc3339(bw.range.from),
                rfc3339(bw.range.to)
            );
            // Backward ranges never touch the forward cursor `modified_after`.
            let value = bw
                .record
                .then(|| host.cursor(SourceKind::GoogleMeet, "drive").ok().flatten())
                .flatten()
                .and_then(|c| extended(&c, bw.range));
            self.drive_pages(host, &q, |_| None, value).await?;
        }
        Ok(())
    }

    /// Forward step: files modified after `from`, oldest first. While paging, the
    /// cursor follows the newest file seen; the last page stores the run start
    /// minus the overlap.
    async fn drive_forward(
        &self,
        host: &dyn SyncHost,
        folder: &str,
        from: DateTime<Utc>,
        run_start: DateTime<Utc>,
        opts: &SyncOptions,
        covered_since: Option<DateTime<Utc>>,
    ) -> Result<(), SourceError> {
        let after = rfc3339(from);
        let q = format!(
            "'{folder}' in parents and mimeType = '{GOOGLE_DOC_MIME}' and trashed = false and modifiedTime > '{after}'"
        );
        let with_since = |mut v: Value| {
            if let Some(s) = covered_since {
                v = with_covered_since(&v, s);
            }
            v
        };
        let last = with_since(
            json!({"modified_after": rfc3339(next_cursor(from, run_start, opts.overlap()))}),
        );
        let mut newest = after;
        self.drive_pages(
            host,
            &q,
            |m| {
                if m > newest.as_str() {
                    newest = m.to_string();
                }
                Some(with_since(json!({"modified_after": newest})))
            },
            Some(last),
        )
        .await
    }

    /// Page through a Drive query and enqueue the notes. `progress` maps each
    /// file's `modifiedTime` to the cursor to store after its page (the listing is
    /// ordered by `modifiedTime`); `final_cursor` replaces it on the last page.
    async fn drive_pages(
        &self,
        host: &dyn SyncHost,
        q: &str,
        mut progress: impl FnMut(&str) -> Option<Value>,
        final_cursor: Option<Value>,
    ) -> Result<(), SourceError> {
        let mut page: Option<String> = None;
        loop {
            if host.is_cancelled() {
                return Err(SourceError::Cancelled);
            }
            let (files, next) = self.api()?.list(q, "modifiedTime", page.as_deref()).await?;
            let mut batch = DiscoveryBatch::default();
            let mut cursor = None;
            for f in &files {
                let Some(id) = f.get("id").and_then(Value::as_str) else {
                    continue;
                };
                if let Some(m) = f.get("modifiedTime").and_then(Value::as_str) {
                    cursor = progress(m).or(cursor);
                }
                batch.enqueue.push(QueueItem {
                    source_kind: SourceKind::GoogleMeet,
                    source_id: id.to_string(),
                    reason: "drive".into(),
                    hint: json!({}),
                });
            }
            let cursor = if next.is_none() {
                final_cursor.clone()
            } else {
                cursor
            };
            if let Some(v) = cursor {
                batch.cursors.push(CursorUpdate {
                    source_kind: SourceKind::GoogleMeet,
                    key: "drive".into(),
                    value: Some(v),
                });
            }
            host.commit(batch)?;
            match next {
                Some(n) => page = Some(n),
                None => return Ok(()),
            }
        }
    }
}

/// `cursor` with `covered_since` moved back to the start of `range`, when the
/// range touches the covered interval and really extends it.
fn extended(cursor: &Value, range: TimeRange) -> Option<Value> {
    let known = covered_since_of(cursor);
    known
        .is_none_or(|s| range.to >= s && range.from < s)
        .then(|| with_covered_since(cursor, range.from))
}

#[async_trait]
impl Source for MeetSource {
    fn kinds(&self) -> &'static [SourceKind] {
        &[SourceKind::GoogleMeet]
    }

    fn account_kind(&self) -> AccountKind {
        AccountKind::Google
    }

    fn supports_sync(&self) -> bool {
        self.meet_enabled()
    }

    async fn sync(&self, host: &dyn SyncHost, opts: &SyncOptions) -> Result<(), SourceError> {
        for s in &self.config.meet_strategies {
            match s.as_str() {
                "calendar" => self.sync_calendar(host, opts).await?,
                "drive" => self.sync_drive(host, opts).await?,
                other => tracing::warn!(strategy = other, "unknown google.meet strategy; skipped"),
            }
        }
        Ok(())
    }

    async fn fetch(
        &self,
        _host: &dyn SyncHost,
        req: &FetchRequest,
    ) -> Result<FetchOutcome, SourceError> {
        let api = self.api()?;
        let Some(file) = api.file(&req.source_id).await? else {
            return Ok(FetchOutcome::NotFound(
                "the notes document is not accessible".into(),
            ));
        };
        if file.get("mimeType").and_then(Value::as_str) != Some(GOOGLE_DOC_MIME) {
            return Ok(FetchOutcome::NotApplicable("not a Google Doc".into()));
        }
        let modified = file
            .get("modifiedTime")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let stored = req
            .fetch_state
            .as_ref()
            .and_then(|s| s.get("modified_time"))
            .and_then(Value::as_str);
        if !req.full && stored == Some(modified.as_str()) {
            return Ok(FetchOutcome::Unchanged);
        }
        let Some(notes) = api.export(&req.source_id, "text/markdown").await? else {
            return Ok(FetchOutcome::NotFound("export failed: not found".into()));
        };
        let notes_text = String::from_utf8_lossy(&notes).to_string();
        let Some(parsed) = gemini::parse(&notes_text) else {
            return Ok(FetchOutcome::NotApplicable(
                "not a Gemini notes document".into(),
            ));
        };
        let mut objects = vec![RawObject {
            role: RawRole::Notes,
            media_type: "text/markdown".into(),
            ext: "md".into(),
            bytes: notes,
        }];
        // A separate transcript document linked from the notes.
        if let Some(tid) = parsed.transcript_url.as_deref().and_then(doc_id_from_url)
            && tid != req.source_id
        {
            match api.export(&tid, "text/markdown").await {
                Ok(Some(t)) => objects.push(RawObject {
                    role: RawRole::Transcript,
                    media_type: "text/markdown".into(),
                    ext: "md".into(),
                    bytes: t,
                }),
                Ok(None) => tracing::debug!(transcript = %tid, "linked transcript not accessible"),
                Err(e) => {
                    tracing::info!(transcript = %tid, error = %e, "could not export the linked transcript")
                }
            }
        }
        let folder_name = match file.pointer("/parents/0").and_then(Value::as_str) {
            Some(p) => api
                .file(p)
                .await
                .ok()
                .flatten()
                .and_then(|f| f.get("name").and_then(Value::as_str).map(str::to_string))
                .filter(|n| n != MEET_FOLDER_NAME),
            None => None,
        };
        let mut metadata = json!({
            "drive_file_id": req.source_id,
            "created_time": file.get("createdTime"),
            "modified_time": modified,
            "folder_name": folder_name,
        });
        if let Some(h) = req.hint.as_object() {
            for (k, v) in h {
                metadata[k] = v.clone();
            }
        }
        let created = metadata
            .get("event_start")
            .and_then(Value::as_str)
            .and_then(parse_ts)
            .or_else(|| {
                file.get("createdTime")
                    .and_then(Value::as_str)
                    .and_then(parse_ts)
            });
        Ok(FetchOutcome::Fetched(Box::new(FetchedEntry {
            source_ref: SourceRef {
                account_id: self.account.id.clone(),
                source_kind: SourceKind::GoogleMeet,
                source_id: req.source_id.clone(),
                source_url: Some(format!(
                    "https://docs.google.com/document/d/{}/edit",
                    req.source_id
                )),
                created_at: created,
                updated_at: parse_ts(&modified),
            },
            bundle: RawBundle {
                mode: RawMode::Replace,
                objects,
                fetch_state: Some(json!({"modified_time": modified})),
                metadata,
            },
        })))
    }

    fn normalize(
        &self,
        _ctx: &NormalizeCtx,
        input: &NormalizeInput,
    ) -> Result<NormalizeOutcome, SourceError> {
        normalize(input)
    }
}

/// Pure normalization of stored notes (and transcript) raw data.
pub fn normalize(input: &NormalizeInput) -> Result<NormalizeOutcome, SourceError> {
    let text_of = |role: RawRole| -> Option<String> {
        let parts: Vec<String> = input
            .segments
            .iter()
            .filter(|s| s.role == role)
            .map(|s| String::from_utf8_lossy(&s.bytes).to_string())
            .collect();
        (!parts.is_empty()).then(|| parts.join("\n"))
    };
    let Some(notes) = text_of(RawRole::Notes) else {
        return Ok(NormalizeOutcome::NotApplicable(
            "no notes raw object".into(),
        ));
    };
    let Some(parsed) = gemini::parse(&notes) else {
        return Ok(NormalizeOutcome::NotApplicable(
            "not a Gemini notes document".into(),
        ));
    };
    let meta = &input.fetch_metadata;
    let s = |k: &str| {
        meta.get(k)
            .and_then(Value::as_str)
            .filter(|v| !v.trim().is_empty())
    };
    let title = match (&parsed.title, parsed.title_is_auto) {
        (Some(t), false) => t.clone(),
        (t, _) => s("event_title")
            .map(str::to_string)
            .or_else(|| {
                s("folder_name")
                    .map(gemini::strip_trailing_date)
                    .filter(|n| !n.is_empty())
            })
            .or_else(|| t.clone())
            .unwrap_or_else(|| "Meeting".to_string()),
    };
    let (title, recurring_suffix) = gemini::strip_recurring(&title);
    let separate = text_of(RawRole::Transcript)
        .map(|t| gemini::clean(&t))
        .filter(|t| !t.is_empty());
    let transcript = separate.or(parsed.transcript.clone());
    let has_transcript = transcript.is_some();
    let participants: Value = if parsed.participants.is_empty() && parsed.absentees.is_empty() {
        meta.get("attendees").cloned().unwrap_or(json!([]))
    } else {
        json!(parsed.participants)
    };
    let recurring = meta
        .get("recurring")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || parsed.recurring_in_title
        || recurring_suffix
        || s("folder_name").is_some_and(|f| f.to_lowercase().contains("(recurring)"));
    let metadata = json!({
        "participants": participants,
        "absentees": parsed.absentees,
        "recurring": recurring,
        "calendar_url": parsed.calendar_url.clone().or_else(|| s("event_url").map(str::to_string)),
        "transcript_url": parsed.transcript_url,
        "has_transcript": has_transcript,
    });
    let created = s("event_start")
        .and_then(parse_ts)
        .or_else(|| s("created_time").and_then(parse_ts))
        .or(input.source_ref.created_at);
    let updated = s("modified_time")
        .and_then(parse_ts)
        .or(input.source_ref.updated_at);
    let body = transcript.unwrap_or_else(|| parsed.notes_text.clone());
    Ok(NormalizeOutcome::Entry(Box::new(Normalized {
        title: title.clone(),
        source_url: Some(format!(
            "https://docs.google.com/document/d/{}/edit",
            input.source_ref.source_id
        )),
        source_created_at: created,
        source_updated_at: updated,
        metadata,
        sections: parsed.sections,
        summary_input: Some(SummaryInput {
            source_kind: SourceKind::GoogleMeet,
            prompt: PromptKind::Meeting,
            title,
            date: created,
            context: None,
            body,
            message_count: None,
            want_details: true,
        }),
        native_summary: Some(Generator::gemini_meet_notes()),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use second_brain_kernel::{AccountId, RawSegment};

    fn input(notes: &str, transcript: Option<&str>, meta: Value) -> NormalizeInput {
        let mut segments = vec![RawSegment {
            role: RawRole::Notes,
            seq: 0,
            media_type: "text/markdown".into(),
            bytes: notes.as_bytes().to_vec(),
        }];
        if let Some(t) = transcript {
            segments.push(RawSegment {
                role: RawRole::Transcript,
                seq: 0,
                media_type: "text/markdown".into(),
                bytes: t.as_bytes().to_vec(),
            });
        }
        NormalizeInput {
            source_ref: SourceRef {
                account_id: AccountId::new("work").unwrap(),
                source_kind: SourceKind::GoogleMeet,
                source_id: "DOC1".into(),
                source_url: None,
                created_at: None,
                updated_at: None,
            },
            fetch_metadata: meta,
            segments,
        }
    }

    const AUTO: &str =
        "## **2026／07／27 09：15 JST に開始した会議**\n\n### **概要**\n\n短い会議。\n";

    #[test]
    fn auto_title_falls_back_to_event_then_folder() {
        let NormalizeOutcome::Entry(n) =
            normalize(&input(AUTO, None, json!({"event_title": "Design Review"}))).unwrap()
        else {
            panic!()
        };
        assert_eq!(n.title, "Design Review");
        let NormalizeOutcome::Entry(n) = normalize(&input(
            AUTO,
            None,
            json!({"folder_name": "Design Review (recurring) - 2026/07/27 09:15 JST"}),
        ))
        .unwrap() else {
            panic!()
        };
        assert_eq!(n.title, "Design Review");
        assert_eq!(n.metadata["recurring"], true);
        assert_eq!(n.metadata["has_transcript"], false);
        assert_eq!(
            n.summary_input.unwrap().body,
            "## **2026／07／27 09：15 JST に開始した会議**\n\n### **概要**\n\n短い会議。"
        );
    }

    #[test]
    fn separate_transcript_wins_and_dates_come_from_metadata() {
        let notes = "## Sync\n\n### Summary\n\nx\n\n# **📖 Transcript**\n\nembedded words\n";
        let meta = json!({"event_start": "2026-09-03T00:15:00Z", "modified_time": "2026-09-03T01:00:00Z",
                          "attendees": [{"name": "Dana", "email": "dana@example.test"}]});
        let NormalizeOutcome::Entry(n) =
            normalize(&input(notes, Some("separate \\- words"), meta)).unwrap()
        else {
            panic!()
        };
        let si = n.summary_input.unwrap();
        assert_eq!(si.body, "separate - words");
        assert!(si.want_details);
        assert_eq!(n.source_created_at, parse_ts("2026-09-03T00:15:00Z"));
        assert_eq!(
            n.metadata["participants"][0]["name"], "Dana",
            "calendar attendees without an Invited line"
        );
        assert_eq!(n.native_summary, Some(Generator::gemini_meet_notes()));
        assert_eq!(
            n.source_url.as_deref(),
            Some("https://docs.google.com/document/d/DOC1/edit")
        );
    }

    #[test]
    fn agenda_docs_are_not_applicable() {
        assert!(matches!(
            normalize(&input("# Agenda\n\n- a\n", None, json!({}))).unwrap(),
            NormalizeOutcome::NotApplicable(_)
        ));
    }
}
