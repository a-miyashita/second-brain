# second-brain

A personal knowledge base for AI agents.

second-brain collects your Slack conversations, Google Meet notes (Gemini), documents
and more. It stores them locally with summaries and makes them searchable from the
command line, from agent skills (GitHub Copilot CLI, Claude Code, ...) and over MCP.

> **Status: design phase.** Nothing is implemented yet. See [docs/](docs/README.md)
> for the architecture decisions and specifications.

## Features

- **Ingest** context from multiple accounts:

  | Source | Phase |
  |---|---|
  | Slack threads and conversations | MVP |
  | Google Meet notes and transcripts | MVP |
  | Google Docs / Drive files | Later |
  | Local files | Later |
  | Arbitrary URLs | Later |
  | Email (Gmail, IMAP) | Later |

- **Summarize** each entry into an overview, decisions and action items. You can
  use:
  - an LLM API (Anthropic, OpenAI, Google);
  - an LLM CLI (Claude Code, GitHub Copilot CLI);
  - a local LLM behind an OpenAI-compatible endpoint, such as Foundry Local on an
    Intel NPU.
- **Re-summarize at any time.** Raw data is kept, and every summary records the
  model and prompt version that produced it. You can later redo the summaries of
  a cheap model with a stronger one, or replace Gemini's meeting notes with another
  model's.
- **Search** with SQLite FTS5, which works for Japanese, including two-character
  terms. The search backend is pluggable, so vector and hybrid search can be added.
- **Run unattended** from Task Scheduler, launchd or cron, with `second-brain doctor`
  to show what needs attention (for example, expired logins).
- **Ship as a single static binary** (`second-brain`, alias `sb`) for Windows,
  macOS and Linux. No runtime dependencies.

## How it works

```text
Slack / Google (Meet, Calendar, Drive) / ...
        │  fetch (per account)
        ▼
  raw data (files)  ──normalize──▶  catalog (SQLite: entries, sections, summaries)
                                        │                     ▲
                                        ├─ summarize (LLM) ───┘
                                        ▼
                                  search index (FTS5; vectors later)
                                        │
          sb search / sb show / MCP ◀───┘ ── used by agents via skill or MCP
```

## Installation (planned)

```sh
# macOS / Linux
curl -LsSf https://github.com/<owner>/second-brain/releases/latest/download/second-brain-installer.sh | sh

# Windows (PowerShell)
irm https://github.com/<owner>/second-brain/releases/latest/download/second-brain-installer.ps1 | iex
```

Then run the setup wizard:

```sh
second-brain setup
```

The wizard walks you through five steps:

1. Create the data directory.
2. Add your Google and Slack accounts.
3. Choose a summarizer.
4. Register the daily sync.
5. Install the agent skill.

Detailed guides (creating the Google OAuth client and the Slack app) will be added
with the implementation. The designs are in
[docs/specs/accounts-and-auth.md](docs/specs/accounts-and-auth.md).

## Quick usage (planned)

```sh
sb sync                                   # fetch new content and summarize
sb search CSV --section decisions         # "what did we decide about CSV?"
sb show 01J9ABC...                        # read one entry
sb resummarize --source slack.thread --where-model claude-haiku-4-5 --profile best --estimate
sb doctor                                 # health and pending issues
```

## Data location

| OS | Default `SECOND_BRAIN_HOME` |
|---|---|
| Windows | `%LOCALAPPDATA%\second-brain` |
| macOS | `~/Library/Application Support/second-brain` |
| Linux | `~/.local/share/second-brain` |

Set `SECOND_BRAIN_HOME` to use another location.

The database contains your credentials (OAuth refresh tokens, Slack token, API keys)
and is readable only by your OS user. Treat copies and backups as secrets.

## Documentation

- [docs/README.md](docs/README.md): index of ADRs and specs
- [AGENTS.md](AGENTS.md): guidelines for contributors and coding agents

## License

[MIT](LICENSE)
