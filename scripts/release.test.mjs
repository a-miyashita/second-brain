// Tests for release.mjs. Run: node --test scripts/release.test.mjs
// No network and no real cargo or git: the context is replaced by fakes.
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import {
  ReleaseError,
  bumpChangelog,
  changelogSection,
  cmdBump,
  cmdCheck,
  cmdNotes,
  cmdPublish,
  cmdVersion,
  compareSemver,
  dependencyOrder,
  indexPath,
  main,
  parseSemver,
  readPins,
  readWorkspaceVersion,
  setPins,
  setWorkspaceVersion,
} from "./release.mjs";

const CARGO = `[workspace]
members = ["crates/*"]

[workspace.package]
version = "0.1.0"
edition = "2024"

[workspace.dependencies]
second-brain-kernel = { path = "crates/sb-kernel", version = "=0.1.0" }
second-brain-store = { path = "crates/sb-store", version = "=0.1.0" }
anyhow = "1"

[profile.release]
lto = "thin"
`;

const CHANGELOG = `# Changelog

## Unreleased

### Added

- A new thing.

## 0.1.0 - 2026-10-01

### Added

- Initial release.
`;

function fixture({ cargo = CARGO, changelog = CHANGELOG } = {}) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "release-test-"));
  fs.writeFileSync(path.join(root, "Cargo.toml"), cargo);
  fs.writeFileSync(path.join(root, "LICENSE"), "MIT License\n");
  fs.writeFileSync(path.join(root, "CHANGELOG.md"), changelog);
  for (const d of ["sb-kernel", "sb-store"]) {
    fs.mkdirSync(path.join(root, "crates", d), { recursive: true });
    fs.writeFileSync(
      path.join(root, "crates", d, "Cargo.toml"),
      `[package]\nname = "x"\nversion.workspace = true\n`,
    );
    fs.writeFileSync(path.join(root, "crates", d, "LICENSE"), "MIT License\n");
  }
  return root;
}

// A fake environment. `git` answers can be overridden per test.
function fakeContext(root, overrides = {}) {
  const calls = [];
  const git = {
    "rev-parse": { status: 0, stdout: "aaa\n" },
    "rev-list": { status: 128, stdout: "" }, // the tag does not exist yet
    "merge-base": { status: 0, stdout: "" },
    tag: { status: 0, stdout: "" },
    ...overrides.git,
  };
  const published = new Set(overrides.published ?? []);
  const ctx = {
    root,
    calls,
    today: () => "2026-11-01",
    run(cmd, args) {
      calls.push([cmd, ...args]);
      if (cmd === "git") return { stderr: "", ...git[args[0]] };
      if (cmd === "cargo" && args[0] === "update" && args.includes("--locked")) {
        return overrides.lock ?? { status: 0, stdout: "", stderr: "" };
      }
      if (cmd === "cargo" && args[0] === "metadata") {
        return { status: 0, stderr: "", stdout: JSON.stringify(overrides.metadata ?? META) };
      }
      return { status: 0, stdout: "", stderr: "" };
    },
    runVisible(cmd, args) {
      calls.push([cmd, ...args]);
      return overrides.failPublishFor && args.includes(overrides.failPublishFor) ? 1 : 0;
    },
    async isPublished(name, version) {
      return published.has(`${name}@${version}`);
    },
  };
  return ctx;
}

const META = {
  packages: [
    { name: "second-brain", dependencies: [{ name: "second-brain-store" }, { name: "serde" }] },
    {
      name: "second-brain-store",
      dependencies: [{ name: "second-brain-kernel" }, { name: "tempfile", kind: "dev" }],
    },
    { name: "second-brain-kernel", dependencies: [] },
  ],
};

// ------------------------------------------------------------------ semver

test("parseSemver accepts valid versions and rejects invalid ones", () => {
  assert.deepEqual(parseSemver("1.2.3"), { major: 1, minor: 2, patch: 3, pre: [] });
  assert.deepEqual(parseSemver("0.2.0-rc.1").pre, ["rc", "1"]);
  for (const bad of ["1.2", "01.2.3", "1.2.3.4", "v1.2.3", "1.2.3+build", "", "1.2.3-"]) {
    assert.equal(parseSemver(bad), null, bad);
  }
});

