#!/usr/bin/env node
// Release helper (ADR-0019, docs/specs/release.md).
//
//   node scripts/release.mjs bump <version> [--date YYYY-MM-DD]
//   node scripts/release.mjs check [--tag vX.Y.Z]
//   node scripts/release.mjs notes <version>
//   node scripts/release.mjs publish [--dry-run]
//   node scripts/release.mjs version
//
// Messages go to stderr. Only `notes` and `version` print to stdout.
// The script never runs `git push`, `git tag` or any GitHub API.
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const SEMVER =
  /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?$/;

export class ReleaseError extends Error {}

// ---------------------------------------------------------------- semver

export function parseSemver(v) {
  const m = SEMVER.exec(v);
  if (!m) return null;
  return {
    major: Number(m[1]),
    minor: Number(m[2]),
    patch: Number(m[3]),
    pre: m[4] ? m[4].split(".") : [],
  };
}

function comparePre(a, b) {
  // A version without a pre-release part is greater than one with it.
  if (a.length === 0 && b.length === 0) return 0;
  if (a.length === 0) return 1;
  if (b.length === 0) return -1;
  for (let i = 0; i < Math.max(a.length, b.length); i++) {
    if (a[i] === undefined) return -1;
    if (b[i] === undefined) return 1;
    const an = /^\d+$/.test(a[i]);
    const bn = /^\d+$/.test(b[i]);
    if (an && bn) {
      const d = Number(a[i]) - Number(b[i]);
      if (d !== 0) return Math.sign(d);
    } else if (an !== bn) {
      return an ? -1 : 1; // numeric identifiers sort before alphanumeric ones
    } else if (a[i] !== b[i]) {
      return a[i] < b[i] ? -1 : 1;
    }
  }
  return 0;
}

export function compareSemver(a, b) {
  const x = typeof a === "string" ? parseSemver(a) : a;
  const y = typeof b === "string" ? parseSemver(b) : b;
  for (const k of ["major", "minor", "patch"]) {
    if (x[k] !== y[k]) return x[k] < y[k] ? -1 : 1;
  }
  return comparePre(x.pre, y.pre);
}

// ------------------------------------------------------------ Cargo.toml

function sectionRange(text, header) {
  const lines = text.split("\n");
  let start = -1;
  for (let i = 0; i < lines.length; i++) {
    if (lines[i].trim() === `[${header}]`) {
      start = i + 1;
    } else if (start >= 0 && /^\s*\[/.test(lines[i])) {
      return { lines, start, end: i };
    }
  }
  if (start < 0) throw new ReleaseError(`Cargo.toml has no [${header}] section`);
  return { lines, start, end: lines.length };
}

export function readWorkspaceVersion(cargoToml) {
  const { lines, start, end } = sectionRange(cargoToml, "workspace.package");
  for (let i = start; i < end; i++) {
    const m = /^version\s*=\s*"([^"]+)"/.exec(lines[i]);
    if (m) return m[1];
  }
  throw new ReleaseError("[workspace.package] has no version");
}

export function setWorkspaceVersion(cargoToml, version) {
  const { lines, start, end } = sectionRange(cargoToml, "workspace.package");
  for (let i = start; i < end; i++) {
    if (/^version\s*=\s*"/.test(lines[i])) {
      lines[i] = `version = "${version}"`;
      return lines.join("\n");
    }
  }
  throw new ReleaseError("[workspace.package] has no version");
}

const PIN = /^(second-brain[a-z-]*)\s*=\s*\{(.*)\}\s*$/;

// Internal dependencies are declared in [workspace.dependencies] with
// `version = "=X.Y.Z"` (an exact pin).
export function readPins(cargoToml) {
  const { lines, start, end } = sectionRange(cargoToml, "workspace.dependencies");
  const pins = [];
  for (let i = start; i < end; i++) {
    const m = PIN.exec(lines[i]);
    if (!m) continue;
    const v = /version\s*=\s*"([^"]*)"/.exec(m[2]);
    pins.push({ name: m[1], version: v ? v[1] : null });
  }
  return pins;
}

