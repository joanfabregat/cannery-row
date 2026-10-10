# Rust implementation architecture

This is the entry point for engineers working on the implementation. It describes how the Rust workspace is built and how a request moves through it. The other documents in this directory cover the rest:

| Document | Holds |
| --- | --- |
| [limits.md](limits.md) | Request budgets, accepted encodings, schema and OIDC support, configuration grammar, and runner and launcher policies. |
| [operations.md](operations.md) | Building, releasing, configuring and deploying the binary, and the CI gates with their local reproduction. |

The product contracts themselves (records, documents, roles, the container contract) live in [spec.md](../spec.md), [contracts.md](../contracts.md) and [deploy.md](../deploy.md).

## Workspace layout

One Cargo workspace (`Cargo.toml`, edition 2024, minimum Rust 1.94.1, built with Rust 1.99.0) produces one binary, `cannery`. `unsafe_code` is forbidden workspace-wide. Every dependency is pinned with `=` in `[workspace.dependencies]` and the lockfile is frozen.

| Crate | Responsibility |
| --- | --- |
| `crates/cannery` | The command line: `serve`, `migrate`, `db`, `openapi`, `runner`, `evaluator`, `import`. |
| `crates/server` | The Axum HTTP application: REST routes, browser login, MCP at `/mcp`, the embedded web distribution, OpenAPI generation, background recovery. |
| `crates/core` | Settings, database connection and migrations, the bounded JSON `Document`, canonical evidence bytes, published-contract validation and the per-phase front matter schemas, strict YAML and front matter documents, audit, IDs, pagination, timestamps. |
| `crates/identity` | Users, sessions, personal and service tokens, login requests, opaque credential minting. |
| `crates/projects` | Projects, memberships and project-scoped authorization. |
| `crates/research` | Science and dashboard revisions, step manifests and their checks, interfaces, job baselines, deadlines and job documents. |
| `crates/tracks`, `crates/units`, `crates/attempts`, `crates/jobs`, `crates/metrics`, `crates/search`, `crates/reviews`, `crates/attention`, `crates/comments-reports` | Persistence for one domain each. They take a caller-owned `PgConnection`; transactions, authorization and audit belong to the caller. |
| `crates/storage` | The object store: local filesystem and S3 (official AWS SDK), presigning and multipart. |
| `crates/runner` | The runner and the stock policy: worker scheduler, HTTP client, code and setup caches, archive extraction, output checking, local/Docker/Kubernetes launchers, gate arithmetic. |
| `crates/managed-postgres` | The managed database (`[database] provider = "managed"`): a private PostgreSQL supervised as a child process, and the unpacking of the bundled PostgreSQL distribution. |
| `crates/imports` | Historical research bundle import ([crates/imports/README.md](../../crates/imports/README.md), [import.md](../import.md)). |

Test-only code (the storage observation route, `POST /__conformance/sweep`, fixture modes) is compiled behind the `conformance-testing` Cargo feature in `core`, `identity`, `projects`, `server` and `cannery`. Release builds use `--no-default-features` and do not contain it.

## Request flow

`production_application` (`crates/server/src/application.rs`) assembles every route with its context from `production_contexts.rs`, the configured object store and the OIDC client. A REST request passes through these stages in order:

