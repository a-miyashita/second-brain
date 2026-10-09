# ADR-0018: Distribute through crates.io with `cargo install` only

- Status: Proposed
- Date: 2026-10-09

## Context

ADR-0009 chose cargo-dist for distribution: prebuilt binaries on GitHub Releases,
shell and PowerShell installers, and a Homebrew formula. None of that exists yet.
The repository has `dist-workspace.toml`, but no release workflow, no tag and no
published artifact. The user manual already tells users to run an installer script
that does not exist.

The project needs a first release with the least machinery. `cargo install` is the
smallest option: it needs no release workflow, no signing, no per-OS installer and
no tap repository. Its cost is that users need a Rust toolchain and a C compiler
(`rusqlite` is built with `bundled`).

`cargo install second-brain` needs the binary crate on crates.io. Because the
binary crate depends on the other workspace crates, all of them must be published
too. A check of crates.io on 2026-10-09 and a review of the code showed these
problems:

1. **Names.** `sb`, `sb-cli`, `sb-core` and `second-brain-cli` belong to other
   projects. `second-brain-core` also belongs to an unrelated project (KuzuDB graph
   storage). `second-brain` and the other `second-brain-*` names we need are free.
2. **Path dependencies.** The crates depend on each other with `path` only.
   `cargo publish` requires a `version` as well.
3. **Files outside the crate.** `sb-setup` embeds the skill files with
   `include_str!("../../../assets/skills/...")`. A published crate contains only
   its own directory, so the published `sb-setup` would not compile.

## Decision

### 1. Support `cargo install` only, for now

- The supported way to install is `cargo install second-brain --locked`. It installs
  both binaries, `second-brain` and `sb`.
- Prebuilt binaries, the shell and PowerShell installers and Homebrew are
  **deferred**. The cargo-dist section of ADR-0009 is not in effect until a later
  ADR revives it. ADR-0009 gets an Amendment that says so.
- `dist-workspace.toml` and the `[profile.dist]` profile stay in the repository,
  unused, so that reviving cargo-dist does not start from zero. No release
  workflow is generated.
- Where the binaries live is decided by Cargo (`~/.cargo/bin`, or
  `%USERPROFILE%\.cargo\bin` on Windows). The ADR-0009 table of binary locations
  no longer applies. This is safe because scheduled jobs embed the path of the
  running executable (`current_exe`), not a fixed location.

### 2. Crate names: `second-brain` and `second-brain-<role>`

The binary crate is `second-brain`. Every other crate is `second-brain-<role>`.
Directories keep the short `sb-` prefix, which is only a local path.

| Directory | Old package | New package |
|---|---|---|
| `crates/sb-cli` | `sb-cli` | `second-brain` |
| `crates/sb-kernel` (was `sb-core`) | `sb-core` | `second-brain-kernel` |
| `crates/sb-store` | `sb-store` | `second-brain-store` |
| `crates/sb-pipeline` | `sb-pipeline` | `second-brain-pipeline` |
| `crates/sb-llm` | `sb-llm` | `second-brain-llm` |
| `crates/sb-google` | `sb-google` | `second-brain-google` |
| `crates/sb-slack` | `sb-slack` | `second-brain-slack` |
| `crates/sb-extract` | `sb-extract` | `second-brain-extract` |
| `crates/sb-ondemand` | `sb-ondemand` | `second-brain-ondemand` |
| `crates/sb-setup` | `sb-setup` | `second-brain-setup` |

- `sb-core` is renamed to **`kernel`** everywhere: the directory, the package, the
  Rust identifier (`second_brain_kernel`) and the documentation. `second-brain-core`
  is taken, and a name that differs only in the package would leave `core` in the
  code and `kernel` on crates.io. The role of the crate does not change.
- Rust identifiers follow the package names (`second_brain_store`, ...). We do not
  use `[lib] name` or `package = "..."` renames to keep the old identifiers. Two
  names for one crate would cost more than a one-time mechanical rename.
- The planned `sb-mcp` crate will be `second-brain-mcp` (free on 2026-10-09).
- The names above are not reserved until published. The release procedure checks
  them again right before the first publish.

### 3. Publish every crate, in lockstep

- All crates in `crates/` are published. Only `second-brain` is a supported entry
  point. The other crates are **internal**: their descriptions and READMEs say that
  they have no API stability and that users should install `second-brain`.
- All crates share `workspace.package.version`. A release bumps it once. Each
  internal dependency is declared with both `path` and `version`
  (`version = "=X.Y.Z"`, an exact pin) in `[workspace.dependencies]`, so a
  published crate can never resolve to a different release of its siblings.
- Publish order follows the dependency direction. With Cargo 1.90 or later,
  `cargo publish --workspace` computes the order.
- Versions follow SemVer for the CLI and its documented contracts (`cli.md`, the
  `--json` schema), not for the Rust APIs of the internal crates.
- `rust-version` stays pinned in the workspace and is checked by CI.

### 4. Make each crate self-contained

- Files that a crate embeds must live inside that crate. The skill files move from
  `assets/skills/second-brain/` to `crates/sb-setup/assets/skills/second-brain/`.
  Symbolic links are not used because they do not work on Windows.
- `assets/slack-app-manifest.yaml` is not embedded in any crate. It stays at the
  repository root as documentation.
- A CI job runs `cargo publish --workspace --dry-run --locked`. It packages each
  crate, and verifies that the packaged crate builds. This catches missing files
  and missing versions before a release.

### 5. Releases follow ADR-0019

- A release is a version-bump pull request followed by a `vX.Y.Z` tag. A GitHub
  Actions workflow publishes the crates, gated by a manual approval. The release
  process, the changelog and the release skill are decided in
  [ADR-0019](0019-tag-triggered-release-and-changelog.md).
- Publishing to crates.io cannot be undone (a version can be yanked, never
  deleted), so the approval gate is part of the decision, not an option.

## Consequences

- Installation needs a Rust toolchain (1.88 or later) and a C compiler. On Windows
  that means the MSVC build tools. The first build takes minutes. The manual must
  say so.
- Updating is `cargo install second-brain --locked --force`. `sb doctor` already
  compares the installed skill version with the program version, so a stale skill
  after an update is reported.
- Ten crates must be kept in step on every release. Lockstep versions and exact
  pins keep that simple, at the cost of publishing crates that did not change.
- The internal crates occupy names on crates.io. They are a public surface in
  name only; the README of each says so.
- Older ADRs and the git history use the old crate names (`sb-core`, `sb-cli`).
  They are not rewritten. The table above is the mapping.
- Users on a machine without a toolchain have no supported path until the
  deferred distribution options return.

## Alternatives considered

- **Install from git** (`cargo install --git ...`). No renaming, no publishing and
  no file moves are needed. It was rejected because it cannot be pinned to a
  release the way a crates.io version can, and it hides the project from
  `cargo search`. It remains a fallback for testing unreleased commits.
- **Keep `sb-core` and rename the others.** A mixed prefix, which the maintainer
  wanted to avoid.
- **Use a different prefix such as `secondbrain-`.** Every name was free, but it
  is easy to confuse with `second-brain-*` and with the binary crate.
- **Merge the workspace into one crate.** Only one name would be needed, but it
  undoes ADR-0001's crate boundaries, which keep `sb-core` free of I/O and
  library crates away from stdout.
- **Publish only `second-brain` and vendor the rest.** Not possible: a published
  crate cannot have path-only dependencies.
