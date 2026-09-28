# second-brain

A personal knowledge base for AI agents.

second-brain collects your Slack conversations, Google Meet notes (Gemini), documents
and more. It stores them locally with summaries and makes them searchable from the
command line, from agent skills (GitHub Copilot CLI, Claude Code, ...) and over MCP.

> **Status: MVP in development.** Slack and Google Meet sync, summaries, search,
> import, `doctor`, setup and the agent skill are implemented. Releases are not
> published yet; build from source (below). See [docs/](docs/README.md) for the
> architecture decisions and specifications.

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
  - an LLM API (Anthropic, OpenAI; Google Gemini later);
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

## Installation

Once releases are published:

```sh
# macOS / Linux
curl -LsSf https://github.com/a-miyashita/second-brain/releases/latest/download/second-brain-installer.sh | sh

# Windows (PowerShell)
irm https://github.com/a-miyashita/second-brain/releases/latest/download/second-brain-installer.ps1 | iex
```

Until then, build from source with a recent Rust toolchain:

```sh
cargo install --path crates/sb-cli --locked   # installs second-brain and sb
```

Then run the setup wizard:

```sh
second-brain setup
```

The wizard walks you through these steps. Each one can also be run on its own
(`sb setup home|llm|schedule|skills|env`):

1. Create the data directory.
2. Add your Google and Slack accounts.
3. Choose a summarizer.
4. Register the daily sync.
5. Install the agent skill.
6. Optionally estimate and run the first sync.

## Adding a Google account (Meet notes)

second-brain uses your own OAuth client, so no third party ever sees your data.
A Workspace admin can create one client for the whole organization and share its
JSON file.

1. In the [Google Cloud console](https://console.cloud.google.com/), create a
   project (or pick one).
2. **APIs & Services → Library**: enable the **Google Drive API** and the
   **Google Calendar API**.
3. **APIs & Services → OAuth consent screen**:
   - Workspace organizations: choose **Internal**. Refresh tokens then do not
     expire weekly, and no verification is needed.
   - Personal accounts: choose **External** and add yourself as a test user. In
     "Testing" status, Google expires refresh tokens after 7 days, so you will
     need `sb auth login <account>` about once a week (`sb doctor` tells you).
4. **APIs & Services → Credentials → Create credentials → OAuth client ID**,
   application type **Desktop app**. Download the JSON file.
5. Add the account (a browser opens for consent; use `--no-browser` on a
   headless machine):

   ```sh
   sb account add google work-google --client-secret ~/Downloads/client_secret_XXXX.json
   ```

   The client JSON and the refresh token are stored in the catalog database, so
   you can delete the downloaded file afterwards. For a second Google account
   that uses the same client, pass `--client-secret` the same file again.

Meet notes are found through your calendar (notes attached to events you
attended) and in your "Meet Recordings" Drive folder.

## Adding a Slack workspace

Each workspace needs its own small Slack app that only you use.

1. Go to <https://api.slack.com/apps> → **Create New App** → **From an app
   manifest**, choose the workspace, and paste
   [assets/slack-app-manifest.yaml](assets/slack-app-manifest.yaml).
2. **Install to Workspace** and approve.
3. Copy the **User OAuth Token** (`xoxp-...`) from **OAuth & Permissions**. A
   user token is required: only it can read your DMs and private channels.
4. Do **not** enable "Distribute App". Distributed apps are limited to one
   `conversations.history` request per minute.
5. Add the account and paste the token at the hidden prompt:

   ```sh
   sb account add slack acme-slack
   ```

By default, DMs and group DMs are ingested fully, and in other channels only
threads that involve you (your messages, mentions, `@here`/`@channel`). To ingest
whole channels, list them in `full_channels`:

```sh
sb config edit --account acme-slack
```

## Choosing a summarizer

```sh
sb setup llm                       # interactive
sb setup llm --preset anthropic    # Claude Haiku via the Anthropic API
sb setup llm --preset claude_cli   # your Claude Code login (`claude -p`)
sb setup llm --preset local --base-url http://127.0.0.1:5273/v1 --model <model>
```

Google Meet entries keep Gemini's own notes by default (no LLM call). Slack
threads are summarized by the default profile. Without a profile, entries stay
`pending` and are still searchable.

## Quick usage

```sh
sb sync                                   # fetch new content and summarize (Ctrl+C to stop; re-run to resume)
sb sync --estimate                        # tokens and cost of pending summaries
sb search CSV --section decisions         # "what did we decide about CSV?"
sb search 契約 納期                        # two-character Japanese terms work
sb show 01J9ABC...                        # read one entry
sb review --limit 3                       # summaries next to their source text
sb resummarize --source slack.thread --where-model claude-haiku-4-5 --profile best --estimate
sb doctor                                 # health and pending issues
```

Agents use the same commands with `--json` through the installed skill.

## Importing existing data

Data from other tools can be imported from a
[`second-brain-import/v1` bundle](docs/specs/import-format.md):

```sh
sb import ./bundle --dry-run
sb import ./bundle
sb refetch --raw-missing     # later: fetch the original raw data
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

To uninstall: `sb setup schedule --remove`, `sb setup skills --remove --target all`,
then delete the binaries. The data directory is kept until you delete it.

## Documentation

- [docs/README.md](docs/README.md): index of ADRs and specs
- [AGENTS.md](AGENTS.md): guidelines for contributors and coding agents

## License

[MIT](LICENSE)
