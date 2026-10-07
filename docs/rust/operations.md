# Building and operations

How to build, release, configure and deploy the `cannery` binary, and what CI checks. The product-level deployment reference (every setting, the runner launchers, the stock evaluator) is [deploy.md](../deploy.md); this document adds what is specific to building and verifying the binary.

## Commands

| Command | Purpose |
| --- | --- |
| `cannery serve [--host 0.0.0.0] [--port 8000] [--forwarded-allow-ips IPS]` | Run the API, browser login, MCP and the embedded web app. Only the listed proxy addresses are trusted for forwarded headers (default `127.0.0.1,::1`, or `FORWARDED_ALLOW_IPS`). With a managed database it applies pending migrations first. |
| `cannery migrate` | Apply pending migrations; prints `applied N migration(s)`. |
| `cannery db dump FILE`, `cannery db restore FILE [--replace]`, `cannery db upgrade --from-bin-dir DIR` | Dump the database; restore or upgrade the managed database ([deploy.md](../deploy.md#managed-database)). |
| `cannery openapi [-o FILE]` | Write the OpenAPI document. Needs no settings or database. |
| `cannery runner …` | Claim and run jobs (see [runner deployment](#runner-deployment-and-policy)). |
| `cannery evaluator --config POLICY --api-url URL --project SLUG [--token-file FILE] [--poll-seconds 10] [--once]` | Run only the stock evaluator. The token file can also come from `CANNERY_EVALUATOR_TOKEN_FILE`. Step policies are refused before any claim. |
| `cannery import …` | Import a historical research bundle ([import.md](../import.md)). |

The global `--settings FILE` option, or `CANNERY_SETTINGS`, names the settings TOML; `CANNERY_<SECTION>_<FIELD>` environment variables override it. [deploy.md](../deploy.md#configuration) lists the settings; `settings.example.toml` shows the common ones. Supply secrets through environment variables or private files only.

## Building

Cargo builds execute dependency code (build scripts and procedural macros), so run builds and tests in a container or another environment you trust with that code. A reproducible way is two phases:

1. Fetch with network and a writable cache, lifecycle scripts disabled, then audit: `cargo fetch --locked`, `cargo deny --locked check` (or `cargo audit`), `npm ci --ignore-scripts` and `npm audit` in `web/`. Stop on any critical finding.
2. Build and test offline with a read-only cache: `CARGO_NET_OFFLINE=true`, `SQLX_OFFLINE=true`, `CARGO_INCREMENTAL=0`, `CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_PROFILE_TEST_DEBUG=0`.

Order of work:

1. Build the web app: in `web/`, `npm rebuild`, `npm run api:types:check`, `npm run build`. The server embeds `web/dist` at compile time with `rust-embed`, so it must exist before compiling `cannery-server`. Cargo never runs Node.
2. Build and test the workspace:

   ```sh
   cargo fmt --all --check
   cargo clippy --locked --offline --workspace --all-targets --all-features -- -D warnings
   cargo clippy --locked --offline --workspace --all-targets -- -D warnings
   cargo test --locked --offline --workspace
   cargo build --locked --offline -p cannery --bin cannery
   ```

3. Tests that need PostgreSQL are `#[ignore]` and take their database URL from an environment variable. Give each run a fresh database migrated with the built CLI (`cannery migrate`), and drop it afterwards.

The committed `.sqlx` directory holds the query descriptions for offline builds. After changing a query or a migration, regenerate it against a freshly migrated database with the standalone SQLx CLI in `tools/sqlx-cli` (upstream `sqlx-cli` 0.9.0, its own lockfile and `deny.toml`; it is never linked into the release):

```sh
PATH="$PWD/tools/sqlx-cli/target/debug:$PATH" \
  CARGO_NET_OFFLINE=true SQLX_OFFLINE=false DATABASE_URL="$CANNERY_DATABASE_URL" \
  cargo sqlx prepare --workspace -- --locked --all-targets --all-features
```

CI runs the same command with `--check`.

### Release builds

```sh
cargo build --locked --offline --release --no-default-features \
  -p cannery --bin cannery --target x86_64-unknown-linux-musl
cargo build --locked --offline --release --no-default-features \
  -p cannery --bin cannery --target aarch64-unknown-linux-musl
MACOSX_DEPLOYMENT_TARGET=11.0 \
  cargo build --locked --offline --release --no-default-features \
  -p cannery --bin cannery --target aarch64-apple-darwin
```

- Linux builds run natively on amd64 and arm64 inside the digest-pinned official Rust 1.99.0 Alpine 3.24 image, whose musl toolchain avoids a cross compiler. The binary must have no ELF interpreter and no `NEEDED` libraries.
- macOS builds run natively on ARM with Rust 1.99.0. The binary must be arm64 and link no external OpenSSL or PostgreSQL library.
- The dependency graph must keep rustls with ring and exclude OpenSSL and aws-lc (`deny.toml` bans `openssl-sys` and `aws-lc-sys`). Each build records its target feature graph in `features.txt`.
- Release builds use `--no-default-features`, so they never contain the `conformance-testing` feature.

Each release bundle (`cannery-<target>.tar.gz`) contains the binary, `checksums.txt` (SHA-256), `Cargo.lock`, `features.txt` and `licenses/` with the third-party notices from `crates/server/licenses/` (for the embedded API documentation and OAuth redirect pages).

### Bundled PostgreSQL

The `bundled-postgres` feature of the `cannery` crate embeds a PostgreSQL distribution for `[database] provider = "managed"` ([deploy.md](../deploy.md#managed-database)). Build the payload first, then the binary:

```sh
dev/build-postgres-bundle.sh OUT_DIR
CANNERY_POSTGRES_BUNDLE=/abs/path/postgresql-17.11-linux-x86_64.tar.zst \
  cargo build --locked --offline --release -p cannery --features bundled-postgres
```

- `dev/build-postgres-bundle.sh` downloads the pinned source tarball and checks its SHA-256 (PostgreSQL publishes checksums, not signatures), fetches `bison`, `flex` and `zstd` through apt (verified against the signed Debian archive), then runs `dev/postgres-bundle-build.sh` offline in the `buildpack-deps:bookworm` image pinned by digest for the host architecture. CI runs the same inner script. PostgreSQL 17 tarballs no longer ship generated parsers, so `bison` (2.3+) and `flex` are needed on every platform.
- There are three bundles, named `postgresql-<version>-<os>-<arch>.tar.zst`: `linux-x86_64` and `linux-aarch64`, each built natively, and `macos-aarch64`, built by the same script directly on macOS with the Xcode command line tools (their `bison` 2.3 is enough), GNU tar, `zstd` and `xz`, with `MACOSX_DEPLOYMENT_TARGET=11.0`. A bundle runs only on the platform it was built for, so build `cannery` with the bundle for its target.
- The build is configured without OpenSSL, ICU, readline, zlib, compression, XML, Kerberos, LDAP, PAM, systemd and LLVM. On Linux the binaries need only glibc 2.34+ (`libc`, `libm`) and the bundled `libpq`, found through `RUNPATH=$ORIGIN/../lib`. On macOS they need only `/usr/lib/libSystem.B.dylib` and the bundled `libpq`: the install names are rewritten to `@loader_path/../lib/libpq.5.dylib` (`@loader_path/libpq.5.dylib` for loadable modules) with `install_name_tool`, and every binary is then re-signed ad hoc, since arm64 macOS refuses an invalid signature. CI fails if `readelf`/`otool -L` shows anything else. Each bundle keeps `postgres`, `initdb`, `pg_ctl`, `pg_dump`, `pg_restore`, `pg_isready`, PL/pgSQL, `pg_trgm` and the time zone data, stripped: about 17 MB unpacked, 4.8 MB as `.tar.zst` on Linux x86_64. The archive is deterministic for a given platform and toolchain (same input, same SHA-256) and contains only regular files and directories.
- The build script reads the digest from the `.sha256` file next to the bundle and embeds both; at run time the payload is checked against that digest before it is unpacked. Without `CANNERY_POSTGRES_BUNDLE` the feature builds with a warning and no payload, so `--all-features` checks need nothing extra.
- The Linux bundles are glibc only: they do not run in the static musl image (`distroless/static`).
- `cannery db dump`, `db restore` and `db upgrade` run the bundle's `pg_dump` and `pg_restore` (`crates/managed-postgres/src/tools.rs`); without zlib they write and read only uncompressed archives, so `db dump` always passes `--compress=0`. `crates/cannery/tests/db_commands.rs` covers dump refused while locked, a dump and restore round trip with `pg_trgm`, refusal of a non-empty database, `--replace`, and an upgrade simulated with the same major.

`dev/local.sh` does all of this and runs the result: `dev/local.sh build` builds the web app (if `web/dist` is missing), the bundle (if `target/pg-bundle` has none) and a release `cannery` with `bundled-postgres`; `dev/local.sh serve` runs it attached in a glibc container with a shell (`initdb` needs `/bin/sh`), with a managed database and local storage, and sign-in through the OIDC client configured in `dev/local.env`. `dev/local.sh` alone does both. Deleting its data directory resets the instance.

### Image

`Containerfile.rust` builds from a small context prepared by the release workflow: the `cannery` binary, `licenses/` and an empty `empty-data/objects/`. It uses the digest-pinned `gcr.io/distroless/static-debian13:nonroot` base, runs as UID/GID 10001, sets `CANNERY_STORAGE_LOCAL_ROOT=/data/objects`, exposes port 8000, and has entrypoint `/cannery` with default command `serve`. There is no shell, source checkout, web tree, compiler or interpreter in the image; in Kubernetes use `args: [migrate]` for the init container and `args: [serve, --host, 0.0.0.0, --port, "8000"]` for the API.

## Release workflow

`.github/workflows/rust-release.yml` builds the release binaries and the image on pull requests and pushes. It checks:

- Audit of the production and `tools/sqlx-cli` dependency graphs and of the frozen npm graph, then an offline build of `web/dist` in a pinned Node image.
- Static Linux (amd64, arm64): offline static build; no interpreter or dynamic libraries; `--help`; `openapi` output equals `web/openapi.json`; `migrate` twice against PostgreSQL 17.11 (the second run applies 0). It assembles the image without pushing and smoke-tests it: nonroot user, CLI and OpenAPI output, migration-backed health, embedded web app and assets, security and cache headers, HEAD and byte ranges, reserved-prefix 404s, MCP method refusal, and absence of conformance routes.
- macOS arm64: native build; serves the embedded app from outside the checkout with an unavailable database and requires `/api/health` to return 503 with `"database":"unavailable"`; `/__conformance/sweep` must be 404.
- SQLx metadata: builds `tools/sqlx-cli`, migrates a fresh database and runs `cargo sqlx prepare --check` across all targets and features.

Workflow artifacts are validation outputs, not published releases.

## Runner deployment and policy

Runner configuration, launchers, GitHub credentials, the cache and the Kubernetes and Container-Optimized OS setups are documented in [deploy.md](../deploy.md#runner) and [deploy/runner-k8s/README.md](../../deploy/runner-k8s/README.md). Points that matter when operating the runner:

- Select the launcher explicitly: `--launcher local|docker|kubernetes` or the launcher in `--config`. `--unisolated-local` (or `--launcher local`) is the explicit opt-in for unisolated local execution.
- Docker and Kubernetes need a persistent `--runner-id`. Startup cleanup removes only resources labelled with that ID, so never reuse an ID that another running runner holds.
- Kubernetes needs an explicit API URL, namespace, private bearer-token file and cluster CA file; the runner does not infer in-cluster credentials and never loads kubeconfig or credential plugins. The token and CA files are reloaded by the client; the supplied manifests refresh a projected ServiceAccount token into a mode-0600 memory file every 15 seconds.
- Operator flags: `--default-step-cpu`, `--default-step-memory`, `--allow-unrestricted-egress`, `--k8s-namespace-policy-acknowledged`, `--k8s-api-service-host`, `--k8s-api-service-port`, `--docker-gpu-devices`. Their defaults and meaning are in [limits](limits.md#container-launchers).
- `cannery runner --config FILE --check-config` validates the configuration, policy files, private credential files and launcher factory without creating clients, claiming jobs or touching work and cache directories ([details](../../crates/runner/CHECK_CONFIG.md)). It does not prove TLS trust, cluster permissions, image availability or dataset readiness.
- Token files must not be group- or world-readable. Updating the runner's Cannery credential requires a runner restart; GitHub token files are reread after a 401.
- `cannery evaluator` and `kind = "eval"` with a stock policy need no database settings, launcher, data root or cache root.
- Recovery on the API side reclaims expired leases and claims; a runner that loses its lease abandons the job without reporting a failure.

## Continuous integration

CI runs on every pull request:

- Formatting (`cargo fmt`), strict Clippy with and without all features, and the workspace tests.
- Tests against real PostgreSQL databases, on the stock PostgreSQL 17 image and on the managed PostgreSQL bundle: the repository, HTTP and storage tests in `ci/database-tests.txt`, each against a fresh database created and migrated by `cannery-test-launcher`, compared with the frozen references in `tests/references/`.
- The runner's process, lease, transfer and policy-evaluator tests through the installed CLI.
- The web app: lint, type check, tests, `api:types:check` and production build.
- The managed PostgreSQL bundles: each bundle (Linux x86_64, Linux aarch64, macOS arm64) is built from the pinned source and must link only glibc or libSystem; the managed-database supervisor tests (first start, migrations, `pg_trgm`, lock, restart, stale and orphaned servers), the CLI tests (`migrate`, `serve` health, server stopped when `cannery` is killed on Linux or reclaimed by the next start on macOS, `db` commands), and a `bundled-postgres` build that unpacks and migrates.
- The release builds described [above](#release-workflow).

`tools/sqlx-cli` has its own `deny.toml` with the same license, advisory, ban and source rules; its duplicate-version exemptions are local to its graph. The CI step that runs a check is the authoritative recipe; read it in `.github/workflows/` when a local run disagrees.
