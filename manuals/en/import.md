# Importing data

Use `sb import` to bring data from another tool into second-brain. The data must be
in an **import bundle**. A bundle is a folder with a fixed format.

Another program creates the bundle. This is an **exporter**. second-brain does not
include exporters.

## Prepare

Before you import, add the accounts that the data belongs to. For example, run
`sb account add slack` for imported Slack threads. The import fails for an account that
does not exist.

## Check the bundle

Run a test first:

```sh
$ sb import ./bundle --dry-run
```

The command reads the whole bundle and shows the errors for each line. It writes
nothing. It skips a bad line and goes on with the next line.

## Import the bundle

```sh
$ sb import ./bundle
```

The command prints the result:

```text
1 lines: 1 created, 0 updated, 0 unchanged, 0 merged into entries with raw data, 0 invalid
1 entries have no raw data (raw_status = missing). Fetch it later with `sb refetch --raw-missing`.
```

You can run the same import again. Nothing changes the second time. If an entry
already has raw data from a sync, the import never replaces its raw data or sections.

### Map accounts

A bundle can name a default account for each source type. To change this, use
`--map`:

```sh
$ sb import ./bundle --map slack=acme-slack
```

## Fetch the missing raw data

Many bundles contain no raw data. Such entries have the raw status `missing`. Fetch
the original data from the source later:

```sh
$ sb refetch --raw-missing
```

You need the raw data to write new summaries for these entries with `sb resummarize`.

## The bundle format

The format is `second-brain-import/v1`. It has these files:

| File | Content |
|---|---|
| `manifest.json` | The format name, the creation time and the producer |
| `entries.jsonl` | One JSON object for each entry. Each line has the account, the source kind, the source ID, the title and the sections |
| `raw/` | Optional. The raw files that `entries.jsonl` names |

The specification [docs/specs/import-format.md](../../docs/specs/import-format.md)
describes every field.
