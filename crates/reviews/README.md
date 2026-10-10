# Reviews and attention persistence

`cannery-reviews` holds the persistence functions for review cases; `cannery-attention` holds those behind the project attention summary, including `stalled_verifications`. Both take a caller-owned `&mut PgConnection` and use stock SQLx checked queries. Authorization, audit, orchestration, HTTP routes and transaction policy belong to the caller.

IDs, CHECK-constrained states, origins, review kinds, failure stages and decision actions are typed enums; an unknown stored label is an `Invariant` error. Verdicts and verifier identifiers and revisions are `Option<String>`: they come from JSON projections of the verification front matter and the job spec that return SQL NULL when the stored document lacks the field. Failure details and log references are decoded into a `cannery_core::json::Document` with the caller's `JsonContext` nesting budget; the production server passes `json::MAX_DEPTH` (128). Records and errors redact document, token, SQL and parameter values; errors keep only a category and the SQLSTATE.

`get_case(lock=true)` first locks only the case row and then separately reads its joined hypothesis and attempt. `open_result_case` is a plain `INSERT … RETURNING`: it adds no conflict handler, lease check, resolution, verifying-state check or ownership validation. Database constraints enforce uniqueness and references, and the caller must roll back after a failing insert. Case lists use project-scoped tuple cursors and newest-first order. Attention pending reviews are oldest first, counts include every constrained kind with a zero default, and running totals are independent of the page limit. Outcomes use the non-superseded decision anti-join. Running, failure and stalled track joins use the attempt's track, while pending and outcome joins use the hypothesis's track. Stalled verifications select pending verify jobs older than the threshold, with no verifier-registration anti-join.

Limits and interval seconds are arbitrary-precision integers bound as SQL parameters, so PostgreSQL applies its own range checks (for example `LIMIT` conversion and `make_interval`). In two-query functions the count runs before the page query.

## Checks

```sh
cargo clippy -p cannery-reviews -p cannery-attention --all-targets --all-features -- -D warnings
cargo test -p cannery-reviews -p cannery-attention
```