test("compareSemver follows SemVer precedence", () => {
  const order = ["0.1.0", "0.1.1", "0.2.0-alpha", "0.2.0-alpha.1", "0.2.0-rc.1", "0.2.0", "1.0.0"];
  for (let i = 0; i < order.length - 1; i++) {
    assert.equal(compareSemver(order[i], order[i + 1]), -1, `${order[i]} < ${order[i + 1]}`);
    assert.equal(compareSemver(order[i + 1], order[i]), 1);
  }
  assert.equal(compareSemver("1.0.0", "1.0.0"), 0);
  assert.equal(compareSemver("1.0.0-2", "1.0.0-10"), -1); // numeric, not text
});

// -------------------------------------------------------------- Cargo.toml

test("workspace version and pins are read and written", () => {
  assert.equal(readWorkspaceVersion(CARGO), "0.1.0");
  assert.deepEqual(readPins(CARGO), [
    { name: "second-brain-kernel", version: "=0.1.0" },
    { name: "second-brain-store", version: "=0.1.0" },
  ]);
  const next = setPins(setWorkspaceVersion(CARGO, "0.2.0"), "0.2.0");
  assert.equal(readWorkspaceVersion(next), "0.2.0");
  assert.ok(readPins(next).every((p) => p.version === "=0.2.0"));
  // Nothing else changes.
  assert.ok(next.includes('anyhow = "1"'));
  assert.ok(next.includes('edition = "2024"'));
  assert.equal(next.replaceAll("0.2.0", "0.1.0"), CARGO);
});

test("setPins adds a version to an internal dependency that has none", () => {
  const bare = CARGO.replace('crates/sb-store", version = "=0.1.0" }', 'crates/sb-store" }');
  assert.equal(readPins(bare)[1].version, null);
  const fixed = setPins(bare, "0.3.0");
  assert.deepEqual(readPins(fixed).map((p) => p.version), ["=0.3.0", "=0.3.0"]);
});

// --------------------------------------------------------------- changelog

