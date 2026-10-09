# ADR-0019: Tag-triggered release, changelog and release skill

- Status: Proposed
- Date: 2026-10-09

## Context

[ADR-0018](0018-crates-io-distribution-via-cargo-install.md) distributes second-brain
through crates.io. It publishes ten crates in lockstep. A manual release means
editing the version in eleven places, publishing ten crates in order, tagging and
writing release notes. Each step is easy to get wrong, and a wrong publish cannot be
undone.

The maintainer wants this flow:

1. Update the version in `Cargo.toml` and merge it.
2. Push a `vX.Y.Z` tag. A GitHub Actions workflow releases from the tag, and fails if
   the tag and `Cargo.toml` disagree.
3. A skill automates steps 1 and 2.

That leaves open the changelog, the safety of publishing from CI, and how to recover
from a partial failure.

## Decision

### 1. The tag is the trigger; `Cargo.toml` is the source of truth

- The version lives in `[workspace.package].version` and in the `version` pins of the
  internal dependencies (ADR-0018). A release commit changes all of them together,
  through the script in §5, never by hand.
- A push of a tag that matches `v<semver>` starts `.github/workflows/release.yml`.
  The tag must point to a commit that is on `main`.
- The workflow fails, before it changes anything outside the repository, when:
  - the tag is not exactly `v` + the workspace version;
  - an internal `version` pin differs from the workspace version;
  - `CHANGELOG.md` has no dated section for this version, or the section is empty;
  - the tag commit is not reachable from `main`;
  - the version is not greater than the latest earlier release tag;
  - `Cargo.lock` is not up to date with the manifests.
- A version with a pre-release part (`0.2.0-rc.1`) is allowed. It is published to
  crates.io as a pre-release and marked as a pre-release on GitHub.

### 2. Workflow stages

| Stage | Runs | Purpose |
|---|---|---|
| `verify` | the checks of §1 | Stops a bad tag early |
| `test` | the full CI workflow, reused (`workflow_call`) | The tagged commit passes fmt, clippy, tests on three OSes, MSRV and the packaging dry run |
| `publish` | needs `verify` and `test`; uses the `release` environment | Publishes the crates |
| `smoke` | needs `publish`; Linux, macOS, Windows | `cargo install second-brain --version X.Y.Z --locked`, then `sb version` |
| `github-release` | needs `publish` | Creates the GitHub Release with the notes from `CHANGELOG.md` |

- A manual run (`workflow_dispatch`) runs `verify` and a publish dry run only. It
  is how the workflow is tested without a tag.
- `publish` is idempotent. The script publishes the crates one by one in dependency
  order and skips any crate whose `name@version` is already on crates.io. A re-run
  after a partial failure continues where it stopped. The workflow does not depend
  on how `cargo publish --workspace` treats versions that already exist.
- Runs of the workflow do not overlap (a `concurrency` group, no cancellation).

### 3. Publishing from CI is gated by a confirmation

ADR-0018 required a human to confirm every publish. That stays true, in a different
form: the `publish` job uses a GitHub **environment** named `release` with a
required reviewer (the maintainer). The run waits there until the reviewer approves.
Pushing a tag alone never publishes. The environment accepts only `v*` tags.

Authentication:

- crates.io **trusted publishing** (OIDC). `rust-lang/crates-io-auth-action` gets a
  short-lived token. No long-lived crates.io token is stored in GitHub. Only the
  `publish` job has `id-token: write`; only `github-release` has `contents: write`.
- Trusted publishing is set up per crate, so the crate must already exist. The first
  release (`v0.1.0`) is therefore **bootstrapped by hand**: the maintainer publishes
  the ten crates with a personal token, configures a trusted publisher on every
  crate, and then pushes the tag. The workflow finds every crate already
  published, skips publishing, and creates the GitHub Release. From the next release
  on, the workflow publishes.
- Third-party actions that run in the `publish` job are pinned to a commit SHA.

### 4. The changelog is `CHANGELOG.md`, curated at release time

- File: `CHANGELOG.md` at the repository root, in the style of Keep a Changelog:
  a top `## Unreleased` section, then `## X.Y.Z - YYYY-MM-DD` sections, newest
  first. Groups inside a section: `Added`, `Changed`, `Deprecated`, `Removed`,
  `Fixed`, `Security`. Empty groups are left out.
- The audience is a user who runs `cargo install`. An entry describes what changed
  for them. Entries about the CLI, `--json` output (schema rules in `AGENTS.md`),
  configuration, the data directory, and **catalog migrations** are always listed,
  and a breaking change is marked `**Breaking:**`. Pure `docs:`, `test:`, `chore:`
  and `refactor:` commits are left out unless a user can notice them.
