# Searching

This chapter shows how to find entries and read them.

## Search

```sh
$ sb search <word>...
```

The tool shows the entries that match **all** the words. The best match is first.
Each result has the date, the title, the source kind, the section, the matching text
and the `id` of the entry.

```sh
$ sb search CSV
1. [2026-09-03 09:15] Release plan (local.file, Local files) — Decisions
   - Ship the CSV export first.
   id: 01M4CDYKPSXV0KSB3F1J2S3JKY
```

### How matching works

- The tool finds the words as parts of text. The search ignores the case of Latin
  letters.
- The search treats full-width and half-width characters as equal. For example,
  `ＣＳＶ` matches `CSV`.
- Words of one or two characters work. This is important for Japanese words such as
  `契約`. If you use only short words, the results show the newest entries first.
- Use specific words, such as the name of a customer, a product or a person. Do not
  use general words, such as "meeting".

### Search in one section

Choose the section with `--section`. Use this table:

| Your question | Section |
|---|---|
| What did we decide about X? | `decisions` |
| Why did we do X? What is the history? | `details` |
| Who owns this task? | `action_items` |
| What is this entry about? | `overview` |
| Why did I add this document? | `background` |

```sh
$ sb search CSV --section decisions
```

**Note:** A decision is often only in the `decisions` section. It can be missing in
`details`.

### Narrow the results

| Option | Result |
|---|---|
| `--section <kind>` | Search only this section |
| `--source <kind>` | Only this source kind, for example `google.meet` or `slack.thread` |
| `--account <id>` | Only this account |
| `--since <date>` | Only entries from this date |
| `--until <date>` | Only entries before this date |
| `--limit <n>` | Show at most this number of results. The default is 8. The maximum is 50 |
| `--all-sections` | Show every matching section. By default, the tool shows only the best section of each entry |

Write dates as `YYYY-MM-DD`, for example `2026-07-01`. The search commands do not accept
periods such as `90d`.

## Read an entry

Use the `id` from the search result:

```sh
$ sb show 01M4CDYKPSXV0KSB3F1J2S3JKY
```

The command prints the entry as Markdown:

```text
# Release plan

- Date: 2026-09-03 09:15
- Source: local.file (Local files)
- Summary by: unknown/unknown/unknown
- ID: 01M4CDYKPSXV0KSB3F1J2S3JKY

## Overview

The team plans the October release.

## Decisions

- Ship the CSV export first.
```

The line **Summary by** shows the provider, the model and the prompt version that
wrote the summary.

| Option | Result |
|---|---|
| `--section <kind>` | Show only this section. You can repeat the option |
| `--meta` | Also show the metadata, such as the participants of a meeting |
| `--raw` | Show the paths of the raw files |
| `--raw --role <role>` | Show the content of the raw file with this role, for example `transcript` |

## List entries

```sh
$ sb list
```

The command shows the newest entries first. It shows 20 entries. Use `--limit` to
change the number. You can use the filters `--account`, `--source`, `--since`,
`--until`, `--raw-status` and `--summary-status`.

## Check the quality of summaries

```sh
$ sb review --limit 3
```

The command shows the summary of an entry next to its source text. Use it to check if
the summaries are good. It is also useful before and after you run `sb resummarize`.

| Option | Result |
|---|---|
| `--limit <n>` | Number of entries. The default is 5 |
| `--detail-lines <n>` | Lines of source text for each entry. The default is 20 |
| `--full` | Show the whole source text |
| `--all` | Also show entries without summaries |
| `--channel <name>` | Only this Slack channel |

## Statistics

```sh
$ sb stats
```

The command shows these totals:

- the number of entries for each account and source;
- the raw status and the summary status;
- the models that wrote the summaries;
- the summary budget.

## Machine-readable output

Add `--json` to any of these commands. The output is one JSON object. It has a field
`schema`, for example `sb.search/v1`. AI agents use this format.
