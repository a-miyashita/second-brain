//! LLM CLIs run as subprocesses in a fresh empty directory, with the prompt on
//! stdin (summarization.md, ADR-0005, ADR-0017).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use second_brain_kernel::Usage;
use second_brain_kernel::summarizer::LlmError;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

use crate::backend::{Backend, Completion, CompletionRequest, Role};
use crate::prices::estimate_tokens;

/// Which CLI a `CliBackend` drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliFlavor {
    Claude,
    Copilot,
    Codex,
    Antigravity,
}

/// Codex features turned off so the model has no tools (ADR-0017). `-s read-only`
/// alone still lets the model run shell reads. Feature names change between Codex
/// versions: `sb doctor --online` probes that a command is really not executed.
pub const CODEX_DISABLED_FEATURES: &[&str] = &[
    "shell_tool",
    "unified_exec",
    "plugins",
    "apps",
    "browser_use",
    "computer_use",
];

/// The permission mode `agy` reports in `init` when no user setting widens it.
const ANTIGRAVITY_SAFE_MODE: &str = "request-review";

/// An LLM CLI run as a subprocess in a fresh empty directory, with the prompt
/// on stdin (summarization.md).
pub struct CliBackend {
    pub flavor: CliFlavor,
    pub program: PathBuf,
    pub model: Option<String>,
    pub extra_args: Vec<String>,
    /// Parent of the per-call empty working directories.
    pub scratch_dir: PathBuf,
    /// Antigravity's state directory (`~/.gemini/antigravity-cli`); `None` means
    /// the default location. Only used by `CliFlavor::Antigravity`.
    pub antigravity_dir: Option<PathBuf>,
}

impl CliBackend {
    fn args(&self) -> Result<Vec<String>, LlmError> {
        let model = self.model.as_deref().filter(|m| !m.is_empty());
        let mut a: Vec<String> = Vec::new();
        match self.flavor {
            CliFlavor::Claude => {
                a.extend(
                    [
                        "-p",
                        "--output-format",
                        "json",
                        "--tools",
                        "",
                        "--no-session-persistence",
                    ]
                    .map(String::from),
                );
                if let Some(m) = model {
                    a.extend(["--model".into(), m.into()]);
                }
            }
            CliFlavor::Copilot => {
                a.extend(["-s", "--no-color", "--stream", "off"].map(String::from));
                if let Some(m) = model {
                    a.extend(["--model".into(), m.into()]);
                }
            }
            CliFlavor::Codex => {
                let m = model.ok_or_else(|| {
                    LlmError::Config("codex_cli needs a model (for example gpt-6-luna)".into())
                })?;
                a.extend(
                    [
                        "exec",
                        "-m",
                        m,
                        "-s",
                        "read-only",
                        "--skip-git-repo-check",
                        "--ephemeral",
                        "--ignore-rules",
                        "--ignore-user-config",
                        "--color",
                        "never",
                        "--json",
                        "-c",
                        "model_reasoning_effort=\"low\"",
                    ]
                    .map(String::from),
                );
                for f in CODEX_DISABLED_FEATURES {
                    a.extend(["--disable".into(), (*f).into()]);
                }
            }
            CliFlavor::Antigravity => {
                let m = model.ok_or_else(|| {
                    LlmError::Config(
                        "antigravity_cli needs a model (for example gemini-3.8-flash)".into(),
                    )
                })?;
                a.extend(
                    [
                        "--input-format",
                        "stream-json",
                        "--output-format",
                        "stream-json",
                        "--model",
                        m,
                        "--effort",
                        "low",
                    ]
                    .map(String::from),
                );
            }
        }
        a.extend(self.extra_args.iter().cloned());
        if self.flavor == CliFlavor::Codex {
            // `-` makes `codex exec` read the prompt from stdin.
            a.push("-".into());
        }
        Ok(a)
    }

    fn render_prompt(req: &CompletionRequest) -> String {
        let mut s = format!("{}\n\n", req.system);
        for (role, text) in &req.messages {
            match role {
                Role::User => s.push_str(text),
                Role::Assistant => {
                    s.push_str("\n\n<previous_answer>\n");
                    s.push_str(text);
                    s.push_str("\n</previous_answer>\n\n");
                }
            }
        }
        s
    }

    fn command(&self, work: &Path) -> Result<tokio::process::Command, LlmError> {
        let mut c = tokio::process::Command::new(&self.program);
        c.args(self.args()?)
            .current_dir(work)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        Ok(c)
    }

    fn spawn(&self, work: &Path) -> Result<tokio::process::Child, LlmError> {
        self.command(work)?
            .spawn()
            .map_err(|e| LlmError::Unreachable(format!("{}: {e}", self.program.display())))
    }

