# Source: Slack (`slack.thread`, `slack.day`)

Related ADRs: 0007, 0008. Target scale: hundreds of conversations and thousands
of workspace users per account, with a daily run of a few minutes.

## Account configuration (`accounts.config`)

| Key | Default | Meaning |
|---|---|---|
| `full_channels` | `[]` | Channel names (or IDs) fully ingested |
| `include_dms` | `true` | All DMs and group DMs |
| `exclude_bot_dms` | `true` | Drop DMs with bots (notification noise) |
| `exclude_channels` | `[]` | Names, or DM partner names/handles, to skip |
| `mention_scan` | `all` | For other joined channels: `all` (history scan + search), `search_only`, `off` |
| `search_terms` | `[]` | Extra names for mention search; empty uses profile display name and handle |
| `backfill_days` | `365` | Initial look-back for new conversations |
| `thread_watch_days` | `45` | Watch threads for new replies up to this age (last activity) |
| `thread_hot_days` | `7` | Threads always checked. Older watched threads are checked only in `--deep` |
| `dormant_days` | `30` | Conversations idle this long are only scanned in `--deep` (except `full_channels`) |
| `fetch_jobs` | `4` | Parallel API workers |
| `users_cache_days` | `7` | User directory cache lifetime |

## What is ingested

| Target | Scope |
|---|---|
| `full_channels` | Every message |
| DMs / group DMs (`include_dms`) | Every message |
| Other joined channels | Only threads "involving me" |

A thread involves me if:

1. **History scan**: the parent message is from me, mentions me, or contains
   `@channel`/`@here`/`@everyone`.
2. **Search**: `search.messages` for `from:me` and my names finds a reply inside the
   thread. This is the only way to find replies-only involvement, because
   `conversations.history` does not return replies.

## Entry units and keys

| Unit | `source_kind` | `source_id` | `source_url` |
|---|---|---|---|
| One thread (parent + replies) | `slack.thread` | `<channel_id>:<thread_ts>` | `chat.getPermalink` of the parent |
| Non-thread messages of one channel on one local date | `slack.day` | `<channel_id>:day:<YYYY-MM-DD>` | Permalink of the first message that day |

- `source_created_at` is the first message ts. `source_updated_at` is the last
  message or reply ts.
- The "local date" for `slack.day` uses the setting `slack.day_timezone` (defaults
  to the OS timezone).

## Raw data

The raw object has role `primary`. It is a JSONL file of the Slack message objects
as returned by the API, unmodified, split into **segments** (ADR-0012):

- Segment 0 holds the messages fetched the first time.
- Each later fetch that finds new replies (or new messages of the day) appends one
  segment containing **only the new messages**.
- `entries.fetch_state = {"last_ts": <newest ts stored>, "reply_count": n}`.

Earlier segments are never rewritten. A long thread is therefore never
re-downloaded because one reply arrived.

User and channel names are **not** baked into the raw file; they are resolved at
normalize time from the cache. Since `normalize` is pure, the user directory
snapshot it needs is passed in through its context.

## Normalization

- All segments are read in order and de-duplicated by `ts`; the last occurrence
  wins.
- **User names:**
  - Resolved from the account's user directory cache (`users.list`).
  - **Slack Connect** users from other workspaces do not appear in `users.list`.
    Their names are harvested from the `user_profile` object embedded in their
    messages (real name, then display name, then first name) and added to the
    cache, marked `external`. Without this, external speakers render as unknown
    and summaries degrade.
  - The directory snapshot given to `normalize` includes these harvested names.
- **Message body rendering:**
  - The `text` field has mrkdwn unescaped and `<@U…>` / `<#C…>` / `<!here>`
    resolved to names.
  - If `text` is empty (workflow posts, company announcements), the body is built
    by walking **Block Kit** `blocks` recursively. Text is collected from section,
    header, rich_text and context blocks (including `elements`, `fields`,
    `accessory`):
    - `user` elements become `@name`;
    - `channel` and `broadcast` elements become `@…`;
    - links become `label (url)`;
    - images and buttons are skipped;
    - duplicate fragments are removed.
  - Legacy `attachments` are appended as quoted lines: `> title / text`, falling
    back to `fallback`.
  - Files are appended as `[attachment: <name>]`.
