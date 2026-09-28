//! Summary policy: which profile applies to a source kind, thresholds, and the
//! decision taken after normalization.

use std::collections::BTreeMap;

use sb_core::util::summary_input_hash;
use sb_core::{Normalized, SourceKind, SummaryInput, SummaryStatus};
use sb_llm::{NATIVE, Profile, prompts::prompt_version};
use sb_store::{Catalog, Entry, SummaryDecision, SummaryRecord};

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

    /// Whether a summary input is below the thresholds (conversations only).
    pub fn below_thresholds(&self, input: &SummaryInput) -> bool {
        match input.message_count {
            Some(n) => n < self.min_messages || input.body.chars().count() < self.min_chars,
            None => input.body.trim().is_empty(),
        }
    }

    /// Input hash for a profile.
    pub fn input_hash(&self, input: &SummaryInput, profile: &Profile) -> String {
        summary_input_hash(
            prompt_version(input.prompt),
            &profile.model_name(),
            &input.body,
        )
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
        let current_hash = existing.and_then(|(_, s)| s).map(|s| s.input_hash.as_str());
        match profile_name.and_then(|p| self.profile(p)) {
            Some(p) if done && current_hash == Some(self.input_hash(input, p).as_str()) => {
                SummaryDecision::Keep
            }
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
    use sb_core::{Generator, PromptKind};
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
}