    /// Run a CLI that reads the whole prompt from stdin and prints its answer.
    async fn run_plain(
        &self,
        work: &Path,
        prompt: &str,
        timeout: Duration,
    ) -> Result<String, LlmError> {
        let mut child = self.spawn(work)?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(prompt.as_bytes())
                .await
                .map_err(|e| LlmError::Provider(format!("writing prompt: {e}")))?;
        }
        let out = tokio::time::timeout(timeout, child.wait_with_output())
            .await
            .map_err(|_| {
                LlmError::Timeout(format!(
                    "{} did not finish in {timeout:?}",
                    self.program.display()
                ))
            })?
            .map_err(|e| LlmError::Provider(e.to_string()))?;
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            // Codex reports failures as JSONL events on stdout, not on stderr.
            let detail = if self.flavor == CliFlavor::Codex {
                codex_error(&stdout).unwrap_or_else(|| stderr.trim().to_string())
            } else if stderr.trim().is_empty() {
                stdout.trim().to_string()
            } else {
                stderr.trim().to_string()
            };
            return Err(self.exit_error(&out.status.to_string(), &detail));
        }
        Ok(stdout)
    }

    fn exit_error(&self, status: &str, detail: &str) -> LlmError {
        let detail: String = detail.chars().take(500).collect();
        let msg = format!("{} exited with {status}: {detail}", self.program.display());
        // Claude and Copilot keep their previous classification; only the two new
        // CLIs are matched on their error text.
        let classify = matches!(self.flavor, CliFlavor::Codex | CliFlavor::Antigravity);
        if classify && looks_like_auth_failure(&detail) {
            LlmError::Auth(msg)
        } else {
            LlmError::Provider(msg)
        }
    }

    /// Run `agy` in stream-json mode (ADR-0017). The prompt is written only after
    /// the `init` event has shown that the permission mode is the safe default.
    async fn run_antigravity(
        &self,
        work: &Path,
        prompt: &str,
        timeout: Duration,
        started: Instant,
    ) -> Result<Completion, LlmError> {
        let dir = self.antigravity_dir.clone().or_else(antigravity_state_dir);
        if let Some(problem) = dir
            .as_deref()
            .and_then(antigravity_permissions_problem)
            .or_else(|| dir.is_none().then(no_home_problem))
        {
            return Err(LlmError::Config(problem));
        }
        let mut child = self.spawn(work)?;
        let mut stdin = child.stdin.take();
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| LlmError::Provider("no stdout".into()))?;
        let mut stderr = child.stderr.take();
        let stderr_task = tokio::spawn(async move {
            let mut buf = String::new();
            if let Some(e) = stderr.as_mut() {
                let _ = e.read_to_string(&mut buf).await;
            }
            buf
        });

        let mut state = AntigravityState::default();
        let ran = tokio::time::timeout(timeout, async {
            let mut lines = BufReader::new(stdout).lines();
            while let Some(line) = lines
                .next_line()
                .await
                .map_err(|e| LlmError::Provider(format!("reading output: {e}")))?
            {
                let Ok(v) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if state.conversation_id.is_none() {
                    state.conversation_id = v
                        .get("conversation_id")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string);
                }
                match v.get("event").and_then(Value::as_str) {
                    Some("init") => {
                        let init = v.get("init").unwrap_or(&Value::Null);
                        // Fail closed: a missing or renamed field is not proof of the
                        // safe mode, so the prompt is not sent.
                        let mode = init
                            .get("permission_mode")
                            .and_then(Value::as_str)
                            .unwrap_or("(not reported)");
                        if mode != ANTIGRAVITY_SAFE_MODE {
                            return Err(LlmError::Config(format!(
                                "agy runs with permission mode `{mode}` (expected `{ANTIGRAVITY_SAFE_MODE}`); \
                                 check ~/.gemini/antigravity-cli/settings.json"
                            )));
                        }
                        state.model = init
                            .get("model")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                            .map(str::to_string);
                        if let Some(mut w) = stdin.take() {
                            let msg = serde_json::json!({
                                "event": "user",
                                "message": {"content": prompt},
                            });
                            w.write_all(format!("{msg}\n").as_bytes())
                                .await
                                .map_err(|e| LlmError::Provider(format!("writing prompt: {e}")))?;
                            // Closing stdin ends the session after this turn.
                        }
                    }
                    Some("result") => {
                        state.result = v.get("result").cloned();
                    }
                    _ => {}
                }
            }
            child
                .wait()
                .await
                .map_err(|e| LlmError::Provider(e.to_string()))
        })
        .await;
        drop(stdin);
        // Whatever happened, the conversation `agy` stored is ours to remove.
        if let (Some(id), Some(dir)) = (&state.conversation_id, &dir) {
            remove_antigravity_conversation(dir, id);
        }
        let status = match ran {
            Err(_) => {
                return Err(LlmError::Timeout(format!(
                    "{} did not finish in {timeout:?}",
                    self.program.display()
                )));
            }
            Ok(r) => r?,
        };
        let stderr = stderr_task.await.unwrap_or_default();
        let duration_ms = started.elapsed().as_millis() as u64;
        let Some(result) = state.result else {
            let detail = stderr.trim().to_string();
            return Err(self.exit_error(&status.to_string(), &detail));
        };
        antigravity_completion(&result, state.model, duration_ms, stderr.trim())
    }
}

