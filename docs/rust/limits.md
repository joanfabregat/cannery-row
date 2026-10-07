# Limits and support boundaries

This is the list of explicit limits and support boundaries of the implementation: request budgets, accepted encodings, schema and OIDC support, configuration grammar, and runner and launcher policies. Each entry says what the implementation does and, where it is not obvious, why. The general reason throughout is the same: the implementation uses maintained libraries and explicit application limits rather than open-ended or library-dependent behaviour. When you change one of these, update this page together with its schemas, the web client, the CLI documentation and the tests.

The product behaviour these limits sit under (the REST operations, the MCP tools, the audit actions, the database schema and its `schema_migrations` bookkeeping, authorization, CSRF, state transitions, leases, recovery, object integrity, transactional audit, the error envelope, the step container contract, the runner job kinds and the stock evaluator's gates) is described in [spec.md](../spec.md), [contracts.md](../contracts.md) and [deploy.md](../deploy.md).

Some fixed formats:

- HTTP datetime formatting: `Z` for a zero UTC offset, six fractional digits only when microseconds are nonzero, offset seconds truncated to minutes (`crates/server/src/timestamps.rs`).
- Canonical evidence bytes and their digests: sorted keys, compact UTF-8, the evaluator protocol's binary64 spelling.
- Unhandled failures return HTTP 500 with the plain-text body `Internal Server Error`.

## Request bodies and JSON

| Area | Behaviour | Why |
| --- | --- | --- |
| Control body size | REST control bodies read by `body::read_body` are capped at 2 MiB (2,097,152 bytes), including requests without `Content-Length`. Overflow returns HTTP 400 `{"detail":"There was an error parsing the body"}`. | Bounded memory per request. Large content belongs in artifact uploads. |
| Body read time | One 30-second deadline for the whole body read. It starts when reading starts and is not renewed by incoming chunks. Expiry returns the same HTTP 400. It does not cover authentication, database work or the response. | Slow clients cannot hold a handler indefinitely. |
| JSON nesting | At most 127 nested containers are accepted. At 128, serde_json's recursion limit rejects the body as a syntax error: HTTP 422 `validation_failed` with a `body/<position>` "JSON decode error" detail. The internal conversion budget is `json::MAX_DEPTH` = 128. | A fixed, documented limit. |
| Encoding and numbers | Standard UTF-8 JSON only. Invalid UTF-8, lone surrogate escapes and `NaN`/`Infinity` literals are syntax errors (HTTP 422 on REST control routes). A numeric literal longer than 1,024 bytes, or a float that overflows binary64, returns HTTP 400 like the byte cap. Runner configuration, policy files and API responses read by the runner follow the same UTF-8-only rule. | Rust strings are UTF-8; nonfinite numbers have no JSON representation. |
| Number formatting | Standard serde JSON number formatting. Runner-written JSON files (`job.json` and similar) are pretty-printed UTF-8, not ASCII-escaped, and floats use Rust's display (`1` rather than `1.0` for an integral binary64 value in evaluator descriptions). | Standard serialization. Canonical evidence bytes are unaffected. |
| Integers in queries and control fields | Checked integers within PostgreSQL's signed 64-bit range. Booleans, fractional values, Unicode digits and underscores are not integers. Consumed science and setup integer fields must be JSON integers in that range. | Typed DTOs. |
| Fixed request shapes | Named serde DTOs. Fixed request envelopes must be JSON objects, even when every field is optional. Explicitly nullable optional bodies also accept JSON null. Unknown fields are rejected where the DTO declares it. PATCH keeps the difference between omitted and null. | One set of types generates both validation and OpenAPI. |
| Validation errors | A sanitized `validation_failed` envelope with ordered paths and static wording. Error codes and paths are the contract; prose is not. | Messages never echo submitted values. |
| Hypothesis creation and revision over budget | Over-budget bodies are refused before the transaction and write nothing. | Refuse early instead of committing an unrenderable record. |
| Submissions | Invalid JSON or excessive nesting returns 422 before evidence processing. First acceptance is 201; an idempotent replay is 200 and does not need the consumed lease; the same key with different evidence is 409; semantically invalid evidence under a valid lease durably fails the attempt with `invalid_submission` and returns 422. | Bounded parsing; replay semantics are explicit. |

Artifact byte streams and MCP requests use their own adapters and limits; the REST budgets do not apply to them.

## Read and write models

- Views may reference an unregistered metric: the response keeps an explicit empty metric object and its warning. Registered metrics use the published metric DTO.
- Read responses keep opaque producer/workflow references and metric control objects, because stored rows can include empty and arbitrary objects. Creation and PATCH use strict producer/workflow DTOs.
- Frozen job parameters stay dynamic project values, including scalar values.
- Agent reports, imported reports and absent reports are distinct typed variants.
- Historical imports have explicit read models for normalized hypothesis headers, imported evidence with provenance and measurement authority, and science revisions written before the evaluator field existed. The historical science variant has no evaluator field in serde or OpenAPI. These read models do not relax creation or update contracts.
- Stored artifact MIME values must be strings; malformed values fail before object-store access.
- Human decision evidence revisions are checked signed 64-bit integers.
- Model number projections serialize nonfinite results as JSON null. Nonfinite values cannot enter canonical evidence.
- Review decisions serialize their typed response before committing. Malformed stored evidence returns an internal error and rolls back the decision, audit events and idempotency record; after the stored state is repaired, the same request and key succeed and replay normally.

## Text, identifiers and names

| Area | Behaviour |
| --- | --- |
| Text representation | UTF-8 `String`; NUL is rejected at the database boundary. Lone surrogates cannot occur. |
| Whitespace trimming | Rust's Unicode whitespace rules for project titles, token names and settings. |
| Decimal digits | ASCII only (`char::to_digit(10)`) in science and step coercion. |
| Lowercasing | Rust `to_lowercase` for login email, allowlist and bootstrap-admin comparisons. |
| Sorting | Rust's native sort and string ordering. |
| Token names and MCP attribution | Control characters, semicolons and Unicode bidirectional formatting controls are rejected, so names cannot mislead in audit records and headers. Ordinary Unicode names stay supported. |
| UUIDs | The `uuid` crate's parser. |
| Metric dimension filters | Name: 1 to 64 characters, a lowercase ASCII letter then `[a-z0-9_]`. Value: nonempty, may contain Unicode and colons, no control characters (a trailing newline is rejected). Duplicate values are dropped keeping first-seen order. Errors report only `filter/<n>`. |
| Resource quantities | ASCII decimal plus the published suffixes, at most 1,024 bytes for the whole spelling, exact arithmetic truncated toward zero. Whitespace, a trailing newline, signs, Unicode digits and exponents are rejected. Numeric configuration identifiers have the same 1,024-byte bound. |
| Science scalars | Booleans are distinct from numeric 0 and 1; nonfinite scalars are rejected. |
| Comparisons API | Cursors and page limits are bounded positive values; dimension filters are typed UTF-8. |

## Schema validation

- Published Draft 2020-12 schemas are validated by the stock `jsonschema` crate. `pattern` follows its standard regular-expression engine (for example `$` handling before a final newline and Unicode classes), with the engine's bounded defaults.
- Error paths sort object keys lexically and array positions numerically, so limits report earlier array elements first. Repeated path/message pairs are deduplicated before the result limit applies. Escaped JSON Pointer keys keep their meaning.
- A forbidden property is reported at the property's path. For example, an agent-mode track containing a workflow is rejected at `/workflow`.
- Colliding schema resource identifiers resolve in a deterministic order.
- Project schemas must have an object root, use Draft 2020-12 when they declare a dialect, and use local fragment references. Supported formats are `date`, `date-time`, `uri` and `uuid`. These restrictions apply only at schema positions, not inside `const`, `enum`, `default` or `examples` data. Compilation never retrieves network resources. Each streamed output checker compiles its schema once and reuses it for every JSONL record.
- Registered output interfaces report at most five issues, with array indices in numeric order.
- Limits apply the same way to cold and repeated validation.

## MCP

| Area | Behaviour |
| --- | --- |
| Input typing | The endpoint accepts standard UTF-8 JSON and typed schema values; strings, booleans and integers are not coerced into one another. UUID and date-time formats are validated before dispatch. |
| JSON-RPC IDs | Strings or signed/unsigned 64-bit integers. Booleans and null are rejected. |
| Diagnostics | Schema violations use JSON Pointers (`/number`), with HTTP 200 and JSON-RPC `-32602`. A forbidden extra argument is reported at the containing object (`""`). Diagnostics carry no submitted values. |
| Header-bound arguments | `lease_token` and `idempotency_key` must be printable ASCII, as advertised in the input schemas, because they become the `x-lease-token` and `idempotency-key` headers of the underlying controller. |
| Output schema | Every tool declares an object `outputSchema` matching the structured-result envelope. Arrays and scalars are wrapped in an `items` object; the text content is the same JSON value. |
| Result limits | Measured on compact UTF-8 structured JSON. A result-too-large error is not itself re-limited. Request limits apply before authentication. |
| Replays | Successful idempotent replays expose `replayed=true` and `_meta.replayed=true`. |

## OIDC and TLS

The workspace uses rustls with the ring provider only and bans OpenSSL. This narrows some accepted inputs; it is a support boundary, not a claim that the excluded options are insecure.

- Supported signature algorithms: `RS256`, `RS384`, `RS512`, `PS256`, `ES256` (P-256), `ES384` (P-384) and `EdDSA` with Ed25519. RSA keys must have a 2048 to 8192-bit modulus and an odd exponent of 2 to 33 bits (ring's ranges).
- Rejected: weaker RSA keys, unsupported RSA parameters, ES512/P-521, Ed448, mismatched EC algorithm/curve pairs, critical JWT header extensions and malformed keys. Rejection is a sanitized authentication failure; signatures are never skipped, and claims are trusted only after verification.
- Issuer, audience, signature, nonce and expiry are checked, with a sixty-second leeway. Dates must be signed 64-bit integers or finite floats that truncate into that range; boolean and string dates are rejected.
- Provider HTTP uses reqwest with rustls: no redirects, a 10-second connect timeout and a 10-second whole-request deadline, responses capped at 1 MiB and JSON depth 128, ID tokens capped at 64 KiB. Discovery is cached for 3,600 seconds; a refresh clears the key cache and a failed refresh keeps the old cache.
- Endpoint URLs use the `url` crate. HTTPS requires a host. HTTP is allowed only for `localhost`, `127.0.0.1` or `[::1]` with an optional valid port. Credentials, raw whitespace or control characters, backslashes, empty ports and alternative numeric loopback spellings are rejected.
- Forwarded ports use a checked unsigned 16-bit parser; out-of-range ports are rejected. Only the proxies given by `--forwarded-allow-ips` are trusted.
- Browser redirects percent-encode unsafe UTF-8 and control bytes before building the `Location` header. A return path falls back to `/` when it is empty, lacks a leading slash, starts with `//`, contains a backslash, a C0 control or DEL.
- Session cookie `Max-Age` is computed as hours × 3,600 with checked 64-bit arithmetic.
- PostgreSQL and HTTPS TLS use rustls certificate-chain and hostname verification with explicit trust configuration. The configured PostgreSQL `sslmode` is honoured within that support; a certificate failure never retries in a weaker mode. `sslmode=verify-full` is recommended for remote databases. OpenSSL process configuration, engines, unusual certificate acceptance and unsupported key-file or trust-store features are not supported; unsupported configuration is refused clearly instead of being ignored.

## Database and time

- Connections, pooling, argument encoding, statement preparation and transactions use unmodified SQLx. Statement names, wire encodings and preparation counters are not part of the contract.
- Connection URLs follow SQLx's parser. Only the query keys listed in [architecture](architecture.md#database-access) are accepted; other libpq options are refused with a redacted diagnostic.

### Timestamps and time zones

SQLx sends `TimeZone=UTC` as a startup parameter on every connection, together with `DateStyle=ISO, MDY` and `client_encoding=UTF8` (`PgConnection::establish` in [`sqlx-postgres` 0.8.6 `src/connection/establish.rs`](https://github.com/launchbadge/sqlx/blob/v0.8.6/sqlx-postgres/src/connection/establish.rs)). Cannery Row sets no time zone of its own: `crates/core/src/db/connection.rs` passes the URL to SQLx's parser, and the managed provider (`crates/managed-postgres`) sets no time zone either. PostgreSQL applies startup parameters after the `options` switches and with priority over database and role defaults, so none of these changes the session time zone:

- a database or role default (`ALTER DATABASE … SET TimeZone`, `ALTER ROLE … SET TimeZone`);
- `-c TimeZone=…` in `PGOPTIONS` or in the URL `options` key, both of which SQLx forwards as the `options` startup parameter;
- `PGTZ`, which SQLx does not read.

This was checked against PostgreSQL 17.11. Only an explicit `SET TimeZone` inside a session changes it afterwards, and no production statement issues one (the conformance storage hook and some tests do). Consequences:

- Calendar-day interval arithmetic in SQL happens in UTC; a day across a daylight-saving change is 86,400 seconds.
- Naive time bounds in metric queries are interpreted in UTC.

UTC sessions are the only supported configuration. Stored timestamps decode only within calendar years 1 to 9999; a value outside that range, including PostgreSQL's infinities, fails to decode with a static error.

## Uploads and storage

- Stored-output validation takes a permit only after the object-store write has completed. A slow client sending a body does not hold a validation slot, so a second complete upload can validate and return 201 while the first body is still arriving, even with one slot.
- Waiting for a permit and reading back and checking the stored content each have a separate 20-second deadline. Either expiry returns retryable 503 `store_unavailable`, removes the stored object and resets the grant to pending; a retry under the same capability can succeed. `storage.max_concurrent_validations` (at least 1) sizes the semaphore; `storage.validate_json_max_bytes` (at least 1 byte) caps content validation. `storage.max_stream_seconds` is used by recovery as the receiving-upload and staging age, not as a live body timeout.
- S3 presigning uses the AWS SDK directly. Expiry must be 1 second to 7 days (604,800 seconds), content length a nonnegative signed 64-bit integer, and multipart part numbers 1 to 10,000. Values out of range are rejected before signing.
- Upload cleanup after cancellation uses explicit 20-second timeouts per operation; local staging files are removed and known S3 multipart uploads are aborted.

## Recovery

- Shutdown waits for the active recovery statement to settle before closing the session and pool.
- `sweeps.batch_size` is at most 10,000 records and `sweeps.batches_per_run` at most 100 pages per phase. Durations must be finite, positive and at most 86,400 seconds. Lease durations and sweep intervals must be positive; stalled-evaluation thresholds may be zero. Missed ticks are skipped.

## Configuration

- Settings and runner configuration use the `toml` crate and its signed 64-bit integer range. Integer settings accept checked integers or trimmed ASCII integer text; booleans, fractional values, Unicode digits and underscores are rejected. Floating-point settings must be finite.
- Boolean environment settings accept explicit common true/false aliases; numeric TOML values are not coerced to booleans.
- Metric aggregates use finite binary64 values and native arithmetic. Nonfinite inputs and overflowing results are rejected.

## Command line

- Usage errors and runner or evaluator configuration refusals exit 2.
- An import that is refused, or that cannot read its bundle or settings, exits 1 with its problems on stderr.
- A repeated option such as `--api-url` is refused.
- Worker commands are checked by exit status and the persisted job, attempt, artifact and audit state, not by console wording.
- `cannery runner` and `cannery evaluator` exit 130 or 143 after SIGINT or SIGTERM, once cleanup has settled.

## Runner

### Credentials and network

- Exactly one GitHub credential mode: anonymous, a token file, or GitHub App credentials. Partial or combined settings fail at startup without falling back to anonymous access.
- GitHub App private keys must be owner-owned, mode 0600, at most 65,536 bytes, opened without following links, and contain one unencrypted PKCS#8 or PKCS#1 PEM key. App and installation IDs are ASCII decimal. The JWT is ring RS256 with `iat = now − 60` and `exp = now + 540`. An installation token is reused while more than 300 seconds remain. Refresh is single-flight. A 401 invalidates the token and retries once. Static token files are reread on a GitHub 401, allowing rotation.
- GitHub requests use API version `2022-11-28`. The tarball download carries no `Authorization` header and follows at most ten redirects.
- Branch reachability checks the default branch, then branch pages with `per_page=100`, for at most 100 pages (10,000 branches). Beyond that the commit is refused. A non-array page or a page with more than 100 entries fails closed. `Link` headers are ignored; page URLs are built from the configured API origin.
- Retry headers: `Retry-After` wins over the reset time, and `X-RateLimit-Remaining` must be exactly `0`. Hints are unsigned ASCII decimal seconds, trimmed of spaces and tabs, at most 2^53−1 so they are exact in binary64; larger values are a sanitized error, and malformed or Unicode spellings fall back to the 403/429 behaviour. A 403/429 without a hint waits 60 seconds. Rate-limit waits have a 300-second ceiling. The 401 refresh and the rate-limit wait are separate one-use budgets.
- The runner's API client uses 60-second connect, read and overall timeouts and no redirects. GitHub downloads have a 300-second read timeout. API JSON and scorer evidence are capped at 16 MiB and 128 nesting levels before typed values are built. Output content checking uses a 64 MiB parse cap.
- Credential-bearing or foreign API URLs are refused. Private token files that are group- or world-readable are refused.

### Execution

- Cancellation (SIGINT, SIGTERM, lease loss) waits for the step process and cleanup to settle before returning. Lease loss stops the process and abandons the job without a failure mutation.
- Local directory copies have one depth limit in every preparation phase: default and maximum 128, root at depth zero. A deeper directory is refused before it is scanned or created. Code copies skip `__pycache__` and `*.pyc`.
- Paths stay native `PathBuf` values, including opaque Unix filename bytes. Paths passed as process arguments or environment text must be UTF-8, or the step fails with a sanitized error before launch. Existing paths use the operating system's canonicalization and symlink limits. Non-strict resolution resolves the longest existing ancestor and normalizes the missing suffix lexically.
- Tree inspection allows a relative dangling link only when its target stays inside the tree; cyclic and absolute links are unsafe, including a cyclic root. Dataset links are refused instead of followed. Unsafe output links and special files are refused.
- Archives use the `tar` and `flate2` crates with explicit extraction limits, commit checks and safe link and destination handling. Gzip checksums are verified before publication. Failed publication and staging cleanup release cache holds.
- A runner whose cache root another runner holds fails its scripted steps with `runner_error` and the reason `runner cache root is already in use`.
- The local launcher resolves the command heads `python` and `python3` to `python3` on `PATH`. The step inherits only `PATH` from the runner's environment.
- Policy evaluation requires setup time plus the step deadline plus 30 seconds of runner margin to fit in the science budget before fetching inputs or launching a step. An impossible budget produces a computed `evaluator_error` reason and no logs.
- Evaluator inputs must match the pinned evidence digests one to one. Duplicate served digests and repeated pins are rejected. Missing, extra or tampered evidence is rejected.
- A denied 401/403 heartbeat is logged once. A 422 on an upload grant is classified `invalid_output` before any PUT.

### Container launchers

These operator policies cannot be set by a workload manifest:

| Policy | Value |
| --- | --- |
| Runner identity | `--launcher docker` and `--launcher kubernetes` require a persistent `--runner-id`, so startup cleanup has a stable owner. |
| Default step resources | `--default-step-cpu` (default 1) and `--default-step-memory` (default `512Mi`) apply when a manifest omits limits. |
| Network | Networked steps are refused unless `--allow-unrestricted-egress` is given; destination allowlists are not enforced. |
| Kubernetes isolation | `--k8s-namespace-policy-acknowledged` is required, confirming that namespace NetworkPolicy, storage and RBAC are installed. Network-free steps gate on refusal from `--k8s-api-service-host` (default `kubernetes.default.svc`) and `--k8s-api-service-port` (default 443). |
| GPUs | Docker uses explicit `--docker-gpu-devices`; there is no ambient `/dev` discovery. A device stays reserved until its container's removal is confirmed. |
| Transfer and log caps | Input transfers 16 GiB; Docker output transfers 16 GiB; Kubernetes output uses its configured cap or the volume capacity; logs 200 MiB; archive members 100,000 unless configured for Kubernetes. |
| Durations | Scheduling 900 seconds, transfers 300 seconds unless configured for Kubernetes, cleanup 60 seconds. Configured durations must be positive, finite and at most one day. |
| Reserved environment | Workloads cannot set `NVIDIA_*` or `CANNERY_*`. `CR_ROOT`, `HOME` and `TMPDIR` are set by the launcher. |
| Exit codes | Command not found and permission failures are step exit 127 and 126; device, image, runtime and disruption failures are runner errors. |
