# wxctl npm packaging

Maintainer notes for the npm distribution channel. Additive: it does not change
`release.yml`'s GitHub-release job or the `curl | sh` installer.

## Layout

- `meta/` is the `wxctl` meta package users install. `bin/wxctl.mjs` resolves the
  host platform sub-package and spawn-and-forwards to the native binary, setting
  `WXCTL_INSTALL_METHOD=npm`. `optionalDependencies` pins all six sub-packages;
  npm keeps only the one matching the host `os`/`cpu` (and `libc` on Linux). On
  Linux the shim detects glibc vs musl at runtime and resolves the matching
  variant only (no cross-variant fallback), so a musl host gets a clean error
  instead of a glibc binary that cannot exec. The shim never writes into
  `node_modules` (resolution + spawn only).
- `platform/package.json` is the template for the six sub-packages. The generator
  fills `name`, `version`, `description`, `os`, `cpu`, `libc`, and `bin`.
- `generate.mjs` stamps the crate version into all seven `package.json` files,
  copies each built binary into its sub-package, and sets the executable bit on
  the Unix binaries. Single source of version truth; no hand-editing.

## Target to sub-package

The two Linux sub-package names carry a `-gnu` suffix; the npm `libc` field value
is `glibc` (it differs from the `gnu` name word). macOS and Windows carry no
`libc` field.

| Rust target | sub-package | os | cpu | libc |
|---|---|---|---|---|
| `x86_64-apple-darwin` | `@randyphoa/wxctl-darwin-x64` | `darwin` | `x64` | n/a |
| `aarch64-apple-darwin` | `@randyphoa/wxctl-darwin-arm64` | `darwin` | `arm64` | n/a |
| `x86_64-unknown-linux-gnu` | `@randyphoa/wxctl-linux-x64-gnu` | `linux` | `x64` | `glibc` |
| `aarch64-unknown-linux-gnu` | `@randyphoa/wxctl-linux-arm64-gnu` | `linux` | `arm64` | `glibc` |
| `x86_64-pc-windows-msvc` | `@randyphoa/wxctl-win32-x64` | `win32` | `x64` | n/a |
| `aarch64-pc-windows-msvc` | `@randyphoa/wxctl-win32-arm64` | `win32` | `arm64` | n/a |

## Generate and publish

    node npm/generate.mjs --version <x.y.z> --artifacts <dir> --out <dir>

The generator expects each target's binary at `<artifacts>/<target-triple>/wxctl`
(or `wxctl.exe` on win32). It skips any target whose binary is absent, so a
single-host run produces `meta/` plus the host sub-package only. It refuses any
version that is not a plain `MAJOR.MINOR.PATCH`, is below `0.1.1` (`0.1.0` is the
reserved placeholder), or differs from the workspace crate version.

Publishing runs in CI (`.github/workflows/release.yml`, `npm-publish` job): the
six sub-packages first, then the meta, each `npm publish --provenance` and
guarded by an `npm view` existence check so a re-run after a partial failure is
idempotent. `0.1.0` is already published as a name reservation, so the first
functional release is `0.1.1`.

## Auth: OIDC trusted publishing (no NPM_TOKEN)

The `npm-publish` job authenticates with npm through GitHub OIDC, not a long-lived
`NPM_TOKEN`. It sets `permissions: id-token: write` and upgrades npm to `>= 11.5.1`
(the first version with OIDC trusted publishing) on the runner's pre-installed
Node. npm then exchanges the workflow's OIDC token for a short-lived publish token
and signs the `--provenance` attestation with it. No secret is stored in the repo.

Trusted publishing has a one-time setup, because OIDC cannot create a package name
that does not yet exist:

1. Reserve every name once, with a token. `wxctl` is already reserved at `0.1.0`.
   The six `@randyphoa/wxctl-<slug>` sub-package names must likewise be published
   once as bare `0.1.0` stubs (via `npm login`, then `npm publish --access public`)
   before any OIDC release. `generate.mjs` refuses to build `0.1.0`, so these
   reservation stubs are hand-authored, not generated. The first functional release
   (`0.1.1`) is also published by hand with a token; every release after that uses
   OIDC.
2. Configure a trusted publisher on all seven packages. On npmjs.com, open each
   package, go to Settings, Trusted Publisher, GitHub Actions, and set the
   organization or user to `randyphoa`, the repository to `wxctl`, and the workflow
   filename to `release.yml` (leave the environment blank). npm validates the OIDC
   claim against these fields at publish time.

The updated `release.yml` must be live in the public repo before the release tag
fires, since the trusted-publisher check validates against the workflow that
actually runs.

## Linux is glibc-only (Alpine/musl deferred)

The Linux set ships glibc (`-gnu`) binaries only, for x64 and arm64. On a musl
host (Alpine) the shim's runtime glibc-vs-musl detection resolves the `-musl`
name, finds nothing, and exits with a clear "no musl build" error rather than
running the glibc binary. This matches `curl | sh`, which is also glibc-only.

Adding musl later is a purely additive drop-in with no rename: two build-matrix
rows (`x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`) and two
`@randyphoa/wxctl-linux-<arch>-musl` sub-packages with `libc: ["musl"]`. The
`-gnu` suffix and the shim's per-libc resolution already reserve the shape, so no
existing package name or code path changes.
