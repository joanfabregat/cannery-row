# Black-box conformance harness

This crate is the black-box scenario suite: test tooling, not part of the application. It talks to a running `cannery serve` only through HTTP, MCP and the installed CLI. `web/openapi.json` is compiled in as the frozen response contract; local component references are validated with JSON Schema Draft 2020-12. Response schemas are checked before operation/status coverage is recorded. Coverage is a record of passed checks, never a list of intended tests.

## Programs

- `oidc-stub LOOPBACK:PORT ISSUER`, for example `oidc-stub 127.0.0.1:9011 http://127.0.0.1:9011/oidc`, is a standalone test OIDC provider. Set `CANNERY_TEST_OIDC_CONTROL_TOKEN` explicitly. It refuses a non-loopback bind, so run the application, the provider and the suite in one network namespace (one container, for example).
- `conformance BASE_URL REPORT_PATH [REQUIREMENTS_PATH]` checks health, unauthenticated REST, and stateless MCP method/authentication/JSON rejection. It writes JSON coverage. Supplying a requirements JSON file fails if coverage is incomplete. It is a small bootstrap check, not the whole suite.
- `coverage REQUIREMENTS_JSON SOURCE_REFERENCE_JSON OUTPUT_JSON REPORT_JSON...` checks the requirements (`fixtures/coverage-requirements.json`) against the frozen OpenAPI and a reference listing the MCP tools and audit actions, merges only passed scenario reports and fails on every missing status, tool or action.

Configure the application with issuer `http://127.0.0.1:9011/oidc`, client ID `cannery-test`, and client secret `test-client-secret`. Simulate approval by posting to `/__conformance/approve` with `Authorization: Bearer <control token>` and JSON:

```json
{"authorization_url":"<Location returned by /auth/login>","claims":{"sub":"alice","email":"alice@example.test","email_verified":true,"name":"Alice"}}
```

The reply has `state` and `code`; call `/auth/callback?state=...&code=...` on the application with the login's original cookies. Optional `nonce_override` and `bad_signature` approval fields allow negative cases. Claims override the base token claims, so expiry/issuer/audience error scenarios can be generated as well. Token exchange uses ES256, advertised EC JWKS, HTTP Basic client authentication, S256 PKCE, and single-use codes consumed even on a failed verifier. These are test credentials and endpoints only.

## Running the scenarios

The scenario tests under `tests/` are `#[ignore]`d: they need a fresh PostgreSQL database migrated with the built CLI, a `cannery` built with the `conformance-testing` feature (which adds the test-only routes such as `POST /__conformance/sweep`) serving it, and `oidc-stub`. They read their environment from:

| Variable | Meaning |
| --- | --- |
| `CANNERY_CONFORMANCE_URL` | The application's base URL. |
| `CANNERY_CONFORMANCE_OIDC_URL`, `CANNERY_TEST_OIDC_CONTROL_TOKEN` | The `oidc-stub` issuer and its control token. |
| `CANNERY_CONFORMANCE_CLI` | The installed `cannery` binary, for CLI scenarios (imports, runner, evaluator, migrations). |
| `CANNERY_CONFORMANCE_WORK_DIR` | A private scratch directory for fixtures and token files. |
| `CANNERY_CONFORMANCE_COVERAGE_DIR` | Where each scenario writes its coverage report. |

Run each test binary with `--ignored --test-threads=1` against its own database, and drop the database afterwards. Release builds never contain the `conformance-testing` feature.

## Scenario API

Create `Harness::new(base_url)`, obtain a request with `request(Method, path)`, add authentication/body/headers as needed, send, and call `check_response(Method, frozen_path_template, response, expected_status)`. `CheckedResponse` retains parsed JSON, status, and headers for scenario assertions. Requests cannot escape the configured server origin and redirects are not followed. The client times out after 30 seconds.

`rpc` constructs stateless MCP requests. `call_tool` checks HTTP/JSON-RPC success, the response ID, `isError`, and identical textual/structured output before crediting that tool. Tool error cases require separate explicit scenario assertions. `observe_audit` records only actions in the `items` of a checked audit response. The test-only audit and sweep endpoints belong to the server's `conformance-testing` feature; this crate introduces no fake clock.

`Coverage::missing` compares actual results with a separately derived requirements inventory; `merge` unions independently executed scenario reports. `documented_requirements` extracts documented successful statuses and 401/403/404/409/422 from the frozen OpenAPI. The OpenAPI document does not list every authentication and domain error, so the requirements inventory adds the reachable errors, all MCP tools and the known audit actions. Do not manufacture impossible error cases (health has no authentication; capability upload endpoints hide invalid grants as 404; creating a comment has no reachable 409). Protocol checks do not credit any tool call. Missing scenarios remain missing.

`CheckedResponse::raw_body` retains bytes for SHA/content assertions. Exactly three GET operations bypass JSON decoding for successful download responses: artifact downloads, job input objects, and predecessor input artifacts. Their 200 responses require a media type and an exact Content-Length; artifact downloads additionally enforce their passive media-type allowlist, SHA-256 ETag and security/cache headers. Presigned 302 responses require HTTP(S) Location, no-store and empty bodies; artifact 304 responses require empty bodies and cache/security/ETag headers. Other endpoints still require frozen JSON validation; all 204 responses must be empty.

## Dependencies

Default networking/file resolution features on `jsonschema` are disabled, with `.offline()` set, so no schema validation can retrieve a URL or local file. reqwest defaults are disabled, its `rustls-tls` feature selects ring, and no OpenSSL is used. Cookie sessions use reqwest's `cookie_store` integration. Error diagnostics report statuses and violation paths without printing response bodies, tokens or server-provided messages.
