# ADR-0006: Native Google OAuth and REST clients (no rclone)

- Status: Accepted
- Date: 2026-09-29

## Context

second-brain reads several Google sources: Drive / Docs export, Calendar, possibly
the Meet REST API, and later Gmail. It must support multiple Google accounts
(ADR-0007).

A common shortcut is to reuse a sync tool such as rclone for Drive and a separate
OAuth client for everything else. That means two authorizations to maintain, and an
external binary that cannot cover Calendar, Meet or Gmail.

Organizations using Google Workspace can configure the OAuth consent screen as
**Internal**, so refresh tokens do not expire on a 7-day cycle. The program must
still work for consumer accounts and "External / Testing" clients, whose refresh
tokens expire after 7 days.

## Decision

- Implement the OAuth 2.0 installed-app flow in Rust:
  - authorization code with PKCE;
  - a loopback redirect on `127.0.0.1` with an ephemeral port;
  - the system browser opened with the `open`/`webbrowser` crate;
  - a fallback that prints the URL for headless machines.
- Use a **user-supplied OAuth client** (Desktop app type) per Google account or
  shared across accounts. The client JSON is imported with
  `sb account add google --client-secret <file>` and stored in the catalog. The
  setup docs explain how to create one; a Workspace admin can create one for the
  whole organization.
- Request scopes incrementally, only for enabled features:

  | Feature | Scope |
  |---|---|
  | Drive export / Docs | `drive.readonly` |
  | Meet via Calendar attachments | `calendar.readonly` (events + attachments) |
  | Meet REST API (if adopted) | `meetings.space.readonly` |
  | Gmail (later) | `gmail.readonly` |

  Adding a feature that needs a new scope triggers re-consent via
  `sb auth login <account>`.
- Refresh tokens are stored as secrets (ADR-0003). Access tokens are refreshed
  automatically.
- Scheduled runs **never open a browser**. An `invalid_grant` or revoked token
  marks the account `needs_reauth`, records an issue for `sb doctor`, and
  processing continues with the other accounts.
- The **re-authentication command** exists for all account kinds:
  - `sb auth login <account>` re-runs consent;
  - `sb auth status` shows validity and expiry for every account.
- Thin REST clients on `reqwest` cover Drive v3 (files.list/get/export), Calendar v3
  (events.list), Meet v2 (conferenceRecords, smartNotes, transcripts) and later
  Gmail v1. There is no generated SDK.

## Consequences

- One consent per Google account covers every Google source. No external sync
  tool is needed.
- With a Workspace "Internal" client, re-authentication is rare. With a
  "Testing" client it is a weekly chore, but `doctor` makes it visible.
- We maintain small hand-written API clients. Fixtures from real responses are
  kept under test data (with personal data scrubbed).

## Alternatives considered

- **rclone for Drive**: a second auth system and an external binary; it
  cannot cover Calendar, Meet or Gmail anyway.
- **`yup-oauth2` / `google-apis-rs`**: viable, but they bring large generated
  crates for a handful of endpoints. They may be used for the OAuth part if the
  in-house flow proves costly; that is an implementation detail.