/// What the `agy` event stream told us.
#[derive(Default)]
struct AntigravityState {
    conversation_id: Option<String>,
    model: Option<String>,
    result: Option<Value>,
}

#[async_trait]
impl Backend for CliBackend {
    async fn complete(&self, req: &CompletionRequest) -> Result<Completion, LlmError> {
        let started = Instant::now();
        let work = self.scratch_dir.join(format!("llm-{}", ulid::Ulid::new()));
        std::fs::create_dir_all(&work)
            .map_err(|e| LlmError::Config(format!("{}: {e}", work.display())))?;
        let prompt = Self::render_prompt(req);
        let result = match self.flavor {
            CliFlavor::Antigravity => {
                self.run_antigravity(&work, &prompt, req.timeout, started)
                    .await
            }
            _ => match self.run_plain(&work, &prompt, req.timeout).await {
                Ok(stdout) => {
                    let duration_ms = started.elapsed().as_millis() as u64;
                    self.parse_plain(&stdout, &prompt, duration_ms)
                }
                Err(e) => Err(e),
            },
        };
        let _ = std::fs::remove_dir_all(&work);
        result
    }
}

impl CliBackend {
    fn parse_plain(
        &self,
        stdout: &str,
        prompt: &str,
        duration_ms: u64,
    ) -> Result<Completion, LlmError> {
        match self.flavor {
            CliFlavor::Claude => parse_claude(stdout, duration_ms),
            CliFlavor::Codex => parse_codex(stdout, duration_ms),
            CliFlavor::Copilot => Ok(Completion {
                usage: Usage {
                    input_tokens: estimate_tokens(prompt),
                    output_tokens: estimate_tokens(stdout),
                    duration_ms,
                    cost_usd: None,
                    calls: 1,
                    model: None,
                },
                text: stdout.to_string(),
            }),
            // Antigravity has its own run loop.
            CliFlavor::Antigravity => Err(LlmError::Provider(
                "antigravity output is not parsed here".into(),
            )),
        }
    }
}

fn u64_at(v: &Value, pointer: &str) -> u64 {
    v.pointer(pointer).and_then(Value::as_u64).unwrap_or(0)
}

/// `claude -p --output-format json`.
fn parse_claude(stdout: &str, duration_ms: u64) -> Result<Completion, LlmError> {
    let v: Value = serde_json::from_str(stdout.trim())
        .map_err(|e| LlmError::Provider(format!("claude output is not JSON: {e}")))?;
    if v.get("is_error").and_then(Value::as_bool) == Some(true) {
        let msg = v
            .get("result")
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        return Err(LlmError::Provider(format!("claude: {msg}")));
    }
    let text = v
        .get("result")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let input = u64_at(&v, "/usage/input_tokens")
        + u64_at(&v, "/usage/cache_read_input_tokens")
        + u64_at(&v, "/usage/cache_creation_input_tokens");
    Ok(Completion {
        text,
        usage: Usage {
            input_tokens: input,
            output_tokens: u64_at(&v, "/usage/output_tokens"),
            duration_ms,
            cost_usd: v.get("total_cost_usd").and_then(Value::as_f64),
            calls: 1,
            model: claude_resolved_model(&v),
        },
    })
}

/// The model Claude Code used: the key of `modelUsage` (the one with the most
/// output tokens if several), e.g. `claude-haiku-4-5-20251001` for `haiku`.
fn claude_resolved_model(v: &Value) -> Option<String> {
    v.get("modelUsage")?
        .as_object()?
        .iter()
        .filter(|(k, _)| !k.is_empty())
        .max_by_key(|(_, u)| u.get("outputTokens").and_then(Value::as_u64).unwrap_or(0))
        .map(|(k, _)| k.clone())
}

/// `codex exec --json`: JSONL events. The answer is the last `agent_message`.
fn parse_codex(stdout: &str, duration_ms: u64) -> Result<Completion, LlmError> {
    let mut text: Option<String> = None;
    let mut usage: Option<Value> = None;
    for line in stdout.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match v.get("type").and_then(Value::as_str) {
            Some("item.completed") => {
                let item = v.get("item").unwrap_or(&Value::Null);
                if item.get("type").and_then(Value::as_str) == Some("agent_message") {
                    text = item.get("text").and_then(Value::as_str).map(str::to_string);
                }
            }
            Some("turn.completed") => usage = v.get("usage").cloned(),
            _ => {}
        }
    }
    let Some(text) = text else {
        let detail = codex_error(stdout).unwrap_or_else(|| "no agent message".into());
        return Err(if looks_like_auth_failure(&detail) {
            LlmError::Auth(format!("codex: {detail}"))
        } else {
            LlmError::Provider(format!("codex: {detail}"))
        });
    };
    let usage = usage.unwrap_or(Value::Null);
    Ok(Completion {
        text,
        usage: Usage {
            input_tokens: u64_at(&usage, "/input_tokens"),
            output_tokens: u64_at(&usage, "/output_tokens")
                + u64_at(&usage, "/reasoning_output_tokens"),
            duration_ms,
            cost_usd: None,
            calls: 1,
            // Codex does not report the model it used: the configured one is recorded.
            model: None,
        },
    })
}

