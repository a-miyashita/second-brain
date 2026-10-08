# Settings

Settings change how the tool works. All settings have defaults.

## Work with settings

| Command | Result |
|---|---|
| `sb config list` | Shows all settings. The text `(default)` marks a setting that you did not change. The output hides secrets |
| `sb config get <key>` | Shows one setting |
| `sb config set <key> <value>` | Changes a setting. The tool reads the value as JSON. If the value is not valid JSON, the tool reads it as text |
| `sb config unset <key>` | Removes your value. The default applies again |
| `sb config set-secret <name>` | Saves a secret, such as an API key. The command asks for the value at a hidden prompt |
| `sb config edit` | Opens all settings as TOML in your editor (`$EDITOR`) |
| `sb config edit --account <id>` | Opens the settings of one account |

The tool checks a value when you save it. It rejects a value that is not valid.

Example:

```sh
$ sb config set sync.initial_days 60
$ sb config set ingest.local.deny '["**/private/**"]'
```

## Sync

| Key | Default | Meaning |
|---|---|---|
| `sync.initial_days` | 30 | Days that the first sync reads |
| `sync.overlap_secs` | 300 | The tool reads this many seconds again at the start of the next sync. This prevents gaps |
| `sync.lock_stale_hours` | 6 | After this time, the tool treats the lock of a stuck sync as old |
| `pipeline.commit_batch` | 20 | The number of entries that the tool saves together |
| `pipeline.shutdown_grace_secs` | 30 | Time that the tool gives to the work in progress when you press Ctrl+C |
| `slack.day_timezone` | time zone of the computer | The time zone for the Slack day entries |

## Summaries

| Key | Default | Meaning |
|---|---|---|
| `summary.profile.default` | none | The default summarizer profile |
| `summary.profile.<source kind>` | | A profile for one source kind. The default for `google.meet` is `native` |
| `summary.language` | `auto` | The language of summaries. `auto` uses the language of the entry. You can also use `ja` or `en` |
| `summary.min_chars` | 400 | The tool does not summarize shorter items |
| `summary.min_messages` | 3 | The tool does not summarize conversations with fewer messages |
| `summary.max_attempts` | 3 | The number of tries for a failed summary |
| `summary.budget.weekly_usd` | 2.0 | The weekly limit in US dollars. `0` turns it off |
| `summary.budget.monthly_usd` | 10.0 | The monthly limit in US dollars. `0` turns it off |
| `summary.budget.timezone` | time zone of the computer | The time zone for the start of a week and a month |
| `llm.prices` | built-in prices | Your own prices for models that the tool does not know |
| `llm.profiles.<name>` | | A summarizer profile. Use `sb setup llm` to create one |

## Adding documents

| Key | Default | Meaning |
|---|---|---|
| `ingest.max_file_bytes` | 52428800 (50 MiB) | The largest file or download that the tool accepts |
| `ingest.max_text_chars` | 300000 | The tool cuts longer extracted text |
| `ingest.min_text_chars` | 20 | The tool rejects items with less text |
| `ingest.keep_original` | `false` | Also keep the original file or page |
| `ingest.extract_timeout_secs` | 60 | The time limit for the text extraction of one item |
| `ingest.web.timeout_secs` | 30 | The time limit to download one page |
| `ingest.web.max_redirects` | 5 | The number of redirects that the tool follows |
| `ingest.web.allow_private` | `false` | Allow pages on private addresses |
| `ingest.local.deny` | `[]` | Extra file patterns that the tool must never read |

## Other

| Key | Default | Meaning |
|---|---|---|
| `display.language` | language of the computer | The language of the section labels: `ja` or `en` |
| `search.backends` | `["sqlite-fts"]` | The search method |
| `notify.sinks` | `["doctor"]` | Where the tool reports problems |
| `notify.min_severity` | `error` | The lowest severity that the tool reports |
| `backup.last_at` | none | The time of your last backup |

## Account settings

Each account has its own settings. Open them with `sb config edit --account <id>`.
[Slack account](slack-account.md) describes the Slack settings.

For a Google account, these settings are available:

| Key | Default | Meaning |
|---|---|---|
| `features` | `["meet"]` | What the account can do |
| `meet_strategies` | `["calendar", "drive"]` | Where the tool looks for Meet notes |
| `calendar_overlap_days` | 3 | Days that the tool reads again in the calendar |
| `initial_days` | `sync.initial_days` | Days for the first sync of this account |
| `meet_folder_id` | found by name | The ID of the "Meet Recordings" folder |
