# Release and installation

Decisions: [ADR-0018](../adrs/0018-crates-io-distribution-via-cargo-install.md)
(distribution, crate names) and
[ADR-0019](../adrs/0019-tag-triggered-release-and-changelog.md) (release process).

## Supported installation

```sh
cargo install second-brain --locked
```

- Installs `second-brain` and `sb` into Cargo's bin directory (`~/.cargo/bin`, or
  `%USERPROFILE%\.cargo\bin` on Windows). rustup normally puts that directory on
  `PATH`.
- Requirements: Rust 1.88 or later, and a C compiler (`rusqlite` is built with
  `bundled`). On Windows, the MSVC build tools. On macOS, the Xcode command line
  tools. On Linux, `cc` (for example `build-essential`).
- `--locked` makes Cargo use the `Cargo.lock` published with the crate, so the build
  uses the dependency versions that CI tested.
- Update: `cargo install second-brain --locked --force`. Then run `sb doctor`. It
  reports an installed skill whose version differs from the program.
- Uninstall: `cargo uninstall second-brain`. Data is not removed (see the
  maintenance chapter of the manual).
- Unreleased commits: `cargo install --git https://github.com/a-miyashita/second-brain second-brain --locked`.

Other installation methods are deferred (ADR-0018). Documents must not mention
installer scripts or Homebrew until they exist.

## Crates

| Directory | Package | Role | Public? |
|---|---|---|---|
| `crates/sb-cli` | `second-brain` | Binaries `second-brain` and `sb` | Yes, the only entry point |
| `crates/sb-kernel` | `second-brain-kernel` | Domain types and traits (no I/O) | Internal |
| `crates/sb-store` | `second-brain-store` | Catalog, raw files, secrets, FTS | Internal |
| `crates/sb-pipeline` | `second-brain-pipeline` | Ingestion and summarization pipeline | Internal |
| `crates/sb-llm` | `second-brain-llm` | Summarizers | Internal |
| `crates/sb-google` | `second-brain-google` | Google OAuth, Meet and Docs | Internal |
| `crates/sb-slack` | `second-brain-slack` | Slack source | Internal |
| `crates/sb-extract` | `second-brain-extract` | Document text extraction | Internal |
| `crates/sb-ondemand` | `second-brain-ondemand` | `web.page`, `local.file` | Internal |
| `crates/sb-setup` | `second-brain-setup` | Setup, scheduler, skill install | Internal |

"Internal" means published only because `second-brain` depends on it. It has no API
stability promise. Its `description` and `readme` say so.

## Manifest rules

- `[workspace.package]` holds `version`, `edition`, `rust-version`, `license`,
  `repository`, `authors`. Every crate inherits them with `.workspace = true`.
- Every crate sets `description`. Add `readme`, `keywords`, `categories`,
  `homepage` and `documentation` in the workspace table when they apply to all.
  - `second-brain` sets `keywords` (at most 5) and `categories`
    (`command-line-utilities`).
  - Internal crates share one short README text that points to `second-brain`.
    They inherit it from `workspace.package.readme` (`crates/INTERNAL.md`).
  - Every crate directory holds a copy of the root `LICENSE`, so the package contains
    the license text. `release.mjs check` verifies that the copies are identical.
- Internal dependencies are declared once in `[workspace.dependencies]`:

  ```toml
  second-brain-store = { path = "crates/sb-store", version = "=0.1.0" }
  ```

  The `version` must equal `workspace.package.version`. The release checklist
  updates both. A CI check compares them.
- A crate embeds only files inside its own directory (`include_str!`,
  `include_bytes!`). The skill files are in `crates/sb-setup/assets/skills/`.
- Add `exclude` where a crate holds files that a user does not need (test
  fixtures that are large, scripts). Package size must stay well under the
  crates.io limit of 10 MiB per crate.
- The lockfile is committed. A binary crate is published with its `Cargo.lock`.

## Runtime strings

- The `extractor` field written into raw bundles (`"sb-extract <version>"`) keeps
  its value. It is provenance data, not a crate reference, and old and new entries
  must stay comparable.
- Version output (`sb version`, `--json` `schema` fields) does not change.

## CI

Added to `.github/workflows/ci.yml`. The workflow also gets a `workflow_call`
trigger, so the release workflow can reuse it.

