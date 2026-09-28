# Summarization

Related ADR: 0005.

## Output structure

Summarizers must return JSON matching:

```json
{"overview": "2-4 sentences",
 "decisions": ["who decided what"],
 "action_items": ["who does what by when"],
 "details": "optional; only for sources whose details are generated (meetings)"}
```

These are rendered to Markdown sections with `origin = generated`:

- `overview` as text;
- `decisions` and `action_items` as bullet lists; empty arrays produce no section;
- `details` as text, only when requested for the source kind.

Invalid JSON is retried once with a repair instruction. If it fails again, the entry
gets `summary_status = failed` and an `llm.bad_output` issue.

## Commit and resume rules (ADR-0012)

- Each summary is committed in its own transaction as soon as it returns. Paid work
  is never lost to a later failure or an interruption.
- Selection for `sb summarize` and the summarize stage of `sb sync` is
  `summary_status IN (pending, failed) AND summary_attempts < summary.max_attempts`
  (default 3). `--retry-failed` resets the attempt counters of the selected
  entries.
- **Limits:** `--max-summaries N`, `--max-cost USD` (estimated from actual token
  usage and the price table) and `--time-limit`. When a limit is reached, no new
  calls are started, in-flight calls finish, and the run ends as
  `stopped_by_limit`.
- **Progress:** during a run, stderr shows done / remaining and the running cost
  estimate. The final line says what remains and how to continue.

## Prompt rules

- Write only what the input says. No speculation.
- Keep proper nouns, numbers and dates verbatim.
- Decisions say who decided what. Action items say who, what and by when; the owner
  is omitted if unknown.
- Chit-chat gets a one-line overview and empty lists.
- Output language: the setting `summary.language`: `auto` (same as the input,
  default), `ja` or `en`.

Prompts are embedded per purpose, each with a version:

| Prompt | Used for |
|---|---|
| `conversation-summary/v1` | slack.* |
| `meeting-summary/v1` | google.meet from transcript. Also produces `details` |
| `document-summary/v1` | google.doc, web.page, local.file, mail.* |

## Input construction

The source adapter produces a `SummaryInput`:

| Source kind | Input body |
|---|---|
| slack.* | The formatted conversation (the same text as the `extracted` details section) |
| google.meet | The transcript if present (embedded in the notes or a separate document; see source-google-meet.md), after mechanical cleanup. Otherwise the Gemini notes document text (the second case re-summarizes Gemini's own summary; allowed, but `doctor` shows how many meet entries lack transcripts) |
| documents | Extracted text; the user context is passed separately as a hint |

- `input_hash` is SHA-256 over the prompt version, the profile's model and the body.
  A sync skips summarization when the stored `input_hash` matches.
- **Long inputs:** if the body exceeds the profile's `max_input_chars` (default
  40 000, tunable per profile), it is summarized map-reduce style:
  1. split into chunks on message or paragraph boundaries;
  2. produce partial summaries;
  3. make a final merge call.

  Head+tail truncation is not used, because transcripts are routinely longer than
  the limit and the middle would be lost.
- **Thresholds:** conversations shorter than `summary.min_chars` (default 400) or
  `summary.min_messages` (default 3) get `summary_status = skipped`. Only the
  extracted details are kept.

## Profiles

Setting `llm.profiles.<name>`:

```toml
[llm.profiles.fast]
kind = "llm_api"            # llm_api | llm_cli | local_llm
provider = "anthropic"      # anthropic | openai | google | openai_compatible | claude_cli | copilot_cli
model = "claude-haiku-4-5"
concurrency = 8
max_input_chars = 40000
secret = "global:anthropic.api_key"   # optional; env fallback per ADR-0003

[llm.profiles.npu]
kind = "local_llm"
provider = "openai_compatible"
base_url = "http://127.0.0.1:5273/v1" # discovered by `sb setup llm` for Foundry Local
model = "phi-4-mini-instruct-openvino-npu"
concurrency = 1
request_timeout_secs = 300
warmup_timeout_secs = 600             # first call may include model load
start_command = ["foundry", "service", "start"]   # optional; run if the server is unreachable
keep_alive = "30m"                    # optional; forwarded where the runtime supports it
```

