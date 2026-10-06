# ADR-0015: Local LLMs are not recommended for summaries; the Foundry Local wizard is dropped

- Status: Accepted
- Date: 2026-10-06

## Context

ADR-0005 listed local LLMs, "ideally on an Intel NPU" through **Foundry Local**, as a
summarizer option, and planned a Foundry Local wizard in `sb setup llm` (phase 2).
Trying it on a Copilot+ PC showed two problems:

- **NPU models are not usable.** The models that run on the NPU hit a limit on the
  output context, so they cannot produce the structured summary of a long input, even
  with map-reduce.
- **Small local models hurt quality.** Models of about 5 to 10B parameters summarize
  and translate with low accuracy. Their summaries contain errors that look
  plausible. A wrong summary is worse than none, because search results and agents
  trust the generated sections. In practice the floor is a model of **Haiku class**.

## Decision

- The **Foundry Local wizard is removed** from the plans (`sb setup llm` in phase 2,
  ADR-0009's `setup llm` row). It is not implemented.
- Local LLMs are **no longer recommended**. Documentation and setup text recommend an
  API or a CLI summarizer of at least Haiku class. A local model of a size that
  reaches that quality is fine, and the user decides.
- The **`local_llm` kind and the `openai_compatible` provider stay**, as does the
  `local` preset of `sb setup llm`. They talk to any OpenAI-compatible server
  (Ollama, llama.cpp, vLLM, a company gateway). Nothing in the code is removed for this
  decision, only the Foundry-specific wizard and wording.
- Documentation examples no longer use an NPU model. Foundry Local stays usable as
  "an OpenAI-compatible server" like any other, without special support.
- The budget rule of ADR-0013 is unchanged: `local_llm` calls are recorded with
  cost `0` and are not gated.

## Consequences

- Smaller scope for phase 2: no hardware detection, model catalog browsing or model
  download logic.
- Users without an API key or a CLI subscription have no endorsed offline path. The
  default for `google.meet` remains `native` (Gemini's own notes), which needs no
  summarizer.
- The text in `README.md`, in `sb setup llm` (preset label) and in the specs changes
  from "Foundry Local on an NPU" to a neutral description.

## Amendments

(none yet)
