# Command reference

This chapter lists all commands. Add `--help` to a command to see its options.

## Options for all commands

| Option | Meaning |
|---|---|
| `--home <dir>` | Use this home directory instead of `SECOND_BRAIN_HOME` |
| `--json` | Print machine-readable output. See "JSON output" below |
| `-v`, `--verbose` | Print more log output. You can repeat the option |
| `-q`, `--quiet` | Print less output |
| `--no-color` | Do not use colors |

## Set up

| Command | Purpose |
|---|---|
| `sb setup` | Run the setup wizard |
| `sb setup home` | Create the home directory and the database |
| `sb setup llm` | Create or change a summarizer profile |
| `sb setup schedule` | Register or remove the daily sync |
| `sb setup skills` | Install or remove the agent skills |
| `sb setup env` | Save `SECOND_BRAIN_HOME` for scheduled jobs and agents |
| `sb config list` | Show all settings |
| `sb config get <key>` | Show one setting |
| `sb config set <key> <value>` | Change a setting |
| `sb config unset <key>` | Remove your value of a setting |
| `sb config set-secret <name>` | Save a secret at a hidden prompt |
| `sb config edit [--account <id>]` | Edit the settings in your editor |

## Accounts

| Command | Purpose |
|---|---|
| `sb account add google <id> --client-secret <file>` | Add a Google account |
| `sb account add slack <id>` | Add a Slack workspace |
| `sb account list` | List the accounts |
| `sb account show <id>` | Show one account |
| `sb account disable <id>` | Stop using an account |
| `sb account enable <id>` | Use an account again |
| `sb account remove <id> [--purge]` | Remove an account. `--purge` also deletes its data |
| `sb auth status [--online]` | Show if the logins are valid |
| `sb auth login <id>` | Log in again |

## Collect data

| Command | Purpose |
|---|---|
| `sb sync` | Collect new data and write summaries |
| `sb ingest <url-or-path>...` | Add a Google Doc, a web page or a file |
| `sb import <bundle>` | Import an import bundle |
| `sb refetch` | Download the raw data again |
| `sb reextract` | Process the saved raw data again (no network) |
| `sb summarize` | Write the pending summaries |
| `sb resummarize` | Write new summaries for entries that have one |

## Find and read

| Command | Purpose |
|---|---|
| `sb search <word>...` | Search the entries |
| `sb show <id>` | Show one entry |
| `sb list` | List the entries, newest first |
| `sb review` | Show summaries next to their source text |
| `sb stats` | Show the totals |
| `sb budget` | Show the summarization spend |

## Operate

| Command | Purpose |
|---|---|
| `sb doctor` | Check the health of the tool |
| `sb index rebuild` | Rebuild the search index |
| `sb version` | Show the version |

## Filters

Several commands accept the same filters to select entries:

| Option | Meaning |
|---|---|
| `--account <id>` | Only this account |
| `--source <kind>` | Only this source kind |
| `--since <date>` | From this date |
| `--until <date>` | Before this date |
| `--entry <id>` | Only this entry. You can repeat the option |
| `--raw-status <status>` | Only entries with this raw status: `present`, `missing` or `fetch_failed` |
| `--summary-status <status>` | Only entries with this summary status: `none`, `pending`, `done`, `skipped` or `failed` |

Write dates as `YYYY-MM-DD`. Only `sb sync` also accepts a period such as `90d` or
`12w`.

## Source kinds

| Source kind | Meaning |
|---|---|
| `slack.thread` | A Slack thread |
| `slack.day` | The messages of one Slack channel on one day, outside threads |
| `google.meet` | Google Meet notes |
| `google.doc` | A Google Doc, Sheet, Slide or Drive file |
| `web.page` | A web page |
| `local.file` | A file on your computer |

## Exit codes

| Code | Meaning |
|---|---|
| 0 | The command succeeded |
| 1 | The command finished with problems. For example, a sync was only partly done, `doctor` found warnings, or an account needs a new login |
| 2 | The command failed. For `doctor`, it found errors |
| 64 | You used the command in a wrong way |
| 75 | Another sync is running |
| 130 | A signal, such as Ctrl+C, interrupted the command. The tool keeps the finished work |

## JSON output

With `--json`, a command prints one JSON object. The field `schema` names the format,
for example `sb.search/v1`. If a command fails, the object has the schema
`sb.error/v1` with a `code` and a `message`.

The tool can add new fields to a format. If it removes or renames a field, it
increases the version number in the schema name.
