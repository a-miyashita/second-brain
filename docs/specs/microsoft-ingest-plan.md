# Implementation plan: SharePoint and OneDrive ingest

Related ADRs: 0006, 0008, 0014, 0021. Specs to read first:
[source-documents.md](source-documents.md) (`microsoft.file`),
[accounts-and-auth.md](accounts-and-auth.md) (Microsoft), [ingest.md](ingest.md).

Scope: `microsoft.file` on demand through `sb ingest`, and the `microsoft` account.
Out of scope: sync, folders, sites, sovereign clouds, SharePoint Server, device code flow.

## Spikes (before the code that depends on them)

| # | Question | Result goes to |
|---|---|---|
| S-MS1 | Which SharePoint URL forms does `GET /shares/u!{id}/driveItem` accept? Check: a sharing link (`/:w:/s/...`), a direct document URL with spaces and non-ASCII names, `Doc.aspx?sourcedoc=`, a `1drv.ms` short link, an `onedrive.live.com` link, a site page. Use a test tenant and a personal account | The table of accepted forms in source-documents.md, and the hint texts |
| S-MS2 | Does a `localhost` redirect with an ephemeral port work for a public client in a work tenant and for a personal account? Does the browser reach the listener when `localhost` resolves to `::1`? | The listener rules in accounts-and-auth.md |
| S-MS3 | Is `@microsoft.graph.downloadUrl` returned by the `shares` call, or must `/content` be used? Does `/content` answer with `302` for every file size and drive type? | The download step in source-documents.md |
| S-MS4 | What do `AADSTS` errors look like for "administrator consent required" and for conditional access? Which error codes mean "sign in again" | The `needs_reauth` mapping |

Spike results use no real tenant data in the repository. Recorded responses are scrubbed
before they become fixtures.

## Tasks

Each task is a small commit (`feat:`, `refactor:`, `test:`, `docs:`).

1. **`refactor`: extract `sb-oauth`.** Move the generic parts of `sb-google/src/oauth.rs`
   (random strings, PKCE challenge, loopback listener and browser opening, token-endpoint
   form requests, error type for those) into a new crate. `sb-google` uses it. No change
   of behavior; existing tests must pass unchanged. Add the crate to the workspace,
   `release.md` crate table and the publish order.
2. **`sb-kernel`:** add `AccountKind::Microsoft` (`"microsoft"`), `SourceKind::MicrosoftFile`
   (`"microsoft.file"`) and the `account_kind` mapping. Update exhaustive matches.
3. **`sb-microsoft`: OAuth.** Authorization-code flow with PKCE for the Entra v2.0
   endpoints (`/{tenant}/oauth2/v2.0/authorize` and `/token`); ID-token claims `tid`,
   `oid`, `preferred_username`; refresh with **rotation** (the store write happens before
   the token is used; refresh is serialized in the process); error mapping to
   `needs_reauth` (S-MS4). Tests with `wiremock`.
4. **`sb-microsoft`: Graph client.** `shares` resolution, metadata, `/content` with a manual
   redirect, streaming download with a size limit, `Retry-After` handling. The token is
   attached only to `graph.microsoft.com` requests.
5. **`sb-microsoft`: `MicrosoftSource`.** `Source` implementation with
   `supports_sync() = false`, `resolve`, `refusal`, `fetch`, and the pure `normalize`
   (shared document normalization with `google.doc` where the code is the same: move the
   common part to `sb-kernel` or a helper only if both crates need it).
6. **`sb-pipeline`:** register the Microsoft adapter in the source factory; account probing
   for `microsoft.file` in `sb ingest`, in the same code path as the Google probe.
7. **`sb-cli`:** `sb account add microsoft`, `sb auth login` and `sb auth status` for the
   new kind, locator classification (ingest.md "Locator classification"), `--account`
   kind check, `--dry-run` result for `microsoft.file` (no network), help texts.
8. **`sb-cli` `doctor`:** `accounts.online` for Microsoft (refresh check).
9. **`sb-setup` and the TUI:** the accounts section lists Microsoft accounts and can add
   one (client ID and tenant fields); the skill assets (`SKILL.md`,
   `references/ingest-guide.md`) say that SharePoint and OneDrive links are accepted. Bump
   the skill version, because `doctor` compares it with the binary.
10. **Docs outside `docs/`:** `README.md` (crate list, a "SharePoint and OneDrive" setup
    guide with the app registration steps and a short text for the administrator),
    `AGENTS.md` / `CLAUDE.md` repository layout, `manuals/`, `CHANGELOG.md` (through
    `scripts/release.mjs` only at release time).

Order and dependencies: spikes S-MS1 to S-MS4 → 1 → 2 → 3 → 4 → 5 → 6 → 7 → 8 → 9 → 10.
Tasks 3 and 4 can run in parallel after 2 if two people work on it. Task 1 is
independent of the Microsoft details and can be merged first.

## Tests

See "Tests" in [source-documents.md](source-documents.md). In addition:

- **Rotation:** a refresh that returns a new refresh token stores it; a failed store makes
  the refresh fail instead of returning an access token.
- **No credential leak:** a test server on a second host receives the download redirect and
  asserts that there is no `Authorization` header; an `http://` redirect is refused.
- **Classification:** every host rule of ingest.md (`*.sharepoint.com`, `*-my.sharepoint.com`,
  `onedrive.live.com`, `1drv.ms`, a `*.sharepoint.cn` host that stays `web.page`, a look-alike
  host such as `sharepoint.com.example.org`).
- **Probing:** two Microsoft accounts, the first without access, the second with access;
  an account in `needs_reauth` is skipped.
- Fixtures contain no real tenant, user or file names. No test uses the real network.

## Done when

- `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings` and
  `cargo test --workspace` pass on the three platforms in CI.
- `sb account add microsoft` and `sb ingest <SharePoint URL>` are checked by hand against a
  real tenant (the maintainer or a volunteer), since CI has no network access to Microsoft.
- ADR-0021 is `Accepted` (2026-11-06); add an Amendments entry for anything
  that the spikes changed.
