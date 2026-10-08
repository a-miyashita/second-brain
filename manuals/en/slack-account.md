# Slack account

This chapter shows how to connect a Slack workspace.

## What the tool reads

The tool reads Slack with **your own** permissions. It sees what you can see. It
cannot post or change anything.

| Content | What the tool reads |
|---|---|
| Direct messages and group direct messages | Every message |
| Channels that you list in `full_channels` | Every message |
| Other channels that you joined | Only the threads that involve you |

A thread involves you in these cases:

- You wrote the first message, or a reply, in the thread.
- A message mentions you.
- A message contains `@channel`, `@here` or `@everyone`.

The tool makes one entry for each thread. It also makes one entry for each channel and
each day for the messages that are not in a thread.

## Create the Slack app

Each workspace needs its own small Slack app. Only you use this app.

1. Open <https://api.slack.com/apps>. Choose **Create New App** > **From an app
   manifest**.
2. Choose your workspace.
3. Paste the content of the file `assets/slack-app-manifest.yaml` from the repository.
4. Choose **Install to Workspace** and approve the request.
5. Open **OAuth & Permissions**. Copy the **User OAuth Token**. It starts with
   `xoxp-`.

**Warning:** Do not enable **Distribute App**. A distributed app has a limit of
one `conversations.history` request each minute. The sync then becomes very slow.

**Note:** You must use the user token. A bot token cannot read your direct messages
and your private channels.

If your workspace asks for administrator approval of new apps, ask an administrator.

## Add the account

Run this command. Replace the account ID with your own value.

```sh
$ sb account add slack acme-slack
```

The command asks for the token. Paste the token at the hidden prompt. The prompt does
not show the token, and the token stays out of your shell history.

The command checks the token and shows a warning if a permission is missing. The
tool saves the token in its database.

Use `--label <text>` to give the account a display name.

**Warning:** Do not use `--token` on the command line. The token then appears in your
shell history.

## Choose the channels

Open the account settings in your editor:

```sh
$ sb config edit --account acme-slack
```

These are the main settings:

| Setting | Default | Meaning |
|---|---|---|
| `full_channels` | `[]` | Channels that the tool reads completely. Use channel names or IDs |
| `include_dms` | `true` | Read all direct messages and group direct messages |
| `exclude_bot_dms` | `true` | Skip direct messages with bots |
| `exclude_channels` | `[]` | Channels or people to skip |
| `mention_scan` | `all` | How the tool finds threads that involve you in other channels: `all`, `search_only` or `off` |
| `initial_days` | 30 | How many days back the first sync reads |

The tool checks the file when you save it.

## Slack time zone

Entries for channel days use a local date. The setting `slack.day_timezone` selects
the time zone. The default is the time zone of your computer.

```sh
$ sb config set slack.day_timezone Europe/Berlin
```

## Log in again

If Slack revokes your token, the account gets the status `needs_reauth`. To fix this,
create a new token and run:

```sh
$ sb auth login acme-slack
```

The command asks for the new token.

## Next step

Run `sb sync`. See [Syncing](sync.md).