| Job | Command | Purpose |
|---|---|---|
| `package` | `cargo publish --workspace --dry-run --locked` on stable | Every crate packages and builds from its own tarball |
| `release-script` | `node --test scripts/release.test.mjs` (Linux, Windows) and `node scripts/release.mjs check` (Linux) | The release script works, and the working tree is consistent (versions, pins, license copies, lock file) |

The `msrv` job (Rust 1.88) stays. `ci.yml` never publishes.

## Changelog

Rules are in ADR-0019 §4. In short: `CHANGELOG.md` at the repository root, `##
Unreleased` on top, then `## X.Y.Z - YYYY-MM-DD` sections, newest first, with the
groups `Added`, `Changed`, `Deprecated`, `Removed`, `Fixed`, `Security`.

Example (illustrative; the version, command names and schema are made up):

```markdown
# Changelog

## Unreleased

## 0.2.0 - 2026-11-03

### Added

- `sb ingest` accepts local PDF files.

### Changed

- **Breaking:** `sb stats --json` renames `by_model` to `by_generator`
  (`schema` is now `sb.stats/v2`).
- Catalog migration 0004 adds the `llm_usage.generator_id` column. The migration
  runs on the next command; back up the catalog first if it is large.

### Fixed

- `sb sync` no longer stops when a Slack channel was archived.
```

## `scripts/release.mjs`

Node.js (ESM), no dependencies, tests in `scripts/release.test.mjs` run with
`node --test scripts/release.test.mjs`. It reads the manifests with plain text processing (the layout of
`Cargo.toml` is fixed by the manifest rules above) and asks Cargo for the crate list
and dependency order (`cargo metadata --no-deps --format-version 1`).

| Command | Behavior | Exit code |
|---|---|---|
| `bump <version>` | Validates SemVer. The version must not be lower than the current one; the current version is allowed while its changelog section does not exist yet (first release). Requires a non-empty `## Unreleased`. Sets `workspace.package.version` and every internal pin. Runs `cargo update --workspace`. Renames `## Unreleased` to `## <version> - <UTC date>` and inserts an empty `## Unreleased` | 0, or 1 with a message |
| `check` | Working-tree checks: version syntax; all internal pins equal the workspace version; every crate inherits the version; every crate holds a copy of the root `LICENSE`; `Cargo.lock` is current (`cargo update --workspace --locked`) | 0 / 1 |
| `check --tag vX.Y.Z` | The above, plus: tag equals `v` + version; the changelog section exists, is dated and non-empty; `HEAD` is on `origin/main` (an ancestor of it) and the tag, if it exists, points to `HEAD`; the version is greater than every earlier `v*` tag | 0 / 1 |
| `notes <version>` | Prints the body of the changelog section (without its heading) | 0 / 1 |
| `publish [--dry-run]` | For each crate in dependency order (from `cargo metadata`, dev-dependencies on workspace crates included, because a versioned dev-dependency stays in the published manifest): query the crates.io sparse index; skip if `name@version` exists; else `cargo publish -p <name> --locked` (Cargo waits until the crate is in the index). `--dry-run` reports which crates are already published and runs one `cargo publish --workspace --dry-run --locked` | 0 / 1 |
| `version` | Prints the workspace version (for the workflow and the skill) | 0 |

Messages go to stderr; machine-readable output (`notes`, `version`) goes to stdout.
The script never calls `git push`, `git tag` or any GitHub API.

## Release workflow

File: `.github/workflows/release.yml`. Triggers: `push` of tags `v[0-9]*.[0-9]*.[0-9]*`
(including `-rc.1` style suffixes) and `workflow_dispatch` (dry run). Default
permissions: `contents: read`. `concurrency: release`, without cancellation.

| Job | Needs | Permissions / environment | Steps |
|---|---|---|---|
| `verify` | — | read | Checkout with full history and tags. `node scripts/release.mjs check --tag "$GITHUB_REF_NAME"`. On manual runs: `check` without `--tag`, then `publish --dry-run` |
| `test` | `verify` | read | `uses: ./.github/workflows/ci.yml` |
| `publish` | `verify`, `test` | `id-token: write`; environment `release` | Get a crates.io token with `rust-lang/crates-io-auth-action` (pinned by SHA). `node scripts/release.mjs publish`. Skipped on manual runs |
| `smoke` | `publish` | read; matrix `ubuntu-latest`, `macos-latest`, `windows-latest` | `cargo install second-brain --version "$VERSION" --locked` with retries (index delay), then `sb version` and `second-brain version` |
| `github-release` | `publish` | `contents: write` | `gh release create "$TAG" --verify-tag --title "$TAG" --notes-file <(node scripts/release.mjs notes "$VERSION")`, with `--prerelease` when the version has a pre-release part |

