# Summaries and cost

This chapter explains how the tool writes summaries, how to choose a summarizer, and
how to control the cost.

## How summaries work

Each entry can have a summary. The summary has these sections: `overview`,
`decisions` and `action_items`. For meetings, it also has `details`.

- **Slack and documents.** The summarizer that you choose writes the summary.
- **Google Meet.** By default, the tool keeps the notes that Gemini wrote. It does
  not call a summarizer for Meet entries.
- **No summarizer.** If you have not chosen a summarizer, the entries wait for their
  summary. The status is `pending`. You can search them at once.

The tool records which model wrote each summary. `sb show` and `sb stats` show this
information.

The tool writes a new summary for an entry only when the text of the entry changes.

## Choose a summarizer

Run:

```sh
$ sb setup llm
```

The command asks you to choose a summarizer. It tests the summarizer with one short
call. To skip the question, use `--preset`:

| Preset | Summarizer | You need |
|---|---|---|
| `anthropic` | Anthropic API, Claude Haiku 4.5 | An API key |
| `openai` | OpenAI API | An API key |
| `claude_cli` | Claude Code (`claude -p`) | The `claude` program, and a login |
| `copilot_cli` | GitHub Copilot CLI (`copilot -p`) | The `copilot` program, and a login |
| `codex_cli` | OpenAI Codex CLI (`codex exec`) | The `codex` program, and a login |
| `antigravity_cli` | Google Antigravity CLI (`agy`) | The `agy` program, and a login |
| `local` | A local server with an OpenAI-compatible interface, for example Ollama | A running server and a model |

Examples:

```sh
$ sb setup llm --preset anthropic
$ sb setup llm --preset claude_cli
$ sb setup llm --preset local --base-url http://127.0.0.1:11434/v1 --model <model>
```

| Option | Meaning |
|---|---|
| `--preset <name>` | Use this preset |
| `--name <name>` | The name of the profile. The default is the name of the preset |
| `--model <model>` | The model to use |
| `--base-url <url>` | The address of a local server |
| `--no-default` | Do not make this profile the default |
| `--no-test` | Do not run the test call |

### API keys

The setup command asks for the API key and saves it. To save or replace a key later,
run:

```sh
$ sb config set-secret anthropic.api_key
```

The command asks for the key at a hidden prompt. The names of the keys are
`anthropic.api_key`, `openai.api_key` and `google.api_key`.

If no key is saved, the tool reads the key from an environment variable:
`ANTHROPIC_API_KEY`, `OPENAI_API_KEY` or `GEMINI_API_KEY`.

### Local models

**Warning:** We do not recommend small local models. Models with about 5 to 10 billion
parameters write summaries of low quality. A wrong summary is worse than no summary,
because search and agents trust it. Use a local model only if its quality is at least
that of Claude Haiku.

### Profiles for different sources

You can use a different profile for each source kind. For example:

```sh
$ sb config set summary.profile.default fast
$ sb config set summary.profile.slack.thread best
```

The tool uses `summary.profile.default` for all sources that have no own setting. The
reserved name `native` means "keep the notes of the source and do not summarize". It
is the default for `google.meet`.

## Control the cost

The tool limits the money that it spends on paid summarization. The default limits
are:

| Limit | Default |
|---|---|
| Each week (starts on Monday) | $2 |
| Each month (starts on the 1st) | $10 |

The week and the month start at 00:00 in your time zone. The setting
`summary.budget.timezone` sets the time zone.

When a limit is reached:

- `sb sync` and `sb summarize` stop without an error. The exit code is 0.
- The remaining entries keep the status `pending`. You can search them.
- The tool continues by itself in the next week or month.
- `sb doctor` shows the warning `llm.budget_exhausted`.

**Note:** The amounts are estimates. The tool computes them from the number of tokens
and the list prices. They are not invoices. Also set a spending limit in the console
of your provider.

### Show and change the limits

```sh
$ sb budget
$ sb budget --by-model
$ sb config set summary.budget.weekly_usd 5
$ sb config set summary.budget.monthly_usd 20
```

To turn a limit off, set it to `0`.

`sb budget` shows the spend of this week and this month, the history, the total, and
the number of calls. `--weeks <n>` and `--months <n>` set the length of the history.
`--by-model` also shows the spend for each model.

Local models are free. The limits never stop them. A summarizer that is run by a CLI
(Claude Code) reports its cost. The CLI summarizers `codex_cli` and `antigravity_cli`
do not report a cost. The limits do not apply to them.

If a paid API model has no known price, the tool does not use it while a limit is
on. The tool opens the issue `llm.unpriced`. Set the price in the setting
`llm.prices`, or turn the limits off.

### Estimate the cost first

```sh
$ sb sync --estimate
$ sb summarize --estimate
```

The commands show the estimated tokens and cost of the pending summaries. They also
show if the cost fits in the remaining budget.

### Reduce the cost

Short threads gain little from a summary. Their text is already short, and the search
index holds it. To skip summaries for short items, raise the minimum length:

```sh
$ sb config set summary.min_chars 2000
```

An item is also skipped if it has fewer than `summary.min_messages` messages (default
3).

## Write summaries for pending entries

```sh
$ sb summarize
```

The command writes summaries for all entries with the status `pending` or `failed`.

| Option | Meaning |
|---|---|
| `--max-summaries <n>` | Write at most this number of summaries |
| `--max-cost <usd>` | Stop at this estimated cost |
| `--time-limit <time>` | Stop after this time |
| `--retry-failed` | Also try the entries that failed three times |
| `--profile <name>` | Use this profile for this run |
| `--estimate` | Show the estimate. Do not write summaries |

If a summary fails, the tool tries again at the next run. After 3 failed attempts
(`summary.max_attempts`), it stops trying until you use `--retry-failed`.

## Write new summaries with another model

You can replace the summaries of some entries with a better model. The tool uses the
raw data for this.

```sh
$ sb resummarize --source slack.thread --where-model claude-haiku-4-5 --profile best --estimate
$ sb resummarize --source slack.thread --where-model claude-haiku-4-5 --profile best
```

Always run the command with `--estimate` first.

| Option | Meaning |
|---|---|
| `--profile <name>` | The profile that writes the new summaries |
| `--native` | Restore the Gemini notes of Google Meet entries |
| `--where-model <model>` | Select only the entries that this model summarized |
| `--where-provider <name>` | Select only the entries that this provider summarized |
| `--account`, `--source`, `--since`, `--until`, `--entry` | Select entries |
| `--estimate` | Show the estimate and stop |
| `--dry-run` | Show what the command would do |
| `--limit <n>` | Process at most this number of entries |
| `--max-cost <usd>`, `--time-limit <time>` | Limits for the run |
| `--force` | Also process the entries that already have the target profile |
| `--yes` | Do not ask for a confirmation |

The command asks for a confirmation if it changes more than 20 entries. The command
skips the entries that have no raw data. To get the raw data, run `sb refetch` first.

You can stop the command and run it again. It skips the entries that are done.

**Note:** If you change the model of a profile, the tool does not see the change.
Use `--force`, or create a new profile with a new name.

## Imported summaries

The summaries in an import bundle keep their original model name. `sb stats` shows
the model `unknown` if the bundle did not name a model.
