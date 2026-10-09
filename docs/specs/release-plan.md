# Implementation plan: crates.io release via `cargo install`

Implements [ADR-0018](../adrs/0018-crates-io-distribution-via-cargo-install.md),
[ADR-0019](../adrs/0019-tag-triggered-release-and-changelog.md) and
[release.md](release.md). **Delete this file when all tasks are done.**

Each task is one commit (conventional prefix). Run `cargo fmt`, `cargo clippy` and
`cargo test` after every task; the tree must build after every commit. Work on a
branch and open a pull request per part.

## Scope

- Part A: make the workspace publishable (rename, move assets, manifests, CI).
- Part B: release automation (script, workflow, changelog, skill).
- Not in scope: the actual publishing of `v0.1.0` (a separate, confirmed step that
  follows "First release" in release.md), cargo-dist, any behavior change.

## Part A — publishable workspace

| # | Task | Notes | Depends on |
|---|---|---|---|
| A1 | Approve ADR-0018 and ADR-0019 (`Status: Accepted`); add the ADR-0009 Amendment (distribution superseded by ADR-0018) | CLAUDE.md: approve ADRs at the start of implementation | — |
| A2 | Move the skill files to `crates/sb-setup/assets/skills/second-brain/` and fix the three `include_str!` paths | `git mv` to keep history. No symbolic links | A1 |
| A3 | Rename directory `crates/sb-core` to `crates/sb-kernel` and the package `sb-core` to `second-brain-kernel` (identifier `second_brain_kernel`) | About 62 files and 376 uses of `sb_*` identifiers. Use a scripted replacement (Node.js), then `cargo build` | A1 |
| A4 | Rename the other nine packages to `second-brain-<role>` / `second-brain` and replace the `sb_<role>` identifiers | Directories stay. Update `[workspace.dependencies]` keys and every `[dependencies]`. `default-run`, `[[bin]]` names and `[package.metadata.dist]` stay valid | A3 |
| A5 | Add `version = "=0.1.0"` to every internal entry of `[workspace.dependencies]` | Verify with `cargo metadata` | A4 |
| A6 | Complete manifest metadata: `readme`, `keywords`, `categories`, `homepage`, `documentation`, `exclude`; add the shared internal-crate README text | `second-brain` gets its own README. Check `cargo package --list` and the size | A5 |
| A7 | Add the CI job `package` and the `workflow_call` trigger to `ci.yml` | `release-script` job comes with B1 | A5, A6 |
| A8 | Update docs: `README.md`, `manuals/en/installation.md`, `AGENTS.md`, `architecture.md`, `setup-and-scheduling.md`, `agent-integration.md`, `mvp-plan.md` (names only), `docs/README.md` | Remove the installer-script and Homebrew text. Add prerequisites, update and uninstall steps. Do not edit other ADRs | A4 |
| A9 | Dry run: `cargo publish --workspace --dry-run --locked`, plus the smoke test `cargo install --path crates/sb-cli --locked --root <tmp>` followed by `sb version`, `sb setup home`, `sb doctor`, `sb setup skills` with a temporary home | Fix any packaging error. Record the result in the pull request | A2–A7 |

## Part B — release automation

| # | Task | Notes | Depends on |
|---|---|---|---|
| B1 | Write `scripts/release.mjs` and `scripts/release.test.mjs` (`bump`, `check`, `check --tag`, `notes`, `publish`, `version`); add the `release-script` CI job | Tests use temporary copies of fixture manifests and a fake `cargo`/index lookup, no network. `publish` takes the index lookup and the `cargo` command as injectable functions | A5 |
| B2 | Create `CHANGELOG.md` with `## Unreleased` and the entries for the initial release | Initial release entry: one short "Initial release" description with the main features, not a commit list | B1 |
| B3 | Write `.github/workflows/release.yml` (jobs of release.md) | Pin third-party actions by SHA. Check with `actionlint` if available. Test with `workflow_dispatch` on the branch | B1, A7 |
| B4 | Write the release skill `.claude/skills/release/SKILL.md` | Contents from ADR-0019 §6 and release.md "Release procedure". Add a pointer in `AGENTS.md` ("Releases") | B1, B2 |
| B5 | Document the GitHub and crates.io settings as a checklist in the pull request, and configure the GitHub side (environment, rulesets) | Maintainer action; Claude cannot do it | B3 |
| B6 | Delete this plan | Per CLAUDE.md | all |

Publishing `v0.1.0` is a separate step after this plan, run by the maintainer with
"First release" in release.md.

## Risks and checks

- **Name taken before the first publish.** The names were free on 2026-10-09, and
  `second-brain-core` was not. Re-check right before the bootstrap. If a name is taken,
  choose another and add an Amendment to ADR-0018.
- **Missing files in a package.** Only the dry run in A9 proves that each crate
  builds from its own tarball. Do not skip it.
- **Test fixtures and `tests/` paths.** `sb-ondemand/tests` reads files; check that
  nothing reads a path outside the crate, and that `exclude` does not drop test data.
- **Scripted rename misses.** A search for `sb-core`, `sb_core`, `sb-cli` and the other
  old names, outside `docs/adrs/`, must return only intended hits (the `EXTRACTOR`
  string and the `sb` binary name).
- **`Cargo.lock` churn.** Renaming packages rewrites lock entries. Review that no
  external dependency version changed.
- **Windows.** Cross-platform paths in the script (`path.join`, no shell string
  building). CI runs the script tests on all three OSes.
- **The workflow cannot be fully tested before a real release.** `workflow_dispatch`
  covers `verify`, `test` and the dry run. The `publish`, `smoke` and
  `github-release` jobs are exercised for the first time by the `v0.1.0` tag, with
  all crates already published (bootstrap). If `github-release` fails there, re-run it.
- **The `package` dry run after a publish.** The `test` job of a release run repeats
  `cargo publish --workspace --dry-run`. With the crates already on crates.io (the
  bootstrap, or a re-run) Cargo may warn or fail. Check this during the `v0.1.0`
  bootstrap; if it fails, skip the `package` job when called from `release.yml`.
- **Trusted publishing with a private repository.** Unverified. Decide before the
  bootstrap (release.md, "First release").
- **crates.io index delay.** `smoke` retries `cargo install` for a few minutes.
- **Release skill drift.** The skill must call `release.mjs` for every number and
  changelog edit. Review it against ADR-0019 §6 when the script changes.

## Definition of done

- `cargo fmt`, `cargo clippy -- -D warnings` and `cargo test --workspace` pass on CI for
  Windows, macOS and Linux, and the `msrv`, `package` and `release-script` jobs pass.
- `cargo install --path crates/sb-cli --locked` works from a clean checkout, and the
  installed skill text equals the files in `crates/sb-setup/assets/`.
- A `workflow_dispatch` run of `release.yml` passes its `verify` and dry-run steps.
- `node scripts/release.mjs check` passes, and the tests cover each failure listed in
  ADR-0019 §1.
- No document outside `docs/adrs/` uses an old crate name or mentions an installer that
  does not exist.
