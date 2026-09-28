# ADR-0009: Installation layout, scheduling and distribution

- Status: Accepted
- Date: 2026-09-29

## Context

second-brain must install and run on Windows, macOS and Linux, and must not
require administrator rights. Known pitfalls to avoid:

- On Windows, Defender's Controlled Folder Access blocks writes under `Documents`
  by processes that are not allow-listed.
- A separate setup script and scheduled job per data source multiplies
  maintenance.
- Credentials kept in environment variables leak into every child process.

## Decision

### Data directory (`SECOND_BRAIN_HOME`)

The default is the OS application-data directory, resolved with the `directories`
crate:

| OS | Default |
|---|---|
| Windows | `%LOCALAPPDATA%\second-brain` |
| macOS | `~/Library/Application Support/second-brain` |
| Linux | `$XDG_DATA_HOME/second-brain` (`~/.local/share/second-brain`) |

- The environment variable `SECOND_BRAIN_HOME` or the global `--home` flag
  overrides the default.
- Reasons for this default:
  - it is outside Controlled Folder Access;
  - it is per-user, which matches the `0600` model of ADR-0003;
  - it needs no admin rights;
  - agents access data through the CLI or MCP, so a hidden location is fine.

### Binaries

- Windows: `%LOCALAPPDATA%\Programs\second-brain\bin`.
- macOS / Linux: `~/.local/bin`.
- Both are added to the user `PATH` by the installer. No admin rights are needed.

### Distribution

- Use **cargo-dist** to build release artifacts on GitHub Releases for:
  - `x86_64/aarch64-pc-windows-msvc`
  - `x86_64/aarch64-apple-darwin`
  - `x86_64/aarch64-unknown-linux-musl`
- It also generates:
  - a shell installer (macOS/Linux),
  - a PowerShell installer (Windows),
  - a Homebrew formula.
- The installer only places the binaries and updates `PATH`. Everything else is
  done by the program itself.

### Setup

`second-brain setup` is interactive and idempotent. Each step can also be run on its
own, non-interactively:

| Step | What it does |
|---|---|
| `setup home` | Create `SECOND_BRAIN_HOME`, the DB and its permissions |
| `setup account` | Add Google / Slack accounts (same as `sb account add`) |
| `setup llm` | Choose summarizer profiles; Foundry Local wizard |
| `setup schedule` | Register scheduled jobs |
| `setup skills` | Install agent skills (ADR-0010) |
| `setup mcp` | Print or apply MCP client configuration (ADR-0010) |
| `setup env` | Set `SECOND_BRAIN_HOME` persistently when a non-default home is used |

### Environment variables

- second-brain's own settings live in the DB. The environment is used only for:
  - `PATH`;
  - `SECOND_BRAIN_HOME`, only if non-default;
  - `SB_LOG`, the log filter;
  - standard variables consumed by external tools or libraries: proxy variables
    (`HTTPS_PROXY`, `NO_PROXY`), fallback API keys (ADR-0003), and variables that
    the Claude / Copilot CLIs themselves read.
- Scheduled job definitions embed the needed environment explicitly (home path,
  `PATH` for LLM CLIs), so they do not depend on the login shell.

### Scheduling

| OS | Mechanism | Catch-up after sleep/off |
|---|---|---|
| Windows | Task Scheduler, registered with `schtasks /Create /XML` (per-user, no admin) | `StartWhenAvailable` |
| macOS | launchd agent plist in `~/Library/LaunchAgents` (`StartCalendarInterval`) | launchd runs missed intervals on wake |
| Linux | user crontab (default), or systemd user timer (`--systemd`) | cron: no; systemd: `Persistent=true` |

- Registered jobs:
  - `second-brain sync`, daily (default 19:30);
  - `second-brain sync --deep`, weekly (default Monday 18:30).

  Times are configurable.
- One sync process runs at a time. A lock file in `$SECOND_BRAIN_HOME/locks/`
  enforces this; stale locks are taken over after a timeout (default 6 h).
- Every run is recorded in the `runs` table, and `sb doctor` reports failed or
  stale runs.

## Consequences

- There is no per-feature setup script. Adding a source never adds a scheduled job;
  `sync` handles all enabled accounts and sources.
- Any other location, such as `C:\second_brain`, can be used by setting
  `SECOND_BRAIN_HOME`.
- cargo-dist's support for custom install paths (especially on Windows) must be
  verified. If it cannot target `%LOCALAPPDATA%\Programs\second-brain\bin`, a
  small custom PowerShell installer is used instead.
