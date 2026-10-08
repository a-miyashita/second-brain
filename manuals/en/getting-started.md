# Getting started

This chapter shows how to set up the tool, collect your first data and find an entry.

## Before you start

You need:

- the `sb` command (see [Installation](installation.md));
- at least one source: a Google account, a Slack workspace, or a document that you
  want to add.

An LLM account is optional. Without one, the tool still collects and searches your
data. The entries wait for a summary. See [Summaries and cost](summaries.md).

## Run the setup wizard

Run this command:

```sh
$ sb setup
```

The wizard asks questions and does these steps in this order:

1. **Home directory.** The wizard creates the home directory and the database. It
   sets the file permissions so that only you can read them.
2. **Accounts.** The wizard asks you to add a Google account or a Slack workspace.
   You can add more than one. You can also skip this step and add accounts later.
3. **Summarizer.** The wizard asks you to choose a summarizer and tests it.
4. **Schedule.** The wizard asks for three values: the time of the daily sync (default
   19:30), the day of the weekly deep sync (default Monday) and its time (default
   18:30). Then it registers the jobs.
5. **Skills.** The wizard finds the AI agents on your computer. For each agent, it
   asks if it must install the skill.
6. **First sync.** The wizard asks how many days back the first sync must reach
   (default 30). It offers to show the cost estimate. Then it offers to run the
   first sync.

You can run each step alone:

```sh
$ sb setup home
$ sb setup llm
$ sb setup schedule
$ sb setup skills
$ sb setup env
```

To accept all defaults without questions, add `--yes`. With `--yes`, the wizard does
not add accounts and does not choose a summarizer. Use `sb account add` and
`sb setup llm` for these steps.

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
