//! Summarizers for second-brain (ADR-0005): LLM HTTP APIs, LLM CLIs and local
//! OpenAI-compatible servers, with embedded versioned prompts.

pub mod backend;
pub mod prices;
pub mod profile;
pub mod prompts;
pub mod summarizer;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sb_core::summarizer::LlmError;
use sb_core::{GeneratorKind, Secret};

use backend::{
    AnthropicBackend, Backend, CliBackend, CliFlavor, CompletionRequest, OpenAiBackend, Role,
};
pub use profile::{NATIVE, Profile, ProfileError, Provider};
pub use summarizer::LlmSummarizer;

/// Inputs to `build` that come from the catalog and the environment.
#[derive(Debug, Clone)]
pub struct BuildOptions {
    /// `summary.language`.
    pub language: String,
    /// Parent of per-call empty working directories for CLI providers.
    pub scratch_dir: PathBuf,
    /// The resolved secret of the profile, if any.
    pub secret: Option<Secret>,
}

/// A built summarizer plus what the summarize stage needs to prepare it.
pub struct Built {
    pub name: String,
    pub profile: Profile,
    pub summarizer: Arc<LlmSummarizer>,
    /// Set for `local_llm` profiles: the server to probe and warm up.
    local: Option<Arc<OpenAiBackend>>,
}

/// Resolve a CLI program: an absolute or relative path as given, else a
/// lookup on `PATH`.
pub fn resolve_program(name: &str) -> Option<PathBuf> {
    let p = Path::new(name);
    if p.components().count() > 1 {
        return p.is_file().then(|| p.to_path_buf());
    }
    which::which(name).ok()
}

/// Build the summarizer for a profile.
pub fn build(name: &str, profile: &Profile, opts: &BuildOptions) -> Result<Built, LlmError> {
    profile
        .validate(name)
        .map_err(|e| LlmError::Config(e.to_string()))?;
    let model = profile.model_name();
    let mut local = None;
    let backend: Arc<dyn Backend> = match profile.provider {
        Provider::Anthropic => {
            let key = opts.secret.clone().ok_or_else(|| {
                LlmError::Config(format!(
                    "profile {name}: no API key (set it with `sb config set-secret anthropic.api_key` or ANTHROPIC_API_KEY)"
                ))
            })?;
            Arc::new(AnthropicBackend::new(
                profile.base_url.clone(),
                key,
                model.clone(),
            )?)
        }
        Provider::Openai => {
            let key = opts.secret.clone().ok_or_else(|| {
                LlmError::Config(format!(
                    "profile {name}: no API key (set it with `sb config set-secret openai.api_key` or OPENAI_API_KEY)"
                ))
            })?;
            Arc::new(OpenAiBackend::new(
                profile.base_url.clone(),
                Some(key),
                model.clone(),
                true,
                None,
            )?)
        }
        Provider::OpenaiCompatible => {
            let b = Arc::new(OpenAiBackend::new(
                profile.base_url.clone(),
                opts.secret.clone(),
                model.clone(),
                false,
                profile.keep_alive.clone(),
            )?);
            if profile.kind == GeneratorKind::LocalLlm {
                local = Some(b.clone());
            }
            b
        }
        Provider::Google => {
            return Err(LlmError::Config(format!(
                "profile {name}: the Gemini API provider is not available yet (planned for phase 2)"
            )));
        }
        Provider::ClaudeCli | Provider::CopilotCli => {
            let bin = profile.cli_binary().unwrap_or_default();
            let program = resolve_program(bin).ok_or_else(|| {
                LlmError::Config(format!("profile {name}: `{bin}` was not found on PATH"))
            })?;
            Arc::new(CliBackend {
                flavor: if profile.provider == Provider::ClaudeCli {
                    CliFlavor::Claude
                } else {
                    CliFlavor::Copilot
                },
                program,
                model: profile.model.clone(),
                extra_args: profile.args.clone(),
                scratch_dir: opts.scratch_dir.clone(),
            })
        }
    };
    let summarizer = Arc::new(LlmSummarizer {
        kind: profile.kind,
        provider: profile.provider.recorded_name().to_string(),
        model,
        max_input_chars: profile.max_input_chars,
        max_output_tokens: profile.max_output_tokens,
        request_timeout: Duration::from_secs(profile.request_timeout_secs),
        retry_timeout: local
            .as_ref()
            .map(|_| Duration::from_secs(profile.warmup_timeout_secs)),
        language: opts.language.clone(),
        backend,
    });
    Ok(Built {
        name: name.to_string(),
        profile: profile.clone(),
        summarizer,
        local,
    })
}

