# Maintenance

This chapter shows how to check the health of the tool, back up your data, and
uninstall the tool.

## Check the health

```sh
$ sb doctor
```

The command runs a list of checks. Each check shows one of these marks:

| Mark | Meaning |
|---|---|
| `✔` | The check passed |
| `!` | A warning. Read the hint below the line |
| `✖` | An error. You must fix it |
| `·` | Information only |

After the checks, the command lists the open problems. For each problem, it shows a
command that can fix it.

The exit code of the command is 0 when all checks passed, 1 for warnings and 2 for
errors.

| Option | Meaning |
|---|---|
| `--online` | Also run the checks that need the network. For example, the tool tests your logins and your summarizers |
| `--fix` | Fix the problems that the tool can fix by itself |

Without `--online`, the command does not use the network.

### What the checks cover

| Area | What the tool checks |
|---|---|
| Home directory | The folder exists, and only you can read it |
| Database | The database is not damaged, and its version is current |
| Backup | You made a backup in the last 30 days |
| Raw data | The files of the entries exist |
| Accounts | No account needs a new login |
| Summarizers | The profiles are complete, and the programs exist |
| Budget | The spend against the weekly and monthly limits |
| Schedule | The daily sync is registered |
| Recent runs | The last scheduled sync finished in the last 36 hours |
| Summaries | The number of pending and failed summaries |
| Skills | The installed skill has the same version as the program |
| Search index | The index matches the entries |
| Disk | The size of the home directory, and the free space. The tool warns below 1 GB |

## Back up your data

The database contains your credentials, and the tool cannot rebuild it from other
data. Make a backup.

1. Make sure that no sync is running.
2. Copy the file `second-brain.db` from the home directory to a safe place.
3. Tell the tool about the backup:

   ```sh
   $ sb config set backup.last_at 2026-10-08T12:00:00Z
   ```

To keep all data, also copy the folder `raw/`.

**Warning:** A copy of the database contains your passwords and tokens. Keep the copy
private. Do not upload it to a shared place.

## Log in again

If an account has the status `needs_reauth`, run:

```sh
$ sb auth login <id>
```

Use `sb auth status` to see all accounts. See [Google account](google-account.md) and
[Slack account](slack-account.md).

## Rebuild the search index

If `sb doctor` reports that the index does not match the entries, run:

```sh
$ sb index rebuild
```

## Fetch data again

| Command | Use it to |
|---|---|
| `sb refetch` | Download the raw data again from the source. It replaces the saved raw data. Use `--entry`, `--account`, `--source`, `--since` or `--until` to choose entries. Use `--raw-missing` for entries without raw data |
| `sb reextract` | Process the saved raw data again. It uses no network |

Use `sb refetch --entry <id>` when a Slack message was edited or deleted after the
first sync. The sync does not see such changes.

## Logs

A scheduled sync writes a log file for each day in the folder `logs/` in the home
directory. The tool keeps the files for 30 days.

For more output in the terminal, add `-v` to a command. You can repeat it (`-vv`). To
set the log level, set the environment variable `SB_LOG`, for example `SB_LOG=debug`.

## Change the language of labels

The setting `display.language` selects the language of the section labels in the
output. The values are `ja` and `en`. The default is the language of your computer.

```sh
$ sb config set display.language en
```

## Move the home directory

To use another folder, set `SECOND_BRAIN_HOME`, or use `--home`. Then run:

```sh
$ sb setup env
```

The command saves the setting so that scheduled jobs and agents find the folder. On
Windows, it writes the user environment variable. On macOS and Linux, it offers to
add an `export` line to the file of your shell.

## Uninstall

1. Remove the scheduled jobs:

   ```sh
   $ sb setup schedule --remove
   ```

2. Remove the skills:

   ```sh
   $ sb setup skills --remove --target all
   ```

3. Delete the programs `second-brain` and `sb`.
4. To delete your data, delete the home directory.

**Warning:** If you delete the home directory, you lose all entries, summaries and
saved credentials. You cannot undo this.

Steps 1 to 3 keep your data. You can install the tool again and continue.