/// The message of the last `error` or `turn.failed` event of a Codex run.
fn codex_error(stdout: &str) -> Option<String> {
    let mut msg = None;
    for line in stdout.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let found = match v.get("type").and_then(Value::as_str) {
            Some("error") => v.get("message").and_then(Value::as_str),
            Some("turn.failed") => v.pointer("/error/message").and_then(Value::as_str),
            _ => None,
        };
        if let Some(m) = found {
            msg = Some(m.to_string());
        }
    }
    msg
}

/// The `result` event of an `agy` stream-json run.
fn antigravity_completion(
    result: &Value,
    model: Option<String>,
    duration_ms: u64,
    stderr: &str,
) -> Result<Completion, LlmError> {
    let status = result.get("status").and_then(Value::as_str).unwrap_or("");
    let response = result.get("response").and_then(Value::as_str).unwrap_or("");
    if status != "SUCCESS" {
        let detail = result
            .get("error")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or(stderr);
        let msg = format!("agy: status {status}: {detail}");
        return Err(if looks_like_auth_failure(detail) {
            LlmError::Auth(msg)
        } else {
            LlmError::Provider(msg)
        });
    }
    if response.trim().is_empty() {
        // A denied tool does not fail the CLI: it ends the turn with no text.
        let denied: Vec<&str> = result
            .get("denied_actions")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|d| d.get("action").and_then(Value::as_str))
                    .collect()
            })
            .unwrap_or_default();
        return Err(LlmError::Provider(if denied.is_empty() {
            "agy returned an empty response".to_string()
        } else {
            format!(
                "agy returned no text: it tried to use a tool that is denied ({})",
                denied.join(", ")
            )
        }));
    }
    let usage = result.get("usage").unwrap_or(&Value::Null);
    Ok(Completion {
        text: response.to_string(),
        usage: Usage {
            input_tokens: u64_at(usage, "/input_tokens"),
            output_tokens: u64_at(usage, "/output_tokens") + u64_at(usage, "/thinking_tokens"),
            duration_ms,
            cost_usd: None,
            calls: 1,
            model,
        },
    })
}

fn looks_like_auth_failure(msg: &str) -> bool {
    let m = msg.to_lowercase();
    // Phrases only: a bare "401" or "login" also appears in request ids, token
    // counts and unrelated messages, and an Auth error aborts the whole batch.
    [
        "please log in",
        "not logged in",
        "logged out",
        "unauthorized",
        "authentication",
        "authenticate",
    ]
    .iter()
    .any(|k| m.contains(k))
}

/// The state directory of the Antigravity CLI, `~/.gemini/antigravity-cli`.
pub fn antigravity_state_dir() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|d| d.home_dir().join(".gemini").join("antigravity-cli"))
}

fn no_home_problem() -> String {
    "cannot locate the home directory to check the Antigravity CLI settings".into()
}

/// Why `agy` must not be run for summaries, if `settings.json` widens its
/// permissions (ADR-0017). A missing file is fine. `None` means safe.
pub fn antigravity_permissions_problem(dir: &Path) -> Option<String> {
    let path = dir.join("settings.json");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => return Some(format!("cannot read {}: {e}", path.display())),
    };
    let v: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => return Some(format!("{} is not valid JSON: {e}", path.display())),
    };
    match v.pointer("/permissions/allow") {
        None | Some(Value::Null) => None,
        Some(Value::Array(a)) if a.is_empty() => None,
        Some(_) => Some(format!(
            "{} has permissions.allow rules; remove them or use another provider \
             (summaries are built from untrusted text, so tools must stay denied)",
            path.display()
        )),
    }
}

/// A canonical, lower-case UUID: `8-4-4-4-12` hex digits.
fn is_uuid(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    parts.len() == 5
        && [8, 4, 4, 4, 12].iter().zip(&parts).all(|(n, p)| {
            p.len() == *n
                && p.chars()
                    .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
        })
}