export function setPins(cargoToml, version) {
  const { lines, start, end } = sectionRange(cargoToml, "workspace.dependencies");
  for (let i = start; i < end; i++) {
    if (!PIN.test(lines[i])) continue;
    lines[i] = lines[i].includes("version")
      ? lines[i].replace(/version\s*=\s*"[^"]*"/, `version = "=${version}"`)
      : lines[i].replace(/\s*\}\s*$/, `, version = "=${version}" }`);
  }
  return lines.join("\n");
}

// -------------------------------------------------------------- changelog

const HEADING = /^## (.+?)\s*$/;

export function parseChangelog(text) {
  const lines = text.split("\n");
  const sections = [];
  let cur = null;
  for (const line of lines) {
    const m = HEADING.exec(line);
    if (m) {
      cur = { heading: m[1], lines: [] };
      sections.push(cur);
    } else if (cur) {
      cur.lines.push(line);
    }
  }
  return sections.map((s) => ({ heading: s.heading, body: s.lines.join("\n").trim() }));
}

export function changelogSection(text, version) {
  for (const s of parseChangelog(text)) {
    const m = /^(\S+) - (\d{4}-\d{2}-\d{2})$/.exec(s.heading);
    if (m && m[1] === version) return { date: m[2], body: s.body };
  }
  return null;
}

