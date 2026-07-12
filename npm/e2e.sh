#!/usr/bin/env bash
# End-to-end verification for the wxctl npm distribution channel
# (spec 2026-07-09-wxctl-npm-distribution). Single entrypoint: rebuilds the
# binary, builds hand-made node_modules fixtures (no published packages needed),
# and exercises the shim, the CLI upgrade branch, and the generator invariants.
# Version-gated legs (real npm install, CI dry-run) SKIP at crate < 0.1.1 and go
# live once the crate is bumped, so this file is also the discharge tool. No
# private paths, no auth, no registry writes.
set -uo pipefail
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$DIR/e2e-lib.sh"

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT
CRATE_VER="$(cd "$WXCTL" && cargo metadata --no-deps --format-version 1 | node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>{const m=JSON.parse(s);process.stdout.write(m.packages.find(p=>p.name==="wxctl").version)})')"
SLUG="$(host_slug)"
echo "crate=$CRATE_VER host-slug=$SLUG"

# (0) Fresh release build (a prior planner found the target binary stale).
( cd "$WXCTL" && cargo build --release -p wxctl ) || fail "cargo build --release -p wxctl"
BIN="$WXCTL/target/release/wxctl"
[ -x "$BIN" ] || fail "no built binary at $BIN"

# Fixture with the REAL binary (AC1, marker-through-shim, I2).
build_fixture "$T/real" "$BIN" "$SLUG"
SHIM_REAL="$T/real/node_modules/wxctl/bin/wxctl.mjs"

# AC1 (shim core): the meta shim resolves the host sub-package and prints the version.
out="$(node "$SHIM_REAL" --version 2>&1)"; rc=$?
{ [ $rc -eq 0 ] && printf '%s' "$out" | grep -q "$CRATE_VER"; } && pass "AC1: shim runs host binary, prints $CRATE_VER" || fail "AC1: shim --version rc=$rc out=$out"

# AC1/AC3 (marker reaches the real process THROUGH the shim): update refuses.
out="$(node "$SHIM_REAL" update --yes 2>&1)"; rc=$?
{ [ $rc -ne 0 ] && printf '%s' "$out" | grep -q 'npm update -g wxctl'; } && pass "AC1/AC3: WXCTL_INSTALL_METHOD=npm reaches the process; update refuses via shim" || fail "AC1/AC3: through-shim update rc=$rc out=$out"

# I2 (shim never writes node_modules): tree byte-identical across a shim run.
man() { find "$T/real/node_modules" -type f -exec shasum {} \; | sort; }
before="$(man)"; node "$SHIM_REAL" --version >/dev/null 2>&1; after="$(man)"
[ "$before" = "$after" ] && pass "I2: node_modules unchanged after shim run" || fail "I2: node_modules mutated by shim"

# Fixture with the controllable fake child (AC2 fidelity legs).
build_fixture "$T/fake" "$DIR/e2e-child.sh" "$SLUG"
SHIM_FAKE="$T/fake/node_modules/wxctl/bin/wxctl.mjs"

[ "$(node "$SHIM_FAKE" --argv a b c 2>/dev/null)" = "argv:a b c" ] && pass "AC2: argv passthrough" || fail "AC2: argv passthrough"
[ "$(printf 'hello\n' | node "$SHIM_FAKE" --cat 2>/dev/null)" = "hello" ] && pass "AC2: stdio passthrough" || fail "AC2: stdio passthrough"
node "$SHIM_FAKE" --exit 7 >/dev/null 2>&1; [ $? -eq 7 ] && pass "AC2: exit-code passthrough" || fail "AC2: exit-code passthrough"
[ "$(node "$SHIM_FAKE" --env 2>/dev/null)" = "WXCTL_INSTALL_METHOD=npm" ] && pass "AC2: shim marks child env npm" || fail "AC2: child env marker"

# AC2 (SUSPECTED DEFECT, constraint E): SIGTERM to the shim must reflect as 143
# (128+15). A buggy shim (re-raise caught by its own still-installed handler)
# exits 0 or hangs; the watchdog turns a hang into a non-143 failure too.
node "$SHIM_FAKE" --sleep 30 & shim_pid=$!
for _ in $(seq 1 50); do pgrep -P "$shim_pid" >/dev/null 2>&1 && break; node -e 'setTimeout(()=>{},100)'; done
kill -TERM "$shim_pid" 2>/dev/null
( node -e 'setTimeout(()=>{},5000)'; kill -9 "$shim_pid" 2>/dev/null ) & wd=$!
wait "$shim_pid"; sig_rc=$?; kill "$wd" 2>/dev/null || true
[ "$sig_rc" -eq 143 ] && pass "AC2: SIGTERM reflected as 143" || fail "AC2: signal reflection expected 143 got $sig_rc (suspected Phase-1 shim defect, constraint E)"

# AC2 (unresolvable platform): remove the host sub-package -> clean exit-1, no stack trace.
mv "$T/fake/node_modules/@randyphoa/wxctl-$SLUG" "$T/fake/hidden"
out="$(node "$SHIM_FAKE" --version 2>&1)"; rc=$?
{ [ $rc -eq 1 ] && printf '%s' "$out" | grep -q 'no prebuilt wxctl binary' && ! printf '%s' "$out" | grep -qE '^[[:space:]]+at '; } && pass "AC2: unresolvable -> clean exit-1 message, no stack trace" || fail "AC2: unresolvable rc=$rc out=$out"
mv "$T/fake/hidden" "$T/fake/node_modules/@randyphoa/wxctl-$SLUG"

# musl-awareness (AC1/AC2): force a linux/musl host; the shim resolves -musl,
# finds nothing, errors cleanly, and NEVER spawns the present -gnu binary.
mkdir -p "$T/musl/node_modules/wxctl/bin" "$T/musl/node_modules/@randyphoa/wxctl-linux-arm64-gnu/bin"
cp "$WXCTL/npm/meta/bin/wxctl.mjs" "$T/musl/node_modules/wxctl/bin/wxctl.mjs"
cp "$WXCTL/npm/platform/package.json" "$T/musl/node_modules/@randyphoa/wxctl-linux-arm64-gnu/package.json"
MARK="$T/musl/SPAWNED"; printf '#!/bin/sh\necho spawned > "%s"\n' "$MARK" > "$T/musl/node_modules/@randyphoa/wxctl-linux-arm64-gnu/bin/wxctl"; chmod +x "$T/musl/node_modules/@randyphoa/wxctl-linux-arm64-gnu/bin/wxctl"
out="$(node "$DIR/e2e-force-libc.mjs" arm64 "$T/musl/node_modules/wxctl/bin/wxctl.mjs" 2>&1)"; rc=$?
{ [ $rc -eq 1 ] && printf '%s' "$out" | grep -q 'linux/arm64/musl' && [ ! -e "$MARK" ] && ! printf '%s' "$out" | grep -qE '^[[:space:]]+at '; } && pass "musl-awareness: clean error, glibc binary never spawned" || fail "musl-awareness rc=$rc mark=$([ -e "$MARK" ] && echo yes || echo no) out=$out"

# CLI-refusal, generator-invariant, and version-gated legs (sourced: shares
# counters + CRATE_VER/SLUG/WXCTL/BIN/T).
source "$DIR/e2e-cli.sh"

echo "-- e2e.sh: PASS=$PASS FAIL=$FAIL SKIP=$SKIP --"
[ $FAIL -eq 0 ]
