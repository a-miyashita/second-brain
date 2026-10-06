# ADR-0005: Summarizer abstraction and generator provenance (overwrite, no history)

- Status: Accepted
- Date: 2026-09-29

## Context

Entries carry LLM-generated sections: overview, decisions and action items. Meet
entries also carry a generated details section. These are produced by one of:

- LLM HTTP APIs: Anthropic, Google (Gemini), OpenAI;
- LLM CLIs: Claude Code (`claude -p`), GitHub Copilot CLI (`copilot -p`);
- local LLMs, ideally on an Intel NPU. The practical runtime is **Foundry Local**,
  which exposes an OpenAI-compatible HTTP server; OpenVINO Model Server, Ollama
  and llama.cpp server expose the same API;
- the source itself: Gemini "Take notes for me" in Google Meet.

The user wants to be able to re-run summaries later with a better model, and to
know which generator produced the current text. Keeping a history of summaries was
considered and **rejected as unnecessary**.

## Decision

- A `Summarizer` trait in `sb-core`:
  `summarize(&SummaryInput) -> Result<SummaryOutput>`.
  - `SummaryInput` carries the source kind, title, date, optional user-provided
    context and the body text.
  - `SummaryOutput` carries the structured sections and token usage.
- Implementations live in `sb-llm`:

  | `generator_kind` | Implementation | Notes |
  |---|---|---|
  | `llm_api` | `anthropic` | Messages API |
  | `llm_api` | `openai` | Chat Completions; also used for any OpenAI-compatible endpoint |
  | `llm_api` | `google` | Gemini API |
  | `local_llm` | `openai_compatible` | Base URL configured; Foundry Local, OVMS, Ollama, llama.cpp |
  | `llm_cli` | `claude_cli` | Run in an empty temporary directory so no project instructions are picked up |
  | `llm_cli` | `copilot_cli` | Same isolation |
  | `source_native` | — | Not a summarizer; set by source adapters (e.g. Gemini Meet notes) |

- **Local LLM runtimes are not embedded.** second-brain talks HTTP to them. NPU
  support is the runtime's job. This keeps the binary static (ADR-0001).
- **Summarizer profiles**:
  - A profile is a named setting (`llm.profiles.<name>`) holding kind, provider,
    model, base URL, concurrency and a secret reference.
  - Each source kind maps to a profile (`summary.profile.<source_kind>`, falling
    back to `summary.profile.default`).
- **Provenance, overwrite semantics**: each entry has at most one summary record.
  It stores:
  - `generator_kind`, `provider`, `model`, `prompt_version`, `generated_at`;
  - `input_hash`: the hash of the exact input text given to the summarizer;
  - token usage.

  Regenerating **overwrites** the generated sections and this record. There is no
  history.
- Sections carry an `origin`:
  - `generated`: written by a summarizer or source-native generator; replaced on
    re-summarization;
  - `extracted`: a deterministic rendering of raw data, e.g. a formatted Slack
    conversation;
  - `user`: e.g. `background` context supplied at ingest.

  Only `generated` sections are replaced.
- **Gemini Meet notes** are recorded as `generator_kind = source_native`,
  `provider = google`, `model = gemini-meet-notes`. The notes document itself is
  stored as raw data. That means the Gemini version can always be restored by
  re-extracting it (`sb resummarize --native`), even after overwriting with
  another model.
- Prompts are compiled into the binary and versioned (`entry-summary/v1`, ...).
  Changing a prompt's wording bumps its version.
- Re-summarization is explicit (`sb resummarize` with filters; see
  [specs/summarization.md](../specs/summarization.md)). Automatic re-summarization
  happens only when `input_hash` changes during sync.

## Consequences

- One code path serves API, CLI and local models. Adding a provider means adding
  one implementation.
- Without history, a bad re-run cannot be undone by rollback. It is undone by
  re-running with the previous profile, which the raw data makes possible.
  `--dry-run` and `--estimate` exist to make re-runs deliberate.
- Configuring Foundry Local is harder than using an API key. `sb setup llm` provides
  an interactive wizard (detect → choose model/device → test → save). A GUI can
  wrap the same logic later.

## Alternatives considered

- **Keep all summary versions**: rejected by the user as unnecessary complexity.
- **Embed OpenVINO / ONNX Runtime**: large native dependencies, per-platform
  builds, and NPU driver coupling inside our binary.

## Amendments

### 2026-10-06: Foundry Local wizard dropped, local LLMs not recommended

See [ADR-0015](0015-local-llm-not-recommended.md). The `local_llm` kind and the
`openai_compatible` provider stay, but:

- the "ideally on an Intel NPU" aim in Context is withdrawn: NPU models hit an output
  context limit and are not usable;
- the interactive Foundry Local wizard in Consequences is **not** built;
- summaries from models smaller than Haiku class are discouraged; the Foundry Local
  runtime is only "an OpenAI-compatible server" like Ollama or llama.cpp.
