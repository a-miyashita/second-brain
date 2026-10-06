# ADR-0017: Richer CLI support — resolved model provenance and body-only regeneration

- Status: Accepted
- Date: 2026-10-06

This ADR has two parts. **Part 1** settles how models are recorded and when a
summary is regenerated. **Part 2** adds the Codex and Antigravity CLI providers. Both
are one change, "richer CLI support": the new providers are what make the model
rules above worth having (a tool switch must not re-summarize the catalog).

## Context

A CLI profile names a model by alias, for example `model = "haiku"` for
`claude_cli`. The alias is the right thing to configure: it follows the newest model
of that class with no config change. But it is the wrong thing to record:

- `summaries.model` and `llm_usage.model` hold `haiku`, so `sb stats` shows
  `llm_cli/claude-cli/haiku` and `sb budget --by-model` cannot tell Haiku 4.5 from
  its successor. ADR-0005 exists to answer "which model produced this text".
- The CLI knows the answer. `claude -p --output-format json` reports the model it
  actually used (`modelUsage`, keyed by the full model id).

The model is also part of the regeneration rules, which makes the alias worse than
cosmetic:

- `input_hash` is `sha256(prompt_version, model, body)` (`summary_input_hash`), and
  it is stored in `summaries.input_hash`. A change of model changes the hash, so the
  next `sync` marks **every** entry `pending` and pays to summarize all of them.
- `sb resummarize` skips an entry only when its provider, model, prompt version and
  hash all match the target.

So switching tools or model, or even upgrading a model, silently triggers a
full-catalog re-summarization. For a paid or rate-limited CLI that is the opposite of
what the user wants: a summary that was good enough yesterday should stay.

## Decision (Part 1)

### 1. Configure the alias, record the resolved model

- The profile `model` is what is passed to the CLI (`haiku`). It is a request.
- The recorded `model` (in `summaries` and `llm_usage`) is the **model the CLI
  reports it used**, as reported, for example `claude-haiku-4-5-20251001`. It is not
  shortened or normalized: stripping a date suffix is lossy and breaks when naming
  changes. Display code may shorten it.
- If the CLI reports nothing usable, the configured name is recorded, as today.
  The profile name is always recorded in `profile`, so the request is not lost.
- A backend returns the resolved model with its completion. The summarizer passes
  the last one it saw up with the output (map-reduce makes several calls; if they
  disagree, the last wins and the difference is logged at `debug`). The pipeline
  uses it in place of the pre-call `Generator.model` in **both** the `summaries` row
  and the `llm_usage` row, in the same transaction as before (ADR-0013).
- Scope: `llm_cli` providers. API providers keep recording the configured model,
  which is already a pinned id. Nothing is rewritten for old rows: they keep `haiku`.
- Price lookup does not change: it uses the profile's model, and a CLI that reports
  its own cost keeps using that.

### 2. A summary is regenerated only when the body changes

Automatic regeneration (the summarize stage of `sync`, `summarize`, `ingest`, the
re-evaluation in `decide`) depends **only on the input body**:

- `input_hash` becomes a hash of the body alone. Model and prompt version are no
  longer part of it. The stored value carries a format tag (`b2:<hex>`), so the two
  generations of hashes can never be confused.
- Changing the model, the provider, the profile or the CLI does not touch existing
  summaries. Neither does a new prompt version. They apply to **new and changed
  entries only**.
- `source_native` hashes are unchanged (they never included a model).
- Re-summarizing with a different model or prompt is an explicit act:
  `sb resummarize` (below). A bulk, model-targeted form of it is a later feature and
  out of scope here; today `--where-model` with `--force` does it.

### 3. `sb resummarize` keeps its resume rule, without the model

`resummarize` must stay resumable: after an interruption, running the same command
again must not redo finished entries. Its skip rule changes from
`provider + model + prompt_version + input_hash` to

> the current summary was produced by the **same profile name**, has the **target
> prompt version**, and has the same `input_hash`.

The recorded model is not compared. Consequences, stated plainly:

- A new prompt version makes `sb resummarize --profile P` pick up entries from older
  prompts without `--force`, so prompt improvements stay reachable.
- If the *model setting of the same profile* changed (`haiku` to `sonnet`), entries
  summarized under that profile name are skipped. Use `--force`, or a new profile
  name. This is the price of not comparing models, and it is intended: model changes
  are never applied implicitly.
