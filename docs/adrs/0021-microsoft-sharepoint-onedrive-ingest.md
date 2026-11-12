# ADR-0021: Microsoft accounts and single-item ingest for SharePoint and OneDrive

- Status: Accepted
- Date: 2026-11-06

## Context

`sb ingest` reads Google Drive files through a Google account (ADR-0014, ADR-0006).
Many organizations keep their documents in SharePoint Online and OneDrive instead.
Their links cannot be ingested today. A SharePoint URL is routed to `web.page`, which
sends no credentials, so it ends with "the page needs a login".

Both services are served by one API, Microsoft Graph (`drive` and `driveItem`
resources), and one identity platform, Microsoft Entra ID (the Microsoft account
platform for work, school and personal accounts). The shape of the problem is the same
as for Google Docs: an OAuth account, a file locator, the original bytes, and
`sb-extract`.

Decisions are needed on:

- the source kind and the account kind;
- how a URL is resolved to a file and what identifies an entry;
- how the user authenticates, and which permissions are requested;
- what is out of scope.

## Decision

1. **One new source kind, `microsoft.file`, and one new account kind, `microsoft`.**
   SharePoint document libraries and OneDrive (work, school and personal) use the same
   Graph calls, so they are one kind. The kind is `file`, not `doc`, because the
   content is whatever the user stored (docx, pdf, xlsx, text, ...), as for `local.file`.

2. **On-demand only (no `sync`), like `google.doc`.** `supports_sync() = false`. One
   locator is one entry. Folders, document libraries, sites and OneDrive roots are not
   ingested (ADR-0014 decision 8). If sync is later added for Google Drive, a
   Microsoft equivalent follows the same specification in a new ADR.

3. **Routing.** `sb ingest` classifies a URL as `microsoft.file` when its host is
   `*.sharepoint.com`, `onedrive.live.com` or `1drv.ms`. This rule comes before the
   generic `web.page` rule. Sovereign clouds (`*.sharepoint.cn`, `*.sharepoint.us`,
   `*.sharepoint.de`) and SharePoint Server on premises are out of scope.

4. **A URL is resolved with the Graph `shares` API.** `GET /shares/{id}/driveItem`
   with `id = "u!" + base64url(url)` (no padding) accepts both sharing links and
   direct document URLs, and returns the drive ID and item ID. Hence `sb` does not need
   to parse the many SharePoint URL forms, and the user's URL is never fetched by `sb`
   itself, so no SSRF guard is needed for it. The access token is sent only to
   `graph.microsoft.com` and `login.microsoftonline.com`.

5. **Natural key** (ADR-0002): `source_id = "<driveId>/<itemId>"`. It survives a rename
   or a move inside the drive. `source_url` is the item's `webUrl`. A copy of a file is
   another item, so it is another entry; a hash duplicate check across Microsoft
   entries is not added (the `local.file` hash check stays local-only).

6. **Content is downloaded as the original file and extracted by `sb-extract`.** Graph
   returns Office files as they are, so no export step exists (unlike Google native
   documents). Supported types are those of `sb-extract`. Legacy `.doc`/`.ppt`, OneNote,
   Loop, whiteboards, SharePoint site pages and folders are `NotApplicable`. The
   download is a redirect to a short-lived pre-authenticated URL; `sb` follows it
   **without** the `Authorization` header and over `https` only. The raw-data rule of
   ADR-0014 applies unchanged (extracted text by default, original with
   `--keep-original`).

