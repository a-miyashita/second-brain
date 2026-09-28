# Setup, installation and scheduling

Related ADR: 0009.

## Installation (end user)

```sh
# macOS / Linux
curl -LsSf https://github.com/<owner>/second-brain/releases/latest/download/second-brain-installer.sh | sh
# Windows (PowerShell)
irm https://github.com/<owner>/second-brain/releases/latest/download/second-brain-installer.ps1 | iex
# Homebrew
brew install <owner>/tap/second-brain
```

The installer places `second-brain` and `sb` in the bin directory from ADR-0009 and
adds that directory to the user `PATH`. Then the user runs:

```sh
second-brain setup
```

## `sb setup` wizard flow

1. **home**
   - Show the resolved home directory and allow changing it.
   - Create the directories and DB, and apply permissions.
   - Create the pseudo-accounts `local` and `web`.
   - If the home is non-default, offer `setup env`.
2. **account**: loop "Add a Google account / Slack account / done". For Slack, print
   the manifest path and the app-creation steps.
3. **llm**
   - Choose the default profile: Anthropic API, OpenAI API, Gemini API, Claude Code
     CLI, Copilot CLI, local (OpenAI-compatible / Foundry Local), or none.
   - Test it.
4. **schedule**: confirm the daily and weekly times, and register the jobs.
5. **skills**: detect which agent CLIs are installed and offer to install the skill
   for each.
6. **first sync**: offer `sb sync --estimate`, then `sb sync`.

Every step is idempotent. `--yes` accepts the defaults for non-interactive use.

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

At registration, the absolute paths of the configured LLM CLIs (`claude`, `copilot`)
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