- `--where-model` matches the recorded (resolved) model **exactly**. After this ADR
  a Haiku summary is `claude-haiku-4-5-20251001`, not `haiku`; patterns are a
  possible future addition.

### 4. Existing hashes must be upgraded, not invalidated

Changing the formula would make every stored `input_hash` stale and trigger the full
re-summarization this ADR is meant to prevent. Therefore:

- A one-time, idempotent **hash upgrade** runs after migrations. It is gated by the
  setting `summary.input_hash_version` (inserted by migration `0003` with value `1`,
  set to `2` on completion). Migrations stay data-free: a SQL migration cannot read
  raw files.
- For each `summaries` row of kind `llm_*`, it rebuilds the input from raw data (the
  same step `resummarize` uses), computes the new hash, and stores it. It never calls
  an LLM and never changes a summary, a section or `summary_status`.
- A row whose input cannot be rebuilt (raw data absent, extraction now failing) gets
  an **empty** `input_hash`. An empty hash means "unknown baseline": the summary is
  kept, and the first time the entry is evaluated with a rebuildable body, that
  body's hash is adopted as the baseline **without** regenerating. It cannot detect
  a change that happened while the baseline was unknown; that is accepted.
- The step is resumable (rows already at `b2:` are skipped), reports
  `upgraded / unknown / total` once, and sets the marker only when every row has been
  visited. `--dry-run` and `--estimate` never run it.
- The upgrade is a correctness requirement of this change, not an optimization:
  shipping the new formula without it is a defect.

## Consequences

- `sb stats` and `sb budget --by-model` show the real model. Old rows keep the alias
  and appear as a separate group; there is no backfill, because the old value is
  all that was ever recorded.
- Switching the default profile to another model or tool is safe and free: nothing is
  re-summarized until the user asks. Equally, **a model change is no longer a trigger
  for improving old summaries**; that moves entirely to the explicit command.
- During the gap before a bulk by-model re-summarization exists, a catalog-wide model
  switch needs `sb resummarize --force`. This is the known cost of deciding it
  explicitly.
- A prompt-version bump no longer refreshes summaries by itself. Authors of prompt
  changes should say so in release notes.
- Hash upgrade is the riskiest part: it depends on `rebuild_input` giving the same
  body as the original ingest. The plan tests this against a real catalog copy
  before release.

## Alternatives considered

- **Keep the model in the hash, record the alias (status quo).** Rejected: provenance
  is wrong and every model change re-summarizes everything.
- **Record the alias, keep the resolved id in `usage` JSON only.** Smaller change,
  but then `stats` and `--by-model` still group by alias. Rejected.
- **Extra column for the requested alias.** Needed only if the alias took part in
  comparisons; it no longer does, and `profile` already records the request.
- **Leave the hash as is and special-case aliases in the comparison.** Rule per
  vendor, and still re-summarizes on a real model change. Rejected.
- **Re-summarize lazily when the hash is empty or stale.** That is the cost the user
  is avoiding; adoption without regeneration is chosen instead.

## Decision (Part 2): Codex and Antigravity providers

Two providers join `claude_cli` and `copilot_cli` under `generator_kind = llm_cli`.
Like the others, each runs in a fresh empty directory under `$SECOND_BRAIN_HOME/tmp/`
with the prompt on stdin, and with its tools turned off or denied. Behaviour below was
observed with `codex-cli 0.160.1` and `agy 1.3.0` and is **re-verified at
implementation** (task T1).

| | `codex_cli` (recorded `codex-cli`) | `antigravity_cli` (recorded `antigravity-cli`) |
|---|---|---|
| Binary | `codex` | `agy` |
| Model | the profile `model`, a versioned id such as `gpt-6-luna`. There is no alias that follows new versions | the profile `model`, such as `gemini-3.8-flash` |
| Reasoning effort | fixed `low`: `-c model_reasoning_effort="low"` | fixed `low`: `--effort low` |
| Invocation | `codex exec -m M -s read-only --skip-git-repo-check --ephemeral --ignore-rules --ignore-user-config --color never --json -`, plus the tool-disabling flags below | `agy --input-format stream-json --output-format stream-json --model M --effort low`; one stdin line `{"event":"user","message":{"content":"<prompt>"}}` |
| Why that input | `-` reads the prompt from stdin | `agy -p` takes the prompt as an argument, and an input of up to 40 000 characters can exceed the argument limit; stream-json takes it on stdin |
| Answer | the last `item.completed` with `item.type = "agent_message"` (`text`) | `result.response` of the `result` event |
| Tokens | `turn.completed.usage`: input `input_tokens`, output `output_tokens + reasoning_output_tokens` | `result.usage`: input `input_tokens`, output `output_tokens + thinking_tokens` |
| Resolved model | not reported: the configured model is recorded | `init.model`; the configured model if absent |
| Cost | not reported | not reported |

