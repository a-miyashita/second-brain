//! Summarizer presets for the basic `sb setup llm` step.

use serde_json::{Value, json};

/// A preset the wizard offers.
#[derive(Debug, Clone, PartialEq)]
pub struct Preset {
    pub key: &'static str,
    pub label: &'static str,
    /// Profile JSON for `llm.profiles.<name>`.
    pub profile: Value,
    /// Global secret the preset needs, if any.
    pub secret: Option<&'static str>,
    /// CLI binary the preset needs, if any.
    pub binary: Option<&'static str>,
}

/// Presets in wizard order. Model names are defaults the user can change.
pub fn presets() -> Vec<Preset> {
    vec![
        Preset {
            key: "anthropic",
            label: "Anthropic API (Claude Haiku 4.5)",
            profile: json!({"kind": "llm_api", "provider": "anthropic", "model": "claude-haiku-4-5", "concurrency": 4}),
            secret: Some("anthropic.api_key"),
            binary: None,
        },
        Preset {
            key: "openai",
            label: "OpenAI API",
            profile: json!({"kind": "llm_api", "provider": "openai", "model": "gpt-4.1-mini", "concurrency": 4}),
            secret: Some("openai.api_key"),
            binary: None,
        },
        Preset {
            key: "claude_cli",
            label: "Claude Code CLI (`claude -p`)",
            profile: json!({"kind": "llm_cli", "provider": "claude_cli", "model": "haiku", "concurrency": 2}),
            secret: None,
            binary: Some("claude"),
        },
        Preset {
            key: "copilot_cli",
            label: "GitHub Copilot CLI (`copilot -p`)",
            profile: json!({"kind": "llm_cli", "provider": "copilot_cli", "concurrency": 1}),
            secret: None,
            binary: Some("copilot"),
        },
        Preset {
            key: "codex_cli",
            label: "OpenAI Codex CLI (`codex exec`)",
            profile: json!({"kind": "llm_cli", "provider": "codex_cli", "model": "gpt-6-luna", "concurrency": 1}),
            secret: None,
            binary: Some("codex"),
        },
        Preset {
            key: "antigravity_cli",
            label: "Google Antigravity CLI (`agy`)",
            profile: json!({"kind": "llm_cli", "provider": "antigravity_cli", "model": "gemini-3.8-flash", "concurrency": 1}),
            secret: None,
            binary: Some("agy"),
        },
        Preset {
            key: "local",
            label: "OpenAI-compatible server (Ollama, llama.cpp, vLLM, ...); not recommended below Haiku-class quality",
            profile: json!({"kind": "local_llm", "provider": "openai_compatible", "base_url": "http://127.0.0.1:11434/v1",
                            "model": "", "concurrency": 1, "request_timeout_secs": 300, "warmup_timeout_secs": 600}),
            secret: None,
            binary: None,
        },
    ]
}