impl Built {
    /// Before the first summary of a run: for local servers, check
    /// reachability (starting the server with `start_command` if needed) and
    /// send a tiny warm-up completion. Returns the warm-up time.
    pub async fn prepare(&self) -> Result<Option<Duration>, LlmError> {
        let Some(server) = &self.local else {
            return Ok(None);
        };
        let probe_timeout = Duration::from_secs(5);
        if server.probe(probe_timeout).await.is_err() {
            let Some(cmd) = self
                .profile
                .start_command
                .as_ref()
                .filter(|c| !c.is_empty())
            else {
                return Err(LlmError::Unreachable(format!(
                    "local LLM server {} is not reachable",
                    self.profile.base_url.as_deref().unwrap_or("?")
                )));
            };
            tracing::info!(command = ?cmd, "local LLM server unreachable; running start_command");
            let program = resolve_program(&cmd[0]).ok_or_else(|| {
                LlmError::Unreachable(format!("start_command `{}` not found", cmd[0]))
            })?;
            // The command may start a daemon and return, or keep running; it is
            // not awaited beyond spawning.
            tokio::process::Command::new(program)
                .args(&cmd[1..])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .map_err(|e| LlmError::Unreachable(format!("start_command failed: {e}")))?;
            let deadline = Instant::now() + Duration::from_secs(self.profile.warmup_timeout_secs);
            loop {
                tokio::time::sleep(Duration::from_secs(2)).await;
                if server.probe(probe_timeout).await.is_ok() {
                    break;
                }
                if Instant::now() > deadline {
                    return Err(LlmError::Unreachable(
                        "local LLM server did not come up in time".into(),
                    ));
                }
            }
        }
        let started = Instant::now();
        server
            .complete(&CompletionRequest {
                system: "Reply with OK.".into(),
                messages: vec![(Role::User, "ping".into())],
                max_tokens: 4,
                timeout: Duration::from_secs(self.profile.warmup_timeout_secs),
            })
            .await?;
        let took = started.elapsed();
        tracing::info!(profile = %self.name, warmup_ms = took.as_millis() as u64, "local LLM warmed up");
        Ok(Some(took))
    }

    /// A one-call test used by `sb setup llm` and `sb doctor --online`.
    pub async fn test_call(&self) -> Result<String, LlmError> {
        let b = self.summarizer.backend.clone();
        let c = b
            .complete(&CompletionRequest {
                system: "Reply with the single word OK.".into(),
                messages: vec![(Role::User, "ping".into())],
                max_tokens: 16,
                timeout: Duration::from_secs(self.profile.warmup_timeout_secs.max(60)),
            })
            .await?;
        Ok(c.text.trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn opts() -> BuildOptions {
        BuildOptions {
            language: "auto".into(),
            scratch_dir: std::env::temp_dir(),
            secret: None,
        }
    }

    #[test]
    fn api_profile_needs_key() {
        let p = Profile::from_value(
            "fast",
            &json!({"kind": "llm_api", "provider": "anthropic", "model": "m"}),
        )
        .unwrap();
        assert!(matches!(
            build("fast", &p, &opts()),
            Err(LlmError::Config(_))
        ));
        let mut o = opts();
        o.secret = Some(Secret::new("k"));
        let b = build("fast", &p, &o).unwrap();
        assert_eq!(b.summarizer.provider, "anthropic");
    }

    #[test]
    fn missing_cli_is_config_error() {
        let p = Profile::from_value(
            "c",
            &json!({"kind": "llm_cli", "provider": "claude_cli", "command": "/nonexistent/claude-xyz"}),
        )
        .unwrap();
        assert!(matches!(build("c", &p, &opts()), Err(LlmError::Config(_))));
    }

    #[tokio::test]
    async fn local_unreachable_without_start_command() {
        let p = Profile::from_value(
            "npu",
            &json!({"kind": "local_llm", "provider": "openai_compatible", "model": "m", "base_url": "http://127.0.0.1:9/v1"}),
        )
        .unwrap();
        let b = build("npu", &p, &opts()).unwrap();
        assert!(matches!(b.prepare().await, Err(LlmError::Unreachable(_))));
    }
}