Common rules:

- **Reasoning effort is fixed to low** and is not a profile setting. A summary needs
  no deep reasoning, and it is cheaper and faster.
- **Cost is not reported by either CLI.** Their calls are recorded with
  `cost_usd = NULL` and counted as *unpriced calls* (ADR-0013). They do not consume
  the budget caps and are not blocked by an exhausted budget beyond the existing
  "unknown price reserves nothing" rule. Pricing them is future work.
- Neither CLI reports the model it resolved to, beyond what is in the table, so the
  Part 1 rule applies unchanged: the reported model if there is one, else the
  configured one.
- Both CLIs add a large fixed prompt of their own (about 12 000 to 30 000 input
  tokens per call). That is recorded as the usage and is why a call is not "free".

### Codex: tools are disabled explicitly

`-s read-only` is **not** enough: with it the model could still run `cat /etc/hostname`.
The invocation therefore adds `--disable shell_tool`, `unified_exec`, `plugins`,
`apps`, `browser_use` and `computer_use`. With these the model replied that it has
no tools. Because feature names change between Codex versions:

- the list is data in one place, with a unit test of the rendered arguments;
- `sb doctor --online` (`llm.online`) sends a probe prompt that asks the model to
  run a command and fails the check for this profile if the command runs. An
  unknown feature name makes Codex print a warning or fail; either is a failed check,
  not a silent pass.

### Antigravity: default-deny, guarded against user allow-rules

In headless mode `agy` soft-denies every tool that needs approval. Observed, also in
a workspace listed under `trustedWorkspaces`: command execution, URL fetching and
file writes are denied; only reads **inside the workspace** are allowed, and the
workspace is the empty directory we create, so there is nothing to read.

The one gap is the user's `~/.gemini/antigravity-cli/settings.json`: a
`permissions.allow` rule applies to our call too, and there is no flag or environment
variable to ignore it (the credentials live in the same directory, so redirecting
`HOME` is not an option). Therefore:

- Before every call (cached for the process), read that file. If it cannot be parsed,
  or `permissions.allow` is present and non-empty, **do not run**: fail with a
  configuration error (`LlmError::Config`) that names the setting. Entries stay
  `pending` and the attempt counter is not incremented, like other configuration
  errors. A missing file is fine.
- `sb doctor` reports the same condition as `llm.antigravity_permissions` (error
  when a profile uses `antigravity_cli`).
- If the `init` event reports a `permission_mode` other than `request-review`, the
  call is aborted with the same configuration error. (Whether `init` arrives before
  the prompt is read is verified in T1; if not, this check is a doctor-only check.)
- A denied tool does not make the CLI fail: it returns `status = SUCCESS` with an
  **empty** `response` and a `denied_actions` list. An empty response is an error
  (`LlmError::Provider`, naming the denied actions), never an empty summary.

### Antigravity: conversations are deleted after the call

Every call stores a conversation: about 450 KB in
`~/.gemini/antigravity-cli/conversations/<id>.db` and an empty `brain/<id>/`. A first
sync of a thousand entries would leave about 450 MB. There is no flag to avoid it.

- The `conversation_id` is in the output. After the call (success or failure), the
  backend deletes exactly those two paths.
- The id must be a canonical UUID before a path is built from it; anything else is
  ignored. Nothing else under that directory is touched.
- A failure to delete is logged at `debug` and never fails the summary.
- The directory is `~/.gemini/antigravity-cli` (the user's home; `dirs` on every
  platform). If it does not exist, nothing is done.

### Setup and checks

