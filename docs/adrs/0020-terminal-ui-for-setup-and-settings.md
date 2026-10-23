# ADR-0020: Terminal UI for setup and settings

- Status: Accepted
- Date: 2026-10-21

## Context

`sb setup` was a line-by-line wizard: numbered menus and `[default]` prompts on
stderr, in a fixed order. To change one answer the user ran the whole wizard again
or switched to the step commands. `sb config edit` opened settings or an account's
config as TOML in `$EDITOR`. That needs an editor the user knows, shows only the
values that differ from the defaults, and reports a validation error only after the
editor closes, so the edit is lost.

The maintainer wants a screen that works like a settings window, inside the
terminal, and wants to keep the command mode for scripts and agents.

## Decision

### 1. A terminal UI built with ratatui

- The screen uses `ratatui` with its `crossterm` backend (pure Rust; Windows
  console, macOS and Linux terminals). No OpenSSL and no system library, so
  `cargo install second-brain` stays the only installation step (ADR-0018).
- It lives in `sb-cli` (`src/tui/`), the only crate that may write to the terminal.
  State and key handling (`app.rs`) are separate from drawing (`view.rs`), so tests
  drive keys against a temporary home and draw with ratatui's test backend.
- The screen reuses the command code: settings are checked by the same validation
  as `sb config set`, account configs by the source's config type, and the setup
  steps call the same functions as `sb setup schedule|skills|llm|env`.

### 2. `sb setup` opens the screen

- `sb setup` with no step opens the screen: a column of sections (home, accounts,
  summarizer, schedule, agent skills, first sync, settings, secrets) and the rows of
  the selected one. Every section can be visited in any order and done again.
- The step commands (`sb setup home|llm|schedule|skills|env`) stay as they are, for
  scripts. `sb setup --yes` runs the steps that have defaults without the screen,
  as before.
- Without a terminal on stdin and stdout, or with `--json`, `sb setup` fails with a
  usage error that names `--yes` and the steps. The old wizard ran its
  non-interactive branch silently.

### 3. `sb config` keeps its commands and gains the screen

- `sb config list|get|set|unset|set-secret` do not change.
- `sb config` without a subcommand opens the screen on the settings section.
- `sb config edit` opens the screen instead of `$EDITOR`; `--account <id>` opens that
  account's config. `sb config edit --editor` keeps the TOML-in-`$EDITOR` flow for
  bulk edits and nested values.

### 4. Work that needs the plain terminal leaves the screen

The Google consent prints a URL and waits for the browser; `sb sync` prints
progress and stops on Ctrl+C. For these the screen restores the terminal, runs the
existing command, waits for Enter and comes back. Running `sb sync` ends the
screen, because a Ctrl+C there cancels the process-wide stop token. Short work (a
setting, a skill install, the scheduler registration, a summarizer test call) runs
inside the screen and reports one line.

### 5. Secrets

Secret fields are masked on screen. Values go straight to the catalog
(`Secret`), never into a command line or a log.

### 6. `sb stats` is unchanged

`sb stats` is a report that agents read with `--json`, not a setting. It keeps its
current output. A read-only dashboard can be a later decision.

## Consequences

- New dependencies: `ratatui` and `crossterm` (MSRV 1.88, same as the workspace).
- `sb setup` in a pipe or a CI job now fails instead of doing a partial run; such
  uses move to `sb setup --yes` or the step commands.
- Labels on the screen are English, like the CLI messages; localization stays a
  display-time concern (`display.language`) for entry sections only.
- The screen cannot be tested end to end without a terminal. Tests cover the state
  machine and drawing; the terminal handling (raw mode, alternate screen, leaving
  and coming back) is small and checked by hand.

## Alternatives considered

- **Prompt library (`inquire`, `dialoguer`)**: smaller, but still a linear wizard;
  it does not give a settings window that can be revisited.
- **Local web UI**: a real GUI, but it needs an HTTP server on localhost that can
  read and write credentials, with its own authentication. Too much attack surface
  for a settings screen.
- **Keep `$EDITOR` for `sb config edit`**: kept only behind `--editor`, for the
  reasons in the context.