7. **Authentication: Entra public client, PKCE, loopback, user-supplied app
   registration.** This mirrors ADR-0006.
   - The user registers an application in their tenant (platform "Mobile and desktop
     applications", redirect URI `http://localhost`, public client flows allowed) and
     passes its Application (client) ID. There is **no client secret**. No application
     ID is built into the binary: the maintainer would otherwise own a multi-tenant
     application, its publisher verification and its abuse handling.
   - The tenant is `organizations` by default and can be set to a tenant ID or domain,
     `consumers` (personal OneDrive) or `common`.
   - Delegated scopes: `openid profile email offline_access Files.Read.All`.
     `Files.Read.All` is the smallest scope that reads files shared with the user in
     SharePoint and OneDrive through `shares`. `Sites.Read.All` (which always needs
     administrator consent) and every write scope are **not** requested.
   - A refresh token is stored as secret `microsoft.refresh_token`. Entra **rotates**
     refresh tokens: every refresh returns a new one, which must be stored before the
     access token is used.
   - The account identity is `<tid>:<oid>` from the ID token; the default label is the
     `preferred_username`.
   - `invalid_grant`, `interaction_required` and `consent_required` mark the account
     `needs_reauth`, the same as for Google. Scheduled runs never open a browser.

8. **Account choice for ingest** follows the Google rule (ingest.md): `--account`, then
   the account of an existing entry with the same key, then each eligible Microsoft
   account in order of creation is probed with the `shares` call.

9. **Administrator consent is outside the software's control and is documented as a
   prerequisite.** Some tenants require an administrator to approve the application
   or to allow user consent for `Files.Read.All`. The error from Entra is shown to the
   user as it is, and the account guide says what to ask the administrator.

10. **A new crate `sb-microsoft`** (package `second-brain-microsoft`, ADR-0018) holds
    the OAuth client, the Graph client and the source. It depends on `sb-kernel` and
    `sb-extract`. The generic parts of the OAuth flow (PKCE, loopback listener,
    opening the browser, token endpoint requests) exist in `sb-google` and are needed
    here. They are moved to a small crate `sb-oauth` first, without changing behavior,
    and both crates use it (see the implementation plan in
    [microsoft-ingest-plan.md](../specs/microsoft-ingest-plan.md)).

## Consequences

- Organizations on Microsoft 365 can use `sb ingest` for their documents. The skill's
  ingest guide must mention that SharePoint and OneDrive links are accepted.
- Every Microsoft user needs an app registration, and possibly administrator
  approval. This is the main adoption barrier, and it is the same trade-off as
  ADR-0006 chose for Google. A shared app registration per organization is the
  expected setup.
- No change to the catalog schema: the account kind is a text column, secrets are
  named values, and entries use the existing tables.
- `AccountKind` and `SourceKind` get one value each. Code that matches on them
  exhaustively must be updated.
- Refresh-token rotation makes a lost write dangerous: if the new token is not stored,
  the old one may be invalid on the next run. The store write is part of the refresh.
- Links that point to site pages, Loop components or OneNote notebooks fail with a
  clear `NotApplicable`. SharePoint URL forms that the `shares` API does not accept
  (for example `Doc.aspx?sourcedoc=...`) fail with a hint to copy a sharing link.
  Spike S-MS1 measures how common this is before implementation.
- Personal OneDrive and `1drv.ms` links work through the same code, but are only tested
  with a recorded API fixture, not against live services, in CI (no real network in
  tests).

## Alternatives considered

- **Reuse the Google account kind with a provider field**: rejected. Authentication,
  token rotation, identity and errors all differ; one adapter per account kind is the
  rule of ADR-0008.
- **Two source kinds, `sharepoint.file` and `onedrive.file`**: rejected. The API,
  the key and the fetch are the same. The difference (`driveType`) is kept in the
  metadata.
- **A built-in multi-tenant application ID** so that users need not register an app:
  rejected for now (see decision 7). It would still need administrator consent in
  most organizations, so the benefit is small. It can be added later without breaking
  accounts.
- **`Sites.Read.All` and site/library enumeration**: needed only for sync, which is out
  of scope, and it always requires an administrator.
- **Device code flow instead of a loopback listener**: friendlier for headless
  machines, but the Google flow has the same limit and `--no-browser` prints the URL
  in both. It can be added later as an option.
- **Fetching the user's URL directly with the access token**: rejected. It would send
  a bearer token to whatever host the locator names. The `shares` API keeps the token
  on Microsoft endpoints.
- **Microsoft Graph SDK crates**: rejected. As in ADR-0006, a thin `reqwest` client for
  three endpoints is enough.

## Amendments

### 2026-11-06: details settled during implementation

- **Key lookup.** The natural key is known only to Graph, so the `Source` trait got
  `locate`, an async `resolve` that may use the network (default: `resolve`). `sb ingest`
  calls it for SharePoint and OneDrive links. As a result, decision 8 changes: there is
  no "account of an existing entry" step, because the key is known only after the first
  account resolves the item. Accounts are probed in order of creation.
- **`sb-oauth`** holds PKCE, the loopback listener (which also listens on `::1` for
  `localhost` redirects), and the token requests. `sb-google` keeps its public API and
  uses it. Errors `interaction_required`, `consent_required` and `login_required` are
  handled like `invalid_grant` (the account needs to sign in again).
- **Config.** A Microsoft account's `config` is `{client_id, tenant, scopes?}`
  (`MicrosoftConfig`). The client ID and tenant are validated (GUID; tenant limited to
  letters, digits, `-` and `.`) because the tenant becomes a URL path segment.
- **Rotation.** The token provider takes a sink that stores the new refresh token through
  a second catalog connection; if the write fails, the refresh fails.
- **Crates.** The workspace has twelve crates. `second-brain-oauth` and
  `second-brain-microsoft` need the bootstrap described in release.md before the release
  that includes them.

### 2026-11-11: decision 2 is superseded in part by ADR-0022

Decision 2 ("On-demand only (no `sync`)") is superseded by
[ADR-0022](0022-folder-sync-and-slack-link-ingest.md): a Microsoft account can have
folders that `sb sync` lists and ingests, and `microsoft.file` links found in Slack
entries can be ingested. The sentence "If sync is later added for Google Drive, a
Microsoft equivalent follows the same specification in a new ADR" is satisfied by that
ADR, which covers both providers. The permission scope is unchanged (`Files.Read.All`,
no `Sites.Read.All`); a folder is named by the user's URL and listed with
`GET /drives/{driveId}/items/{itemId}/children`.
