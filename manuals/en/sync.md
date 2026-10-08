# Syncing

A **sync** collects new data from your accounts. After the collection, the tool writes
summaries for the new entries and updates the search index.

## Run a sync

```sh
$ sb sync
```

The command does these steps:

1. It reads new content from all enabled accounts.
2. It writes summaries for the new and changed entries.
3. It updates the search index.

On the first run, the tool reads the last 30 days. After that, it reads only what is
new.

## Stop and continue

You can stop a sync at any time. Press Ctrl+C one time. The tool finishes the work
that is in progress and then stops. It keeps everything that is finished.

To continue, run `sb sync` again. The tool starts where it stopped.

**Note:** If you press Ctrl+C a second time, the tool stops at once. The tool still
keeps the finished work.

## Check before you run

| Option | Result |
|---|---|
| `--dry-run` | Shows what the sync would do. The command uses no network |
| `--estimate` | Shows the number of tokens and the cost of the pending summaries |

## Limit a sync

| Option | Result |
|---|---|
| `--account <id>` | Sync only this account |
| `--source <kind>` | Sync only this source kind, for example `slack.thread` |
| `--no-summary` | Collect data, but do not write summaries |
| `--max-summaries <n>` | Write at most this number of summaries |
| `--max-cost <usd>` | Stop when the estimated cost reaches this amount |
| `--time-limit <time>` | Stop after this time, for example `30m`, `2h` or `1h30m` |

When a limit stops the command, the exit code is 0. The command shows what remains.
The remaining entries stay searchable.

## Read older data

The first sync reads 30 days. To read older data, use `--since`:

```sh
$ sb sync --since 90d
```

The command reads only the days that you did not read before. In this example, it
reads days 31 to 90.

You can write the date in these ways:

| Format | Example |
|---|---|
| Date | `2026-07-01` |
| Number of days | `90d` |
| Number of weeks | `12w` |

To repair a time range, add `--until`:

```sh
$ sb sync --since 2026-07-01 --until 2026-07-15
```

The command then reads this range again, from the start date up to, but not including,
the end date. You cannot use `--until` without `--since`.

To change the length of the first sync, change the setting `sync.initial_days`:

```sh
$ sb config set sync.initial_days 60
```

## Run a sync every day

The setup wizard registers the daily sync. To register it again, or to change the
times, run:

```sh
$ sb setup schedule --time 19:30 --deep-day mon --deep-time 18:30
```

| Option | Default | Meaning |
|---|---|---|
| `--time <HH:MM>` | `19:30` | The time of the daily sync |
| `--deep-day <day>` | `mon` | The day of the weekly deep sync |
| `--deep-time <HH:MM>` | `18:30` | The time of the weekly deep sync |
| `--systemd` | | On Linux, use systemd timers instead of cron |
| `--dry-run` | | Show what the command would register. The command registers nothing |
| `--remove` | | Remove the scheduled jobs |

The tool uses the scheduler of your operating system:

| Operating system | Scheduler |
|---|---|
| Windows | Task Scheduler. The tasks run only while you are logged on |
| macOS | launchd |
| Linux | cron. With `--systemd`, systemd user timers |

The normal sync reads recent content. The **deep sync** also reads conversations that
were quiet for a long time, and older threads that can have new replies. Use
`sb sync --deep` to run a deep sync yourself.

If your computer is off at the scheduled time, Windows and systemd run the job when
the computer starts again. Cron does not do this.

## Run two syncs at the same time

Only one sync can run at a time. If another sync is running, the new command stops
with exit code 75. Wait, and then run it again.

## Read the result

When the command finishes, it prints what changed and what remains. Use `sb stats` to
see the totals:

```sh
$ sb stats
```

Use `sb doctor` to see problems, for example an account that needs a new login.
