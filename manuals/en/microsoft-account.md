# Microsoft account

This chapter shows how to connect a Microsoft account. With a Microsoft account, the
tool reads files in SharePoint and OneDrive when you add them with `sb ingest`.

## What the tool reads

- **Files in SharePoint document libraries.** For example, Word, Excel, PowerPoint and
  PDF files of your team site.
- **Files in OneDrive.** This includes OneDrive for work or school, and personal
  OneDrive.

The tool reads one file for each link. It does not read folders, document libraries or
whole sites, and it has no sync for these. See [Adding documents](ingest.md).

The tool has read-only access. It cannot change your files.

## Why you need your own application

The tool uses an **application** that you register in Microsoft Entra ID (the account
service of Microsoft 365). Because of this, no third party can see your data. The
application has no password or secret. The tool needs only its **Application (client)
ID**.

If you use Microsoft 365 at work, an administrator can register one application for the
whole organization. The administrator then gives the client ID to all users.

## Register the application

You must do these steps one time.

1. Open the [Microsoft Entra admin center](https://entra.microsoft.com/). Go to
   **Applications** > **App registrations** > **New registration**.
2. Enter a name, for example `second-brain`. For **Supported account types**, choose:
   - **Single tenant**, if only your organization uses the application;
   - **Accounts in any organizational directory and personal Microsoft accounts**, if
     you also use personal OneDrive.
3. Under **Redirect URI**, choose the platform **Public client/native (mobile &
   desktop)**. Enter `http://localhost`. Choose **Register**.
4. Open **Authentication**. Make sure that **Allow public client flows** is set to
   **Yes**.
5. Open **API permissions** > **Add a permission** > **Microsoft Graph** >
   **Delegated permissions**. Add `Files.Read.All`. The permissions `openid`,
   `profile` and `offline_access` are also needed. Do not add any other permission.
6. Copy the **Application (client) ID** from the **Overview** page. It looks like
   `11111111-2222-3333-4444-555555555555`. Copy the **Directory (tenant) ID** too.

**Warning:** In many organizations, an administrator must approve the permission
`Files.Read.All` before you can use it. If your account is refused, send this text to
your administrator:

> Please approve the delegated Microsoft Graph permission `Files.Read.All` for the
> application `<name>` (client ID `<id>`). It is a read-only permission. The
> application is a public client that runs on my computer. It cannot change files.

## Add the account

Run this command. Replace the account ID and the client ID with your own values.

```sh
$ sb account add microsoft work-ms --client-id 11111111-2222-3333-4444-555555555555
```

1. The command opens your browser.
2. Sign in to Microsoft and give the permissions that the tool asks for.
3. Close the browser tab when the page tells you that the login is done.

If your computer has no browser, add `--no-browser`. The command then prints a URL.
Open the URL on the same computer, and complete the login there. The login must end on
the computer that runs the command, because the browser sends the result to
`http://localhost`.

### Options

| Option | Meaning |
|---|---|
| `<id>` | A name that you choose for the account, for example `work-ms`. You cannot change it later |
| `--client-id <GUID>` | The Application (client) ID |
| `--tenant <value>` | The tenant ID or domain of your organization. Other values: `organizations` (work or school accounts, the default), `consumers` (personal accounts) and `common` (both) |
| `--label <text>` | A display name for the account |
| `--no-browser` | Print the URL instead of opening a browser |

## Sync folders and read linked documents

The permission `Files.Read.All` is enough to sync a SharePoint or OneDrive folder, and
to read the documents that people link in Slack. You do not need `Sites.Read.All`, and
the tool never lists a whole site. It reads only the folders that you name with
`sb folder add`. See [Folders and linked documents](folders-and-links.md).

## Check the account

```sh
$ sb account list
$ sb auth status --online
```

`sb auth status --online` tests the login with the service.

## Log in again

Sometimes Microsoft ends a login. This happens, for example, after 90 days without use,
or when your organization changes its sign-in rules. The account then gets the status
`needs_reauth`. To fix this, run:

```sh
$ sb auth login work-ms
```

If you sign in with a different Microsoft identity, the command refuses. Add
`--allow-identity-change` only if you want to change the identity on purpose.

## Disable or remove an account

The commands are the same as for a Google account. See
[Google account](google-account.md#disable-or-remove-an-account).
