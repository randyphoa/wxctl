#!/usr/bin/env bash
# Shared helpers for the npm E2E harness (e2e.sh, e2e-cli.sh). Sourced, not run.
# Result accounting: pass/fail/skip increment counters; the entrypoint exits
# non-zero if FAIL > 0. No private paths (SI2): everything resolves under wxctl/.

PASS=0; FAIL=0; SKIP=0
pass() { PASS=$((PASS + 1)); printf 'PASS  %s\n' "$1"; }
fail() { FAIL=$((FAIL + 1)); printf 'FAIL  %s\n' "$1" >&2; }
skip() { SKIP=$((SKIP + 1)); printf 'SKIP  %s\n' "$1"; }

# This file's dir (the npm harness) and the wxctl workspace root.
NPM_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WXCTL="$(cd "$NPM_DIR/.." && pwd)"

# Host sub-package slug, computed exactly as the shim's hostPackage() does.
host_slug() {
  node -e '
    const a = process.arch, p = process.platform;
    let libc = "";
    if (p === "linux") { try { libc = process.report.getReport().header.glibcVersionRuntime ? "-gnu" : "-musl"; } catch { libc = "-gnu"; } }
    process.stdout.write(p === "linux" ? `linux-${a}${libc}` : `${p}-${a}`);
  '
}

# version_ge A B -> exit 0 if A >= B (semver, via sort -V).
version_ge() { [ "$(printf '%s\n%s\n' "$1" "$2" | sort -V | tail -1)" = "$1" ]; }

# build_fixture <dest> <bin-src> <slug>: lay down node_modules/wxctl (real shim +
# meta package.json) and @randyphoa/wxctl-<slug>/bin/wxctl (bin-src, +x).
build_fixture() {
  local dest="$1" bin="$2" slug="$3"
  mkdir -p "$dest/node_modules/wxctl/bin" "$dest/node_modules/@randyphoa/wxctl-$slug/bin"
  cp "$WXCTL/npm/meta/package.json" "$dest/node_modules/wxctl/package.json"
  cp "$WXCTL/npm/meta/bin/wxctl.mjs" "$dest/node_modules/wxctl/bin/wxctl.mjs"
  cp "$WXCTL/npm/platform/package.json" "$dest/node_modules/@randyphoa/wxctl-$slug/package.json"
  cp "$bin" "$dest/node_modules/@randyphoa/wxctl-$slug/bin/wxctl"
  chmod +x "$dest/node_modules/@randyphoa/wxctl-$slug/bin/wxctl"
}