export function bumpChangelog(text, version, date) {
  const sections = parseChangelog(text);
  const unreleased = sections.find((s) => s.heading === "Unreleased");
  if (!unreleased) throw new ReleaseError("CHANGELOG.md has no '## Unreleased' section");
  if (unreleased.body === "") {
    throw new ReleaseError("'## Unreleased' in CHANGELOG.md is empty; write the release notes first");
  }
  if (sections.some((s) => s.heading.startsWith(`${version} `))) {
    throw new ReleaseError(`CHANGELOG.md already has a section for ${version}`);
  }
  return text.replace(/^## Unreleased[ \t]*$/m, `## Unreleased\n\n## ${version} - ${date}`);
}

// -------------------------------------------------------------- crates.io

// Path of a crate in the sparse index (https://index.crates.io/).
export function indexPath(name) {
  const n = name.toLowerCase();
  if (n.length === 1) return `1/${n}`;
  if (n.length === 2) return `2/${n}`;
  if (n.length === 3) return `3/${n[0]}/${n}`;
  return `${n.slice(0, 2)}/${n.slice(2, 4)}/${n}`;
}

export async function isPublishedOnCratesIo(name, version) {
  const res = await fetch(`https://index.crates.io/${indexPath(name)}`, {
    headers: { "user-agent": "second-brain-release-script" },
  });
  if (res.status === 404) return false;
  if (!res.ok) throw new ReleaseError(`crates.io index returned ${res.status} for ${name}`);
  const text = await res.text();
  return text
    .split("\n")
    .filter(Boolean)
    .some((l) => JSON.parse(l).vers === version);
}

// Orders the workspace packages so that dependencies come first.
export function dependencyOrder(metadata) {
  const members = new Map(metadata.packages.map((p) => [p.name, p]));
  const ordered = [];
  const state = new Map();
  const visit = (name) => {
    if (state.get(name) === "done") return;
    if (state.get(name) === "visiting") throw new ReleaseError(`dependency cycle at ${name}`);
    state.set(name, "visiting");
    const deps = members
      .get(name)
      .dependencies.filter((d) => members.has(d.name) && d.kind !== "dev")
      .map((d) => d.name)
      .sort();
    for (const d of deps) visit(d);
    state.set(name, "done");
    ordered.push(name);
  };
  for (const name of [...members.keys()].sort()) visit(name);
  return ordered;
}

// ------------------------------------------------------------ environment

export function defaultContext(root) {
  return {
    root,
    today: () => new Date().toISOString().slice(0, 10),
    run(cmd, args, opts = {}) {
      const r = spawnSync(cmd, args, { cwd: root, encoding: "utf8", ...opts });
      if (r.error) throw new ReleaseError(`cannot run ${cmd}: ${r.error.message}`);
      return { status: r.status, stdout: r.stdout ?? "", stderr: r.stderr ?? "" };
    },
    // The same as run(), but the output goes to the terminal.
    runVisible(cmd, args) {
      const r = spawnSync(cmd, args, { cwd: root, stdio: ["ignore", 2, 2] });
      if (r.error) throw new ReleaseError(`cannot run ${cmd}: ${r.error.message}`);
      return r.status;
    },
    isPublished: isPublishedOnCratesIo,
  };
}

const read = (ctx, rel) => fs.readFileSync(path.join(ctx.root, rel), "utf8");
const write = (ctx, rel, text) => fs.writeFileSync(path.join(ctx.root, rel), text);
const log = (msg) => process.stderr.write(`${msg}\n`);

function crateDirs(ctx) {
  const dir = path.join(ctx.root, "crates");
  return fs
    .readdirSync(dir, { withFileTypes: true })
    .filter((e) => e.isDirectory() && fs.existsSync(path.join(dir, e.name, "Cargo.toml")))
    .map((e) => e.name)
    .sort();
}

function metadata(ctx) {
  const r = ctx.run("cargo", ["metadata", "--no-deps", "--format-version", "1", "--locked"]);
  if (r.status !== 0) throw new ReleaseError(`cargo metadata failed:\n${r.stderr}`);
  return JSON.parse(r.stdout);
}

// ---------------------------------------------------------------- commands

export function cmdVersion(ctx) {
  return readWorkspaceVersion(read(ctx, "Cargo.toml"));
}

export function cmdNotes(ctx, version) {
  const s = changelogSection(read(ctx, "CHANGELOG.md"), version);
  if (!s) throw new ReleaseError(`CHANGELOG.md has no dated section for ${version}`);
  if (s.body === "") throw new ReleaseError(`the CHANGELOG.md section for ${version} is empty`);
  return s.body;
}

export function cmdBump(ctx, version, date) {
  if (!parseSemver(version)) throw new ReleaseError(`'${version}' is not a valid version (X.Y.Z)`);
  const cargo = read(ctx, "Cargo.toml");
  const current = readWorkspaceVersion(cargo);
  if (compareSemver(version, current) <= 0) {
    throw new ReleaseError(`the new version ${version} must be greater than the current ${current}`);
  }
  // Validate the changelog first so that a failure leaves the tree untouched.
  const changelog = bumpChangelog(read(ctx, "CHANGELOG.md"), version, date ?? ctx.today());
  write(ctx, "Cargo.toml", setPins(setWorkspaceVersion(cargo, version), version));
  write(ctx, "CHANGELOG.md", changelog);
  const r = ctx.run("cargo", ["update", "--workspace"]);
  if (r.status !== 0) throw new ReleaseError(`cargo update --workspace failed:\n${r.stderr}`);
}

// Returns a list of problems. An empty list means the checks passed.
export function cmdCheck(ctx, { tag } = {}) {
  const errors = [];
  const cargo = read(ctx, "Cargo.toml");
  let version;
  try {
    version = readWorkspaceVersion(cargo);
  } catch (e) {
    return [e.message];
  }
  if (!parseSemver(version)) errors.push(`workspace version '${version}' is not valid SemVer`);

  const pins = readPins(cargo);
  if (pins.length === 0) errors.push("no internal dependency pins found in [workspace.dependencies]");
  for (const p of pins) {
    if (p.version !== `=${version}`) {
      errors.push(`${p.name} is pinned to '${p.version}', expected '=${version}'`);
    }
  }

  const license = read(ctx, "LICENSE");
  for (const dir of crateDirs(ctx)) {
    const manifest = read(ctx, `crates/${dir}/Cargo.toml`);
    if (!/^version\.workspace\s*=\s*true/m.test(manifest)) {
      errors.push(`crates/${dir} does not inherit the workspace version`);
    }
    const copy = path.join(ctx.root, "crates", dir, "LICENSE");
    if (!fs.existsSync(copy) || fs.readFileSync(copy, "utf8") !== license) {
      errors.push(`crates/${dir}/LICENSE is missing or differs from the root LICENSE`);
    }
  }

  const lock = ctx.run("cargo", ["update", "--workspace", "--locked"]);
  if (lock.status !== 0) errors.push(`Cargo.lock is not up to date:\n${lock.stderr.trim()}`);

  if (tag !== undefined) errors.push(...checkTag(ctx, tag, version));
  return errors;
}

function checkTag(ctx, tag, version) {
  const errors = [];
  if (tag !== `v${version}`) {
    errors.push(`tag '${tag}' does not match the workspace version (expected 'v${version}')`);
  }

  let section = null;
  try {
    section = changelogSection(read(ctx, "CHANGELOG.md"), version);
  } catch {
    errors.push("CHANGELOG.md is missing");
  }
  if (section === null) {
    errors.push(`CHANGELOG.md has no '## ${version} - YYYY-MM-DD' section`);
  } else if (section.body === "") {
    errors.push(`the CHANGELOG.md section for ${version} is empty`);
  }

  const head = ctx.run("git", ["rev-parse", "HEAD"]).stdout.trim();
  const tagged = ctx.run("git", ["rev-list", "-n", "1", `refs/tags/${tag}`]);
  if (tagged.status === 0 && tagged.stdout.trim() !== head) {
    errors.push(`tag '${tag}' does not point to HEAD`);
  }
  const anc = ctx.run("git", ["merge-base", "--is-ancestor", "HEAD", "origin/main"]);
  if (anc.status !== 0) errors.push("HEAD is not on origin/main");

  const mine = parseSemver(version);
  if (mine) {
    const earlier = ctx
      .run("git", ["tag", "--list", "v*"])
      .stdout.split("\n")
      .map((t) => t.trim())
      .filter((t) => t && t !== tag && parseSemver(t.slice(1)));
    for (const t of earlier) {
      if (compareSemver(version, t.slice(1)) <= 0) {
        errors.push(`version ${version} is not greater than the existing tag ${t}`);
      }
    }
  }
  return errors;
}

export async function cmdPublish(ctx, { dryRun = false } = {}) {
  const version = cmdVersion(ctx);
  const order = dependencyOrder(metadata(ctx));
  if (dryRun) {
    for (const name of order) {
      const done = await ctx.isPublished(name, version);
      log(`${name}@${version}: ${done ? "already on crates.io" : "not published yet"}`);
    }
    // A workspace dry run checks that every crate packages and builds from its own
    // tarball. Per-crate dry runs fail while the dependencies are not published.
    const status = ctx.runVisible("cargo", ["publish", "--workspace", "--dry-run", "--locked"]);
    if (status !== 0) throw new ReleaseError("cargo publish --dry-run failed");
    return;
  }
  for (const name of order) {
    if (await ctx.isPublished(name, version)) {
      log(`${name}@${version}: already on crates.io, skipped`);
      continue;
    }
    log(`${name}@${version}: publishing`);
    // Cargo waits until the crate is available in the index before it returns.
    const status = ctx.runVisible("cargo", ["publish", "-p", name, "--locked"]);
    if (status !== 0) throw new ReleaseError(`cargo publish failed for ${name}`);
  }
}

// -------------------------------------------------------------------- CLI

function option(args, name) {
  const i = args.indexOf(name);
  if (i < 0) return undefined;
  const v = args[i + 1];
  if (v === undefined || v.startsWith("--")) throw new ReleaseError(`${name} needs a value`);
  return v;
}

export async function main(argv, ctx = defaultContext(process.cwd())) {
  const [cmd, ...args] = argv;
  switch (cmd) {
    case "version":
      process.stdout.write(`${cmdVersion(ctx)}\n`);
      return 0;
    case "notes":
      if (!args[0]) throw new ReleaseError("usage: release.mjs notes <version>");
      process.stdout.write(`${cmdNotes(ctx, args[0])}\n`);
      return 0;
    case "bump":
      if (!args[0]) throw new ReleaseError("usage: release.mjs bump <version> [--date YYYY-MM-DD]");
      cmdBump(ctx, args[0], option(args, "--date"));
      log(`bumped to ${args[0]}`);
      return 0;
    case "check": {
      const errors = cmdCheck(ctx, { tag: option(args, "--tag") });
      for (const e of errors) log(`error: ${e}`);
      if (errors.length === 0) log("release checks passed");
      return errors.length === 0 ? 0 : 1;
    }
    case "publish":
      await cmdPublish(ctx, { dryRun: args.includes("--dry-run") });
      return 0;
    default:
      throw new ReleaseError(
        "usage: release.mjs <bump <version> | check [--tag vX.Y.Z] | notes <version> | publish [--dry-run] | version>",
      );
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  // The repository root is the parent of the scripts directory.
  const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
  main(process.argv.slice(2), defaultContext(root)).then(
    (code) => process.exit(code),
    (e) => {
      log(`error: ${e instanceof ReleaseError ? e.message : (e.stack ?? e)}`);
      process.exit(1);
    },
  );
}
