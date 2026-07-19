#!/usr/bin/env node
// Produce the seven ready-to-publish npm package directories from built binaries:
// the wxctl meta package plus six platform sub-packages. Single source of
// version truth: stamps --version into all seven package.json files and copies
// each target's binary into its sub-package. Invariants:
//   I1 - --version must equal the workspace crate version and be >= 0.1.1
//        (0.1.0 is the immutable reserved placeholder and can never be reused).
//   I3 - plain MAJOR.MINOR.PATCH only (no prerelease / build-metadata).
// All assertions run before any package directory is written. glibc-only: the
// two Linux sub-packages carry a -gnu name suffix and libc: ["glibc"]; musl is
// deferred (P4) and is a purely additive drop-in later. See wxctl/npm/README.md.

import { execFileSync } from 'node:child_process';
import { chmodSync, copyFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const WORKSPACE = path.resolve(HERE, '..'); // wxctl/ (holds the Cargo workspace + LICENSE)

// Rust target triple -> npm sub-package identity. `slug` is the sub-package name
// suffix (following the Rust triple libc word `-gnu` on Linux). `libc` is the
// npm libc field value (`glibc`), which differs from the slug word (`gnu`).
const TARGETS = [
  { triple: 'x86_64-apple-darwin', slug: 'darwin-x64', os: 'darwin', cpu: 'x64', win: false },
  { triple: 'aarch64-apple-darwin', slug: 'darwin-arm64', os: 'darwin', cpu: 'arm64', win: false },
  { triple: 'x86_64-unknown-linux-gnu', slug: 'linux-x64-gnu', os: 'linux', cpu: 'x64', win: false, libc: 'glibc' },
  { triple: 'aarch64-unknown-linux-gnu', slug: 'linux-arm64-gnu', os: 'linux', cpu: 'arm64', win: false, libc: 'glibc' },
  { triple: 'x86_64-pc-windows-msvc', slug: 'win32-x64', os: 'win32', cpu: 'x64', win: true },
  { triple: 'aarch64-pc-windows-msvc', slug: 'win32-arm64', os: 'win32', cpu: 'arm64', win: true },
];

function fail(msg) {
  console.error(`generate.mjs: ${msg}`);
  process.exit(1);
}

function arg(name) {
  const i = process.argv.indexOf(`--${name}`);
  return i >= 0 ? process.argv[i + 1] : undefined;
}

const version = arg('version');
const artifacts = arg('artifacts');
const out = arg('out');
if (!version || !artifacts || !out) {
  fail('usage: node generate.mjs --version <x.y.z> --artifacts <dir> --out <dir>');
}

// I3: plain MAJOR.MINOR.PATCH only (no prerelease / build-metadata).
if (!/^\d+\.\d+\.\d+$/.test(version)) {
  fail(`refusing non-plain version "${version}": npm publishes require a plain MAJOR.MINOR.PATCH`);
}
// I1: the first functional npm release must be >= 0.1.1. 0.1.0 (and anything
// below it) is refused; 0.1.0 is published as an immutable name reservation.
{
  const [maj, min, pat] = version.split('.').map(Number);
  if (maj === 0 && (min === 0 || (min === 1 && pat < 1))) {
    fail(`refusing ${version}: the first functional npm release must be 0.1.1 or higher (0.1.0 is the reserved placeholder)`);
  }
}
// I1: --version must equal the workspace crate version.
let crateVersion;
try {
  const meta = JSON.parse(execFileSync('cargo', ['metadata', '--no-deps', '--format-version', '1'], { cwd: WORKSPACE, encoding: 'utf8' }));
  crateVersion = meta.packages.find((p) => p.name === 'wxctl')?.version;
} catch (e) {
  fail(`could not read the crate version via \`cargo metadata\`: ${e.message}`);
}
if (version !== crateVersion) {
  fail(`version drift: --version ${version} != crate version ${crateVersion}`);
}

const license = path.join(WORKSPACE, 'LICENSE');

// Meta package (bin shim + optionalDependencies on all six sub-packages).
const metaOut = path.join(out, 'meta');
mkdirSync(path.join(metaOut, 'bin'), { recursive: true });
const metaPkg = JSON.parse(readFileSync(path.join(HERE, 'meta', 'package.json'), 'utf8'));
metaPkg.version = version;
metaPkg.optionalDependencies = Object.fromEntries(TARGETS.map((t) => [`@randyphoa/wxctl-${t.slug}`, version]));
writeFileSync(path.join(metaOut, 'package.json'), `${JSON.stringify(metaPkg, null, 2)}\n`);
copyFileSync(path.join(HERE, 'meta', 'bin', 'wxctl.mjs'), path.join(metaOut, 'bin', 'wxctl.mjs'));
if (existsSync(license)) copyFileSync(license, path.join(metaOut, 'LICENSE'));
// Ship the project README in the meta tarball so npmjs.com renders a real page
// body. npm always includes README.md when present; with none bundled the page
// shows only the description (the "empty page"). Single source of truth: reuse
// the workspace README rather than a separate npm copy that would drift. npm
// rewrites its relative links/images against the `repository` field at render.
const readme = path.join(WORKSPACE, 'README.md');
if (existsSync(readme)) copyFileSync(readme, path.join(metaOut, 'README.md'));

// Platform sub-packages: generate only those whose binary is present under
// <artifacts>. A single-host run yields meta + the host sub-package only; the CI
// publish path passes all six.
const template = JSON.parse(readFileSync(path.join(HERE, 'platform', 'package.json'), 'utf8'));
let generated = 0;
for (const t of TARGETS) {
  const binName = t.win ? 'wxctl.exe' : 'wxctl';
  const src = path.join(artifacts, t.triple, binName);
  if (!existsSync(src)) {
    console.warn(`skip @randyphoa/wxctl-${t.slug}: no binary at ${src}`);
    continue;
  }
  const dir = path.join(out, t.slug);
  mkdirSync(path.join(dir, 'bin'), { recursive: true });
  const pkg = structuredClone(template);
  pkg.name = `@randyphoa/wxctl-${t.slug}`;
  pkg.version = version;
  pkg.description = `wxctl native binary for ${t.triple}`;
  pkg.os = [t.os];
  pkg.cpu = [t.cpu];
  if (t.libc) pkg.libc = [t.libc];
  else delete pkg.libc;
  pkg.bin = { wxctl: `bin/${binName}` };
  writeFileSync(path.join(dir, 'package.json'), `${JSON.stringify(pkg, null, 2)}\n`);
  copyFileSync(src, path.join(dir, 'bin', binName));
  // npm preserves the tarball file mode, so a non-executable staged Unix binary
  // would install as EACCES. Set 0755 explicitly.
  if (!t.win) chmodSync(path.join(dir, 'bin', binName), 0o755);
  if (existsSync(license)) copyFileSync(license, path.join(dir, 'LICENSE'));
  generated++;
}
if (generated === 0) {
  fail(`no target binaries found under ${artifacts}; nothing to generate`);
}
console.log(`generated meta + ${generated} sub-package(s) at ${version} into ${out}`);
