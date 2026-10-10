# Units persistence

This crate holds the persistence operations for units, their revisions, relations, mentions, review cases and decisions, as free async functions in `src/repo.rs`. It adds no HTTP routes, authorization, audit events, CLI import behavior, migrations or transaction policy. Each function takes a caller-owned `&mut PgConnection` and runs SQLx `query!`, `query_as!` or `query_scalar!` macros checked against the schema; the offline metadata is in the workspace `.sqlx` directory. Connection pooling and statement preparation are SQLx's own.

IDs, unit states, track modes, origins, relation kinds, case kinds and states, decision actions, link kinds and mention sources are typed. Revision and decision `via_channel` values decode to `ViaChannel::Known` for the five current channels and `ViaChannel::Other` for any other stored label, because the schema has no channel CHECK.

Integer inputs are `BigInt` and bind as `i64`. A value outside the `i64` range fails with `IntegerBinding` before any SQL is sent. Text inputs must be valid UTF-8 without NUL (`TextEncoding`, `TextNul`).

`JsonContext` requires explicit encode and decode nesting budgets and has no default. Revision content is a lossless `cannery_core::json::Document`, encoded as JSONB text and decoded from `content::text`. For the optional `imported` column, SQL NULL is `None` and JSON null is a document node. Database failures become `UnitError::Database` with only a well-formed server SQLSTATE; driver messages and parameter values are never retained.

Transactions are the caller's responsibility. `replace_relations` and `replace_mentions` delete and then insert the sorted unique edges, so a later failure needs a caller rollback. Missing rows in operations that require one return `Invariant`; updates that match no row stay no-ops. `get_unit` with `lock = true` first locks only the unit row, then reads it with its joined track in a separate statement. `set_state` without an approved revision keeps the existing approved fields. `record_decision` resolves only a pending case; a correction keeps the original resolved timestamp. `revision_requested` picks the latest decision by `decided_at`, with no ID tie-breaker.

## Checks

```sh
cargo fmt -p cannery-units -- --check
cargo clippy -p cannery-units --all-targets --all-features -- -D warnings
cargo test -p cannery-units
```