- The `details` section (origin `extracted`) is the rendered conversation, one
  message per line block: `HH:MM Name: body`. The date is included when a
  conversation spans several days.
- **Title:**
  - Built from a conversation label: `#channel` for channels and private
    channels, `DM: <name>` for DMs, `Group DM: <names>` for group DMs.
  - `slack.thread`: `<label> <first meaningful line of the thread>`. The first
    line is taken with the speaker prefix, URLs and leading/trailing `@mentions`
    removed. It must be at least 4 characters, and is truncated to about 46.
  - `slack.day`: `<label> <YYYY-MM-DD> conversation`.
  - The label words ("Group DM", "conversation") follow `display.language`.
- Metadata: `channel_id`, `channel_name`, `channel_kind`, `thread_ts`,
  `message_count`, participants.
- Summary input: the rendered conversation, with prompt `conversation-summary/v1`.
  The thresholds come from summarization.md.

## Sync algorithm

- **Normal run** (daily):
  1. Drain `sync_queue` left over from an interrupted run.
  2. List conversations and skip excluded ones. Skip dormant ones unless they are
     `full_channels`.
  3. For each conversation, run `conversations.history` forward from the stored
     cursor, **one window at a time** (default 7 days, oldest first). For each
     window, in one transaction:
     - upsert the channel-day entries of that window (append segments for days
       that already exist);
     - enqueue threads into `sync_queue`: new parents with replies, involved
       parents, and parents whose `latest_reply` is newer than the stored
       `fetch_state.last_ts`;
     - advance the conversation cursor to the window end.
  4. Enqueue watched threads within `thread_hot_days`, and threads found by the
     involvement search.
  5. Process the queue:
     - **Known thread:** `conversations.replies` with `oldest = fetch_state.last_ts`
       (exclusive), paginated. This fetches only the new replies. The parent may
       be returned again and is de-duplicated. If nothing is new, the queue row is
       dropped without writing anything.
     - **New thread:** fetch the full thread into segment 0.
     - Commit in chunks of `pipeline.commit_batch`. Each commit writes the entry,
       the new segment, `fetch_state`, and deletes the queue row.
  6. Normalize the changed entries. Summaries are redone only when `input_hash`
     changes: a grown thread is re-summarized **as a whole**, while its fetch was
     incremental.
- **Deep run** (`--deep`, weekly): also dormant conversations and all watched threads
  up to `thread_watch_days`.
- **New `full_channels`** are backfilled `backfill_days` automatically. Other
  conversations continue from their cursors.
- **Rate limits:** honour `Retry-After` on HTTP 429. Parallelism across methods is
  bounded by `fetch_jobs`, because limits are per method.
- **Edits and deletions** of earlier messages are not tracked by incremental
  fetches. `sb refetch --entry <uid>` (or with filters) replaces all segments with
  a fresh full copy.
- **Interruption:** any run can be stopped. It resumes from the cursors and the
  queue, repeating at most one window or commit chunk (ADR-0012).

State is kept in:

- `sync_state`: per-conversation cursor, and the thread watch list with last
  activity;
- `entries.fetch_state`: per-thread and per-day `last_ts`;
- `sync_queue`: pending work.

The user directory is kept in `cache`.

## `--estimate`

It counts pending summarization inputs and
estimated tokens and cost without calling the LLM.

## Link scanning (phase 2)

`sb scan-links` scans the `details` of Slack entries that have `decisions` or
`action_items` sections. It lists Google Docs, Slides and Sheets links whose Drive
file ID is not yet an entry in any Google account. It never fetches anything.
