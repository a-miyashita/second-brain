//! Secrets in the catalog (ADR-0003). Values are never logged or printed.

use rusqlite::params;
use sb_core::{AccountId, Secret};

use crate::catalog::{Catalog, OptionalExt};
use crate::error::Result;

/// Scope of a secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretScope {
    Global,
    Account(AccountId),
}

impl SecretScope {
    pub fn as_key(&self) -> String {
        match self {
            SecretScope::Global => "global".to_string(),
            SecretScope::Account(a) => format!("account:{a}"),
        }
    }

    /// Parse `global` or `account:<id>`.
    pub fn parse(s: &str) -> Option<Self> {
        if s == "global" {
            return Some(SecretScope::Global);
        }
        s.strip_prefix("account:")
            .and_then(|a| AccountId::new(a).ok())
            .map(SecretScope::Account)
    }
}

/// Environment fallbacks for well-known global secrets (ADR-0003).
pub fn env_fallback(name: &str) -> Option<&'static str> {
    match name {
        "anthropic.api_key" => Some("ANTHROPIC_API_KEY"),
        "openai.api_key" => Some("OPENAI_API_KEY"),
        "google.api_key" => Some("GEMINI_API_KEY"),
        "slack.user_token" => Some("SLACK_USER_TOKEN"),
        _ => None,
    }
}

/// Listing entry of a secret (without its value).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SecretInfo {
    pub scope: String,
    pub name: String,
    pub masked: String,
    pub expires_at: Option<String>,
    pub updated_at: String,
}

impl Catalog {
    pub fn set_secret(&self, scope: &SecretScope, name: &str, value: &Secret) -> Result<()> {
        self.conn.execute(
            "INSERT INTO secrets(scope, name, value, updated_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(scope, name) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            params![scope.as_key(), name, value.expose(), self.now_ts()],
        )?;
        Ok(())
    }

    pub fn secret(&self, scope: &SecretScope, name: &str) -> Result<Option<Secret>> {
        let v: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM secrets WHERE scope = ?1 AND name = ?2",
                params![scope.as_key(), name],
                |r| r.get(0),
            )
            .opt()?;
        Ok(v.map(Secret::new))
    }

    /// A stored secret, falling back to its standard environment variable.
    pub fn secret_or_env(&self, scope: &SecretScope, name: &str) -> Result<Option<Secret>> {
        if let Some(s) = self.secret(scope, name)? {
            return Ok(Some(s));
        }
        Ok(env_fallback(name)
            .and_then(|var| std::env::var(var).ok())
            .filter(|v| !v.is_empty())
            .map(Secret::new))
    }

    /// Resolve a secret reference like `global:anthropic.api_key` or
    /// `account:acme:slack.user_token`, with the environment fallback.
    pub fn resolve_secret_ref(&self, reference: &str) -> Result<Option<Secret>> {
        let (scope, name) = if let Some(rest) = reference.strip_prefix("global:") {
            (SecretScope::Global, rest)
        } else if let Some(rest) = reference.strip_prefix("account:") {
            match rest.split_once(':') {
                Some((a, n)) => match AccountId::new(a) {
                    Ok(a) => (SecretScope::Account(a), n),
                    Err(_) => return Ok(None),
                },
                None => return Ok(None),
            }
        } else {
            (SecretScope::Global, reference)
        };
        self.secret_or_env(&scope, name)
    }

    pub fn delete_secret(&self, scope: &SecretScope, name: &str) -> Result<bool> {
        let n = self.conn.execute(
            "DELETE FROM secrets WHERE scope = ?1 AND name = ?2",
            params![scope.as_key(), name],
        )?;
        Ok(n > 0)
    }

    pub fn list_secrets(&self) -> Result<Vec<SecretInfo>> {
        let mut stmt = self.conn.prepare(
            "SELECT scope, name, value, expires_at, updated_at FROM secrets ORDER BY scope, name",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(SecretInfo {
                    scope: r.get(0)?,
                    name: r.get(1)?,
                    masked: Secret::new(r.get::<_, String>(2)?).masked(),
                    expires_at: r.get(3)?,
                    updated_at: r.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::test_util::temp_catalog;

    #[test]
    fn secrets_round_trip() {
        let (_d, cat) = temp_catalog();
        let scope = SecretScope::parse("account:acme").unwrap();
        cat.set_secret(&scope, "slack.user_token", &Secret::new("xoxp-aaaaaaaaaa"))
            .unwrap();
        cat.set_secret(&scope, "slack.user_token", &Secret::new("xoxp-bbbbbbbbbb"))
            .unwrap();
        assert_eq!(
            cat.secret(&scope, "slack.user_token")
                .unwrap()
                .unwrap()
                .expose(),
            "xoxp-bbbbbbbbbb"
        );
        let list = cat.list_secrets().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].masked, "xoxp****");
        assert!(
            cat.resolve_secret_ref("account:acme:slack.user_token")
                .unwrap()
                .is_some()
        );
        assert!(cat.delete_secret(&scope, "slack.user_token").unwrap());
    }
}
