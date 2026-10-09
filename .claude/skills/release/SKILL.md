---
name: release
description: Prepare or tag a second-brain release (version bump pull request with a drafted CHANGELOG.md, then the vX.Y.Z tag that starts the release workflow). Use only when the maintainer asks to prepare, tag or cut a release.
disable-model-invocation: true
---

# Release skill

This skill runs the release flow of [ADR-0019](../../../docs/adrs/0019-tag-triggered-release-and-changelog.md).
The details are in [docs/specs/release.md](../../../docs/specs/release.md). Read that
spec first if anything here is unclear.

It has two phases. **Prepare** makes a release pull request. **Tag** runs after the
pull request is merged. The maintainer says which phase to run. If they only say
"release", start with Prepare.

## Rules

- All version numbers and `CHANGELOG.md` headings are changed by
  `node scripts/release.mjs`, never by hand. The only hand-written part is the text
  under `## Unreleased`.
- The skill never runs `cargo publish`. The GitHub Actions workflow publishes, after the
  maintainer approves the `release` environment.
- Never push to `main`. Never move, re-create or force-push a tag.
- Delete a tag only if the maintainer asks for it, and only if no crate of that version
  is on crates.io. Check with `node scripts/release.mjs publish --dry-run`
  (it prints which crates are published).
- Pushing a tag is outward-facing. Always ask for confirmation first, even if the
  maintainer asked for "a release" earlier.
- If a command fails, show the error and stop. Do not work around a failed check.

## Phase 1: Prepare

1. **Preconditions.** Stop and report if any of these fails:
   - `git status --porcelain` is empty;
   - the current branch is `main`, after `git fetch origin --tags`, and it equals
     `origin/main`;
   - the latest `CI` run on `main` succeeded
     (`gh run list --branch main --workflow CI --limit 1 --json conclusion`);
   - `gh auth status` succeeds.
2. **First release?** If `git tag --list 'v*'` prints nothing, this is the first release.
   Tell the maintainer that the first release is bootstrapped by hand
   ("First release" in `docs/specs/release.md`). Continue with the pull request, but
   stop before the Tag phase until they confirm that steps 4 to 6 of the bootstrap are done.
3. **Collect the changes.** Let `LAST` be the latest tag
   (`git describe --tags --abbrev=0 --match 'v*'`), or the first commit for a first release.
   - `git log --no-merges --format='%h %s' LAST..HEAD`
   - `gh pr list --state merged --base main --json number,title,mergedAt` (merged since
     `LAST`)
   - `git diff --name-only LAST..HEAD -- 'crates/sb-store/migrations/*'` (new catalog
     migrations)
   - Search the diff for changed `schema` fields of `--json` output
     (`git diff LAST..HEAD -- crates | grep -n 'schema'`).
4. **Draft `## Unreleased`** in `CHANGELOG.md` for a person who runs `cargo install`
   (ADR-0019 §4):
   - Groups: `Added`, `Changed`, `Deprecated`, `Removed`, `Fixed`, `Security`. Leave out
     empty groups.
   - Describe what changed for the user, in plain English, one line each. Do not paste
     commit subjects.
   - Leave out `docs:`, `test:`, `chore:` and `refactor:` commits unless a user can notice
     them.
   - Always list changes to the CLI, `--json` output, settings, the data directory and
     catalog migrations. Mark a breaking change with `**Breaking:**`. For a migration,
     say that it runs on the next command.
   - For a first release, keep the existing "Initial release" entry and update it only
     if it is wrong.
5. **Propose the version** (ADR-0019 §4): a breaking change gives a new minor version
   while the major version is 0 (a new major version from 1.0.0). Otherwise `feat`
   commits give a new minor version, and anything else a new patch version. Ask the
   maintainer to confirm or change the version and to edit the draft. Wait for the answer.
6. **Make the release commit.**
   - `git switch -c release/vX.Y.Z`
   - Write the confirmed text under `## Unreleased`.
   - `node scripts/release.mjs bump X.Y.Z`
   - `node scripts/release.mjs check`
   - `cargo fmt --all --check`
   - `cargo clippy --workspace --all-targets -- -D warnings`
   - `cargo test --workspace`
   - `node scripts/release.mjs publish --dry-run`
   - `git add -A` and commit `chore: release vX.Y.Z` (follow the commit rules in
     `AGENTS.md`).
7. **Open the pull request.** `git push -u origin release/vX.Y.Z`, then `gh pr create`
   with the title `chore: release vX.Y.Z` and the changelog section as the body
   (`node scripts/release.mjs notes X.Y.Z`).
8. Tell the maintainer to review `CHANGELOG.md` in the pull request and merge it. Stop.

## Phase 2: Tag

1. **Preconditions.**
   - The release pull request is merged
     (`gh pr view release/vX.Y.Z --json state` shows `MERGED`).
   - `git switch main && git pull --ff-only`.
   - `node scripts/release.mjs check --tag vX.Y.Z` passes. If it fails, report each
     error. Fix it through a new pull request; do not tag.
2. **Confirm.** Show the maintainer: the version, the commit (`git log -1 --oneline`),
   and the changelog section. Remind them that the tag starts a workflow that waits
   for their approval before it publishes. Ask for confirmation, and wait.
3. **Tag and push.**
   - `git tag -a vX.Y.Z -m "vX.Y.Z"`
   - `git push origin vX.Y.Z`
4. **Follow the run.** `gh run list --workflow Release --limit 1`, then
   `gh run watch <id>`. When the run waits at the `publish` job, give the maintainer the run
   URL and ask them to approve the `release` environment. Then report the result of
   each job and the URL of the GitHub Release.
5. If a job fails, use the table in ADR-0019 §7:
   - before anything is published, the fix is a new pull request, and then a new tag
     (delete the old tag only if the maintainer asks);
   - after any crate is published, re-run the failed job, or fix forward with a new patch
     version. Never reuse a version.