test("bumpChangelog dates the Unreleased section and adds an empty one", () => {
  const out = bumpChangelog(CHANGELOG, "0.2.0", "2026-11-01");
  assert.match(out, /## Unreleased\n\n## 0\.2\.0 - 2026-11-01\n\n### Added\n\n- A new thing\./);
  assert.equal(changelogSection(out, "0.2.0").body.includes("A new thing."), true);
  assert.equal(changelogSection(out, "0.1.0").date, "2026-10-01");
  assert.equal(changelogSection(out, "0.3.0"), null);
});

test("bumpChangelog refuses an empty Unreleased section or a duplicate version", () => {
  const empty = CHANGELOG.replace("### Added\n\n- A new thing.\n\n", "");
  assert.throws(() => bumpChangelog(empty, "0.2.0", "2026-11-01"), /empty/);
  assert.throws(() => bumpChangelog("# Changelog\n", "0.2.0", "2026-11-01"), /no '## Unreleased'/);
  assert.throws(() => bumpChangelog(CHANGELOG, "0.1.0", "2026-11-01"), /already has/);
});

test("changelogSection ignores undated headings", () => {
  assert.equal(changelogSection("## 0.1.0\n\n- x\n", "0.1.0"), null);
});

// ------------------------------------------------------------------- bump

test("bump updates the manifest, the pins and the changelog, then updates the lock file", () => {
  const root = fixture();
  const ctx = fakeContext(root);
  cmdBump(ctx, "0.2.0");
  const cargo = fs.readFileSync(path.join(root, "Cargo.toml"), "utf8");
  assert.equal(readWorkspaceVersion(cargo), "0.2.0");
  assert.ok(readPins(cargo).every((p) => p.version === "=0.2.0"));
  assert.ok(fs.readFileSync(path.join(root, "CHANGELOG.md"), "utf8").includes("## 0.2.0 - 2026-11-01"));
  assert.deepEqual(ctx.calls.at(-1), ["cargo", "update", "--workspace"]);
  assert.equal(cmdVersion(ctx), "0.2.0");
});

test("bump refuses an invalid or non-increasing version and leaves the files alone", () => {
  const root = fixture();
  const ctx = fakeContext(root);
  assert.throws(() => cmdBump(ctx, "1.0"), /not a valid version/);
  assert.throws(() => cmdBump(ctx, "0.0.9"), /must not be lower/);
  // 0.1.0 already has a dated section in the fixture changelog.
  assert.throws(() => cmdBump(ctx, "0.1.0"), /already has a section/);
  assert.equal(fs.readFileSync(path.join(root, "Cargo.toml"), "utf8"), CARGO);
});

test("bump accepts the current version when it is not released yet (first release)", () => {
  const first = "# Changelog\n\n## Unreleased\n\n- Initial release.\n";
  const root = fixture({ changelog: first });
  cmdBump(fakeContext(root), "0.1.0", "2026-11-01");
  assert.equal(fs.readFileSync(path.join(root, "Cargo.toml"), "utf8"), CARGO);
  assert.match(fs.readFileSync(path.join(root, "CHANGELOG.md"), "utf8"), /## Unreleased\n\n## 0\.1\.0 - 2026-11-01/);
});

test("bump with an empty changelog does not touch Cargo.toml", () => {
  const root = fixture({ changelog: "# Changelog\n\n## Unreleased\n\n## 0.1.0 - 2026-10-01\n\n- x\n" });
  assert.throws(() => cmdBump(fakeContext(root), "0.2.0"), ReleaseError);
  assert.equal(fs.readFileSync(path.join(root, "Cargo.toml"), "utf8"), CARGO);
});

// ------------------------------------------------------------------ check

const releaseChangelog = bumpChangelog(CHANGELOG, "0.2.0", "2026-11-01");
const releaseCargo = setPins(setWorkspaceVersion(CARGO, "0.2.0"), "0.2.0");

test("check passes for a consistent release commit", () => {
  const ctx = fakeContext(fixture({ cargo: releaseCargo, changelog: releaseChangelog }));
  assert.deepEqual(cmdCheck(ctx), []);
  assert.deepEqual(cmdCheck(ctx, { tag: "v0.2.0" }), []);
});

test("check reports a pin that differs from the workspace version", () => {
  const cargo = releaseCargo.replace('second-brain-store = { path = "crates/sb-store", version = "=0.2.0" }',
    'second-brain-store = { path = "crates/sb-store", version = "=0.1.0" }');
  const errors = cmdCheck(fakeContext(fixture({ cargo, changelog: releaseChangelog })));
  assert.equal(errors.length, 1);
  assert.match(errors[0], /second-brain-store is pinned to '=0\.1\.0', expected '=0\.2\.0'/);
});

test("check reports a crate that does not inherit the version and a stale license copy", () => {
  const root = fixture({ cargo: releaseCargo, changelog: releaseChangelog });
  fs.writeFileSync(path.join(root, "crates", "sb-store", "Cargo.toml"), '[package]\nversion = "0.2.0"\n');
  fs.writeFileSync(path.join(root, "crates", "sb-kernel", "LICENSE"), "other\n");
  const errors = cmdCheck(fakeContext(root));
  assert.ok(errors.some((e) => /sb-store does not inherit/.test(e)));
  assert.ok(errors.some((e) => /sb-kernel\/LICENSE/.test(e)));
});

test("check reports a lock file that is out of date", () => {
  const ctx = fakeContext(fixture({ cargo: releaseCargo, changelog: releaseChangelog }), {
    lock: { status: 101, stdout: "", stderr: "the lock file needs to be updated" },
  });
  assert.match(cmdCheck(ctx).join("\n"), /Cargo\.lock is not up to date/);
});

test("check --tag reports a tag that differs from the version", () => {
  const ctx = fakeContext(fixture({ cargo: releaseCargo, changelog: releaseChangelog }));
  assert.match(cmdCheck(ctx, { tag: "v0.2.1" }).join("\n"), /does not match the workspace version/);
  assert.match(cmdCheck(ctx, { tag: "0.2.0" }).join("\n"), /does not match the workspace version/);
});

test("check --tag reports a missing or empty changelog section", () => {
  const missing = fakeContext(fixture({ cargo: releaseCargo, changelog: CHANGELOG }));
  assert.match(cmdCheck(missing, { tag: "v0.2.0" }).join("\n"), /no '## 0\.2\.0 - YYYY-MM-DD' section/);
  const emptySection = releaseChangelog.replace(/## 0\.2\.0 - 2026-11-01[\s\S]*?(?=## 0\.1\.0)/, "## 0.2.0 - 2026-11-01\n\n");
  const empty = fakeContext(fixture({ cargo: releaseCargo, changelog: emptySection }));
  assert.match(cmdCheck(empty, { tag: "v0.2.0" }).join("\n"), /section for 0\.2\.0 is empty/);
});

test("check --tag reports a commit that is not on origin/main", () => {
  const ctx = fakeContext(fixture({ cargo: releaseCargo, changelog: releaseChangelog }), {
    git: { "merge-base": { status: 1, stdout: "" } },
  });
  assert.match(cmdCheck(ctx, { tag: "v0.2.0" }).join("\n"), /HEAD is not on origin\/main/);
});

test("check --tag reports an existing tag that points to another commit", () => {
  const ctx = fakeContext(fixture({ cargo: releaseCargo, changelog: releaseChangelog }), {
    git: { "rev-list": { status: 0, stdout: "bbb\n" } },
  });
  assert.match(cmdCheck(ctx, { tag: "v0.2.0" }).join("\n"), /does not point to HEAD/);
});

test("check --tag reports a version that is not greater than an earlier tag", () => {
  const files = { cargo: releaseCargo, changelog: releaseChangelog };
  const older = fakeContext(fixture(files), { git: { tag: { status: 0, stdout: "v0.1.0\nv0.2.0\nnot-a-version\n" } } });
  // The tag being released is ignored; v0.1.0 is older, so this passes.
  assert.deepEqual(cmdCheck(older, { tag: "v0.2.0" }), []);
  const newer = fakeContext(fixture(files), { git: { tag: { status: 0, stdout: "v0.1.0\nv0.3.0\n" } } });
  assert.match(cmdCheck(newer, { tag: "v0.2.0" }).join("\n"), /not greater than the existing tag v0\.3\.0/);
});

// ------------------------------------------------------------------ notes

test("notes prints the body of a dated section", () => {
  const ctx = fakeContext(fixture());
  assert.equal(cmdNotes(ctx, "0.1.0"), "### Added\n\n- Initial release.");
  assert.throws(() => cmdNotes(ctx, "9.9.9"), /no dated section/);
});

// ---------------------------------------------------------------- publish

test("indexPath follows the sparse index layout", () => {
  assert.equal(indexPath("a"), "1/a");
  assert.equal(indexPath("ab"), "2/ab");
  assert.equal(indexPath("abc"), "3/a/abc");
  assert.equal(indexPath("second-brain"), "se/co/second-brain");
  assert.equal(indexPath("Second-Brain-Kernel"), "se/co/second-brain-kernel");
});

test("dependencyOrder puts dependencies first and ignores dev dependencies", () => {
  assert.deepEqual(dependencyOrder(META), ["second-brain-kernel", "second-brain-store", "second-brain"]);
  const cyclic = {
    packages: [
      { name: "a", dependencies: [{ name: "b" }] },
      { name: "b", dependencies: [{ name: "a" }] },
    ],
  };
  assert.throws(() => dependencyOrder(cyclic), /cycle/);
});

const publishedCalls = (ctx) => ctx.calls.filter((c) => c[1] === "publish").map((c) => c[3]);

test("publish publishes the crates in dependency order", async () => {
  const ctx = fakeContext(fixture());
  await cmdPublish(ctx);
  assert.deepEqual(publishedCalls(ctx), ["second-brain-kernel", "second-brain-store", "second-brain"]);
});

test("publish skips crates that are already on crates.io, so a re-run continues", async () => {
  const ctx = fakeContext(fixture(), {
    published: ["second-brain-kernel@0.1.0", "second-brain-store@0.1.0"],
  });
  await cmdPublish(ctx);
  assert.deepEqual(publishedCalls(ctx), ["second-brain"]);
});

test("publish stops at the first failure", async () => {
  const ctx = fakeContext(fixture(), { failPublishFor: "second-brain-store" });
  await assert.rejects(cmdPublish(ctx), /failed for second-brain-store/);
  assert.deepEqual(publishedCalls(ctx), ["second-brain-kernel", "second-brain-store"]);
});

test("publish --dry-run runs one workspace dry run and publishes nothing", async () => {
  const ctx = fakeContext(fixture());
  await cmdPublish(ctx, { dryRun: true });
  const publishes = ctx.calls.filter((c) => c[1] === "publish");
  assert.deepEqual(publishes, [["cargo", "publish", "--workspace", "--dry-run", "--locked"]]);
});

// -------------------------------------------------------------------- CLI

test("main reports usage errors and returns the check status", async () => {
  const ctx = fakeContext(fixture({ cargo: releaseCargo, changelog: releaseChangelog }));
  await assert.rejects(main([], ctx), /usage/);
  await assert.rejects(main(["bump"], ctx), /usage/);
  await assert.rejects(main(["check", "--tag"], ctx), /needs a value/);
  assert.equal(await main(["check", "--tag", "v0.2.0"], ctx), 0);
  assert.equal(await main(["check", "--tag", "v0.9.9"], ctx), 1);
});
