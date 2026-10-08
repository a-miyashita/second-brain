# Concepts

This chapter explains the words that the manual uses.

## What the tool does

second-brain does four things:

1. It **collects** data from your accounts. For example, it reads Slack threads and
   Google Meet notes.
2. It **saves** the data on your computer.
3. It **summarizes** each item. A summary has an overview, decisions and action items.
4. It **searches**. You can search from the command line. An AI agent can also search.

The tool works on Windows, macOS and Linux.

## Terms

| Term | Meaning |
|---|---|
| Entry | One item in the knowledge base. An entry can be a Slack thread, a meeting, a document or a web page |
| Source | The place where an entry comes from. Examples: Slack, Google Meet, Google Docs, web pages, local files |
| Source kind | The type of an entry, such as `slack.thread`, `slack.day`, `google.meet`, `google.doc`, `web.page` or `local.file` |
| Account | A connection to a service. You can have more than one account. For example, you can connect two Slack workspaces |
| Sync | The action that collects new data from your accounts |
| Summary | A short text that describes an entry |
| Section | One part of an entry. See the table below |
| Summarizer | The program that writes summaries. It can be an LLM API, an LLM command-line tool, or a local model |
| Profile | A saved summarizer setting with a name |
| Home directory | The folder where the tool keeps all its data |
| Raw data | The original data of an entry, saved as files. The tool keeps it so that it can write new summaries later |
| Agent | An AI program, such as Claude Code or GitHub Copilot CLI, that can run commands for you |
| Skill | A file that tells an agent how to use `sb` |

## Sections of an entry

Each entry has up to five sections.

| Section | Content |
|---|---|
| `overview` | A short description of the entry |
| `decisions` | What people decided, and who decided it |
| `action_items` | Who does what, and by when |
| `details` | The full text of the entry, or a longer summary of the discussion |
| `background` | Why you added the entry. You write this section yourself |

A document often has no decisions and no action items. This is normal.

## How the data flows

```text
Slack, Google Meet, Google Docs, web pages, files
        │  sync or ingest
        ▼
raw data (files on your computer)
        │
        ▼
entries and sections (a database on your computer)
        │  summarize
        ▼
search index
        │
        ▼
sb search, sb show, AI agents
```

The tool always keeps the raw data. Because of this, you can write new summaries with
a better model later. Each summary records which model wrote it.

## Where the tool keeps data

The tool keeps all data in the **home directory**. The default location depends on
your operating system.

| Operating system | Default home directory |
|---|---|
| Windows | `%LOCALAPPDATA%\second-brain` |
| macOS | `~/Library/Application Support/second-brain` |
| Linux | `~/.local/share/second-brain` |

To use a different folder, set the environment variable `SECOND_BRAIN_HOME`. You can
also add the option `--home <folder>` to any command.

**Warning:** The database in the home directory contains your passwords and tokens.
These are the Google refresh tokens, the Slack token and the API keys. Only your
operating system user can read the folder. Do not share a copy of it. Keep backups
private.

## What the tool does not do

- It does not send your data to any third party. The only exception is the
  summarizer that you choose. If you choose an LLM API, the tool sends the text of an
  entry to that service to get a summary.
- It does not change your data in Slack or Google. It only reads.
- It does not read Slack content that you cannot read yourself.
