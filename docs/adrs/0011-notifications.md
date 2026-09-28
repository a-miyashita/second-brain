# ADR-0011: Notifications — pluggable sinks, `doctor` as the baseline

- Status: Accepted
- Date: 2026-09-29

## Context

Unattended runs can fail: expired or revoked auth, rate limits, LLM errors, disk
problems. Log files alone are not enough, because nobody reads them until
something is visibly missing.

## Decision

- Every problem found during a run is recorded as an **issue** in the catalog
  (`issues` table). Each issue has a severity, a stable code (e.g.
  `auth.needs_reauth`), an account and a message. An issue is resolved
  automatically when the condition clears.
- `sb doctor` is the baseline channel. It always shows open issues and health
  checks. With no notification settings, this is the only channel.
- A `Notifier` trait allows additional sinks, chosen by the setting
  `notify.sinks` (a list, so several can be selected):

  | Sink | Behaviour | Phase |
  |---|---|---|
  | `doctor` | Always on; issues are visible via `sb doctor` | MVP |
  | `slack_dm` | DM to yourself from a configured Slack account (`notify.slack_dm.account`). This is the suggested default once a Slack account exists | Later |
  | `desktop` | OS notification (Windows toast / macOS / libnotify) | Later |

- Sinks are called for new issues at or above `notify.min_severity` (default
  `error`), once per issue rather than on every run.

## Consequences

- The MVP only needs the `issues` table and `sb doctor`. Adding sinks later does not
  change how issues are produced.
- Sending a Slack DM needs `chat:write` in the Slack app manifest. It is added when
  the `slack_dm` sink is implemented, and users re-install the app then.
