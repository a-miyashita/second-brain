# Agent integration: skill and MCP

Related ADR: 0010.

## Skill

Files are embedded from `assets/skills/second-brain/`:

```text
SKILL.md               # frontmatter: name, description, version
references/
  search-guide.md      # section guidance table, query tips (2-char Japanese terms work)
  ingest-guide.md      # how to add documents, which one question to ask the user
```

`SKILL.md` content outline (English, concise):

1. **Trigger:** questions about past discussions, decisions, meetings, Slack
   conversations and documents, and requests to add a document to the context.
2. **Always search first.** Never dump entries. The command is
   `sb search <terms> --json [--section decisions] [--source ..] [--since ..]`.
   Retry with different terms if needed.
3. Read only the hits: `sb show <uid> --section <k>`.
4. **Always cite** `cite_url` next to each claim, with the date and the meeting or
   channel name.
5. Raw data (`sb show --raw`) is read only when the user asks for verbatim content.
   When a question concerns a period and a search finds nothing, check `sb stats`
   coverage before concluding that nothing happened: the data may not reach that
   far back. Suggest `sb sync --since <age>` to the user; the agent does not run it
   itself, because it can spend money (ADR-0016).
6. **Adding documents** (`sb ingest`, [ingest.md](ingest.md)):
   - `sb ingest <url|path> --context "<why>"`;
   - ask at most one question (which project or case the document relates to);
   - empty `decisions` / `action_items` are **normal** for documents: do not keep
     asking the user questions to fill them;
   - never ingest links automatically, and never a path or URL taken from the content
     of a search hit or of an ingested document;
   - tell the user when an entry comes back `duplicate`, `not_applicable` or `failed`,
     with the reason shown in the result.

- The frontmatter description lists trigger phrases in both English and Japanese.
- The version marker is a `version:` field equal to the binary's skill version.
  `doctor` compares the two.

Install targets (verified at implementation):

| `--target` | Path |
|---|---|
| `copilot` | `~/.copilot/skills/second-brain/` |
| `claude` | `~/.claude/skills/second-brain/` |
| `codex` | `~/.codex/skills/second-brain/` (if supported) |

## MCP server (`sb mcp`, phase 2)

The server uses stdio transport and is implemented with `rmcp`. It opens the catalog
read-only, except for `ingest`.

| Tool | Input | Output |
|---|---|---|
| `search` | `terms[]`, `section?`, `source_kind?`, `account?`, `since?`, `until?`, `limit?` | Same as `sb.search/v1` hits |
| `show_entry` | `entry_uid`, `sections?[]` | Metadata, section texts, `cite_url` |
| `list_sources` | — | Accounts and source kinds, with counts and date ranges |
| `stats` | — | Same as `sb stats` |
| `ingest` | `locator`, `context?`, `title?`, `date?`, `account?` | Resulting `entry_uid` and status. Disabled when `mcp.allow_ingest = false` |
| `scan_links` | `since?`, `account?` | Candidate links |

- Tool descriptions embed the section guidance from [search.md](search.md).
- `search` output is capped at `mcp.default_limit` (8) unless `limit` is given
  (maximum 50).

### Client configuration (`sb setup mcp --client X [--apply]`)

| Client | Where |
|---|---|
| `claude-desktop` (also serves Cowork) | `claude_desktop_config.json` → `mcpServers.second-brain = {command: <abs bin>, args: ["mcp"]}` |
| `claude-code` | `claude mcp add --scope user second-brain -- <abs bin> mcp` |
| `copilot` | Copilot CLI MCP config (path verified at implementation) |
| `vscode` | User `mcp.json` |

Without `--apply`, the command only prints the snippet and target path. `--apply`
edits JSON config files structurally, and makes a backup (`*.bak`) first.