1. The transport layer (`transport.rs`) decodes the path once and resolves the route. A path that matches only with or without its trailing slash gets a 307 redirect. A 405 lists the matched route's methods. API GET routes do not accept HEAD; `/openapi.json`, `/docs` and `/docs/oauth2-redirect` accept GET and HEAD.
2. The request context (`request_context.rs`) assigns a request identifier and resolves forwarded client information only from the trusted proxies given by `--forwarded-allow-ips`. Tracing records the identifier, method, matched route template and status, never the concrete path, query, headers, cookies or client address.
3. Control bodies are read by `body::read_body` with a byte cap, a single read deadline and a nesting limit (see [request budgets](limits.md#request-bodies-and-json)). Parsing happens before authentication, so a refused body needs no database access.
4. Authentication (`authentication.rs`, `crates/identity/src/auth.rs`) accepts a bearer token or a session cookie. A bearer token takes precedence and a malformed one does not fall back to the cookie. CSRF is checked on unsafe methods for cookie sessions. Session and token touches commit in autocommit, so they persist even when the request later fails.
5. Path and query parameters are validated, then the typed serde DTO is decoded from the body, then project permissions are checked.
6. The handler opens its transaction after authentication. The mutation and its audit events commit together; the response is built and serialized after the commit. A failed rollback marks the connection `close_on_drop` so it never returns to the pool, and the original error is returned.

Domain failures use the JSON error envelope `{"error": {"code", "message", "details"}}`. Unhandled failures return HTTP 500 with the plain-text body `Internal Server Error`; their detail goes only to the log with the request identifier.

Idempotent routes (unit creation and revision, job claims, review decisions, submissions) take `pg_advisory_xact_lock(hashtextextended('<scope>\n<actor>\n<key>', 0))` inside the request transaction before checking the stored request hash.

Artifact downloads release the database connection before any object-store I/O. Local objects stream lazily; S3 objects return an SDK-signed redirect.

Browser login uses `GET /auth/login`, `GET /auth/callback` and `POST /auth/logout`. The callback consumes the pending login state, then upserts the user, grants bootstrap administration, creates the session and writes the audit event in one transaction.

## Database access

The server uses stock SQLx 0.8.6 with its PostgreSQL driver, the Tokio runtime and rustls with ring. There is no patched or vendored driver and no custom pool. Queries are `sqlx::query!`/`query_file!` macros checked against the schema; offline builds use the committed `.sqlx` metadata (`SQLX_OFFLINE=true`).

- Connection URLs (`crates/core/src/db/connection.rs`) go through SQLx's URL parser. Only `postgres`/`postgresql` schemes and these query keys are accepted: `sslmode`, `sslrootcert`, `sslcert`, `sslkey` (and their hyphenated spellings and `ssl-ca`), `statement-cache-capacity`, `host`, `hostaddr`, `port`, `dbname`, `user`, `password`, `application_name`, `options`. Anything else is refused before SQLx sees it. The connect timeout is 30 seconds. Debug output redacts credentials.
- Migrations (`crates/core/src/db/migrations.rs`) are append-only. The SQL files under `crates/core/migrations/` are compiled into the binary with `include_str!`. Applied versions are recorded in `schema_migrations` (version, name, SHA-256 checksum of the script text, applied time). A session advisory lock serializes concurrent migrators with a 60-second acquisition deadline. A changed checksum for an applied version is refused. `cannery migrate` prints `applied N migration(s)`.
- Sessions run in UTC (see [timestamps and time zones](limits.md#timestamps-and-time-zones)).
- With `provider = "managed"`, `cannery` starts its own server before the command runs and points `database.url` at it (a Unix socket in a mode 0700 directory, a generated password, no TCP listener); `serve` then also applies migrations. `crates/managed-postgres` locks the data directory, runs `initdb` once (UTF-8, builtin `C.UTF-8` locale), stops a server a previous process left behind (found through `postmaster.pid` and its working directory), waits for readiness, and stops the server with a fast shutdown on exit. On Linux the server is started through a private `argv[0]` of the `cannery` binary that sets `PR_SET_PDEATHSIG` before executing `postgres`, so the kernel stops it if `cannery` dies; safe Rust cannot set it between `fork` and `exec` otherwise. The server runs in its own process group, so a terminal interrupt reaches only `cannery`.

Recovery (`crates/server/src/sweeps.rs`) runs at startup and then at the configured interval. It holds a `pg_try_advisory_lock` on a dedicated direct connection that never returns to the request pool, so only one server instance sweeps at a time. Each record is handled in its own transaction under `FOR UPDATE SKIP LOCKED`, locking the attempt before the job, with System audit attribution. It reconciles expired attempt leases and deadlines, expired job claims, abandoned uploads, failed-upload object deletion and local staging files. Verified artifact keys are never deleted. A run handles at most 10,000 records and 100 pages per phase; missed ticks are skipped. On shutdown the active run is cancelled and its statement settles before the session closes and the pool shuts down.

## JSON handling

There are two representations, each used where it fits:

- Fixed request and response shapes are named serde DTOs in `crates/server/src/api_models.rs` and related modules. They carry `utoipa::ToSchema`, so the same types generate the OpenAPI document. PATCH DTOs preserve the difference between an omitted field and an explicit null.
- Free-form documents (project-defined fields, evidence extensions, embedded JSON Schemas, stored JSONB, frozen job parameters) use `cannery_core::json::Document` (`crates/core/src/json.rs`). It is a flat arena of nodes (null, bool, `BigInt` integer, finite `f64` float, UTF-8 `String`, array, object) addressed by `NodeId`, so traversal and drop need no recursion. Objects keep the first key's position and the last duplicate's value. Decoding goes through `serde_json` (with `arbitrary_precision` and `preserve_order`), then converts with a nesting budget (`json::MAX_DEPTH` = 128) and a numeric-literal limit of 1,024 bytes. Lone surrogates, `NaN` and `Infinity` are rejected.

HTTP responses are compact UTF-8 JSON (`json::encode_http`). Canonical evidence bytes (`json::canonical`) are a separate versioned format: sorted keys, compact UTF-8 and the runner protocol's binary64 notation including signed zero and exponent spelling. They are hashed with SHA-256 for evidence and manifest digests. Changing them would require a protocol migration.

Published document schemas (Draft 2020-12) are validated by the stock `jsonschema` 0.58.5 crate with default features disabled, an in-memory registry and no remote retrieval (`crates/core/src/contracts.rs`). Project-supplied schemas are compiled by the same engine under the restrictions in [limits](limits.md#schema-validation).

## OpenAPI

Route handlers and DTOs carry utoipa 6 annotations. `GET /openapi.json`, `cannery openapi` and the checked-in `web/openapi.json` are the same generated document; `cannery openapi` needs no settings or database. The checked-in file is compared in two places:

- `crates/server/tests/openapi.rs` compares the generated contract with `web/openapi.json` and rejects structural drift.
- The release workflow compares `cannery openapi` output from every built binary and image with `web/openapi.json` byte for byte.

The web client's TypeScript types are generated from `web/openapi.json`; `npm run api:types:check` fails if they are stale. To change the API, change the DTOs or annotations, regenerate `web/openapi.json` with `cannery openapi -o web/openapi.json`, and regenerate the client types with `npm run api:types` in `web/` (`dev/openapi.sh` does both). If the change touches a documented limit, update [limits.md](limits.md).

## MCP

`/mcp` (`crates/server/src/mcp/`) implements the stateless 2025-06-18 Streamable HTTP transport and tools protocol directly; the `rmcp` SDK was not adopted because its HTTP service requires `Accept` and `Content-Type` headers that existing clients do not send. POST returns JSON; notifications and client responses get an empty 202. GET, HEAD and DELETE return 405 with `Allow: POST`. There are no sessions or SSE streams. The `MCP-Protocol-Version` header must match when supplied, and an `Origin` header must match the configured public base URL.

The 44 tool definitions live in `crates/server/src/mcp/tools.json`; each input is validated against its JSON Schema before dispatch, and each tool declares an object output schema. MCP accepts bearer tokens only. Authentication checks the token once and passes a private single-use capability to the existing REST controller through a request extension, so a tool call needs only one pool connection and no public header can impersonate a caller. Audit events record `via=mcp` with the token label and User-Agent. Idempotent replays return `replayed=true` and `_meta.replayed=true`. The server also offers resources: each project's brief at `cannery-row://projects/{project}/brief` (and `/brief/revisions/{revision}`), listed by `resources/list` and read by `resources/read` through the same brief routes and single-use authentication.

## Runner and launchers

`cannery runner` (`crates/runner/src/runtime/`) reads a TOML configuration with one entry per job kind (`verify`, `experiment`). Each entry has one worker running N concurrent loops on a shared scheduler. A loop claims a job, renews its lease, prepares code and setup, stages inputs, runs steps through the launcher, checks and uploads outputs, and posts completion or failure. `cannery evaluator` applies the stock policy alone, offline.

- Backends implement `runtime::backend::Backend`: local processes (`local_process.rs`, unisolated, explicitly selected), Docker through Bollard 0.21.1 over an explicit Unix socket, and Kubernetes through kube 4.2.0 / k8s-openapi 0.28.0 with an explicit API URL, namespace, token file and CA file. No ambient kubeconfig, credential plugin or CLI is used.
- The code provisioner fetches GitHub tarballs (token file or GitHub App credentials), verifies the commit, extracts with `tar`/`flate2` under entry and byte limits, and stores trees in a size-bounded cache (20 GiB default) locked with `flock`. Setup results are cached by a key that excludes the commit.
- Cancellation (SIGINT, SIGTERM, lease loss) waits for the step process, backend release and cache holds to settle before work directories are removed. A failed backend release keeps the directory.
- The API client sends the runner token only to the configured API origin; uploads use upload capabilities, signed transfers and GitHub tarball redirects use credential-free clients, and step environments never receive API, lease, upload or GitHub credentials.
- Stock gate arithmetic (`gates.rs`) is exact rational arithmetic over verified (`tester_verified`) measurements only.

Deployment and options are in [deploy.md](../deploy.md#runner) and [operations.md](operations.md#runner-deployment-and-policy).

## Storage backends

`crates/storage` provides the local and S3 backends behind one factory.

- Local publication writes an exclusive staging file with streaming SHA-256, fsyncs it, publishes with an atomic create-only hard link and fsyncs the directory. The object generation is the inode plus nanosecond modification time; conditional deletion compares it before unlinking. Reads use 1 MiB buffers.
- S3 uses the official AWS SDK (`aws-sdk-s3` 1.152.0) with the ring-based rustls HTTP client, a 10-second connect and 120-second read timeout and at most four attempts. Request checksum calculation and response checksum validation are `WhenRequired`; single PUTs send SHA-256 and multipart parts send Content-MD5. Streamed writes use 8 MiB parts. A small interceptor keeps the existing query paths and signed-header selection.
- The server drains tracked upload cancellation work before closing the pool. Unpublished local staging files are removed and known S3 multipart uploads are aborted, each with a 20-second timeout. This does not make PostgreSQL and the object store atomic.

## TLS and cryptography

The workspace uses rustls 0.23 with the ring 0.17 provider for HTTPS, PostgreSQL TLS, S3 and Kubernetes, and ring for OIDC and GitHub App signatures. `deny.toml` bans `openssl-sys` and `aws-lc-sys`; any new dependency must keep that provider choice through its feature graph. There is no second provider and no hand-written cryptographic primitive. The supported OIDC algorithms and TLS behaviour are in [limits](limits.md#oidc-and-tls).

## Library choices

| Concern | Choice | Reason |
| --- | --- | --- |
| HTTP server | `axum` 0.8, `tower-http`, `tokio` | Maintained Tokio stack. |
| Database | `sqlx` 0.8.6 (PostgreSQL, rustls-ring) | Compile-time checked queries, async, no OpenSSL. |
| JSON | `serde`, `serde_json` (`arbitrary_precision`, `preserve_order`) | Standard; arbitrary precision keeps integers exact in `Document`. |
| JSON Schema | `jsonschema` 0.58.5, default features off | Draft 2020-12, in-memory registry, no network resolution. |
| OpenAPI | `utoipa` 6 | Generates the document from the same DTOs and handlers. |
| MCP | Own transport over Axum | `rmcp`'s HTTP service changes status codes for clients that omit `Accept`/`Content-Type`. |
| YAML (imports, front matter) | `serde-saphyr` 0.0.29 | `serde_yaml` is unmaintained and `serde_yml` has a RustSec advisory. The strict options are shared by imports and phase documents (`cannery_core::yaml`); the front matter delimiters are split by `cannery_core::front_matter`, without a separate front matter crate. Bounded reads and alias restrictions are documented in [crates/imports/README.md](../../crates/imports/README.md). |
| Settings | `toml` 0.8 | Maintained TOML parser. |
| Object storage | `aws-sdk-s3` and related official AWS crates | Official SDK, rustls with ring. |
| Containers | `bollard` 0.21.1, `kube` 4.2.0, `k8s-openapi` 0.28.0 | Official-API clients without a CLI or ambient credentials. |
| HTTP client | `reqwest` 0.12 with rustls | OIDC, GitHub and the runner's API client. |
| Archives | `tar` 0.4, `flate2` 1.1 | Maintained; extraction limits are applied by the runner. |
| Bundled PostgreSQL | `ruzstd` 0.9 (pure Rust zstd decoder, no dependencies) with `tar` | Unpacks the embedded `.tar.zst` without a C library; only regular files and directories are accepted. |
| Cryptography | `rustls` 0.23, `ring` 0.17 | Single provider; OpenSSL banned. |
| Date-time query values | `speedate` 0.17 | Date-time parsing for query values. |
| Web assets | `rust-embed` | The production `web/dist` build is compiled into the binary. |
| Allocator | `mimalloc` | Global allocator of the `cannery` binary. |