The profile model strings above are examples only.

Selecting a profile:

- `summary.profile.default = "fast"`
- `summary.profile.<source_kind> = "..."` overrides the default per source kind.
- `native` is a reserved profile name (not definable).
  `summary.profile.google.meet = "native"` means "keep Gemini notes, do not
  summarize". This is the default for google.meet.

### Local LLM servers (`kind = "local_llm"`)

The model is loaded and kept by the server process (Foundry Local, OpenVINO Model
Server, Ollama, llama.cpp server), not by second-brain. Each summary is one HTTP
request. Loading happens at most once per server lifetime, or again after the
runtime's idle unload. It never happens per entry.

Before the first summary of a run, the summarize stage does the following:

1. **Reachability:** `GET <base_url>/models`, with a short timeout (5 s).
   - If the server is unreachable and `start_command` is set, run it, then poll
     the endpoint until `warmup_timeout_secs`.
   - If the server is still unreachable, leave the entries `pending` (they are not
     counted as failed attempts), open an `llm.local_unreachable` issue, and
     continue the run without summaries. Fetched data is unaffected.
2. **Warm-up:** send one tiny completion (a few tokens) with
   `warmup_timeout_secs` (default 600). This absorbs lazy model loading, and the
   elapsed time is logged. Real requests then use `request_timeout_secs`.
3. **Keep-alive:** when `keep_alive` is set and the provider supports it, it is
   sent with every request, so the model is not unloaded between the fetch and
   summarize stages. Ollama supports this with `keep_alive`. Other runtimes may
   ignore it; their own idle-unload setting applies.

If a request times out in the middle of a run (for example, because the runtime
unloaded the model), it is retried once with `warmup_timeout_secs` before it counts
as a failed attempt.

`start_command` is also used by the scheduled job. The job's `PATH` includes the
command's directory (see setup-and-scheduling.md).

### LLM CLI providers

- They run in a fresh empty directory under `$SECOND_BRAIN_HOME/tmp/`, so no
  project `AGENTS.md` / `CLAUDE.md` is picked up.
- The prompt goes on stdin, so there are no argv length limits. A timeout applies
  (default 300 s).
- `claude_cli`: `claude -p --output-format json [--model M]`.
- `copilot_cli`: `copilot -p <prompt>` with the equivalent non-interactive flags.
  The exact flags are verified at implementation time.
- The CLI's login state is checked by `sb doctor --online` with a trivial prompt.

### Foundry Local wizard (`sb setup llm`, phase 2)

1. Detect the `foundry` CLI and the service status (`foundry service status`), and
   discover the endpoint URL.
2. List catalog models, highlighting NPU/OpenVINO variants for the detected
   hardware.
3. Download and load the selected model if needed. The user confirms first, because
   models are large.
4. Run a test summarization with a fixed sample and show the output and latency.
5. Save the profile. Optionally set it as the default for a source kind.

The wizard is a library function in `sb-setup`, so a future GUI can reuse it.

## `sb resummarize`

```text
sb resummarize [--account A] [--source K] [--since D] [--until D] [--entry UID]...
               [--where-model M] [--where-provider P]
               (--profile P | --native)
               [--estimate] [--dry-run] [--limit N] [--yes]
```

1. Select entries by the filters. `--where-model` matches the current
   `summaries.model`. Entries whose current summary already has the target
   generator (provider, model, prompt version) and the same `input_hash` are
   **skipped** unless `--force` is given. That is what makes an interrupted
   re-summarization resumable: run the same command again.
2. Skip entries with `raw_status != present`, and report how many. The suggested
   remedy is `sb refetch` on the same filters.
3. `--estimate`: count tokens (character-based heuristic) and show the estimated
   cost from the per-model price table in the binary. Prices are overridable by the
   setting `llm.prices`.
4. Without `--yes`, ask for confirmation when more than 20 entries are affected.
5. For each entry, rebuild the input from raw data, summarize, then in one
   transaction replace the `generated` sections and the `summaries` row and
   re-index. The limits and graceful-stop rules above apply.
6. `--native` (google.meet only) re-extracts Gemini notes from raw data and records
   `source_native` again.
