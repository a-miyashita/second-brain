# Getting started

This chapter shows how to set up the tool, collect your first data and find an entry.

## Before you start

You need:

- the `sb` command (see [Installation](installation.md));
- at least one source: a Google account, a Slack workspace, or a document that you
  want to add.

An LLM account is optional. Without one, the tool still collects and searches your
data. The entries wait for a summary. See [Summaries and cost](summaries.md).

## Run the setup screen

Run this command:

```sh
$ sb setup
```

The command opens the setup screen in your terminal. The left column lists the
sections. The right side shows the selected section. Do the sections in this order
the first time:

1. **Home.** The command creates the home directory and the database before the
   screen opens. It sets the file permissions so that only you can read them.
2. **Accounts.** Add a Google account or a Slack workspace. You can add more than
   one, or add accounts later.
3. **Summarizer.** Choose a summarizer. The screen can test it for you.
4. **Schedule.** Check the time of the daily sync (default 19:30), the day of the
   weekly deep sync (default Monday) and its time (default 18:30). Then register the
   jobs.
5. **Agent skills.** Install the skill for the AI agents that you use.
6. **First sync.** Choose how many days back the first sync reads (default 30). You
   can see the cost estimate, and run the first sync.

A mark (✓) in the left column shows the sections that are done. You can go back to
any section later: run `sb setup` again.

Use these keys:

| Key | Action |
|---|---|
| ↑ ↓ | Select a row |
| Tab, or 1 to 8 | Go to another section |
| Enter | Do the action of the row (the bottom line names it) |
| x | Do the second action of the row, for example remove or disable |
| Esc | Close a form. In the main screen: quit |
| q | Quit |

In a form, Tab and the arrow keys move between the fields, Space switches a yes/no
field, and Enter goes to the next field. Enter on the last button saves. If a value
is not valid, the form stays open and shows the error.

When you add a Google account, the screen pauses. The browser opens for the Google
consent, and the terminal shows its messages. Press Enter to go back to the screen.

You can also run each step alone, without the screen:

```sh
$ sb setup home
$ sb setup llm --preset anthropic
$ sb setup schedule
$ sb setup skills
$ sb setup env
```

To accept all defaults without the screen, run `sb setup --yes`. With `--yes`, the
command does not add accounts and does not choose a summarizer. Use `sb account add`
and `sb setup llm` for these steps. The screen needs a terminal: in a script, use
`--yes` or the steps.

**Note:** Before you add a Google account or a Slack workspace, you must prepare
credentials. See [Google account](google-account.md) and
[Slack account](slack-account.md).

## Collect your first data

Run the sync:

```sh
$ sb sync
```

The first sync reads the last 30 days. You can stop the command at any time with
Ctrl+C. The tool keeps all finished work. Run the command again to continue.

To see the estimated cost of the summaries before you start, run:

```sh
$ sb sync --estimate
```

## Find an entry

Search for a word:

```sh
$ sb search CSV
```

The command shows the best matches. For example:

```text
1. [2026-09-03 09:15] Release plan (local.file, Local files) — Decisions
   - Ship the CSV export first.
   id: 01M4CDYKPSXV0KSB3F1J2S3JKY
```

Each result shows the date, the title, the source kind, the section, and the `id` of
the entry.

Read the whole entry with its `id`:

```sh
$ sb show 01M4CDYKPSXV0KSB3F1J2S3JKY
```

## Check the health of the tool

Run:

```sh
$ sb doctor
```

The command checks the setup and shows what needs your attention. See
[Maintenance](maintenance.md).

## Next steps

- [Syncing](sync.md): learn how to collect data every day.
- [Searching](search.md): learn how to search better.
- [Adding documents](ingest.md): add a Google Doc, a web page or a file.
- [AI agents](agents.md): let an agent search for you.
