# Accounts and authentication

Related ADRs: 0003, 0006, 0007, 0021, 0022.

## Google

### Prerequisite: OAuth client

The user (or a Workspace admin) creates a Google Cloud project and enables the APIs
needed: Drive, Calendar, and optionally Meet and Gmail. Then:

- Configure the OAuth consent screen:
  - **Internal** for Workspace organizations: no 7-day expiry, no verification;
  - otherwise **External**, which in Testing status expires refresh tokens after 7
    days.
- Create an OAuth client ID of type **Desktop app**, and download its JSON.

`README.md` links to a step-by-step guide. A Workspace admin can share one client
JSON with the whole organization.

### `sb account add google <id> --client-secret <file> [--features meet,docs]`

1. Store the client JSON as secret `google.oauth_client` for the account (or reuse
   an existing global one with `--client-secret global`).
2. Compute scopes from the features:

   | Feature | Scopes |
   |---|---|
   | `meet` | `drive.readonly`, `calendar.readonly` (plus `meetings.space.readonly` if the Meet API strategy is enabled) |
   | `docs` | `drive.readonly` |
   | `gmail` | `gmail.readonly` |

   The base scopes are always `openid` and `email`.

   `sb ingest` of Google Docs (ADR-0014) needs only `drive.readonly`, so an account
   added with either `meet` or `docs` can ingest. Which account reads a given file is
   described in [ingest.md](ingest.md#choosing-the-google-account).
3. Run the flow:
   - PKCE;
   - a loopback listener on `127.0.0.1:0`;
   - open the browser, or print the URL if `--no-browser` is set or no display is
     available;
   - exchange the code;
   - read `email` from the ID token and store it as `identity`.
4. Store `google.refresh_token`. Access tokens are cached in memory only.

### Token lifecycle

- Access tokens are refreshed on demand.
- On `invalid_grant`:
  - set the account to `status = needs_reauth`;
  - open an `auth.needs_reauth` issue;
  - skip the account for this run.
- `sb auth login <id>` re-runs the flow and resolves the issue on success.
- A different Google identity at re-login is refused unless `--allow-identity-change`
  is given.

## Microsoft (SharePoint and OneDrive)

### Prerequisite: application registration

The user (or a Microsoft 365 administrator) registers an application in Microsoft Entra
ID (Azure portal, "App registrations"):

- Supported account types: "single tenant" for one organization, or "accounts in any
  organizational directory" (add "personal Microsoft accounts" for personal OneDrive).
- Platform: **Mobile and desktop applications**, redirect URI `http://localhost`.
  Entra ignores the port of a `localhost` redirect, so the ephemeral loopback port works.
- Authentication: **Allow public client flows** is on. There is **no client secret**.
- API permissions (delegated, Microsoft Graph): `Files.Read.All`, `offline_access`,
  `openid`, `profile`, `email`. No other permission is needed, and no write permission
  should be granted.

Some tenants require an administrator to grant consent for the application, or have
turned off user consent. That is a tenant policy, not something `sb` can change. The
error from Entra is shown as it is, and `README.md` has the text to send to the
administrator. A registration can be shared by the whole organization: only the
Application (client) ID and the tenant are needed, and neither is a secret.

### `sb account add microsoft <id> --client-id <GUID> [--tenant <t>] [--label ..] [--no-browser]`

1. Store `client_id` and `tenant` in the account `config` (they are not secrets).
   `--tenant` is a tenant ID, a verified domain, or `organizations` (default),
   `consumers` or `common`.
2. Scopes: `openid profile email offline_access Files.Read.All`.
3. Run the flow, the same as for Google (PKCE, loopback listener on `127.0.0.1:0`,
   browser or printed URL). The redirect URI is `http://localhost:<port>`, and the
   listener accepts the connection on both loopback addresses because a browser may
   resolve `localhost` to `::1`.
4. Read `tid` and `oid` from the ID token and store `identity = <tid>:<oid>`. The default
   label is `preferred_username`.
5. Store the secret `microsoft.refresh_token`. Access tokens are cached in memory only.

### Token lifecycle

- Access tokens are refreshed on demand.
- **Entra rotates refresh tokens.** Every refresh response carries a new refresh token.
  It is written to the secrets table **before** the new access token is returned for use.
  Inside one process the refresh is serialized. Ingest takes the sync lock, but
  `sb auth status --online`, `doctor` and `sb auth login` do not, so two processes can
  refresh at the same time. The last writer wins. Entra normally keeps the previous
  refresh token valid for a while, so the damage is a sign-in again at worst.
- On `invalid_grant`, `interaction_required` or `consent_required`: set the account to
  `needs_reauth`, open an `auth.needs_reauth` issue, and skip the account for this run.
  Refresh tokens of public clients expire after 90 days of inactivity, and conditional
  access policies can force a new sign-in earlier.
- `sb auth login <id>` re-runs the flow and resolves the issue. A different identity
  (`tid:oid`) is refused unless `--allow-identity-change` is given.

## Slack

### Prerequisite: Slack app

The manifest is shipped in `assets/slack-app-manifest.yaml`. It requests only user
scopes: `channels:*`, `groups:*`, `im:*`, `mpim:*` history/read, `users:read`,
`users:read.email`, `search:read` and `files:read`.

- Do **not** enable Distribute App. Internal-only apps keep the normal rate limits
  (~50 req/min for `conversations.history`). Distributed non-Marketplace apps are
  limited to 1 req/min.
- Use the **User OAuth Token** (`xoxp-`), not a bot token. DMs and private channels
  can only be read with the user's own token.
- One app per workspace. Each workspace becomes one second-brain account.

### `sb account add slack <id> [--token ...]`

1. Read the token from the flag, or from an interactive hidden prompt (preferred, so
   it stays out of shell history).
2. Call `auth.test`, take `team_id`, `user_id` and `team`, and store the identity as
   `team_id:user_id`. Take the label from the team name if none was given.
3. Verify the scopes (the `x-oauth-scopes` response header) and warn if any are
   missing.
4. Store `slack.user_token`, and write default `config` (see
   [source-slack.md](source-slack.md)).

A token revoked or invalidated (`invalid_auth`, `token_revoked`) triggers the same
`needs_reauth` handling. `sb auth login <id>` prompts for a new token.

## LLM credentials

API keys are global secrets: `anthropic.api_key`, `openai.api_key`,
`google.api_key` (Gemini). `sb setup llm` sets them and they can be replaced with
`sb config set-secret <name>` (hidden prompt). The environment fallbacks follow
ADR-0003.

## `sb auth status`

For each account it shows: kind, identity, status, last successful use, scopes (for
Google and Microsoft), and, with `--online`, a live check (Google and Microsoft token
refresh, Slack `auth.test`). A Microsoft check refreshes the token, which rotates it.

## Folders and link settings in the account `config` (ADR-0022)

- **Google and Microsoft accounts** can have `folders`, a list added with `sb folder add`
  and edited with `sb config edit --account <id>`. Keys and rules:
  [folder-sync.md](folder-sync.md#folder-settings). No scope and no consent change: Google
  uses `drive.readonly`, Microsoft uses `Files.Read.All`. Folder sync of a Microsoft
  account lists one folder named by the user; it never lists a site or a drive root that
  the user did not name.
- **Slack accounts** can have `ingest_links` and `ingest_link_hosts` (source-slack.md,
  link-ingest.md). They only decide which already-recorded links are fetched with the
  user's **Google and Microsoft** accounts; Slack scopes do not change.
