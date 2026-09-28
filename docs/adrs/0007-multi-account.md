# ADR-0007: Multiple accounts for every source

- Status: Accepted
- Date: 2026-09-29

## Context

Some intended users work across several Slack workspaces. Some also have several
Google accounts, such as a company Workspace and a client's Workspace. Assuming
exactly one account per service would exclude them.

## Decision

- An **account** is a first-class record: an ID, a kind, a label, an identity and
  its own configuration and secrets.
  - Kinds: `google`, `slack`, `imap` (later), and the pseudo-accounts `local` and
    `web` for sources without authentication.
  - The ID is a short slug chosen by the user, for example `work-google` or
    `acme-slack`. It is unique and immutable.
  - The identity is the verified remote identity, for example the Google account
    email or the Slack `team_id` plus `user_id`.
- Every entry belongs to exactly one account. The natural key is
  `(account_id, source_kind, source_id)` (ADR-0002).
- Source settings, such as Slack `full_channels`, are stored per account.
- Sync iterates over enabled accounts. A failure in one account (auth, rate limit)
  is recorded and does not stop the others.
- CLI and MCP filters accept `--account <id>` (repeatable). Search results always
  include the account ID and label.
- The same underlying item reached through two accounts is two entries. For
  example, a Google Doc shared with both of your Google accounts. This is
  deliberate: access rights and provenance differ.

## Consequences

- The schema and every source adapter take an account context from day one.
  Retrofitting this later would touch everything.
- Notification targets (ADR-0011) reference an account, such as the Slack account
  to DM from.
