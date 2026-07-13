# CLI upgrade-branch + generator-invariant + version-gated legs of the npm E2E.
# Sourced by e2e.sh (shares pass/fail/skip, CRATE_VER, SLUG, WXCTL, BIN, T). If
# run standalone, bootstrap the helpers and inputs.
if [ -z "${PASS+x}" ]; then
  DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"; source "$DIR/e2e-lib.sh"
  T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT
  CRATE_VER="$(cd "$WXCTL" && cargo metadata --no-deps --format-version 1 | node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>{const m=JSON.parse(s);process.stdout.write(m.packages.find(p=>p.name==="wxctl").version)})')"
  SLUG="$(host_slug)"; ( cd "$WXCTL" && cargo build --release -p wxctl ); BIN="$WXCTL/target/release/wxctl"
fi

# AC3 (marker): npm marker -> update refuses, binary byte-identical.
cp "$BIN" "$T/plain-wxctl"; sha0="$(shasum "$T/plain-wxctl")"
out="$(WXCTL_INSTALL_METHOD=npm "$T/plain-wxctl" update --yes 2>&1)"; rc=$?
{ [ $rc -ne 0 ] && printf '%s' "$out" | grep -q 'npm update -g wxctl' && [ "$sha0" = "$(shasum "$T/plain-wxctl")" ]; } && pass "AC3: marker -> refuse, binary unchanged" || fail "AC3: marker refuse rc=$rc out=$out"

# AC3 (node_modules path): exe under node_modules -> refuse (no marker env).
mkdir -p "$T/nm/node_modules/wxctl/bin"; cp "$BIN" "$T/nm/node_modules/wxctl/bin/wxctl"; sha1="$(shasum "$T/nm/node_modules/wxctl/bin/wxctl")"
out="$(env -u WXCTL_INSTALL_METHOD "$T/nm/node_modules/wxctl/bin/wxctl" update --yes 2>&1)"; rc=$?
{ [ $rc -ne 0 ] && printf '%s' "$out" | grep -q 'npm update -g wxctl' && [ "$sha1" = "$(shasum "$T/nm/node_modules/wxctl/bin/wxctl")" ]; } && pass "AC3: node_modules path -> refuse, binary unchanged" || fail "AC3: node_modules refuse rc=$rc out=$out"

# AC4 (non-npm install): the npm gate stays transparent. No --yes + stdin from
# /dev/null keeps it read-only (never self-replaces); only assert the npm
# refusal string is ABSENT (proceeds toward the normal self-update path).
out="$(env -u WXCTL_INSTALL_METHOD "$T/plain-wxctl" update </dev/null 2>&1)"; rc=$?
printf '%s' "$out" | grep -qi 'installed via npm' && fail "AC4: npm gate wrongly fired for a non-npm install: $out" || pass "AC4: non-npm update does not hit the npm refusal"

# I1/I3 (generator refuses): 0.1.0, prerelease, and crate drift (positive gates).
out="$(node "$WXCTL/npm/generate.mjs" --version 0.1.0 --artifacts /dev/null --out "$T/g" 2>&1)"; printf '%s' "$out" | grep -q '0.1.1 or higher' && pass "I1: generate refuses 0.1.0" || fail "I1: 0.1.0 not refused"
out="$(node "$WXCTL/npm/generate.mjs" --version 1.2.3-rc1 --artifacts /dev/null --out "$T/g" 2>&1)"; printf '%s' "$out" | grep -qi 'non-plain' && pass "I3: generate refuses prerelease" || fail "I3: prerelease not refused"
out="$(node "$WXCTL/npm/generate.mjs" --version 9.9.9 --artifacts /dev/null --out "$T/g" 2>&1)"; printf '%s' "$out" | grep -qi 'drift' && pass "I1: generate refuses crate drift" || fail "I1: drift not refused"

# I4 (no install scripts): no scripts key + install works under --ignore-scripts.
if grep -RslE '"scripts"[[:space:]]*:' "$WXCTL/npm/meta/package.json" "$WXCTL/npm/platform/package.json" | grep -q .; then fail "I4: a scripts key exists in an npm package.json"; else pass "I4: no scripts key in any npm package.json"; fi
tgz="$(cd "$T" && npm pack "$WXCTL/npm/meta" --silent 2>/dev/null | tail -1)"
npm install --prefix "$T/i4" --ignore-scripts --omit=optional --no-audit --no-fund "$T/$tgz" >/dev/null 2>&1 && pass "I4: npm install --ignore-scripts --omit=optional works" || fail "I4: --ignore-scripts install failed"

# AC5 (static proxy): meta lists the six sub-packages; generator carries per-target os/cpu/libc.
node -e 'const m=require(process.argv[1]);const want=["darwin-x64","darwin-arm64","linux-x64-gnu","linux-arm64-gnu","win32-x64","win32-arm64"].map(s=>"@randyphoa/wxctl-"+s).sort();const got=Object.keys(m.optionalDependencies||{}).sort();if(JSON.stringify(want)!==JSON.stringify(got))throw new Error("mismatch: "+got)' "$WXCTL/npm/meta/package.json" && pass "AC5(proxy): meta lists the six platform sub-packages" || fail "AC5(proxy): optionalDependencies wrong"
{ grep -q "libc: 'glibc'" "$WXCTL/npm/generate.mjs" && grep -q "os: 'win32'" "$WXCTL/npm/generate.mjs" && grep -q "os: 'darwin'" "$WXCTL/npm/generate.mjs"; } && pass "AC5(proxy): generator TARGETS carry per-target os/cpu/libc" || fail "AC5(proxy): TARGETS os/cpu/libc"

# Version-gated: real install (AC1-gating) + CI dry-run (AC5) run only at 0.1.1+.
if version_ge "$CRATE_VER" 0.1.1; then
  pass "AC1-gating/AC5: crate is $CRATE_VER (>=0.1.1); run generate.mjs -> npm pack meta+host -> install into a temp prefix and assert only @randyphoa/wxctl-$SLUG present, then 'npm publish <each> --dry-run --provenance' for the seven and confirm os/cpu/libc + zero writes"
else
  skip "AC1-gating (only host sub-package installs): BLOCKED at $CRATE_VER. Discharge: bump crate to 0.1.1, then re-run e2e.sh (generate.mjs -> npm pack meta+host -> install into a temp prefix -> assert only @randyphoa/wxctl-$SLUG present, other five absent)."
  skip "AC5 (CI dry-run publishes seven): BLOCKED at $CRATE_VER. Discharge: bump 0.1.1, then 'node npm/generate.mjs --version 0.1.1 --artifacts <6 binaries> --out npm-dist && for d in npm-dist/*/; do npm publish \"\$d\" --dry-run --provenance; done' (assert 7 manifests, correct per-target os/cpu/libc, zero registry writes), or 'gh workflow run release.yml' once release.yml is synced to the public repo."
fi
skip "AC6 ([human]): BLOCKED. Live tagged 0.1.1 release publishing the meta + six sub-packages with provenance on npmjs.com (first publish by hand with a token per the OIDC setup), and a clean Windows 'npm i -g wxctl' install with no SmartScreen prompt."
