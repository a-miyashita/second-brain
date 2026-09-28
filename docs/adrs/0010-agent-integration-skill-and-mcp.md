# ADR-0010: Agent integration through a global skill and an MCP server

- Status: Accepted
- Date: 2026-09-29

## Context

Agents are the primary consumers of the knowledge base.

- Terminal agents (GitHub Copilot CLI, Claude Code, Codex) work well with a
  **global skill** that calls a CLI.
- Desktop agents such as Claude Desktop / Cowork cannot always run a local binary.
  Cowork runs tools inside a Linux VM sandbox, so it cannot execute a Windows
  binary directly. It can, however, reach local MCP servers configured in the
  Claude desktop app.

Agents also need search guidance, learned from practice:

- search first, never read everything;
- try the `decisions` section first for "what was decided" questions;
- always cite the original link.

That guidance must survive.

## Decision

Provide two integration surfaces over the same library code.

### 1. Agent skill

- A `SKILL.md` (plus reference files) is compiled into the binary and installed with
  `second-brain setup skills --target <copilot|claude|codex|all>`.
- Target directories: `~/.copilot/skills/second-brain/`,
  `~/.claude/skills/second-brain/`, and the Codex equivalent. The exact paths are
  verified per tool at implementation time.
- The skill tells the agent to call the CLI with `--json`. The CLI's JSON output is
  a versioned, stable contract.
- `sb doctor` reports a skill that is outdated relative to the binary; the skill
  has a version marker.

### 2. MCP server

- `second-brain mcp` runs an MCP server over stdio, using the official Rust SDK
  (`rmcp`).
- Tools:
  - `search`, `show_entry`, `list_sources`, `stats`: read-only;
  - `ingest`: add a URL or file with context;
  - `scan_links`: read-only.
- Configuration:
  - `mcp.allow_ingest` (default `true`) disables `ingest` for users who want a
    read-only server.
  - `mcp.default_limit` controls the default number of search results.
- `second-brain setup mcp --client <claude-desktop|claude-code|copilot|vscode>`
  prints the configuration snippet, and applies it with `--apply` where the
  client's config file location is known.
- Tool descriptions carry the same search guidance as the skill.

### Output rules shared by both

- Every hit includes `source_url` (with fallbacks), the date and the account label,
  so agents can cite sources.
- Output is bounded by `--limit` and snippet length, so an agent cannot flood its
  own context by accident.

## Consequences

- Skill and MCP are thin. All logic lives in the library crates, so both behave the
  same.
- The CLI JSON schema becomes a public interface. Breaking changes need a schema
  version bump (see [specs/cli.md](../specs/cli.md)).
- The skill covers terminal agents, which are the MVP target. The MCP server is
  scheduled after the MVP core (see [specs/mvp-plan.md](../specs/mvp-plan.md)).