/// Delete the conversation `agy` stored for one call (ADR-0017). Only the two
/// paths of that conversation are touched, and only for a well-formed id.
fn remove_antigravity_conversation(dir: &Path, id: &str) {
    if !is_uuid(id) {
        return;
    }
    let db = dir.join("conversations").join(format!("{id}.db"));
    if let Err(e) = std::fs::remove_file(&db)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        tracing::debug!(path = %db.display(), error = %e, "could not remove the agy conversation");
    }
    let brain = dir.join("brain").join(id);
    if let Err(e) = std::fs::remove_dir_all(&brain)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        tracing::debug!(path = %brain.display(), error = %e, "could not remove the agy brain directory");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Only the stub-executable tests (unix) build requests.
    #[cfg(unix)]
    fn req() -> CompletionRequest {
        CompletionRequest {
            system: "sys".into(),
            messages: vec![(Role::User, "hello".into())],
            max_tokens: 100,
            timeout: Duration::from_secs(10),
        }
    }

    fn backend(flavor: CliFlavor, program: PathBuf, dir: &Path) -> CliBackend {
        CliBackend {
            flavor,
            program,
            model: Some(
                match flavor {
                    CliFlavor::Claude => "haiku",
                    CliFlavor::Codex => "gpt-6-luna",
                    CliFlavor::Antigravity => "gemini-3.8-flash",
                    CliFlavor::Copilot => "m",
                }
                .into(),
            ),
            extra_args: vec![],
            scratch_dir: dir.join("tmp"),
            antigravity_dir: Some(dir.join("agy-state")),
        }
    }

    #[cfg(unix)]
    fn stub(dir: &Path, name: &str, script: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join(name);
        std::fs::write(&p, script).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[cfg(unix)]
    fn tmp_is_empty(dir: &Path) -> bool {
        std::fs::read_dir(dir.join("tmp")).unwrap().count() == 0
    }

    #[test]
    fn claude_args_and_resolved_model() {
        let dir = tempfile::tempdir().unwrap();
        let b = backend(CliFlavor::Claude, "claude".into(), dir.path());
        let a = b.args().unwrap();
        assert_eq!(a[..2], ["-p", "--output-format"]);
        assert!(a.windows(2).any(|w| w == ["--model", "haiku"]));
        let v = json!({"modelUsage": {
            "claude-haiku-4-5-20251001": {"outputTokens": 39},
            "claude-other": {"outputTokens": 1}}});
        assert_eq!(
            claude_resolved_model(&v).as_deref(),
            Some("claude-haiku-4-5-20251001")
        );
        assert_eq!(claude_resolved_model(&json!({})), None);
    }

    #[test]
    fn codex_argv_is_exactly_this() {
        let dir = tempfile::tempdir().unwrap();
        let b = backend(CliFlavor::Codex, "codex".into(), dir.path());
        let mut want: Vec<String> = [
            "exec",
            "-m",
            "gpt-6-luna",
            "-s",
            "read-only",
            "--skip-git-repo-check",
            "--ephemeral",
            "--ignore-rules",
            "--ignore-user-config",
            "--color",
            "never",
            "--json",
            "-c",
            "model_reasoning_effort=\"low\"",
        ]
        .map(String::from)
        .to_vec();
        for f in CODEX_DISABLED_FEATURES {
            want.push("--disable".into());
            want.push((*f).into());
        }
        want.push("-".into());
        assert_eq!(b.args().unwrap(), want);
    }

    #[test]
    fn antigravity_argv_is_exactly_this() {
        let dir = tempfile::tempdir().unwrap();
        let b = backend(CliFlavor::Antigravity, "agy".into(), dir.path());
        assert_eq!(
            b.args().unwrap(),
            [
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--model",
                "gemini-3.8-flash",
                "--effort",
                "low"
            ]
        );
    }

    #[test]
    fn codex_and_antigravity_need_a_model() {
        let dir = tempfile::tempdir().unwrap();
        for f in [CliFlavor::Codex, CliFlavor::Antigravity] {
            let mut b = backend(f, "x".into(), dir.path());
            b.model = None;
            assert!(matches!(b.args(), Err(LlmError::Config(_))));
        }
    }

    #[test]
    fn parses_codex_events() {
        let out = r#"{"type":"thread.started","thread_id":"t"}
{"type":"turn.started"}
{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"{\"overview\":\"ok\"}"}}
{"type":"turn.completed","usage":{"input_tokens":17010,"cached_input_tokens":11008,"output_tokens":5,"reasoning_output_tokens":3}}
"#;
        let c = parse_codex(out, 7).unwrap();
        assert_eq!(c.text, "{\"overview\":\"ok\"}");
        assert_eq!(c.usage.input_tokens, 17010);
        assert_eq!(c.usage.output_tokens, 8);
        assert_eq!(c.usage.cost_usd, None);
        assert_eq!(c.usage.model, None);
    }

    #[test]
    fn codex_failure_events_become_errors() {
        let out = r#"{"type":"thread.started","thread_id":"t"}
{"type":"item.completed","item":{"id":"item_0","type":"error","message":"Model metadata not found"}}
{"type":"error","message":"model is not supported"}
{"type":"turn.failed","error":{"message":"model is not supported"}}
"#;
        let e = parse_codex(out, 1).unwrap_err();
        assert!(matches!(&e, LlmError::Provider(m) if m.contains("not supported")));
        // A turn without any message and without an error event.
        assert!(matches!(
            parse_codex("{\"type\":\"turn.started\"}\n", 1),
            Err(LlmError::Provider(_))
        ));
        let auth = r#"{"type":"error","message":"401 Unauthorized: please log in"}"#;
        assert!(matches!(parse_codex(auth, 1), Err(LlmError::Auth(_))));
    }

    #[test]
    fn antigravity_results() {
        let ok = json!({"status": "SUCCESS", "response": "OK\n",
            "usage": {"input_tokens": 11807, "output_tokens": 1, "thinking_tokens": 4}});
        let c = antigravity_completion(&ok, Some("gemini-3.8-flash".into()), 5, "").unwrap();
        assert_eq!(c.text, "OK\n");
        assert_eq!(c.usage.input_tokens, 11807);
        assert_eq!(c.usage.output_tokens, 5);
        assert_eq!(c.usage.model.as_deref(), Some("gemini-3.8-flash"));

        let denied = json!({"status": "SUCCESS", "response": "",
            "denied_actions": [{"action": "command", "display_name": "RunCommand"}]});
        let e = antigravity_completion(&denied, None, 1, "").unwrap_err();
        assert!(
            matches!(&e, LlmError::Provider(m) if m.contains("denied") && m.contains("command"))
        );

        let empty = json!({"status": "SUCCESS", "response": "  "});
        assert!(matches!(
            antigravity_completion(&empty, None, 1, ""),
            Err(LlmError::Provider(_))
        ));
        let err = json!({"status": "ERROR", "response": "", "error": "invalid model selection"});
        assert!(matches!(
            antigravity_completion(&err, None, 1, ""),
            Err(LlmError::Provider(m)) if m.contains("invalid model")
        ));
    }

    #[test]
    fn antigravity_permissions_guard() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.json");
        // Missing file: fine.
        assert_eq!(antigravity_permissions_problem(d.path()), None);
        for ok in [
            "{}",
            r#"{"trustedWorkspaces":["/home/x"]}"#,
            r#"{"permissions":{}}"#,
            r#"{"permissions":{"allow":[]}}"#,
            r#"{"permissions":{"allow":null}}"#,
        ] {
            std::fs::write(&p, ok).unwrap();
            assert_eq!(antigravity_permissions_problem(d.path()), None, "{ok}");
        }
        for bad in [
            r#"{"permissions":{"allow":["command(git)"]}}"#,
            r#"{"permissions":{"allow":"all"}}"#,
            "not json",
        ] {
            std::fs::write(&p, bad).unwrap();
            assert!(antigravity_permissions_problem(d.path()).is_some(), "{bad}");
        }
    }

    #[test]
    fn conversation_cleanup_is_exact() {
        let d = tempfile::tempdir().unwrap();
        let id = "7c9f45a5-4d89-496c-adf1-d31a1bfaf7fb";
        let other = "c3a42656-59f9-48fd-ad9e-d0888a633901";
        for i in [id, other] {
            std::fs::create_dir_all(d.path().join("brain").join(i).join("scratch")).unwrap();
        }
        std::fs::create_dir_all(d.path().join("conversations")).unwrap();
        for i in [id, other] {
            std::fs::write(d.path().join("conversations").join(format!("{i}.db")), "x").unwrap();
        }
        std::fs::write(d.path().join("settings.json"), "{}").unwrap();
        remove_antigravity_conversation(d.path(), id);
        assert!(
            !d.path()
                .join("conversations")
                .join(format!("{id}.db"))
                .exists()
        );
        assert!(!d.path().join("brain").join(id).exists());
        // Everything else stays.
        assert!(
            d.path()
                .join("conversations")
                .join(format!("{other}.db"))
                .exists()
        );
        assert!(d.path().join("brain").join(other).exists());
        assert!(d.path().join("settings.json").exists());
        // Not a UUID: nothing is touched, even a traversal.
        std::fs::create_dir_all(d.path().join("victim")).unwrap();
        remove_antigravity_conversation(d.path(), "../victim");
        remove_antigravity_conversation(d.path(), "");
        assert!(d.path().join("victim").exists());
        // A missing directory is a no-op.
        remove_antigravity_conversation(&d.path().join("nope"), id);
        assert!(is_uuid(id));
        assert!(
            !is_uuid("7C9F45A5-4D89-496C-ADF1-D31A1BFAF7FB"),
            "upper case"
        );
        assert!(!is_uuid("7c9f45a5-4d89-496c-adf1-d31a1bfaf7f"), "too short");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn claude_cli_stub() {
        let dir = tempfile::tempdir().unwrap();
        // The stub checks that it runs in an empty directory and echoes JSON.
        let s = stub(
            dir.path(),
            "claude",
            "#!/bin/sh\ncat > /dev/null\n[ -z \"$(ls -A .)\" ] || exit 3\nprintf '{\"type\":\"result\",\"is_error\":false,\"result\":\"{\\\\\"overview\\\\\":\\\\\"ok\\\\\"}\",\"usage\":{\"input_tokens\":5,\"output_tokens\":2},\"total_cost_usd\":0.001,\"modelUsage\":{\"claude-haiku-4-5-20251001\":{\"outputTokens\":2}}}'\n",
        );
        let b = backend(CliFlavor::Claude, s, dir.path());
        let c = b.complete(&req()).await.unwrap();
        assert_eq!(c.text, "{\"overview\":\"ok\"}");
        assert_eq!(c.usage.cost_usd, Some(0.001));
        assert_eq!(c.usage.model.as_deref(), Some("claude-haiku-4-5-20251001"));
        // The working directory was removed.
        assert!(tmp_is_empty(dir.path()));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn claude_without_model_usage_records_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let s = stub(
            dir.path(),
            "claude",
            "#!/bin/sh\ncat > /dev/null\nprintf '{\"is_error\":false,\"result\":\"x\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}'\n",
        );
        let c = backend(CliFlavor::Claude, s, dir.path())
            .complete(&req())
            .await
            .unwrap();
        assert_eq!(c.usage.model, None);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn codex_cli_stub() {
        let dir = tempfile::tempdir().unwrap();
        // Reads the prompt from stdin and checks the isolation and the `-` argument.
        let s = stub(
            dir.path(),
            "codex",
            "#!/bin/sh\n[ -z \"$(ls -A .)\" ] || exit 3\nfor a in \"$@\"; do last=$a; done\n[ \"$last\" = - ] || exit 4\ncat > /dev/null\nprintf '%s\\n' '{\"type\":\"thread.started\",\"thread_id\":\"t\"}' '{\"type\":\"item.completed\",\"item\":{\"id\":\"i\",\"type\":\"agent_message\",\"text\":\"fine\"}}' '{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":30,\"output_tokens\":2,\"reasoning_output_tokens\":0}}'\n",
        );
        let c = backend(CliFlavor::Codex, s, dir.path())
            .complete(&req())
            .await
            .unwrap();
        assert_eq!(c.text, "fine");
        assert_eq!(c.usage.input_tokens, 30);
        assert!(tmp_is_empty(dir.path()));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn codex_failure_reports_the_event_message() {
        let dir = tempfile::tempdir().unwrap();
        let s = stub(
            dir.path(),
            "codex",
            "#!/bin/sh\ncat > /dev/null\nprintf '%s\\n' '{\"type\":\"turn.failed\",\"error\":{\"message\":\"model is not supported\"}}'\nexit 1\n",
        );
        let e = backend(CliFlavor::Codex, s, dir.path())
            .complete(&req())
            .await
            .unwrap_err();
        assert!(matches!(&e, LlmError::Provider(m) if m.contains("model is not supported")));
    }

    /// An `agy` stand-in: prints `init`, waits for the prompt line, prints `result`.
    #[cfg(unix)]
    fn agy_stub(dir: &Path, mode: &str, result: &str) -> PathBuf {
        let script = format!(
            "#!/bin/sh\n[ -z \"$(ls -A .)\" ] || exit 3\nprintf '%s\\n' '{{\"event\":\"init\",\"conversation_id\":\"7c9f45a5-4d89-496c-adf1-d31a1bfaf7fb\",\"init\":{{\"model\":\"gemini-3.8-flash\",\"permission_mode\":\"{mode}\"}}}}'\nread -r line || exit 4\ncase \"$line\" in *'\"event\":\"user\"'*) ;; *) exit 5;; esac\nprintf '%s\\n' '{result}'\n"
        );
        stub(dir, "agy", &script)
    }

    #[cfg(unix)]
    fn plant_conversation(state: &Path) -> (PathBuf, PathBuf) {
        let id = "7c9f45a5-4d89-496c-adf1-d31a1bfaf7fb";
        let db = state.join("conversations").join(format!("{id}.db"));
        let brain = state.join("brain").join(id);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        std::fs::create_dir_all(&brain).unwrap();
        std::fs::write(&db, "x").unwrap();
        (db, brain)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn antigravity_cli_stub_success_and_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let s = agy_stub(
            dir.path(),
            "request-review",
            r#"{"event":"result","result":{"conversation_id":"7c9f45a5-4d89-496c-adf1-d31a1bfaf7fb","status":"SUCCESS","response":"fine\n","usage":{"input_tokens":12,"output_tokens":1,"thinking_tokens":0}}}"#,
        );
        let b = backend(CliFlavor::Antigravity, s, dir.path());
        let state = b.antigravity_dir.clone().unwrap();
        let (db, brain) = plant_conversation(&state);
        let c = b.complete(&req()).await.unwrap();
        assert_eq!(c.text, "fine\n");
        assert_eq!(c.usage.model.as_deref(), Some("gemini-3.8-flash"));
        assert_eq!(c.usage.cost_usd, None);
        assert!(!db.exists() && !brain.exists(), "conversation removed");
        assert!(tmp_is_empty(dir.path()));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn antigravity_denied_tool_is_an_error_and_still_cleans_up() {
        let dir = tempfile::tempdir().unwrap();
        let s = agy_stub(
            dir.path(),
            "request-review",
            r#"{"event":"result","result":{"conversation_id":"7c9f45a5-4d89-496c-adf1-d31a1bfaf7fb","status":"SUCCESS","response":"","denied_actions":[{"action":"command","display_name":"RunCommand"}]}}"#,
        );
        let b = backend(CliFlavor::Antigravity, s, dir.path());
        let (db, brain) = plant_conversation(b.antigravity_dir.as_ref().unwrap());
        let e = b.complete(&req()).await.unwrap_err();
        assert!(matches!(&e, LlmError::Provider(m) if m.contains("command")));
        assert!(!db.exists() && !brain.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn antigravity_refuses_widened_permissions_without_running() {
        let dir = tempfile::tempdir().unwrap();
        // The stub would leave a marker if it ran.
        let s = stub(
            dir.path(),
            "agy",
            &format!("#!/bin/sh\ntouch {}/ran\n", dir.path().display()),
        );
        let b = backend(CliFlavor::Antigravity, s, dir.path());
        let state = b.antigravity_dir.clone().unwrap();
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(
            state.join("settings.json"),
            r#"{"permissions":{"allow":["command(git)"]}}"#,
        )
        .unwrap();
        let e = b.complete(&req()).await.unwrap_err();
        assert!(matches!(&e, LlmError::Config(m) if m.contains("permissions.allow")));
        assert!(!dir.path().join("ran").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn antigravity_aborts_on_an_unsafe_permission_mode_before_the_prompt() {
        let dir = tempfile::tempdir().unwrap();
        // Exits 9 if it ever receives the prompt.
        let s = stub(
            dir.path(),
            "agy",
            "#!/bin/sh\nprintf '%s\\n' '{\"event\":\"init\",\"conversation_id\":\"7c9f45a5-4d89-496c-adf1-d31a1bfaf7fb\",\"init\":{\"model\":\"m\",\"permission_mode\":\"always-proceed\"}}'\nsleep 5\n",
        );
        let b = backend(CliFlavor::Antigravity, s, dir.path());
        let started = Instant::now();
        let e = b.complete(&req()).await.unwrap_err();
        assert!(matches!(&e, LlmError::Config(m) if m.contains("always-proceed")));
        assert!(started.elapsed() < Duration::from_secs(4), "aborted early");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn antigravity_refuses_an_init_without_a_permission_mode() {
        let dir = tempfile::tempdir().unwrap();
        let s = stub(
            dir.path(),
            "agy",
            "#!/bin/sh\nprintf '%s\\n' '{\"event\":\"init\",\"conversation_id\":\"7c9f45a5-4d89-496c-adf1-d31a1bfaf7fb\",\"init\":{\"model\":\"m\"}}'\nsleep 5\n",
        );
        let started = Instant::now();
        let e = backend(CliFlavor::Antigravity, s, dir.path())
            .complete(&req())
            .await
            .unwrap_err();
        assert!(matches!(&e, LlmError::Config(m) if m.contains("not reported")));
        assert!(started.elapsed() < Duration::from_secs(4));
    }

    #[test]
    fn auth_classification_needs_a_phrase() {
        assert!(looks_like_auth_failure("401 Unauthorized: please log in"));
        assert!(looks_like_auth_failure("Authentication required"));
        // Request ids, token counts and unrelated words are not auth failures.
        assert!(!looks_like_auth_failure("rate limited, request id 4017abc"));
        assert!(!looks_like_auth_failure("login shell exited"));
        assert!(!looks_like_auth_failure("used 4012 input tokens"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn claude_exit_errors_stay_provider_errors() {
        let dir = tempfile::tempdir().unwrap();
        let s = stub(
            dir.path(),
            "claude",
            "#!/bin/sh\ncat > /dev/null\necho 'please log in' >&2\nexit 1\n",
        );
        let e = backend(CliFlavor::Claude, s, dir.path())
            .complete(&req())
            .await
            .unwrap_err();
        assert!(matches!(e, LlmError::Provider(_)), "{e:?}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn antigravity_error_status_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let s = stub(
            dir.path(),
            "agy",
            "#!/bin/sh\nprintf '%s\\n' '{\"event\":\"result\",\"result\":{\"conversation_id\":\"\",\"status\":\"ERROR\",\"response\":\"\",\"error\":\"invalid model selection\"}}'\nexit 1\n",
        );
        let e = backend(CliFlavor::Antigravity, s, dir.path())
            .complete(&req())
            .await
            .unwrap_err();
        assert!(matches!(&e, LlmError::Provider(m) if m.contains("invalid model")));
    }
}
