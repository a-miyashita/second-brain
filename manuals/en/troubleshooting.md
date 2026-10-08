# Troubleshooting

This chapter lists problems and their solutions. Start with `sb doctor`. It finds
most problems and tells you what to do.

## The tool says that the home directory is not set up

**Message:** `... is not set up; run sb setup (or sb setup home)`

**Cause:** The home directory does not exist, or `SECOND_BRAIN_HOME` points to the
wrong folder.

**Solution:**

1. Run `sb setup home`.
2. If you use another folder, set `SECOND_BRAIN_HOME`, or run `sb setup env`.

## An account needs a new login

**Symptom:** `sb doctor` or `sb auth status` shows `needs_reauth`. `sb sync` skips the
account.

**Cause:** Google or Slack ended the login. For a Google OAuth client with the type
**External** and the status **Testing**, this happens every 7 days.

**Solution:** Run `sb auth login <id>`.

## Another sync is running

**Symptom:** The command stops with exit code 75 and the error `sync.locked`.

**Solution:** Wait until the other sync finishes. Then run the command again. If no
sync is running, the lock is old. The tool removes a lock after 6 hours
(`sync.lock_stale_hours`).

## Summaries stay `pending`

**Possible causes and solutions:**

| Cause | Solution |
|---|---|
| You have no summarizer | Run `sb setup llm` |
| The weekly or monthly limit is used up | Run `sb budget`. Wait for the next period, or raise the limit. See [Summaries and cost](summaries.md) |
| The price of the model is unknown (`llm.unpriced`) | Set the price in `llm.prices`, or turn the limits off |
| The local server is not running (`llm.local_unreachable`) | Start the server. The entries stay `pending` and the tool tries again |
| The text is shorter than `summary.min_chars` | This is normal. The text is searchable. The status is `skipped` |

Run `sb summarize` to write the pending summaries.

## A summary failed

**Symptom:** The status of the entry is `failed`.

**Solution:**

1. Run `sb doctor` and read the issue `llm.failed` or `llm.bad_output`.
2. Fix the cause. For example, check the API key or the login of the CLI.
3. Run `sb summarize --retry-failed`.

After 3 failed attempts, the tool stops. `--retry-failed` resets the counters.

## The search finds nothing

**Possible causes and solutions:**

| Cause | Solution |
|---|---|
| The data is older than your first sync | Run `sb sync --since 90d` (or a longer period). See [Syncing](sync.md) |
| You used too many words | The search needs **all** words. Use fewer, more specific words |
| The words are in another section | Search without `--section` |
| The sync did not run | Run `sb sync`, and read its output |
| The search index is wrong | Run `sb doctor`. If it reports a problem, run `sb index rebuild` |

## Slack is slow

**Cause:** The Slack app is distributed. A distributed app can make only one
`conversations.history` request each minute.

**Solution:** Create an internal app with the manifest, and do not enable **Distribute
App**. See [Slack account](slack-account.md).

## Meet notes are missing

**Possible causes and solutions:**

| Cause | Solution |
|---|---|
| The meeting had no Gemini notes | The tool reads only notes that Gemini wrote |
| The notes are not in your calendar events or in your "Meet Recordings" folder | Open the notes in Google Drive, copy the link, and run `sb ingest <link>` |
| The meeting is older than the first sync | Run `sb sync --since 90d` |
| The Google login ended | Run `sb auth login <id>` |

## `sb ingest` fails

| Message or status | Solution |
|---|---|
| `HTTP 403` or `401` for a web page | The page needs a login. Save the page as a file. Add the file |
| `no extractable text` | The file has no text layer, or the page needs JavaScript. Use another format |
| `path is not allowed` | The tool never reads this file. See [Adding documents](ingest.md) |
| `not found or no access` for a Google link | None of your Google accounts can open the file. Add the right account, or ask for access |
| A private address is refused | Set `ingest.web.allow_private` to `true` only if you trust the page |
| `folders are not supported` | Add the files in the folder one by one |
| `Slack content is ingested by sb sync` | Use `sb sync` for Slack links |

## A scheduled sync does not run

1. Run `sb doctor`. Look at the checks `schedule.registered` and `runs.recent`.
2. If the schedule is missing, run `sb setup schedule`.
3. On Windows, the task runs only while you are logged on.
4. Read the log files in the folder `logs/` of the home directory.
5. If you use a non-default home directory, run `sb setup env`.

## The tool cannot find a CLI summarizer

**Symptom:** `sb doctor` reports that a program such as `claude` is not on `PATH`.

**Solution:** Install the program, or add its folder to `PATH`. Then run
`sb setup schedule` again, so that the scheduled jobs get the right `PATH`.

## The Antigravity summarizer refuses to run

**Cause:** The file `~/.gemini/antigravity-cli/settings.json` has a non-empty
`permissions.allow` list, or the tool cannot read the file. The tool refuses to run in
this case, for safety.

**Solution:** Remove the allow rules from the file. Or use another summarizer.

## You need more help

1. Run the command again with `-vv` to see more output.
2. Run `sb doctor --online`.
3. Report the problem to the project. Do not include tokens or private text.
