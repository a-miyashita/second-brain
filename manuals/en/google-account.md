# Google account

This chapter shows how to connect a Google account. With a Google account, the tool
reads your Google Meet notes and your Google Docs.

## What the tool reads

- **Meet notes.** These are the notes that Gemini writes when you use "Take notes for
  me" in Google Meet. The tool finds them in two places:
  - the files that are attached to calendar events that you attended;
  - the "Meet Recordings" folder in your Google Drive.
- **Google Docs, Sheets and Slides**, when you add them with `sb ingest`. See
  [Adding documents](ingest.md).

The tool has read-only access. It cannot change your files.

## Why you need your own OAuth client

The tool uses an **OAuth client** that you create. Because of this, no third party
can see your data. The OAuth client is a small credential file that belongs to a
Google Cloud project.

If you use Google Workspace, an administrator can create one client for the whole
organization. The administrator can then share the client file with all users.

## Create an OAuth client

You must do these steps one time.

1. Open the [Google Cloud console](https://console.cloud.google.com/). Create a
   project, or choose an existing project.
2. Open **APIs & Services** > **Library**. Enable the **Google Drive API**. Enable
   the **Google Calendar API**.
3. Open **APIs & Services** > **OAuth consent screen**. Choose the user type:
   - If you use Google Workspace, choose **Internal**. The refresh tokens do not
     expire after a week. Google does not ask for a verification.
   - If you use a personal account, choose **External**. Add yourself as a test
     user.
4. Open **APIs & Services** > **Credentials**. Choose **Create credentials** >
   **OAuth client ID**. Set the application type to **Desktop app**.
5. Download the JSON file of the client.

**Warning:** With the user type **External** and the status **Testing**, Google
ends your login after 7 days. You must then run `sb auth login <id>` about once a
week. `sb doctor` tells you when this is necessary.

## Add the account

Run this command. Replace the account ID and the file name with your own values.

```sh
$ sb account add google work-google --client-secret ~/Downloads/client_secret_XXXX.json
```

1. The command opens your browser.
2. Sign in to Google and give the permissions that the tool asks for.
3. Close the browser tab when the page tells you that the login is done.

If your computer has no browser, add `--no-browser`. The command then prints a URL.
Open the URL on another computer, and complete the login there.

The tool saves the client file and the refresh token in its database. You can
delete the downloaded file after the command finishes.

### Options

| Option | Meaning |
|---|---|
| `<id>` | A name that you choose for the account, for example `work-google`. You cannot change it later |
| `--client-secret <file>` | The client JSON file. Use `global` to reuse a client that you saved before |
| `--label <text>` | A display name for the account |
| `--features <list>` | The things that the account can do, separated by commas. The default is `meet`. Use `meet,docs` to also read Google Docs |
| `--no-browser` | Print the URL instead of opening a browser |

Both features `meet` and `docs` give the tool read-only access to Google Drive. An
account with either feature can ingest Google Docs.

### Add a second account

You can add more than one Google account. To reuse the same client, give the same
client file again, or use `--client-secret global`.

## Check the account

```sh
$ sb account list
$ sb auth status
```

`sb auth status` shows if the credentials of each account are valid. Add `--online`
to test the login with the service.

## Log in again

Sometimes Google ends a login. Then the account gets the status `needs_reauth`, and
`sb sync` skips it. To fix this, run:

```sh
$ sb auth login work-google
```

If you sign in with a different Google identity, the command refuses. Add
`--allow-identity-change` only if you want to change the identity on purpose.

## Disable or remove an account

| Command | Result |
|---|---|
| `sb account disable <id>` | `sb sync` skips the account. All data stays |
| `sb account enable <id>` | `sb sync` uses the account again |
| `sb account remove <id>` | The tool removes the account. The entries stay |
| `sb account remove <id> --purge` | The tool removes the account, its entries and its raw files |

**Warning:** `--purge` deletes data. You cannot undo it.
