# Contributing to Cannery Row

Cannery Row is open source but not open to contributions yet: it is still in active development and its interfaces change without notice. Pull requests from outside the project are closed without review. Bug reports and questions are welcome as issues.

## Licences

See [LICENSE](LICENSE): the contracts under `contracts/` are Apache-2.0, everything else is AGPL-3.0-only. New files carry an SPDX header:

```
// SPDX-License-Identifier: AGPL-3.0-only
```

or `Apache-2.0` under `contracts/`.

## Building and testing

Cargo builds run dependency code (build scripts and procedural macros), so build and test in a container or another environment you trust with that code. The web app is embedded at compile time: build it before the Rust workspace.

```sh
tests/references/unpack.sh   # the frozen expected outputs many tests read
(cd web && npm ci && npm run lint && npm run typecheck && npm run test && npm run api:types:check && npm run build)
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace
```

Tests that need PostgreSQL are `#[ignore]`d. `ci/database-tests.sh` runs them, each against a fresh migrated database that `cannery-test-launcher` creates and drops; [tests/references/README.md](tests/references/README.md) explains the frozen references. After changing a query or a migration, regenerate the `.sqlx` metadata with `tools/sqlx-cli`; after changing an API route or model, regenerate `web/openapi.json` and the web client types (`dev/openapi.sh`). [docs/rust/operations.md](docs/rust/operations.md) has the details, and `.github/workflows/` the exact CI steps.
