//! Summarizer profiles (`llm.profiles.<name>`, summarization.md).

use serde::{Deserialize, Serialize};

use sb_core::GeneratorKind;

/// Reserved profile name meaning "keep the source-native summary".
pub const NATIVE: &str = "native";

/// Default `max_input_chars`.
pub const DEFAULT_MAX_INPUT_CHARS: usize = 40_000;

/// Provider of a profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Anthropic,
    Openai,
    Google,
    OpenaiCompatible,
    ClaudeCli,
    CopilotCli,
}

impl Provider {
    /// The provider string recorded in `summaries.provider`.
    pub fn recorded_name(self) -> &'static str {
        match self {
            Provider::Anthropic => "anthropic",
            Provider::Openai => "openai",
            Provider::Google => "google",
            Provider::OpenaiCompatible => "openai-compatible",
            Provider::ClaudeCli => "claude-cli",
            Provider::CopilotCli => "copilot-cli",
        }
    }

    /// The default secret for API providers.
    pub fn default_secret(self) -> Option<&'static str> {
        match self {
            Provider::Anthropic => Some("global:anthropic.api_key"),
            Provider::Openai => Some("global:openai.api_key"),
            Provider::Google => Some("global:google.api_key"),
            _ => None,
        }
    }

    /// The executable name of CLI providers.
    pub fn cli_binary(self) -> Option<&'static str> {
        match self {
            Provider::ClaudeCli => Some("claude"),
            Provider::CopilotCli => Some("copilot"),
            _ => None,
        }
    }
}

/// A summarizer profile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub kind: GeneratorKind,
    pub provider: Provider,
    /// Model name; optional for CLI providers (their default model is used).
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default = "default_concurrency")]
    pub concurrency: usize,
    #[serde(default = "default_max_input_chars")]
    pub max_input_chars: usize,
    /// Secret reference, e.g. `global:anthropic.api_key`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
    #[serde(default = "default_request_timeout")]
    pub request_timeout_secs: u64,
    #[serde(default = "default_warmup_timeout")]
    pub warmup_timeout_secs: u64,
    /// Command run when a local server is unreachable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_command: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_alive: Option<String>,
    /// Override the CLI executable path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Extra CLI arguments.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default = "default_max_output_tokens")]
    pub max_output_tokens: u32,
}

fn default_concurrency() -> usize {
    4
}
fn default_max_input_chars() -> usize {
    DEFAULT_MAX_INPUT_CHARS
}
fn default_request_timeout() -> u64 {
    300
}
fn default_warmup_timeout() -> u64 {
    600
}
fn default_max_output_tokens() -> u32 {
    8192
}

/// Profile validation error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid profile {name:?}: {reason}")]
pub struct ProfileError {
    pub name: String,
    pub reason: String,
}

impl Profile {
    /// Parse and validate a profile from its settings JSON.
    pub fn from_value(name: &str, v: &serde_json::Value) -> Result<Self, ProfileError> {
        let p: Profile = serde_json::from_value(v.clone()).map_err(|e| ProfileError {
            name: name.into(),
            reason: e.to_string(),
        })?;
        p.validate(name)?;
        Ok(p)
    }

    pub fn validate(&self, name: &str) -> Result<(), ProfileError> {
        let err = |reason: &str| ProfileError {
            name: name.into(),
            reason: reason.into(),
        };
        if name == NATIVE {
            return Err(err("\"native\" is a reserved profile name"));
        }
        let expected = match self.provider {
            Provider::Anthropic | Provider::Openai | Provider::Google => {
                &[GeneratorKind::LlmApi][..]
            }
            Provider::OpenaiCompatible => &[GeneratorKind::LocalLlm, GeneratorKind::LlmApi][..],
            Provider::ClaudeCli | Provider::CopilotCli => &[GeneratorKind::LlmCli][..],
        };
        if !expected.contains(&self.kind) {
            return Err(err(&format!(
                "kind {} does not match provider {}",
                self.kind,
                self.provider.recorded_name()
            )));
        }
        if self.cli_binary().is_none() && self.model.as_deref().unwrap_or("").is_empty() {
            return Err(err("model is required"));
        }
        if self.provider == Provider::OpenaiCompatible && self.base_url.is_none() {
            return Err(err("base_url is required for openai_compatible"));
        }
        if self.concurrency == 0 {
            return Err(err("concurrency must be at least 1"));
        }
        if self.max_input_chars < 2_000 {
            return Err(err("max_input_chars must be at least 2000"));
        }
        Ok(())
    }

    pub fn cli_binary(&self) -> Option<&str> {
        self.command.as_deref().or(self.provider.cli_binary())
    }

    /// Model string recorded in provenance and used in the input hash.
    pub fn model_name(&self) -> String {
        self.model.clone().unwrap_or_else(|| "default".to_string())
    }

    /// The secret reference to use, if any.
    pub fn secret_ref(&self) -> Option<String> {
        self.secret
            .clone()
            .or_else(|| self.provider.default_secret().map(str::to_string))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_api_profile_with_defaults() {
        let p = Profile::from_value(
            "fast",
            &json!({"kind": "llm_api", "provider": "anthropic", "model": "claude-haiku-4-5"}),
        )
        .unwrap();
        assert_eq!(p.concurrency, 4);
        assert_eq!(p.max_input_chars, DEFAULT_MAX_INPUT_CHARS);
        assert_eq!(p.secret_ref().as_deref(), Some("global:anthropic.api_key"));
    }

    #[test]
    fn validation_errors() {
        assert!(
            Profile::from_value(
                "native",
                &json!({"kind": "llm_cli", "provider": "claude_cli"})
            )
            .is_err()
        );
        assert!(
            Profile::from_value(
                "x",
                &json!({"kind": "llm_cli", "provider": "anthropic", "model": "m"})
            )
            .is_err()
        );
        assert!(
            Profile::from_value("x", &json!({"kind": "llm_api", "provider": "anthropic"})).is_err()
        );
        assert!(
            Profile::from_value(
                "x",
                &json!({"kind": "local_llm", "provider": "openai_compatible", "model": "m"})
            )
            .is_err()
        );
        let cli = Profile::from_value("c", &json!({"kind": "llm_cli", "provider": "claude_cli"}))
            .unwrap();
        assert_eq!(cli.model_name(), "default");
        assert_eq!(cli.cli_binary(), Some("claude"));
    }
}
