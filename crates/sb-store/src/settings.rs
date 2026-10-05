//! Global settings: dotted keys with JSON values (data-model.md).

use rusqlite::params;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::catalog::{Catalog, OptionalExt};
use crate::error::{Result, StoreError};

/// Known settings with their defaults. `None` means "no default" (unset).
pub fn default_value(key: &str) -> Option<Value> {
    Some(match key {
        "display.language" => json!(os_language()),
        "summary.language" => json!("auto"),
        "summary.max_attempts" => json!(3),
        "summary.min_chars" => json!(400),
        "summary.min_messages" => json!(3),
        "summary.profile.google.meet" => json!("native"),
        "summary.budget.weekly_usd" => json!(2.0),
        "summary.budget.monthly_usd" => json!(10.0),
        "summary.budget.timezone" => json!(os_timezone()),
        "pipeline.commit_batch" => json!(20),
        "pipeline.shutdown_grace_secs" => json!(30),
        "search.backends" => json!(["sqlite-fts"]),
        "slack.day_timezone" => json!(os_timezone()),
        "notify.sinks" => json!(["doctor"]),
        "notify.min_severity" => json!("error"),
        "mcp.allow_ingest" => json!(true),
        "mcp.default_limit" => json!(8),
        "sync.lock_stale_hours" => json!(6),
        _ => return None,
    })
}

/// Keys that may be set without a default.
const KNOWN_KEYS: &[&str] = &[
    "display.language",
    "summary.language",
    "summary.max_attempts",
    "summary.min_chars",
    "summary.min_messages",
    "summary.budget.weekly_usd",
    "summary.budget.monthly_usd",
    "summary.budget.timezone",
    "summary.profile.default",
    "pipeline.commit_batch",
    "pipeline.shutdown_grace_secs",
    "search.backends",
    "slack.day_timezone",
    "notify.sinks",
    "notify.min_severity",
    "notify.slack_dm.account",
    "mcp.allow_ingest",
    "mcp.default_limit",
    "backup.last_at",
    "llm.prices",
    "sync.lock_stale_hours",
    "skills.installed",
    "schedule.registered",
];

/// Key prefixes for open-ended families of settings.
const KNOWN_PREFIXES: &[&str] = &["summary.profile.", "llm.profiles.", "google.meet."];

/// Whether `key` is a recognized setting.
pub fn is_known_key(key: &str) -> bool {
    KNOWN_KEYS.contains(&key)
        || KNOWN_PREFIXES
            .iter()
            .any(|p| key.starts_with(p) && key.len() > p.len())
}

/// The OS display language: `ja` for Japanese locales, otherwise `en`.
pub fn os_language() -> &'static str {
    let loc = sys_locale::get_locale().unwrap_or_default().to_lowercase();
    if loc.starts_with("ja") { "ja" } else { "en" }
}

/// The OS IANA time zone, or `UTC`.
pub fn os_timezone() -> String {
    iana_time_zone::get_timezone().unwrap_or_else(|_| "UTC".to_string())
}

/// A setting as listed by `sb config list`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SettingItem {
    pub key: String,
    pub value: Value,
    /// `true` when the value is the built-in default.
    pub default: bool,
}

impl Catalog {
    /// The stored value of a setting (no default).
    pub fn setting_raw(&self, key: &str) -> Result<Option<Value>> {
        let v: Option<String> = self
            .conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .opt()?;
        v.map(|s| serde_json::from_str(&s).map_err(StoreError::from))
            .transpose()
    }

    /// The stored value, or the built-in default.
    pub fn setting(&self, key: &str) -> Result<Option<Value>> {
        Ok(self.setting_raw(key)?.or_else(|| default_value(key)))
    }

    /// Typed setting with a fallback when unset or of the wrong type.
    pub fn setting_or<T: DeserializeOwned>(&self, key: &str, fallback: T) -> Result<T> {
        Ok(self
            .setting(key)?
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or(fallback))
    }

    pub fn set_setting(&self, key: &str, value: &Value) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings(key, value, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            params![key, value.to_string(), self.now_ts()],
        )?;
        Ok(())
    }

    pub fn unset_setting(&self, key: &str) -> Result<bool> {
        Ok(self
            .conn
            .execute("DELETE FROM settings WHERE key = ?1", [key])?
            > 0)
    }

    /// Stored settings whose key starts with `prefix`.
    pub fn settings_with_prefix(&self, prefix: &str) -> Result<Vec<(String, Value)>> {
        let mut stmt = self.conn.prepare(
            "SELECT key, value FROM settings WHERE substr(key, 1, length(?1)) = ?1 ORDER BY key",
        )?;
        let rows = stmt
            .query_map([prefix], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter()
            .map(|(k, v)| Ok((k, serde_json::from_str(&v)?)))
            .collect()
    }

    /// All stored settings plus defaults for known keys.
    pub fn list_settings(&self) -> Result<Vec<SettingItem>> {
        let mut items: Vec<SettingItem> = self
            .settings_with_prefix("")?
            .into_iter()
            .map(|(key, value)| SettingItem {
                key,
                value,
                default: false,
            })
            .collect();
        for key in KNOWN_KEYS {
            if let Some(v) = default_value(key)
                && !items.iter().any(|i| i.key == *key)
            {
                items.push(SettingItem {
                    key: key.to_string(),
                    value: v,
                    default: true,
                });
            }
        }
        if !items.iter().any(|i| i.key == "summary.profile.google.meet") {
            items.push(SettingItem {
                key: "summary.profile.google.meet".into(),
                value: json!("native"),
                default: true,
            });
        }
        items.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(items)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::test_util::temp_catalog;

    #[test]
    fn defaults_and_overrides() {
        let (_d, cat) = temp_catalog();
        assert_eq!(
            cat.setting_or::<u64>("pipeline.commit_batch", 0).unwrap(),
            20
        );
        cat.set_setting("pipeline.commit_batch", &json!(5)).unwrap();
        assert_eq!(
            cat.setting_or::<u64>("pipeline.commit_batch", 0).unwrap(),
            5
        );
        assert!(cat.unset_setting("pipeline.commit_batch").unwrap());
        assert_eq!(
            cat.setting_or::<u64>("pipeline.commit_batch", 0).unwrap(),
            20
        );
        assert_eq!(
            cat.setting("summary.profile.google.meet").unwrap(),
            Some(json!("native"))
        );
        assert!(cat.setting("summary.profile.default").unwrap().is_none());
    }

    #[test]
    fn prefix_listing() {
        let (_d, cat) = temp_catalog();
        cat.set_setting("llm.profiles.fast", &json!({"kind": "llm_api"}))
            .unwrap();
        cat.set_setting("llm.profiles.best", &json!({"kind": "llm_api"}))
            .unwrap();
        cat.set_setting("llm.prices", &json!({})).unwrap();
        let p = cat.settings_with_prefix("llm.profiles.").unwrap();
        assert_eq!(p.len(), 2);
        assert!(is_known_key("llm.profiles.fast"));
        assert!(!is_known_key("llm.profiles."));
        assert!(!is_known_key("nope"));
    }
}
