# SQLx metadata tooling

This standalone Cargo workspace exposes `cargo sqlx` through the unchanged `sqlx-cli` 0.9.0 library, with only its PostgreSQL and Rustls features enabled. The small argument adapter follows upstream's `cargo-sqlx` binary. Production SQLx remains separately pinned to 0.8.6 in the application workspace. The tooling workspace requires Rust 1.94 or newer; it does not change the application's minimum Rust version.

The committed tooling lockfile must pass its own dependency audit before executing a build. Version 0.8.6 of the CLI resolved an unused MySQL/RSA dependency with an unpatched vulnerability even through this library adapter. Version 0.9.0 makes that RSA feature optional. Do not suppress advisories or patch upstream dependencies to manufacture a passing audit.

The 0.9.0 tooling lock audits with zero known vulnerabilities. Cargo still records inactive MySQL and SQLite packages; the selected build graph excludes both, RSA, OpenSSL and AWS LC. The audit reports upstream's unmaintained `backoff` 0.4.0 (`RUSTSEC-2025-0012`) and its `instant` 0.1.13 dependency (`RUSTSEC-2024-0384`). These warnings are disclosed and are not suppressed by this workspace.

Fetch and audit this workspace's frozen graph first. Build with `cargo build --locked --offline --manifest-path tools/sqlx-cli/Cargo.toml`, then add `tools/sqlx-cli/target/debug` to the container's `PATH`. From the application workspace root, against a disposable database migrated with the application's migration command, run:

```sh
cargo sqlx --no-dotenv prepare --check --workspace -- \
  --locked --all-targets
```

Set `DATABASE_URL` for that disposable database. Preparation compiles the application's locked SQLx macros, so the metadata comes from version 0.8.6. Ordinary application builds use `SQLX_OFFLINE=true` and the committed `.sqlx` directory.

Compatibility was checked with PostgreSQL 17 against all application targets and all eight query descriptions in the initial migration implementation. A separate 0.8.6 macro fixture passed with matching metadata and returned exit status 1 for both missing metadata and a changed query description. These checks regenerate descriptions from the live database; they do not convert or rewrite the committed metadata. The adapter also registers the default drivers exactly as upstream's 0.9.0 binary does.

The application subsequently passed the same check against all nine committed descriptions, including health, on a fresh database migrated by the Rust application.

Run dependency work in a container: fetch and audit with networking and a writable Cargo cache; compile and check offline with a read-only cache. This helper does not install a global executable. The stock metadata check fails on missing or changed query descriptions and warns about unused descriptions; CI can additionally compare exact query filename sets.