- `sb setup llm --preset` gains `codex_cli` and `antigravity_cli`. The presets use
  `gpt-6-luna` and `gemini-3.8-flash` as the starting model, which the user may change
  with `--model`. These are examples of a Haiku-class model at the time of writing
  and are not defaults baked into summarization.
- `profile.rs` accepts the new providers for `kind = llm_cli` only.
- The absolute paths of `codex` and `agy` are resolved at scheduler registration like
  `claude` and `copilot`.

## Consequences (Part 2)

- Four CLI providers share one backend with per-flavour argument and parsing code.
  A CLI that reports no cost or model is handled by the two fallbacks above.
- Antigravity is refused outright for users whose settings allow tools. That is the
  safe direction: a summary is built from other people's text, which is untrusted
  data (ADR-0014), and an allowed `read_url` or `command` would let it act.
- Codex safety rests on a list of feature names that can drift; the doctor probe is
  the guard.
- Antigravity's deletion touches another tool's state directory. It is limited to the
  two paths of our own conversation and is the only place the project writes outside
  its home, which the spec for the backend states.

## Alternatives considered (Part 2)

- **Skip Antigravity** because the user's settings cannot be overridden. Rejected
  once the default was verified safe; the residual risk is detectable and refused.
- **`agy -p "<prompt>"`.** Simple, but the argument limit is reachable with 40 000
  Japanese characters.
- **Profile-level `effort`.** Rejected: nobody should tune it for a summary.
- **Leave Antigravity conversations.** About 450 MB per thousand entries is not
  harmless.
- **Rely on `-s read-only` for Codex.** It still allows shell reads.

## Amendments

### 2026-10-07: details settled during implementation

- The resolved model travels in `Usage.model` (not a new field on `Completion`); the
  summarizer's `Usage::add` keeps the last reported one, and the pipeline records it
  in `summaries`, `llm_usage` and the `summaries.usage` JSON. Verified against
  `claude` 2.x: `modelUsage` is keyed by the full id (`claude-haiku-4-5-20251001` for
  `--model haiku`).
- Antigravity: `init` is emitted by `agy` before it reads the prompt (about 3 s after
  start, with stdin still open), so the `permission_mode` check aborts the call
  **before** the prompt is sent. The settings file is read on every call, not cached
  for the process; it is a tiny file and a stale answer would be the unsafe one.
- Codex: an unknown `--disable` feature makes `codex` exit 1 (`Unknown feature
  flag`), so a renamed feature fails loudly instead of silently re-enabling a tool.
  The `llm.online` probe additionally checks that a command is not executed.
- A model that the CLI rejects (`codex`, `agy`) is reported as a provider error from
  the error events (`turn.failed`; `result.status = ERROR`), not as an empty answer.
- Hash upgrade: it runs at the start of `sync`, `summarize`, `resummarize` and
  `ingest` (never for `--dry-run` or `--estimate`). Release gate, on a copy of a real
  catalog (2907 entries, 913 LLM summaries): the upgrade took 15 s, rewrote 913 hashes
  with none unknown, and a following `sb reextract` of all entries left all 1427
  `done` summaries `done`. Nothing was sent to an LLM.
- Migration `0003` only inserts the marker `summary.input_hash_version = 1`; the
  data upgrade is code, as decided above.

### 2026-10-07: changes after code review

- Hash upgrade: only a row with **no** input to rebuild becomes "unknown". A rebuild
  that fails (or a source that cannot be built) is *deferred*: the row is left as it
  is, the marker stays at `1`, and the next run retries it, so a transient failure
  cannot permanently lose the baseline.
- Antigravity fails **closed**: an `init` event without `permission_mode` is refused
  like an unsafe mode, and the prompt is not sent.
- Error-text matching for authentication failures is limited to the two new CLIs and
  to phrases (`please log in`, `unauthorized`, `authentication`, ...), not bare
  substrings such as `401`; Claude and Copilot keep reporting provider errors.
- The Codex doctor probe keeps its random token out of the file name, so a reply that
  only echoes the command is not read as proof that the shell ran.
- The conversation id used for cleanup must be a canonical **lower-case** UUID.
- Not changed, on purpose: a `resummarize` that ignores a changed model under the
  same profile (decided above, `--force` applies it), and a legacy hash that is
  kept until the upgrade has run (the upgrade runs first on every path that decides).
