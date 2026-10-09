//! Summary policy: which profile applies to a source kind, thresholds, and the
//! decision taken after normalization.

use std::collections::BTreeMap;

use second_brain_kernel::util::{BODY_HASH_PREFIX, body_hash, summary_input_hash};
use second_brain_kernel::{
    GeneratorKind, Normalized, PromptKind, SourceKind, SummaryInput, SummaryStatus,
};
use second_brain_llm::{NATIVE, Profile};
use second_brain_store::{Catalog, Entry, SummaryDecision, SummaryRecord};

use crate::error::PipelineError;

/// Settings relevant to summarization, loaded once per run.
#[derive(Debug, Clone)]
pub struct SummaryPolicy {
    pub default_profile: Option<String>,
    pub per_kind: BTreeMap<SourceKind, String>,
    pub profiles: BTreeMap<String, Profile>,
    /// Profiles that failed validation, with the reason.
    pub invalid_profiles: BTreeMap<String, String>,
    pub min_chars: usize,
    pub min_messages: usize,
    pub max_attempts: i64,
    pub language: String,
    pub prices: Option<serde_json::Value>,
    /// Weekly and monthly spend caps in USD; `None` = disabled (ADR-0013).
    pub weekly_cap_usd: Option<f64>,
    pub monthly_cap_usd: Option<f64>,
    /// IANA time zone that defines the week and month boundaries.
    pub budget_timezone: String,
}

/// Fallback caps if a stored cap is not a number (ADR-0013).
const DEFAULT_WEEKLY_CAP_USD: f64 = 2.0;
const DEFAULT_MONTHLY_CAP_USD: f64 = 10.0;

/// A cap setting: a positive number, `0` or `null` (disabled). Anything else
/// falls back to the default, so a damaged value never removes the guard.
fn cap_setting(cat: &Catalog, key: &str, default: f64) -> Result<Option<f64>, PipelineError> {
    Ok(match cat.setting(key)? {
        Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::Number(n)) => match n.as_f64() {
            Some(0.0) => None,
            Some(v) if v > 0.0 && v.is_finite() => Some(v),
            _ => Some(default),
        },
        _ => Some(default),
    })
}

impl SummaryPolicy {
    pub fn load(cat: &Catalog) -> Result<Self, PipelineError> {
        let mut per_kind = BTreeMap::new();
        for kind in SourceKind::ALL {
            if let Some(v) = cat.setting(&format!("summary.profile.{kind}"))?
                && let Some(s) = v.as_str()
            {
                per_kind.insert(*kind, s.to_string());
            }
        }
        let mut profiles = BTreeMap::new();
        let mut invalid_profiles = BTreeMap::new();
        for (key, v) in cat.settings_with_prefix("llm.profiles.")? {
            let name = key.trim_start_matches("llm.profiles.").to_string();
            match Profile::from_value(&name, &v) {
                Ok(p) => {
                    profiles.insert(name, p);
                }
                Err(e) => {
                    invalid_profiles.insert(name, e.reason);
                }
            }
        }
        Ok(SummaryPolicy {
            default_profile: cat
                .setting("summary.profile.default")?
                .and_then(|v| v.as_str().map(str::to_string)),
            per_kind,
            profiles,
            invalid_profiles,
            min_chars: cat.setting_or("summary.min_chars", 400usize)?,
            min_messages: cat.setting_or("summary.min_messages", 3usize)?,
            max_attempts: cat.setting_or("summary.max_attempts", 3i64)?,
            language: cat.setting_or("summary.language", "auto".to_string())?,
            prices: cat.setting("llm.prices")?,
            weekly_cap_usd: cap_setting(cat, "summary.budget.weekly_usd", DEFAULT_WEEKLY_CAP_USD)?,
            monthly_cap_usd: cap_setting(
                cat,
                "summary.budget.monthly_usd",
                DEFAULT_MONTHLY_CAP_USD,
            )?,
            budget_timezone: cat.setting_or(
                "summary.budget.timezone",
                second_brain_store::settings::os_timezone(),
            )?,
        })
    }

    /// The profile name for a source kind (may be `native`), if any.
    pub fn profile_name(&self, kind: SourceKind) -> Option<&str> {
        self.per_kind
            .get(&kind)
            .or(self.default_profile.as_ref())
            .map(String::as_str)
    }

    pub fn profile(&self, name: &str) -> Option<&Profile> {
        self.profiles.get(name)
    }