- The text is **drafted at release time** from the Conventional Commit subjects and
  merged pull request titles since the previous tag, by the release skill (§6). The
  maintainer reviews and edits it in the release pull request. It is not generated
  without review, and contributors do not have to edit it in every pull request.
- The same section becomes the body of the GitHub Release. `release.mjs notes`
  extracts it, so the workflow and `CHANGELOG.md` cannot differ.
- Version choice, proposed by the skill and confirmed by the maintainer:
  - a breaking change: new minor version while the major version is 0, new major
    version from 1.0.0;
  - otherwise, `feat` commits: new minor version;
  - otherwise: new patch version.

### 5. One script holds the release logic

`scripts/release.mjs` (Node.js, no dependencies, tested with `node --test`) is the
only code that reads or edits version numbers and `CHANGELOG.md`. The workflow, the
skill and a human at a terminal all call it, so they cannot disagree.

| Command | What it does |
|---|---|
| `bump <version>` | Sets the workspace version and all internal pins, runs `cargo update --workspace`, and turns `## Unreleased` into `## <version> - <today>` with a new empty `## Unreleased` above it |
| `check [--tag vX.Y.Z]` | Runs the checks of §1. Without `--tag` it checks the working tree for a release commit about to be made |
| `notes <version>` | Prints the changelog section of that version |
| `publish [--dry-run]` | Publishes the crates in dependency order, skipping those already on crates.io |

### 6. The release skill

A repository skill, `.claude/skills/release/SKILL.md`, runs the release in two
phases. It is for people working on this repository; it is not the user-facing
skill in `assets/skills/second-brain/`.

- **Prepare:** check that the tree is clean, on `main`, up to date, with a green CI;
  collect the changes since the last tag; draft `CHANGELOG.md` entries and propose a
  version; let the maintainer edit both; create `release/vX.Y.Z`; run
  `release.mjs bump` and the local checks (fmt, clippy, test, `publish --dry-run`);
  commit `chore: release vX.Y.Z`; push the branch and open the pull request.
- **Tag:** after the pull request is merged, update `main`, run
  `release.mjs check --tag`, show a summary, **ask for confirmation**, create an
  annotated tag and push it. Then follow the workflow run and remind the maintainer
  to approve the `release` environment.
- The skill never publishes, never edits version numbers by hand, never moves or
  force-pushes a tag, and never pushes to `main`.

### 7. Failure handling

| Situation | Action |
|---|---|
| `verify` or `test` fails; nothing is published | Fix on `main` through a pull request. Delete the tag (local and remote) and tag the fixed commit |
| `publish` fails after some crates are published | Re-run the failed job. It skips the published crates. If the cause is in the code, yank the crates of that version and release the next patch version |
| `github-release` fails | Re-run the job |
| `smoke` fails | The crates are already published. Investigate; yank and release a patch version if the release is unusable |
| Any crate of the version is published | The tag is never moved or reused. Fix forward with a new version |

## Consequences

- A release is a pull request and a tag push, plus one approval click. The
  maintainer cannot publish a version that does not match its tag, or one without
  release notes.
- The first release needs a manual bootstrap. The release spec has the checklist.
- The `release` environment, the `v*` tag rule and the trusted publishers are
  GitHub and crates.io settings. They are not in the repository, so the spec lists
  them and the first release checks them.
- The changelog depends on the quality of commit subjects. The repository already
  requires conventional prefixes (`AGENTS.md`). A bad draft is caught in the release
  pull request.
- `scripts/release.mjs` becomes code that must be kept correct, which is why it has
  tests that run in CI.
- The repository is private on 2026-10-09. Before the first release the maintainer
  checks that crates.io trusted publishing works for it, or makes it public.

## Alternatives considered

- **release-plz or cargo-release.** They automate version bumps, changelogs and
  publishing, but they add a tool with its own configuration and opinions about
  workspaces. The flow here is small, and its core rule (tag must equal
  `Cargo.toml`) is easy to state in 60 lines of Node.js.
- **git-cliff for the changelog.** It is deterministic, but its output is a list of
  commit subjects. The maintainer wants a curated user-facing text, and the skill
  already reads the same commits.
- **GitHub generated release notes only.** There is no file in the repository, so the
  notes are not reviewed in a pull request and are lost if the repository moves.
- **Edit `CHANGELOG.md` in every pull request.** It gives the best detail, but it
  causes merge conflicts and forgotten entries. It can be added later if drafting at
  release time proves too weak.
- **A long-lived `CARGO_REGISTRY_TOKEN` secret.** It works for the first release,
  but a leaked secret can publish any version. Trusted publishing avoids that.
- **Publish on every merge to `main`.** It removes the tag, but it removes the
  control point and the confirmation as well.
