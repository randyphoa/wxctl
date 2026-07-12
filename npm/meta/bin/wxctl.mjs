#!/usr/bin/env node
// npm meta shim: resolve the host platform sub-package, mark the install method,
// and spawn-and-forward to the native wxctl binary. Never writes into
// node_modules (invariant I2): resolution + spawn only.

import { spawn } from 'node:child_process';
import { existsSync } from 'node:fs';
import { createRequire } from 'node:module';
import path from 'node:path';

const require = createRequire(import.meta.url);

// Linux glibc vs musl. process.report.getReport().header.glibcVersionRuntime is
// present on glibc and absent on musl (the same signal detect-libc uses).
// Returns the sub-package name suffix word: 'gnu' on glibc, 'musl' on musl.
// Assumes glibc if the report is somehow unavailable (glibc is the common host).
function linuxLibc() {
  try {
    return process.report.getReport().header.glibcVersionRuntime ? 'gnu' : 'musl';
  } catch {
    return 'gnu';
  }
}

// Exactly one sub-package name for the host, no cross-variant fallback.
// darwin/win32: @randyphoa/wxctl-<os>-<arch>.
// linux: @randyphoa/wxctl-linux-<arch>-<libc> for the DETECTED libc only.
// Only -gnu is published today (glibc-only, P4). A musl host resolves -musl,
// finds nothing, and errors cleanly below. It must NEVER fall back to the -gnu
// binary: a libc-blind package manager may have installed -gnu, but the glibc
// binary cannot exec on musl, so running it would reproduce the cryptic crash
// this shim exists to prevent.
function hostPackage() {
  const arch = process.arch;
  switch (process.platform) {
    case 'darwin':
    case 'win32':
      return `@randyphoa/wxctl-${process.platform}-${arch}`;
    case 'linux':
      return `@randyphoa/wxctl-linux-${arch}-${linuxLibc()}`;
    default:
      return null;
  }
}

function unresolved() {
  const libc = process.platform === 'linux' ? `/${linuxLibc()}` : '';
  process.stderr.write(`no prebuilt wxctl binary for ${process.platform}/${process.arch}${libc}; ` + `see https://github.com/randyphoa/wxctl/releases, install via the curl | sh script, or build from source\n`);
  process.exit(1);
}

const pkg = hostPackage();
const binName = process.platform === 'win32' ? 'wxctl.exe' : 'wxctl';
let binPath;
if (pkg) {
  try {
    const p = path.join(path.dirname(require.resolve(`${pkg}/package.json`)), 'bin', binName);
    if (existsSync(p)) binPath = p;
  } catch {
    // sub-package not installed for this host (musl, --omit=optional, or an
    // unsupported target); fall through to the clean unresolved() error.
  }
}
if (!binPath) unresolved();

const child = spawn(binPath, process.argv.slice(2), {
  stdio: 'inherit',
  env: { ...process.env, WXCTL_INSTALL_METHOD: 'npm' },
});

for (const sig of ['SIGINT', 'SIGTERM', 'SIGHUP']) {
  process.on(sig, () => {
    if (!child.killed) child.kill(sig);
  });
}

child.on('error', (err) => {
  process.stderr.write(`failed to launch wxctl: ${err.message}\n`);
  process.exit(1);
});

child.on('exit', (code, signal) => {
  if (signal) {
    // Reflect a signal death in our own exit status rather than a bare 0.
    // Remove our own forwarding handlers first, otherwise the re-raise below is
    // caught by them (a no-op, since the child is already dead) and the process
    // exits 0. With them gone, the re-raised signal terminates this process so
    // the parent shell observes 128 + signum.
    for (const s of ['SIGINT', 'SIGTERM', 'SIGHUP']) process.removeAllListeners(s);
    process.kill(process.pid, signal);
  } else {
    process.exit(code ?? 0);
  }
});
