# Setup, installation and scheduling

Related ADRs: 0009, 0018, 0020.

## Installation (end user)

```sh
cargo install second-brain --locked
```

Cargo places `second-brain` and `sb` in its bin directory (`~/.cargo/bin`), which
rustup puts on the user `PATH` (ADR-0018, [release.md](release.md)). Scheduled jobs
embed the path of the running executable, so they work wherever Cargo installed it.
Then the user runs:

```sh
second-brain setup
```

## `sb setup` screen

`sb setup` opens a terminal UI (ADR-0020, `crates/sb-cli/src/tui/`). It first runs
the `home` step (idempotent), then shows a column of sections and the rows of the
selected one. Sections can be visited in any order and done again; a mark shows
which ones are complete.

| # | Section | Rows and actions |
|---|---|---|
| 1 | Home | The home directory, whether it is the default; for a custom home, persist `SECOND_BRAIN_HOME` (`setup env`, without the confirmation prompt) |
| 2 | Accounts | Google and Slack accounts (id, kind, status, identity). Enter edits the account config, `x` disables or enables. Add a Google account (id, client JSON path or `global`, features, label) or a Slack workspace (id, masked token, label) |
| 3 | Summarizer | Profiles (`llm.profiles.*`), the default marked `*`. Enter makes one the default, `x` removes it (and the default, if it was). Add: pick a preset (as `setup llm`), then name, model, base URL (local only), the API key if the preset needs one that is not stored (masked), make default, test call |
| 4 | Schedule | Daily time, weekly deep day and time, systemd timers (Linux), the registered state; register or remove the jobs |
| 5 | Agent skills | Per target: installed version against the embedded one, and whether the agent CLI is on `PATH`. Enter installs or updates, `x` removes |
| 6 | First sync | `sync.initial_days`; estimate (`sb sync --estimate`); run `sb sync` |
| 7 | Settings | Every setting of `sb config list`, defaults in grey (below) |
| 8 | Secrets | Stored secrets, masked; replace, delete or add one (global or per account) |

Keys: ↑/↓ (or `j`/`k`) move, Tab / Shift+Tab, ←/→ or `1`–`8` change the section,
Enter runs the row's action, `x` / Delete its second action, `r` reloads, `q` /
Esc / Ctrl+C quit. Forms: Tab/↑/↓ move between fields, Space toggles, Enter goes
to the next field or submits; a one-field form submits on Enter; Esc cancels. A
value that fails validation keeps the form open with the error in the status line.
Removals ask for confirmation.

Adding a Google account, the estimate and `sb sync` run outside the screen: the
terminal is restored, the command runs with its usual output, and Enter returns to
the screen. `sb sync` ends the screen and returns its exit code; a Ctrl+C during a
job ends it with exit code 130. Adding a Slack account also runs outside, so the
token check and its warnings are visible. The other actions run inside and report
one line.

Every action is idempotent. Without a terminal on stdin and stdout, or with
`--json`, `sb setup` is a usage error (exit 64).

### `sb setup --yes`

Runs the steps that have defaults, without the screen: `home`, `env` for a custom
home, `schedule` with the default times, and `skills` for each agent CLI found on
`PATH`. Accounts and the summarizer are skipped; use `sb account add` and
`sb setup llm --preset ...`.

### Settings section and `sb config`

`sb config` (no subcommand) and `sb config edit` open the same screen on the
Settings section; `sb config edit --account <id>` opens that account's config.

- Values are edited by shape: booleans toggle on Enter; `display.language`,
  `summary.language`, `notify.min_severity` and `summary.profile.*` pick from a
  list; lists of strings are comma-separated (or a JSON array, for items with a
  comma); strings are taken as typed; other values are JSON.
- Every write goes through the validation of `sb config set`. `x` unsets a value
  (back to the default); a key without a default asks first.
- `backup.last_at`, `skills.installed` and `schedule.registered` are shown read-only.
- The account config lists the source's default keys (grey when not stored) and any
  stored extra keys. `scopes` is read-only. `x` removes a stored key, so the default
  applies (for example the legacy keys of `config.legacy_keys`). Each change is
  checked against the source's config type before it is written.

## Scheduled job definitions

The command line is always:

```text
<abs path to second-brain> --home <abs home> sync [--deep]
```

The trigger is `schedule`, detected from the env var `SB_TRIGGER=schedule` set in
the job definition. Task Scheduler actions cannot set environment variables, so
the Windows task passes the hidden global flag `--trigger schedule` instead.
Scheduled runs also write their log to `logs/<date>.log`.

### Windows (Task Scheduler)

- Tasks `\second-brain\sync` and `\second-brain\sync-deep`, registered via a
  generated task XML and `schtasks /Create /XML <file> /TN <name> /F`.
- Settings:
  - `LogonType = InteractiveToken` (runs while logged on; no stored password);
  - `StartWhenAvailable = true`;
  - `MultipleInstancesPolicy = IgnoreNew`;
  - `ExecutionTimeLimit = PT6H`.
- No console window flashes: the binary is a console app, so the task runs
  `conhost.exe --headless <binary> ...`. Spike S5 verifies this on current
  Windows versions; the fallback is a `windows_subsystem` variant `sbw.exe`.
- The task XML is written in UTF-16 LE with a BOM, as `schtasks /XML` expects.
- The task inherits the user's environment, so `PATH` is not embedded.

### macOS (launchd)

- `~/Library/LaunchAgents/com.github.a-miyashita.second-brain.sync.plist`, plus
  `sync-deep`.
- Uses `StartCalendarInterval`, with `EnvironmentVariables` holding `PATH` and
  `SB_TRIGGER`, and `StandardOutPath`/`StandardErrorPath` pointing to `logs/`.
- Loaded with `launchctl bootstrap gui/<uid> <plist>`.

### Linux

- Default: crontab lines between the markers `# BEGIN second-brain` and
  `# END second-brain`. They are edited through `crontab -l` / `crontab -`, and only
  the block between the markers is replaced.
- `--systemd`: `~/.config/systemd/user/second-brain-sync.{service,timer}` and
  `second-brain-sync-deep.{service,timer}` with `Persistent=true`, enabled with
  `systemctl --user enable --now`.

### PATH for LLM CLIs

At registration, the absolute paths of the configured LLM CLIs (`claude`, `copilot`, `codex`, `agy`)
are resolved, along with the executable of any local LLM profile's `start_command`.
Their directories are added to the job's `PATH`, so they are found in the minimal
cron and launchd environment.

## Environment setup (`sb setup env`)

- Windows: write `HKCU\Environment\SECOND_BRAIN_HOME` and broadcast
  `WM_SETTINGCHANGE`.
- macOS / Linux: print the `export` line and offer to append it to the detected
  shell rc file, inside a marked block.

## Uninstall

`sb setup schedule --remove`, `sb setup skills --remove --target all`, and then
deleting the binaries. The data in the home directory is kept unless the user
deletes it.