    /// Whether a summary input is below the thresholds: conversations by message
    /// count and length, documents by length. Other inputs only when empty.
    pub fn below_thresholds(&self, input: &SummaryInput) -> bool {
        match input.message_count {
            Some(n) => n < self.min_messages || input.body.chars().count() < self.min_chars,
            None if input.prompt == PromptKind::Document => {
                input.body.chars().count() < self.min_chars
            }
            None => input.body.trim().is_empty(),
        }
    }

    /// The input hash of a summary input: the body alone (ADR-0017). A summary
    /// is regenerated automatically only when this changes, never because the
    /// model, the profile or the prompt version did.
    pub fn input_hash(&self, input: &SummaryInput) -> String {
        body_hash(&input.body)
    }

    /// Decide the summary state of an entry after normalization.
    pub fn decide(
        &self,
        kind: SourceKind,
        n: &Normalized,
        existing: Option<(&Entry, Option<&SummaryRecord>)>,
    ) -> SummaryDecision {
        let profile_name = self.profile_name(kind);
        let native_hash = |input: Option<&SummaryInput>| {
            let body = input.map(|i| i.body.as_str()).unwrap_or("");
            summary_input_hash(NATIVE, "", body)
        };
        let Some(input) = &n.summary_input else {
            return match &n.native_summary {
                Some(g) => SummaryDecision::Native {
                    generator: g.clone(),
                    input_hash: native_hash(None),
                },
                None => SummaryDecision::NoSummary,
            };
        };
        if profile_name == Some(NATIVE) {
            return match &n.native_summary {
                Some(g) => SummaryDecision::Native {
                    generator: g.clone(),
                    input_hash: native_hash(Some(input)),
                },
                None => SummaryDecision::NoSummary,
            };
        }
        if self.below_thresholds(input) {
            return SummaryDecision::Skipped;
        }
        let done = existing.is_some_and(|(e, _)| e.summary_status == SummaryStatus::Done);
        let record = existing.and_then(|(_, s)| s);
        let hash = self.input_hash(input);
        match profile_name.and_then(|p| self.profile(p)) {
            Some(_) if done => match record {
                // A native summary is replaced by the first LLM one, as before.
                Some(s) if s.generator_kind == GeneratorKind::SourceNative => {
                    SummaryDecision::Pending
                }
                // Unknown baseline: keep the summary and adopt this body's hash.
                Some(s) if s.input_hash.is_empty() => {
                    SummaryDecision::KeepAdopt { input_hash: hash }
                }
                Some(s) if s.input_hash == hash => SummaryDecision::Keep,
                // A hash from before ADR-0017 that the upgrade has not reached yet.
                Some(s) if !s.input_hash.starts_with(BODY_HASH_PREFIX) => SummaryDecision::Keep,
                _ => SummaryDecision::Pending,
            },
            Some(_) => SummaryDecision::Pending,
            // No usable profile: the input hash cannot be computed, so a
            // finished summary is kept and anything else waits for a profile.
            None if done => SummaryDecision::Keep,
            None => SummaryDecision::Pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use second_brain_kernel::{Generator, PromptKind};
    use serde_json::json;

    fn policy() -> SummaryPolicy {
        let mut profiles = BTreeMap::new();
        profiles.insert(
            "fast".to_string(),
            Profile::from_value(
                "fast",
                &json!({"kind": "llm_api", "provider": "anthropic", "model": "m"}),
            )
            .unwrap(),
        );
        let mut per_kind = BTreeMap::new();
        per_kind.insert(SourceKind::GoogleMeet, NATIVE.to_string());
        SummaryPolicy {
            default_profile: Some("fast".into()),
            per_kind,
            profiles,
            invalid_profiles: BTreeMap::new(),
            min_chars: 10,
            min_messages: 2,
            max_attempts: 3,
            language: "auto".into(),
            prices: None,
            weekly_cap_usd: None,
            monthly_cap_usd: None,
            budget_timezone: "UTC".into(),
        }
    }

    fn normalized(body: &str, messages: Option<usize>, native: bool) -> Normalized {
        Normalized {
            title: "t".into(),
            source_url: None,
            source_created_at: None,
            source_updated_at: None,
            metadata: json!({}),
            sections: vec![],
            summary_input: Some(SummaryInput {
                source_kind: SourceKind::SlackThread,
                prompt: PromptKind::Conversation,
                title: "t".into(),
                date: None,
                context: None,
                body: body.into(),
                message_count: messages,
                want_details: false,
            }),
            native_summary: native.then(Generator::gemini_meet_notes),
        }
    }

    #[test]
    fn documents_are_judged_by_length_only() {
        let p = policy(); // min_chars = 10, min_messages = 2
        let doc = |body: &str| {
            let mut n = normalized(body, None, false);
            if let Some(i) = n.summary_input.as_mut() {
                i.prompt = PromptKind::Document;
                i.source_kind = SourceKind::LocalFile;
            }
            n
        };
        assert_eq!(
            p.decide(SourceKind::LocalFile, &doc("tiny"), None),
            SummaryDecision::Skipped
        );
        assert_eq!(
            p.decide(
                SourceKind::LocalFile,
                &doc("a document that is long enough"),
                None
            ),
            SummaryDecision::Pending
        );
    }

    #[test]
    fn decisions() {
        let p = policy();
        assert_eq!(
            p.decide(
                SourceKind::SlackThread,
                &normalized("short", Some(5), false),
                None
            ),
            SummaryDecision::Skipped
        );
        assert_eq!(
            p.decide(
                SourceKind::SlackThread,
                &normalized("long enough body", Some(1), false),
                None
            ),
            SummaryDecision::Skipped
        );
        assert_eq!(
            p.decide(
                SourceKind::SlackThread,
                &normalized("long enough body", Some(3), false),
                None
            ),
            SummaryDecision::Pending
        );
        assert!(matches!(
            p.decide(
                SourceKind::GoogleMeet,
                &normalized("transcript", None, true),
                None
            ),
            SummaryDecision::Native { .. }
        ));
    }

    fn entry(done: bool) -> Entry {
        let now = chrono::Utc::now();
        Entry {
            id: 1,
            entry_uid: "u".into(),
            account_id: "a".into(),
            source_kind: SourceKind::SlackThread,
            source_id: "s".into(),
            source_url: None,
            title: "t".into(),
            source_created_at: None,
            source_updated_at: None,
            ingested_at: now,
            updated_at: now,
            raw_status: second_brain_kernel::RawStatus::Present,
            raw_hash: None,
            summary_status: if done {
                SummaryStatus::Done
            } else {
                SummaryStatus::Pending
            },
            summary_attempts: 0,
            summary_error: None,
            fetch_state: None,
            metadata: json!({}),
            origin: second_brain_kernel::EntryOrigin::Sync,
        }
    }

    fn record(kind: GeneratorKind, hash: &str) -> SummaryRecord {
        SummaryRecord {
            entry_id: 1,
            generator_kind: kind,
            provider: "p".into(),
            model: "haiku".into(),
            profile: Some("fast".into()),
            prompt_version: Some("conversation-summary/v1".into()),
            input_hash: hash.into(),
            generated_at: chrono::Utc::now(),
            usage: None,
        }
    }

    /// ADR-0017: only the body decides; the model, the profile and the prompt
    /// version are not part of the hash.
    #[test]
    fn regeneration_depends_on_the_body_alone() {
        let p = policy();
        let n = normalized("long enough body", Some(3), false);
        let hash = body_hash("long enough body");
        let decide = |e: &Entry, r: &SummaryRecord| {
            p.decide(SourceKind::SlackThread, &n, Some((e, Some(r))))
        };
        let done = entry(true);
        // Same body: kept, whatever model produced it.
        assert_eq!(
            decide(&done, &record(GeneratorKind::LlmCli, &hash)),
            SummaryDecision::Keep
        );
        // A different body: summarize again.
        assert_eq!(
            decide(
                &done,
                &record(GeneratorKind::LlmCli, &body_hash("older body"))
            ),
            SummaryDecision::Pending
        );
        // Unknown baseline: keep, and adopt this body's hash.
        assert_eq!(
            decide(&done, &record(GeneratorKind::LlmCli, "")),
            SummaryDecision::KeepAdopt { input_hash: hash }
        );
        // A pre-ADR-0017 hash that the upgrade has not reached: keep.
        assert_eq!(
            decide(&done, &record(GeneratorKind::LlmApi, "9f2c-legacy")),
            SummaryDecision::Keep
        );
        // A source-native summary is replaced by the first LLM summary.
        assert_eq!(
            decide(&done, &record(GeneratorKind::SourceNative, "native")),
            SummaryDecision::Pending
        );
        // Not done yet: pending, whatever the hash says.
        assert_eq!(
            decide(
                &entry(false),
                &record(GeneratorKind::LlmCli, &body_hash("long enough body"))
            ),
            SummaryDecision::Pending
        );
    }
}