Manual runs never publish and never create a release.

### GitHub and crates.io settings

These are not in the repository. Set them up before the first release, and keep this
list current.

| Where | Setting |
|---|---|
| GitHub: environment `release` | Required reviewer: the maintainer. Deployment tags: `v*` only. Self-review allowed while there is one maintainer |
| GitHub: ruleset for tags `v*` | Only the maintainer can create tags; tags cannot be deleted or moved by others |
| GitHub: ruleset for `main` | Pull request required (the release commit goes through a pull request) |
| crates.io: each of the ten crates | Trusted publisher: repository `a-miyashita/second-brain`, workflow `release.yml`, environment `release` |
| crates.io: account | A token for the bootstrap only (scope `publish-new`, `publish-update`); revoke it after the first release |

## Release procedure

Normal release, with the release skill (`.claude/skills/release/SKILL.md`):

1. **Prepare.** The maintainer asks the skill to prepare a release. The skill checks
   that the tree is clean, on an up-to-date `main`, and that CI is green. It lists
   the commits and merged pull requests since the last tag, drafts the `Unreleased`
   entries, and proposes a version (ADR-0019 §4). The maintainer edits both.
2. The skill creates `release/vX.Y.Z`, runs `node scripts/release.mjs bump X.Y.Z`,
   then `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D
   warnings`, `cargo test --workspace` and `node scripts/release.mjs publish
   --dry-run`. It commits `chore: release vX.Y.Z`, pushes the branch and opens a pull
   request.
3. The maintainer reviews the pull request (especially `CHANGELOG.md`) and merges it.
4. **Tag.** The maintainer asks the skill to tag. The skill updates `main`, runs
   `node scripts/release.mjs check --tag vX.Y.Z`, and shows the version, the
   changelog section and the commit. **(confirm)** After approval it creates the
   annotated tag and pushes it.
5. The workflow runs `verify` and `test`, then waits at the `release` environment.
   **(confirm)** The maintainer approves it in GitHub.
6. The workflow publishes, runs the install smoke test and creates the GitHub Release.
   The skill (or the maintainer) checks the run result.

Without the skill, the same steps work by hand: edit `Unreleased`, run `bump`, make
the pull request, tag, push the tag.

### First release (`v0.1.0`) — bootstrap

Trusted publishing needs the crates to exist, so the first release is partly manual.

1. Re-check that all ten package names are free (ADR-0018). Stop if one is taken.
2. Decide whether the repository becomes public before the release, or confirm
   that trusted publishing works for the private repository.
3. Prepare the release pull request as usual (steps 1–3 above) and merge it.
4. Create a crates.io token, and run
   `CARGO_REGISTRY_TOKEN=... node scripts/release.mjs publish` from the merged `main`.
   **(confirm)**
5. On crates.io, add the trusted publisher to every crate. Revoke the token.
6. Create the GitHub settings from the table above.
7. Push the tag `v0.1.0`. The workflow finds all crates published, skips publishing
   (still waiting for the approval), runs the smoke test and creates the GitHub
   Release. This also tests the workflow end to end.
8. Remove the "first release is not published yet" note from
   `manuals/en/installation.md`, in a follow-up pull request.

### When something fails

See ADR-0019 §7. In short: before anything is published, delete the tag, fix and
tag again. After any crate is published, never move the tag. Re-run the failed job,
or yank and release a new patch version.

## Documents that mention installation

Keep these consistent with this spec:

- `README.md` (install section and repository layout)
- `manuals/en/installation.md` (including the prerequisites and the update steps)
- `docs/specs/setup-and-scheduling.md` (the installer paragraph)
- `docs/specs/architecture.md` (crate names and layout)
- `AGENTS.md` (repository layout, location of the skill files, a pointer to the release skill)
- `CHANGELOG.md` (created in the first release pull request)
