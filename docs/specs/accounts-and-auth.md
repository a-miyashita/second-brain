# Accounts and authentication

Related ADRs: 0003, 0006, 0007.

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
Google), and, with `--online`, a live check (Google token refresh, Slack
`auth.test`).
