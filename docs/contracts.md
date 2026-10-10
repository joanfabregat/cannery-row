# Contracts (draft v0.2)

All request and evidence documents are JSON. `schema_version` identifies an immutable core schema; `science_revision` pins project fields, metrics and policy. The implementation will publish canonical Draft 2020-12 JSON Schema files and conformance fixtures before accepting real experiments. These examples are normative for *shape and semantics*, not yet a complete machine-enforceable schema. Unknown top-level fields are rejected; project additions live under `project_fields` or `extensions` and are checked against the pinned project schema. A project schema is Draft 2020-12, uses only local `#` references that resolve within itself, and may use only the formats Cannery Row enforces: `date`, `date-time`, `uri` and `uuid`. The project is identified by the request path (`/projects/{slug}/…`); exported records carry `project` so they stay self-describing outside the installation.

## Track

```json
{
  "slug": "compact-sparse",
  "title": "Compact sparse",
  "description": "Markdown: the approach this track explores and why it competes with the others.",
  "producer": {"name": "sparse-producer", "revision": 3}
}
```

The server assigns `id`, state (`planning` on creation), actor, via, timestamps and revision. `slug` is unique within the project. `producer` references a registered producer step manifest revision; omitted, the project default applies. A track declares no gates: the policy belongs to whoever verifies the run (see [verification policy](#verification-policy-and-the-stock-policy)). A state change supplies `to_state` (`active`, `paused`, `archived`; `planning` is left only by approving a plan), the expected revision and a non-empty `reason`; the server enforces the transitions in [the spec](spec.md#tracks).

`mode` says who runs the track's experiments ([experiment modes](spec.md#experiment-modes)): `agent` (the default when omitted) or `workflow`. A `workflow` track also names its `workflow`, `{"steps": [{"name", "revision"}, …]}` (1 to 16 registered experiment step manifests, run in order); an `agent` track has none, and sending one is refused. The rules a workflow must meet are in [workflow tracks](#workflow-tracks).

```json
{
  "slug": "scripted",
  "title": "Scripted top-k search",
  "description": "A Cannery Row runner claims these hypotheses and runs the experiment workflow.",
  "producer": {"name": "overlap-producer", "revision": 1},
  "mode": "workflow",
  "workflow": {"steps": [{"name": "fixture-experiment", "revision": 1}]}
}
```

A project is created with its first tracks: `POST /api/projects` takes `tracks`, 1 to 32 entries of `slug`, `title` and an optional `description`, with distinct slugs. They are created in the same transaction as the project, `planning`, in `agent` mode and with the project's default producer, each recorded as `track.created`; a project without a track is `422` at `body/tracks`. A researcher binds a producer or switches a track to `workflow` afterwards, once the project has a science revision.

A researcher changes a track's `producer`, `mode` or `workflow` with `PATCH /projects/{slug}/tracks/{track}` (`expected_revision`, the fields to change, and a non-empty `reason`, which any of the three requires). Switching to `workflow` needs a `workflow` in the same request; switching to `agent` drops it. The merged track is checked as on creation. Each change records `track.updated` with the prior and new producer, mode and workflow, plus `track.mode_changed` when the mode changes. Hypothesis summaries and attempts carry the `mode` of their track.

## Hypothesis

```json
{
  "schema_version": "0.2",
  "track": "compact-sparse",
  "title": "Test a compact sparse candidate",
  "question": "Can the candidate exceed the pinned base camp without material language regressions?",
  "rationale": "A predeclared architectural change may improve the quality/cost frontier.",
  "intervention": "Train the specified candidate from the pinned initialization.",
  "control": {"kind": "baseline", "id": "base-camp", "revision": "immutable-revision"},
  "plan": {
    "selection_splits": ["dev"],
    "confirmation_splits": ["fresh-held-out"],
    "primary_metric": "ndcg_at_10",
    "required_slices": ["language", "task"],
    "success_criteria": "Use project policy revision; no material slice regression",
    "falsification_criteria": "Fails the primary metric or a required regression gate",
    "regression_gates": ["language", "task"],
    "compute_budget": {"gpu_hours_max": 12}
  },
  "relations": [{"kind": "derived_from", "hypothesis": 42}],
  "project_fields": {}
}
```

A hypothesis is created only by the approval of its track's plan: each [unit entry](#track-plans) (`contracts/schemas/hypothesis.schema.json`) becomes a hypothesis document like this one, its `acceptance` as `plan` and its `parameters` as `project_fields`, and the hypothesis is `queued` at revision 1. A later approved plan revision that changes a queued unit writes its next revision. The server assigns `id`, the project-local sequential `number` (displayed `#123`), actor, via, timestamps, state and revision. A `relations[].hypothesis` is a number in the same project, or `{"project": "slug", "number": 42}` for another project the caller can read. Free-text criteria help humans but do not replace the executable pinned policy. The project schema may require additional typed fields; it cannot remove the track, question, plan or budget. `control` is optional: an opaque label `{kind, id, revision}` naming what the hypothesis is compared against, which must be a registered baseline of the current science revision when the plan is checked. Cannery Row does not interpret it: when the hypothesis names one, the verify job pins it as `control`, the verification report gives its revision as `control_revision`, and the policy decides what it means. The verify job stages it as a baseline input only while the pinned science revision registers it. A claim keeps a control a later science revision stopped registering (pinned, not staged), and is refused with 409 only when a step takes that baseline id as input (`from: baseline`), which the runner could not stage. Without a control, no `control_revision` is expected.

## Artifact manifest

```json
{
  "schema_version": "0.2",
  "attempt_id": "attempt-id",
  "objects": [{
    "role": "candidate_checkpoint",
    "storage": {"backend": "gcs", "bucket": "private-bucket", "key": "projects/pilchards/attempts/attempt-id/checkpoint.bin", "generation": "optional-provider-id"},
    "size_bytes": 123,
    "sha256": "64 lowercase hexadecimal characters",
    "media_type": "application/octet-stream"
  }]
}
```

`storage` is a structured internal reference, not a public URL. API responses may add an authorized download endpoint. The server checks ownership of each create-only key, existence, bytes and SHA-256; the verified manifest is immutable. Required roles and retention vary by project. The `report_asset` role is readable by viewers; every other role requires `member` or above. A checksum is the application's SHA-256, never an assumed S3 ETag.

## Uploads and downloads

Every file reaches the store through a one-time **upload grant**: `POST …/attempts/{sequence}/uploads` for an agent (with the lease headers) or `POST …/jobs/{id}/uploads` for a job output, declaring the `role`, the name or path, `size_bytes`, `sha256` (required, 64 lowercase hexadecimal characters) and `media_type`. The server chooses the key (`storage.key`, under the attempt or the job's `output_prefix`). What a grant reserves is its role and name (or path): it is granted again only after its grant expired unused, and never once it holds a verified artifact. With the local store the key is that name. With an S3 store every grant's key adds a segment of its own before the file name (`…/candidate/3f9c0e1a7b2d4c55/model.bin`), so a presigned PUT of an expired grant that is still in flight can never overwrite what a later grant verified; the file name, and so a download's name, is unchanged. Read the key from the grant or the artifact, never build it. The grant answers with `upload_url`, `headers` (the grant's `X-Upload-Token`, shown once) and `expires_at`. Whatever the transfer, the bytes must match the declared size and SHA-256: an agent's mismatch fails the attempt for review, a job output's fails the job run, and the bytes are deleted.

**Streaming (every store).** `PUT upload_url` with the grant's headers and the exact bytes. The API hashes them as they arrive, checks a job output against its interface, and answers with the verified artifact. With an S3 store this stays available for a file up to the multipart threshold; a multipart grant refuses it (`409 conflict`).

**Direct (S3 stores).** With an S3 store, the grant also carries `direct`, and the bytes go straight to the bucket (an API response, not a contract document):

```jsonc
{
  "upload_url": "https://cannery.example.org/api/uploads/<id>",
  "headers": {"X-Upload-Token": "cr_upl_…"},
  "expires_at": "2026-09-30T13:00:00Z",
  "storage": {"backend": "s3", "bucket": "artifacts", "key": "projects/…/candidate/3f9c0e1a7b2d4c55/model.bin"},
  "direct": {
    "transfer": "single",
    "request": {
      "method": "PUT",
      "url": "https://s3.example.org/artifacts/projects/…?X-Amz-Signature=…",
      "headers": {"Content-Length": "1234", "x-amz-checksum-sha256": "base64 of the declared SHA-256"},
      "expires_at": "2026-09-30T12:15:00Z"
    },
    "part_size": null,
    "part_count": null,
    "parts": [],
    "presign_url": "https://cannery.example.org/api/uploads/<id>/presign",
    "finish_url": "https://cannery.example.org/api/uploads/<id>/finish"
  }
}
```

1. **Send the bytes.** For `transfer: "single"` (files up to the threshold, 256 MiB by default), one `PUT request.url` with exactly `request.headers` and the whole file. The URL signs `Content-Length` and `x-amz-checksum-sha256`, and the store verifies the checksum: other bytes are refused (`400 InvalidDigest`) and nothing is stored, so the URL can only ever write the declared bytes. For `transfer: "multipart"`, the API created a multipart upload: send part *n* (from 1 to `part_count`) as bytes `[(n-1)·part_size, min(n·part_size, size_bytes))` of the file with `PUT parts[n].url` and its headers (it signs that part's `Content-Length`). `parts` holds the first 100 parts' URLs.
2. **Refresh URLs when needed.** A URL is valid for the store's presign TTL (15 minutes by default) and never past the grant. `POST presign_url` with the grant's headers answers `{"request": …}` with a fresh single PUT, or, with `{"part_numbers": [101, 102, …]}` (at most 100), `{"parts": […]}`. Retry a part that failed; sending a part again replaces it.
3. **Finish.** `POST finish_url` with the grant's headers, before `expires_at`. The API checks what the bucket holds: after a single PUT, its size and the SHA-256 the store verified (a HEAD); after parts, it completes the upload, then reads the object back once to hash it. A job output named against an interface is read back and checked against it, exactly as when it streams (`invalid_content` on a mismatch; the object is deleted). The answer is the verified artifact (`201`), as from the streaming PUT. While bytes are missing (no PUT yet, or parts missing or not of their exact size, listed in `details.missing_parts`), finish answers `409 conflict` and the grant stays open: send them and finish again. A store that refuses to complete the upload from the parts it listed (`InvalidPart`, `EntityTooSmall`: a part changed under a concurrent retry) gets the same answer, never a failed verification; `details.missing_parts` then names the parts to send again, or is empty, meaning finish again. A multipart grant whose upload and object are both gone answers `upload_expired`: request a new grant once it expires. The same token authorizes the presign and finish requests; neither carries a lease header, but both fail once the lease ended (`stale_lease`) or the grant ended (`upload_expired`). Any upload request (grant, PUT, presign, finish) whose object store failed answers `503 store_unavailable` and changes nothing: retry it after a pause, a bounded number of times (the runner tries 4 times).

The API never hands out store credentials, only URLs that each sign one request on one key. Part URLs do not sign a checksum (the API does not know part digests): the finish step's read-back is what verifies a multipart upload.

**Downloads.** `GET /api/projects/{slug}/artifacts/{id}` checks access, then the stored object (present, same size and generation as verified). With the local store it streams the bytes. With an S3 store it answers `302` to a presigned `GET` valid for at most 5 minutes, which names the attachment's filename (`response-content-disposition`) and the served media type (`response-content-type`); a plain link or a client that follows redirects needs no change. A job's `GET …/jobs/{id}/inputs/object?key=…` checks the stored object the same way and redirects the same way, to a URL valid for at most 5 minutes. A redirected download is served by the bucket, without the API's `X-Content-Type-Options: nosniff` and `Content-Security-Policy: sandbox` headers (see [deploy](deploy.md#object-storage) for why that is acceptable). Follow these redirects without the API's headers (the URL is the credential), and check the bytes against the manifest's size and SHA-256 as always.

## Verification reports

An agent submits a [run document](#run-documents): its claims are labeled `agent_claim` and never drive promotion. A [verify job](#verify-jobs) receives the run's front matter and its verified artifact manifest, checks the claims independently, applies the policy, and publishes one **verification report**: a [phase document](#phase-documents) of the `verification` phase whose front matter holds the verdict, the gates, the `tester_verified` measurements and explicit discrepancies, and whose optional body holds what the verifier observed. The framework derives final scientific status only after the separate, reasoned human decision.

```markdown
---
verdict: pass
reason: "Pass: all 1 gates pass on verified measurements."
policy_revision: policy-r1
gates:
  - {id: primary-beats-control, result: pass, detail: "ndcg_at_10 on dev: uncertainty.lower 0.41 - control 0.4 = 0.01 > 0"}
measurements:
  - {metric: ndcg_at_10, value: 0.42, authority: tester_verified, unit: ratio, direction: higher, split: dev, dimensions: {language: fr}, sample_count: 100, control_value: 0.40, uncertainty: {method: cluster-bootstrap, lower: 0.39, upper: 0.45}}
  - {metric: ndcg_at_10, value: 0.44, authority: tester_verified, unit: ratio, direction: higher, split: dev, dimensions: {language: en}, sample_count: 100}
discrepancies: []
comparisons:
  - {metric: ndcg_at_10, split: dev, dimensions: {language: fr}, value: 0.42, source: tester, reference: {value: 0.40, label: Base camp, kind: baseline, ref: base-camp}}
  - {metric: ndcg_at_10, split: dev, dimensions: {language: en}, value: 0.44, source: evaluator, reference: {value: 0.45, label: Best promoted attempt, kind: promoted_attempt, ref: "#12.3"}}
provenance: {source_revision: 7d41c2e, science_revision: "4", dataset_revision: immutable-dataset-id, control_revision: immutable-control-id, seed: 1}
artifact_roles: [raw_results, step_log]
extensions: {}
---
Full run completed with no missing query rows.
```

The front matter (`contracts/schemas/verification.schema.json`) holds:

- `verdict`: `pass`, `fail` or `inconclusive`. It is an auditable gate, not the final decision.
- `reason`: a non-empty reason for the verdict.
- `policy_revision`: the revision of the policy that gave it. A runner's report must name the revision the science revision registers for its verifier.
- `gates` (at least one): each gate's `id`, its `result` (`pass`, `fail` or `unknown`) and an optional `detail`. A `pass` verdict requires every gate to pass; which gates exist, and that missing required evidence never yields `pass`, are the policy's responsibility (the stock policy reports such a gate `unknown`).
- `measurements`: what the verifier measured itself, each with `authority: tester_verified`.
- `discrepancies` (optional): where the run's claims disagree with the verified measurements, each with a `description` and optionally the `metric`, `split`, `dimensions`, `claimed_value` and `verified_value`.
- `comparisons` (optional): what the verdict compared, below.
- `provenance`: `source_revision` and `science_revision`, and optionally `dataset_revision`, `control_revision` and `seed`.
- `artifact_roles` (optional): roles of the job's output manifest the report refers to; each must be in that manifest.
- `extensions` (optional): an open object, checked against the science revision's `result_extensions`.

The body is optional Markdown, at most the science revision's `report_max_bytes`.

Numbers must be finite; missing/unsupported metrics are explicit with a reason rather than zero or omitted where required. A metric registry defines key, unit, direction, aggregation, permitted dimensions, required slices and applicable splits. A required slice `{dimension: value}` is covered only by a measurement whose `dimensions` are exactly that one pair (with a value or a `missing_reason`); a measurement with several dimensions, such as `{"language": "fr", "task": "retrieval"}`, may be reported as well but covers no required slice and is never read by a stock policy gate. `control_value` is the value of the pinned control the verifier reports for the same slice: informational, for the policy to use or cross-check (see [the stock policy](#verification-policy-and-the-stock-policy)), and never a chart overlay. Any value used for a decision includes source data identity. Store per-query results as verified artifacts and summarized values as queryable rows. Producer identity, timestamps, configuration revision and measurement authority are server-validated or server-assigned, not trusted merely because a field claims them.

`comparisons` says what the verdict compared, so the metrics overlay and the per-track history can show it: each entry is `{metric, split, dimensions, value, source, reference}`, where `reference` is `{value, label, kind, ref?}` and `kind` one of `paper`, `benchmark`, `promoted_attempt`, `baseline`, `manual` or `other` (`ref` is free text: a baseline id, an attempt reference, a URL or a DOI). Cannery Row does not judge a comparison, but refuses one it cannot chart or that misquotes the report: numbers must be finite; the metric, split and each dimension and value must be registered in the pinned science revision; at most one entry per metric, split and slice; and `source: "tester"` cites a measurement of the same report for exactly that slice, whose value it must equal as an exact decimal. `source: "evaluator"` is a value the policy derived. Accepted comparisons are stored with the report's verdict and policy revision and are read through `GET /projects/{slug}/comparisons` (filters: `metric`, `split`, `dimensions` for an exact slice, `overall`, `filter=dimension:value`, `track`, `attempt_state`, `verdict`, `since`, `until`; ordered by id, newest first, like `/metrics`, and paged by `before` with `{items, next_before}`), and through the MCP tool `query_comparisons`. The verified measurements are indexed the same way and read through `/metrics`.

The report is stored as a phase output of the `verification` stage: the front matter as JSON, the body as written, and the SHA-256 of the document's UTF-8 bytes. Every completed verification leaves the attempt awaiting human review, regardless of verdict, and opens a `result` review case on the report. A failure before a valid report follows the rerun rules and then opens a `failure` review case with the stage (`verify`), error code, sanitized details and artifact/log references.

The **evidence envelope** (`evidence_envelope.schema.json`) remains for two things: it defines the measurement, discrepancy and comparison shapes the run and verification schemas reference, and it is the **claimed result sheet** (stage `agent`, with its structured `report`) that attempts submitted before run documents; those records stay readable, and their reports stay searchable. Test and evaluation records written before verify jobs were merged into one verification report each: the evaluation's verdict, reason, policy revision, gates and comparisons with the measurements, discrepancies and provenance of the test evidence it assessed, and the test observations as the body.

A human decision always supplies `review_case_id`, `evidence_revision`, `action`, and a non-empty `reason`. Result actions are `promote`, `reject`, `inconclusive`; failure actions are `retry`, `close_failed`. Plans are reviewed through [their own route](#track-plans). The server derives actor, via and timestamp from authentication and binds the decision to the exact verification report or failure revision. `promote` is valid only when the verdict is `pass`. A later correction is another linked, reasoned decision record; it never edits the verdict or original human decision. This preserves the distinction between measurements, the verifier's judgment, and human validation.

## Phase documents

What an attempt's phases produce is kept as phase outputs, each a Markdown document with YAML front matter:

```markdown
---
kind: retrospective
author: A. Researcher
written_on: 2024-03-01
---
# Seed 1

The run diverged after epoch 3 …
```

The front matter is the machine-readable part and the body the narrative. The document opens with a line `---` and the front matter closes with a line `---` (or `...`); everything after it is the body, kept byte for byte. Lines end with LF or CRLF, and a leading byte order mark is ignored. The front matter must be present and be one YAML mapping (an empty one is allowed); the body may be empty. YAML is read strictly, as in an [import bundle](import.md#layout): a duplicate key is an error, anchors, aliases and merge keys are refused, dates stay text, and only `true` and `false` are booleans. A document is at most 1 MiB, with front matter nested at most 64 deep and at most 100,000 nodes.

Each phase has a JSON Schema for its front matter:

| Phase | Schema | Front matter |
| --- | --- | --- |
| `brief` | `brief.schema.json` | The project's [brief](#the-brief): `title` and a one-paragraph `goal`, nothing else. |
| `run` | `run.schema.json` | A [run document](#run-documents)'s `claims`, `provenance`, `artifact_roles`, `manifest` and `extensions`. |
| `verification` | `verification.schema.json` | A [verification report](#verification-reports)'s `verdict`, `reason`, `policy_revision`, `gates`, `measurements`, `discrepancies`, `comparisons`, `provenance`, `artifact_roles` and `extensions`. |
| `writeup` | `writeup.schema.json` | `kind` (`retrospective`), `author`, and exactly one of `written_on` (a date) or `written_at` (an instant). |

`GET /api/schemas/{phase}` returns a phase's schema as one self-contained document (`application/schema+json`): the published schemas it references are embedded under `$defs` with their own `$id`, so a client validates against it without fetching anything else. It needs no authentication, like the OpenAPI document; an unknown phase is `404 not_found`.

A run is submitted as its document, and a verify job completes with its verification report; each is stored as its front matter and its body. The only write-ups are imported ones: the report of an imported attempt, stored with `origin: imported`, its path in the bundle as `source_ref` and the report's SHA-256 (see [import](import.md#reports)).

## Run documents

A completed run is submitted as one phase document of the `run` phase, `POST …/hypotheses/{number}/attempts/{sequence}/submission` with `{"document": "…"}` (MCP `submit_attempt`), under the attempt's lease and with an `Idempotency-Key`:

```markdown
---
claims:
  - {metric: ndcg_at_10, value: 0.43, authority: agent_claim, unit: ratio, direction: higher, split: dev, dimensions: {language: fr}}
provenance: {source_revision: 7d41c2e, science_revision: "4"}
artifact_roles: [candidate_checkpoint]
manifest: {ref: manifest-id, sha256: "…"}
extensions: {}
---
# Merged byte pairs

The merge pass halved the vocabulary; recall held on every language …
```

The front matter holds what the run claims and what it ran, nothing else:

- `claims` (optional): the measurements the run claims, each a measurement as in a [verification report](#verification-reports) but with `authority: agent_claim`. They are stored as unverified claims; the verifier must never silently adopt them as verified.
- `provenance`: `source_revision` and `science_revision` (the attempt's pinned revision), and optionally `dataset_revision`, `control_revision` and `seed`.
- `artifact_roles` (optional): roles of the manifest's objects the notes refer to; each must be in the manifest.
- `manifest`: the verified artifact manifest of the attempt (`ref` and `sha256`), which must cover the science revision's `required_artifact_roles.attempt`.
- `extensions` (optional): an open object.

The body holds the run notes, optional Markdown at most the science revision's `report_max_bytes`; images it embeds are `report_asset` artifacts. Run notes are shown on the attempt's report and are not indexed for search. The server records timestamps, the producer and the duration itself.

A run that failed submits no document: it releases the attempt with a failure report (`POST …/release`). Front matter with a `status` is refused with `422` at `body/document`, and the attempt is left as it was. A request whose body is not `{"document": "…"}` is refused the same way. Any other invalid document, one that is not a phase document or breaks the schema or the checks above, fails the attempt with the code `invalid_submission` and opens a failure review case, as the lease holder submitted it.

The run is stored as a phase output of the `agent` stage: the front matter as JSON, the body as written, and the SHA-256 of the document's UTF-8 bytes (`run_sha256` in the `attempt.submitted` audit row). The submission moves the attempt to `verifying` and creates its [verify job](#verify-jobs), which pins the front matter alone as its `run` input, by the SHA-256 of its canonical JSON, and serves it at `GET …/jobs/{id}/inputs/run`; the notes are not an input. `GET …/attempts/{sequence}` returns the front matter as `claimed_sheet`; the attempt's report (`GET …/attempts/{sequence}/report`) returns the notes as `report.body_markdown` and the claims as `claimed_measurements`.

## The brief

The [brief](spec.md#the-brief) is a phase document of the `brief` phase, at most the project's `brief_max_bytes` (64 KiB by default, at most 256 KiB; see [track plans](#track-plans)):

```markdown
---
title: Bakery demand forecast
goal: >-
  Daily demand forecasts per shop and product, one and seven days ahead,
  accurate enough to cut unsold bread without running out before closing.
---
# Domain

A fictional bakery chain: 40 shops, 12 products …
```

`title` (at most 200 characters) and `goal` (at most 4,000, with no blank line) are required, and the front matter may hold nothing else. The body is free Markdown.

- `GET /api/projects/{slug}/brief` returns the current revision: `revision`, `document` as written, `front_matter`, `title`, `goal`, `body`, `sha256` (of the document's UTF-8 bytes), `created_by`, `created_by_name`, `via_channel`, `via_client` and `created_at`. Before the first save it is `404 not_found`.
- `GET /api/projects/{slug}/brief/revisions` lists every revision newest first, without documents (`before`, `limit` up to 200); `GET /api/projects/{slug}/brief/revisions/{revision}` returns one revision like the current one.
- `POST /api/projects/{slug}/brief` with `{"document": "…", "expected_revision": 2}` saves revision 3 and answers `201` with it. `expected_revision` is the current revision, `0` for the first brief; any other is `409 stale_revision`. Only a researcher of the project may save (`403` for anyone else, service accounts included). A document that is not a phase document, or whose front matter breaks the schema, is `422` at `body/document`, each detail naming the front matter pointer. Each save records `brief.revised` in the audit log. Revisions are immutable.

Every claim names the brief revision the work runs under, as a `brief` object beside the attempt or job, absent while the project has no brief:

```json
{"revision": 3, "sha256": "…", "ref": "/api/projects/pilchards/brief/revisions/3"}
```

An attempt pins the current revision when it is claimed (`brief_revision` in the `attempt.claimed` audit row); `GET …/attempts/{sequence}` reports it as `brief`, and the attempt's verify job claims hand out the same revision, never a later one.

MCP serves the brief through the tools `get_brief` (the current revision, or the one named by `revision`) and `revise_brief` (`document`, `expected_revision`), and as resources: `cannery-row://projects/{project}/brief` for the current revision and `cannery-row://projects/{project}/brief/revisions/{revision}` for one revision, each a `text/markdown` document listed by `resources/list` for the projects the caller reads.

## Track plans

A [track plan](spec.md#track-plans) is built through these routes, under `/api/projects/{slug}`, each with an MCP tool of the same shape. Only a researcher of the project writes or reviews (`403` for anyone else, service accounts included); everyone who reads the project reads plans. Every write is recorded with its `via`.

| Route | MCP tool | What it does |
| --- | --- | --- |
| `POST tracks/{track}/plans` | `start_plan_revision` | Opens the next revision (`201`), copying the approved one, or the newest one sent back after it. One revision is open at a time (`409 conflict`); an archived track takes none. |
| `PUT tracks/{track}/plans/draft/approach` | `set_plan_approach` | `{"approach": "…"}`, Markdown. |
| `POST tracks/{track}/plans/draft/units` | `add_unit` | One unit (`201`), fields below. |
| `PUT tracks/{track}/plans/draft/units/{key}` | `update_unit` | The fields to change; omitted fields are kept. |
| `DELETE tracks/{track}/plans/draft/units/{key}` | `drop_unit` | Removes the entry and answers with the revision. Dropping a queued unit cancels it on approval. |
| `PUT tracks/{track}/plans/draft/alignments/{number}` | `set_alignment` | `{"decision": "keep" \| "obsolete" \| "redo", "reason": "…"}` for a unit done or in flight. `redo` adds an entry `redo-{number}` copying the unit with a `derived_from` relation to it; changing the decision away from `redo` removes it. |
| `GET tracks/{track}/plans/draft/check` | `check_plan` | `{"revision", "ready", "problems": [{"code", "path", "message"}]}`; codes `no_brief`, `empty_approach`, `no_units`, `limit_exceeded`, `unknown_unit`, `unit_not_queued`, `missing_alignment`, `redo_without_unit`. |
| `POST tracks/{track}/plans/draft/submission` | `submit_plan` | Opens a `plan` review case; `409 conflict` with the problems as details while any remain. |
| `POST tracks/{track}/plans/{revision}/review` | `review_plan` | `{"action": "approve" \| "send_back" \| "decline", "reason": "…"}` on the submitted revision. |
| `GET tracks/{track}/plans` | `list_plan_revisions` | Revisions newest first (`before`, `limit`). |
| `GET tracks/{track}/plans/{revision}` | `get_plan` | A number, `draft` (the open revision, draft or submitted) or `current` (the approved one, else the newest). |
| `GET tracks/{track}/plans/{revision}/plan.md` | | A rendered Markdown view of the revision. |
| `GET tracks/{track}/units` | `list_units` | The track's units, one line each (`number`, `key`, `title`, `state`, `obsolete`), filtered by `state`, paged by `before` and `limit`. |
| `GET units/{number}` | `get_unit` | A unit: its state, whether it is obsolete, its current fields and brief, its key and the newest approved plan revision listing it. |
| `GET units/{number}/history` | `get_unit_history` | Its hypothesis revisions with their briefs, and the alignment entries that named it. |
| `GET limits`, `PUT limits` | | The project's limits; a researcher changes them (`project.limits_changed`). |

A unit entry:

```json
{
  "key": "token-merge",
  "title": "Merge frequent byte pairs before the sparse step",
  "question": "Does merging frequent pairs cut tokens per document by 5% without hurting recall?",
  "intervention": "Add a pair-merge pass before the sparse encoder.",
  "acceptance": {"selection_splits": ["validation"], "confirmation_splits": ["test"], "primary_metric": "tokens_per_doc", "required_slices": [], "success_criteria": "…", "falsification_criteria": "…", "regression_gates": ["recall"], "compute_budget": {"gpu_hours_max": 2}},
  "parameters": {"merges": 4096},
  "relations": [{"kind": "derived_from", "unit": "baseline"}],
  "context": [{"kind": "writeup", "unit": 12, "attempt": 1, "note": "the baseline's numbers"}],
  "brief": "Markdown: what the performer of this unit should know."
}
```

`key` is 1 to 63 lowercase letters, digits and hyphens, unique in the track; later revisions keep naming earlier units by their keys. `acceptance` is the hypothesis `plan` and `parameters` its `project_fields`; the entry is checked as the hypothesis document it becomes, and errors name the unit's fields (`body/acceptance/primary_metric`). A relation names another unit by `unit` (its key) or a hypothesis by `hypothesis`. A context item is `{"kind": "unit", "unit": <number or key>}`, `{"kind": "writeup", "unit": <number>, "attempt": <sequence>}` or `{"kind": "artifact", "artifact": "<id>"}`, each with an optional `note`.

Approval writes the hypotheses in one transaction and records `plan.approved` with what it created, revised, cancelled and made obsolete; the first one also records `track.state_changed` from `planning` to `active`. The revision's lifecycle is audited as `plan.started`, `plan.submitted`, `plan.sent_back`, `plan.approved` and `plan.declined` (subject `plan_revision`); edits to an open draft live in the draft itself. Plan review cases are reviewed through the plan routes, not the review-case decisions route, and do not appear in review-case lists or attention counts.

### Limits

`GET /api/projects/{slug}/limits`:

```json
{"brief_max_bytes": 65536, "plan_approach_max_bytes": 65536, "unit_brief_max_bytes": 32768, "context_items_max": 64, "index_line_max_bytes": 100, "context_summary_max_bytes": 300}
```

A write over a limit is `422 validation_failed`, its detail naming the `path`, the `size` and the `limit`. The index and summary lengths are not refused: the context bundle truncates those lines.

### The context bundle

A claim of a track with an approved plan pins the plan revision (`plan_revision` in the `attempt.claimed` audit row) and names it and the attempt's context bundle beside the attempt or job:

```json
{"plan": {"revision": 4, "ref": "/api/projects/pilchards/tracks/compact-sparse/plans/4"},
 "context": {"ref": "/api/projects/pilchards/hypotheses/12/attempts/1/context.md", "bytes": 5321}}
```

`GET /api/projects/{slug}/hypotheses/{number}/attempts/{sequence}/context.md` assembles it on demand from what the attempt pinned: YAML front matter (`project`, `attempt`, `track`, `unit`, `hypothesis_revision`, `brief_revision`, `plan_revision`, `detail` and the bundle's own `bytes`), then the brief, the plan's approach, the unit's fields and brief, a one-line index of the track's other units, a summary line and reference for each context item, and one for each output of the units it derives from. `?detail=compact` keeps the brief's goal, the unit and the index, cut at a paragraph under 16 KiB. MCP serves it as the resource `cannery-row://projects/{project}/hypotheses/{number}/attempts/{sequence}/context` (append `/compact` for the compact form). The runner does not stage the bundle into step containers; a step that needs it reads it through the API.

## Verify jobs

Submitting a run moves the attempt to `verifying` and creates its **verify job**. Verifiers pull work: `POST /api/projects/{slug}/jobs/claims` (MCP `claim_job`) returns one job, a lease token, and its generation. Who may claim it is the job's `performer`, from the pinned science revision's `verify` section:

- `runner`: the registered verifier, a `verifier` service account running `cannery runner`, claims it with `{"phase": "verify", "revision": "policy-r1"}`. A verifier must name the policy revision it applies and is handed only jobs registered to its name under that revision, so verifiers of different revisions can run side by side and a switch of revision never fails a job.
- `agent`: an `agent` service account or a researcher (a person with the `researcher` role, with a personal token) claims it with `{"phase": "verify"}`, naming no revision. It is never handed a job of an attempt it claimed itself: no one verifies their own run. When the only jobs waiting are of its own attempts, the claim answers `409` and says so.

Heartbeats extend the lease; completion or failure requires the current token, and only the identity that claimed a job may act on it. The job lease token authorizes only reading the listed inputs and writing under `output_prefix`. The claim response names the job, the attempt, the brief revision and, in a planned track, the plan revision and the attempt's [context bundle](#the-context-bundle), which holds the unit.

```json
{
  "schema_version": "0.2",
  "job_id": "verify-run-id",
  "phase": "verify",
  "attempt_id": "attempt-id",
  "performer": "runner",
  "verifier": {"id": "pilchards-verifier", "revision": "policy-r1"},
  "track": "compact-sparse",
  "control": {"id": "base-camp", "revision": "immutable-revision"},
  "parameters": {},
  "steps": [
    {"name": "sparse-producer", "revision": 3, "manifest": {"…": "resolved step manifest"}},
    {"name": "scorer", "revision": "science-revision", "manifest": {"…": "resolved step manifest"}}
  ],
  "science_revision": "science-revision",
  "inputs": {
    "run": {"ref": "evidence-id", "sha256": "…"},
    "manifest": {"ref": "manifest-id", "sha256": "…"},
    "baselines": [{"id": "base-camp", "revision": "immutable-revision"}],
    "datasets": [{"id": "dataset-id", "revision": "immutable-revision"}]
  },
  "output_prefix": "projects/pilchards/attempts/attempt-id/verify-runs/verify-run-id/",
  "deadline": "2026-09-29T06:00:00Z",
  "limits": {"max_output_bytes": 10737418240},
  "lease": {"token": "opaque", "generation": 1, "expires_at": "2026-09-29T00:10:00Z"}
}
```

The job document is `contracts/schemas/job.schema.json`. `verifier` is present only when the performer is `runner`. `steps` are the track's producer and the project's scorer: the runner executes them, and an agent may run them or verify another way. `control` is present when the hypothesis names one: the report gives its revision as `control_revision`. `parameters` are the hypothesis's (the `project_fields` of the hypothesis revision the attempt pinned, `{}` when it has none), pinned when the job is created: a later revision of the hypothesis never changes what a running verification, or a rerun of it, sees. `inputs.baselines` lists the baselines the steps may stage: the control first, then each baseline a step takes as input (`from: baseline`), by id from the pinned science revision. The deadline of a runner job covers its steps plus the science revision's `limits.max_deadline_seconds` for the policy (3600 by default); an agent job's whole deadline is `max_deadline_seconds`.

The inputs are the run and its artifacts, never the run notes:

- `GET …/jobs/{id}/inputs/run`: the frozen front matter of the run document (claims, provenance, manifest reference), which the runner stages as `claimed.json`.
- `GET …/jobs/{id}/inputs/manifest`: the run's verified artifact manifest.
- `GET …/jobs/{id}/inputs/object?key=…`: an object of that manifest, or a reused output listed in `resume`.

The unit, the brief and the plan come with the claim. MCP serves the inputs through `get_job_input`.

A job uploads its outputs through `POST …/jobs/{id}/uploads` (MCP `create_job_upload`) and completes with `POST …/jobs/{id}/completion` (MCP `complete_job`): `{"schema_version": "0.2", "job_id": "…", "document": "…", "manifest": {…}}`, where `document` is the [verification report](#verification-reports) and `manifest` (optional) the artifact manifest of the job's outputs (`contracts/schemas/job_completion.schema.json`). Cannery Row checks:

- the front matter against the `verification` schema, and the body against `report_max_bytes`;
- the pinned provenance: `science_revision`, `source_revision` equal to the run's, `control_revision` equal to the pinned control's (none without one), and `dataset_revision` among the job's datasets;
- the measurements against the metric registry and the required slices, and the extensions against the project's `result_extensions`;
- that a `pass` verdict reports every gate as passed, and that every comparison cites the report's own verified measurements;
- for a runner job, that the report comes from the registered verifier and names the registered `policy_revision`;
- that every object of the manifest is a verified upload of this job (or a listed `resume` output), that it covers `required_artifact_roles.verify`, and that the report's `artifact_roles` are in it.

An invalid report from a runner is an infrastructure failure (`invalid_output`) and follows the rerun rules. An invalid report from an agent is refused with `422` and the details, and the job keeps its lease, so the agent can correct and complete it again. A second completion of the same job is idempotent and cannot publish a second result. A valid completion publishes the report, indexes its measurements and comparisons, moves the attempt and its hypothesis to `awaiting_human_review`, opens a `result` review case on the report, and records `attempt.verified` in the audit log.

A job fails with `POST …/jobs/{id}/failure` (MCP `fail_job`): an error code, a sanitized reason, the failing step and log references, never invented metrics (`contracts/schemas/job_failure.schema.json`). A failure, or a lease or deadline the sweep finds expired, reruns the job automatically up to the science revision's `max_auto_retries`: the rerun is a new job run (`origin: auto_retry`, linked to the failed one), and when the failure named a step, its document carries `resume`, `{"from_step": "scorer", "outputs": [{"step", "name", "key", "size_bytes", "sha256", "media_type", "interface"}]}`, so it starts from that step and reuses the verified outputs of the steps before it. When retries are exhausted, the attempt is `failed` and a `failure` review case opens with stage `verify`; a researcher's `retry` creates a fresh verify job from the run. `GET …/jobs/{id}` and `GET …/attempts/{sequence}/jobs` (MCP `get_job`, `list_attempt_jobs`) read a job and an attempt's job history.

## Step manifests and the container contract

A step manifest describes one container run. Field names follow Argo Workflows templates; `from`, `interface`, `network`, `sandbox`, `code` and `setup` are Cannery Row extensions. Manifests are authored as YAML or JSON and stored as JSON. A step either runs code baked into its image, as below, or runs a script from a repository at a pinned commit on a stock image (see [Steps as scripts](#steps-as-scripts-code-setup-and-the-dependency-cache)).

```yaml
apiVersion: cannery-row/v1
kind: Step
metadata:
  name: sparse-producer
spec:
  role: producer                  # producer | scorer | validator | experiment | policy
  container:
    image: registry.example/pilchards/sparse-producer@sha256:…
    command: ["/app/produce"]
    args: []
    env: [{name: BATCH_SIZE, value: "64"}]
    resources:
      limits: {cpu: "8", memory: 32Gi, nvidia.com/gpu: "1"}
  activeDeadlineSeconds: 14400
  network: none                   # none | a declared egress allowlist
  sandbox: "Declared sandbox and capability policy for candidate code."
  inputs:
    artifacts:
      - {name: candidate_checkpoint, from: attempt, path: /cr/inputs/candidate_checkpoint}
      - {name: queries, from: dataset, path: /cr/inputs/queries}
      - {name: corpus, from: dataset, id: nanobeir-corpus, path: /cr/inputs/corpus}
  outputs:
    artifacts:
      - {name: run, interface: ranked-run/v1, path: /cr/outputs/run}
```

The scorer manifest has the same shape with `role: scorer`. Its inputs typically are the producer's `run` (`from: step`), `qrels` (`from: dataset`) and `claimed_sheet` (`from: attempt`, the frozen front matter of the run document, staged as `claimed.json`); its outputs must include `evidence` (`interface: cr-evidence/v0.2`: one JSON object with the `provenance` and `measurements` of the verification report, and optionally its `discrepancies`, `artifact_roles` and `extensions`, plus `observations`, Markdown that becomes the report's body) and may include further roles such as `per_query_results`. Input sources are `attempt` (verified submission artifacts and the frozen run front matter), `dataset`, `baseline` and `step` (a previous step's output of the same name). Each entry of `inputs.artifacts` has these fields:

| Field | Meaning |
| --- | --- |
| `name` | The input's name, snake case (`^[a-z][a-z0-9_]{0,63}$`): unique within the step and its key in `job.json`; where it is staged is `path`. For `from: attempt` it is the artifact role; for `from: step`, the previous step's output. |
| `from` | `attempt`, `dataset`, `baseline` or `step`. |
| `id` | Only for `from: dataset` and `from: baseline` (the schema refuses it on any other source): the registered dataset or baseline id the input reads, a slug that may hold hyphens (`nanobeir-corpus`). When absent, the input reads the id equal to its `name`, so a manifest without `id` keeps working unchanged. An unregistered `id` is refused at `/…/id`; an unregistered `name` used as the id, at `/…/name`. |
| `interface` | Required for `from: step`: the interface the previous step's output must match. |
| `path` | Where the step finds it, under `/cr/inputs/`. |

Rules enforced when a manifest is registered, when a track binds a producer, and at claim time:

- `image` must be pinned by digest; `env` carries no secrets and no `NVIDIA_*` names (the GPU runtime would act on them); `resources` must fit the science revision's ceilings.
- Every dataset and baseline an input reads (its `id`, else its `name`) is registered in the science revision.
- A producer (or an experiment) must not declare a dataset input, under whatever name, whose id the science revision marks as held-out labels; they run before verification, as the candidate. Only trusted steps that judge it may receive them: the scorer, and a [policy step](#policy-steps).
- The producer's output interface must equal the scorer's `from: step` input interface.
- Names are unique within a step; paths live under `/cr/inputs/` or `/cr/outputs/`.
- `code.commit` is a full 40-character commit SHA; `code.repo` is listed in the science revision's `code_repositories` for the step's trust class (`candidate` for a producer or experiment, `trusted` otherwise); `code.path` and each `setup.cache.key_files` entry are relative paths without `..`; `setup` requires `code`; the setup's deadline, like the step's, fits the science revision's `max_deadline_seconds`.

The `role` is one of:

| Role | Where it runs | Rules |
| --- | --- | --- |
| `producer` | First step of a verify job, from the track's binding. | At least one output, each naming its interface; never a held-out labels dataset; no `from: step` input. |
| `scorer` | Last step of a verify job, from the science revision. | Outputs `evidence` (`cr-evidence/v0.2`). |
| `validator` | On a step output, from the interface naming it (see below). | One `from: step` input, no outputs, `network: none`. |
| `experiment` | A step of a `workflow` track's experiment, run by the runner's experiment kind ([workflow tracks](#workflow-tracks)); registered at `/projects/{slug}/experiment-steps`. A verify job refuses it (`invalid_job`). | At least one output, each naming its interface; never a held-out labels dataset; `code.repo` in the candidate repositories. A `from: attempt` input reads the predecessor attempt's artifacts of that role, so it may share its name with an output. |
| `policy` | A verifier's policy run as a step by the runner's `verify` kind after the scorer, named in the verifier's own policy file (see [policy steps](#policy-steps)), never registered through an endpoint; in the job's chain of steps it is refused (`invalid_job`). | Exactly one output, `verdict`; `network: none` for the step itself; no `from: attempt` input (the run's claims and notes are not its business); a `from: step` input reads a producer or scorer output, the scorer's `evidence` by the interface `cr-evidence/v0.2`. Its `setup`, if any, may have network like any setup: it sees no inputs. |

An **interface** is registered in the science revision as `{"name": "ranked-run", "version": 1, "schema": {…}}` (optionally with a `media_type`) or with a `format` identifier and media type when the content is not described by a JSON Schema; `schema` and `format` exclude each other. Every step output that names a registered interface is checked against it before it is uploaded or handed to the next step. These optional fields say how:

| Field | Meaning | When absent |
| --- | --- | --- |
| `encoding` | `json` (one document, parsed whole), `jsonl` (one document per line, streamed and validated line by line) or `binary` (not parsed). A `schema` describes a JSON document, or each line of a JSON Lines file, so it cannot be `binary`. | `jsonl` for a JSON Lines media type (`application/jsonl`, `application/x-ndjson`, …) or format (`jsonl`, `ndjson`); `json` for `application/json`, a `+json` media type, the `json` format, or a `schema` alone; otherwise `binary`. |
| `max_bytes` | Largest file accepted, in bytes. It also caps the JSON document the runner parses, which is never more than 64 MiB whatever `max_bytes` says. | No size cap; a JSON document over 64 MiB fails as too large to validate. |
| `allow_empty` | Whether a zero-byte file is accepted. | `false`: an empty file fails. |
| `magic` | What a file must start with: `gzip`, `parquet`, `zip`, `json` (the first non-blank byte starts a JSON value), or a prefix in lowercase hexadecimal such as `934e554d5059`. | `json` for JSON and JSON Lines; `gzip`, `zip` or `parquet` when the media type (`application/gzip`, `application/zip`, `application/vnd.apache.parquet`, …) or format says so; otherwise nothing is checked. |
| `validate` | `false` skips parsing and validating JSON and JSON Lines content, for a JSON document too large to parse; the other checks still apply. | `true`. |
| `validator` | Name of a validator step in the science revision's `validators`, run on each output of this interface once the other checks pass. | No validator. |

An interface registered before these fields keeps working unchanged, except that its files must now be non-empty and start as its media type (or its `schema`) says, and the JSON a `schema` interface describes is validated against it.

Every file of an output must be present, non-empty unless `allow_empty`, within `max_bytes`, and start with its `magic`. A `json` file is then parsed whole (only up to `max_bytes` and never more than 64 MiB: a larger one fails with "too large to validate as one JSON document; declare encoding jsonl, or validate: false", unless `validate` is `false`) and validated against the `schema`. A `jsonl` file is streamed and validated line by line, so it never sits in memory whatever its size (blank lines are skipped; a single line is capped like a JSON document). JSON must be UTF-8: a byte order mark is accepted at the very start of the file only, and UTF-16 or UTF-32 is refused. Numbers must be finite, however they are written (`NaN`, `Infinity`, or a literal such as `1e999` that overflows). A document nested too deeply to parse or validate is a problem of the file ("nested too deeply to check"), never a crash. At most five problems are reported per file, each with its JSON Pointer and, for JSON Lines, its line number. A problem says where it is and what the schema expects (its keyword and constraint, such as `/queries/q1/1: must be of type string` or `/rank: must be >= 1`), never the file's own values: reasons and audit events must not leak a step's output.

Other formats get the checks above only; a project can register a **validator step** for anything more. It is a step manifest with `role: validator` in the science revision's `validators`: exactly one input, `from: step`, whose `interface` is the interface that names it, no outputs, and `network: none`. The runner runs it like any other step (its own directory and copy of the step code, its own deadline and resource limits) with the output mounted read-only at its input path, and uploads its log as a `validator_log`. A verify job's deadline includes the deadlines of the validator steps its outputs go through. The validator's exit code is its answer:

| Exit | Meaning | Outcome |
| --- | --- | --- |
| `0` | The output is accepted. | The job goes on. |
| `1` | The output is rejected: the only content verdict. The validator says why in its log. | `invalid_step_output`, a verifier-side failure (the API holds no evidence of its own, see below). |
| Anything else: another code, a signal, `137` (killed, as by the out-of-memory killer), a deadline | The validator failed; that says nothing about the output. | `step_failed` (or `deadline_exceeded`), a verifier-side failure. |

Either way the failure reason names the validator and points at its `validator_log` artifact; it never quotes the log, which may quote the output. A validator must catch its own errors and exit with a code other than 1 when it cannot check: an uncaught Python exception exits 1, which would read as a rejection. The scorer's `evidence` (`cr-evidence/v0.2`) is not a registered interface and keeps its own checks at completion.

A rejected output fails the job with `invalid_step_output` and a reason naming the step, output, file, interface and version, JSON Pointer and line, such as `producer overlap-producer output "run" (run.json) does not match ranked-run/v1: /queries/q1/1: must be of type string`. Who is at fault decides what follows, and the API decides it on evidence it holds itself, never on a verifier's word. A producer's output that the API itself refused (see below) is the candidate's fault: the failure is agent-side, so the attempt fails at once and a `failure` review case opens with stage `agent`, with no automatic rerun (rerunning the same candidate would fail the same way); a researcher's `retry` requeues the hypothesis. The Cannery Row runner makes this the normal path: when a producer's output fails its checks, it first uploads the failing file naming its interface, the API checks it and refuses it, and the runner then reports `invalid_step_output` naming the step. Everything else is the verifier's: a scorer's output, a validator's rejection, a file the API could not check (over its validation cap, or `validate: false`), or a report the API has no refusal on record for. That is an infrastructure failure of the verify job: it follows the rerun rules, then failure review with the reason preserved, where a researcher can retry.

An agent verifier runs its own checks, but the API checks too when it can: a job output upload may name its step output's `interface` (`POST …/jobs/{id}/uploads` with `interface`, which must be registered in the job's science revision). The bytes are checked against the interface as they stream in, or, sent directly to an S3 store, when the upload finishes (never on the declared size alone): size, emptiness and leading bytes always, and for JSON or JSON Lines no larger than the server's `storage.validate_json_max_bytes` (64 MiB by default), the content too, line by line for JSON Lines. At most `storage.max_concurrent_validations` uploads (2 by default) have their content validated at once per API process; the others wait for a slot, up to `storage.max_stream_seconds`, then get `503 unavailable` and may retry the PUT. A file that does not match is refused with the error code `invalid_content` and its problems as details, as soon as a fatal problem shows (wrong leading bytes, over `max_bytes`, five problems), without reading the rest of the body. Nothing of it is kept, the grant is spent and records the refusal (and an `artifact.refused` audit event), and the job keeps its lease, so the verifier reports the job's failure (`invalid_step_output`, naming the step). That record is the evidence: a report of `invalid_step_output` naming a producer step is agent-side only when the API refused, in the same job, an upload under that step's output path (`<step>/<output>/…`) with that output's role and declared interface. An accepted output records its `interface` and `content_validated`: `false` when only size and leading bytes were checked (binary content, a file over the server's cap, or `validate: false`). The Cannery Row runner names the interface of every output it uploads.

The runner runs the steps in order and gives each container the same contract:

| Path / signal | Meaning |
| --- | --- |
| `/cr/job.json` (read-only) | Job id, attempt id and `#number`, track, science and producer revisions, step name, and manifest parameters. `inputs.datasets` lists the datasets the step declares, each `{name, id, revision}`: the input's `name`, the registered `id` it reads and the revision the job pins. An experiment step gets the attempt, the workflow, the hypothesis's `parameters` and its inputs instead ([workflow tracks](#jobjson-of-an-experiment-step)). No tokens. |
| `/cr/inputs/<name>/` (read-only) | Each declared input, downloaded and SHA-256-verified by the runner before the container starts. |
| `/cr/outputs/<name>/` | Each declared output. After exit the runner checks it against its interface, then uploads it under the job's create-only prefix and records SHA-256 and size. |
| `/cr/code` (read-only) | With `code`: the repository's tree at `code.commit` (only `code.path`, when given), and the command's working directory. Absent otherwise; the image's own working directory applies. |
| `/cr/cache` (read-only) | With `setup`: what the setup left in its `/cr/cache`, shared by every step run with the same cache key. During setup itself, `/cr/cache` is writable and `/cr/code` holds only the `key_files`. |
| Exit code | `0` is success. Non-zero, deadline exceeded, OOM kill or a missing declared output is an infrastructure failure of the job and follows the rerun rules. An output that does not match its interface is `invalid_step_output`: agent-side for a producer output the API itself refused, an infrastructure failure otherwise (see above). |
| stdout/stderr | Captured by the runner and stored as a `step_log` artifact for every run, successful or not. |
| Network and credentials | No network unless the manifest declares egress. No Cannery Row, database or storage credentials are ever mounted; containers never call the API. |

The runner completes the job with the verification report it composes from the scorer's `evidence` output and the policy's verdict, plus a manifest of every step's outputs and logs, or fails it with the failing step, error code, sanitized reason and log references.

### Steps as scripts: code, setup and the dependency cache

A step does not need an image of its own. Like a GitHub Actions job, it can run a script from a repository at a pinned commit on a stock image (`python:3.13-slim`, `node:22-slim`, pinned by digest), with its dependencies installed once by a **setup** command and cached by the runner. Two optional fields of `spec` say so:

| Field | Meaning |
| --- | --- |
| `code.repo` | GitHub repository, `owner/name`. It must be listed in the science revision's `code_repositories` for the step's trust class (below). |
| `code.commit` | Full commit SHA, 40 lowercase hexadecimal characters. A branch or a tag is refused at registration: it can move. The commit must be on a branch of `code.repo` itself (below). |
| `code.path` | Optional subdirectory of the repository. Only it is mounted at `/cr/code`, and `key_files` are relative to it. |
| `setup.run` | Shell command (`/bin/sh -c`), run in `/cr/code` in the step's image, with the step's `env` and resource limits. |
| `setup.cache.key_files` | Files under `/cr/code` whose content is part of the cache key: lockfiles such as `requirements.txt` or `package-lock.json`, and any script `setup.run` calls. **They are the only files of the code setup sees.** May be empty. |
| `setup.cache.paths` | Optional paths under `/cr/cache` that setup must create; a setup that exits 0 without them fails. |
| `setup.network` | Setup's network: unrestricted egress when absent, `none`, or a declared allowlist (`{egress: [...]}`, enforced only where the launcher enforces allowlists). |
| `setup.activeDeadlineSeconds` | Setup's own deadline, 600 seconds when absent. A verify job's deadline includes it. |

`setup` requires `code`: the cache key is derived from the code's repository and key files, so a setup without code would have nothing pinning what it installs. A step can have `code` without `setup` (a script with no dependencies beyond its image).

**Trust classes and allowed repositories.** A step's role gives its code a trust class: `candidate` for a `producer` or an `experiment` (the code under test), `trusted` for the `scorer`, a `validator` or a `policy` step (the code that judges it). The science revision lists the repositories each class may run code from:

```yaml
code_repositories:
  candidate: [pilchards/retrieval-scripts]
  trusted: [pilchards/scoring]
```

Repository names compare in any case. A class the revision does not list runs no code from any repository. A manifest whose `code.repo` is not listed for its class is refused when it is registered, when a track binds it, at claim time, and in a new science revision (its scorer and validators), with the JSON Pointer `/spec/code/repo`. The runner can narrow this further with `--github-allowed-repos` ([deploy](deploy.md#the-runners-github-credential)); a repository outside that list fails the job with `code_not_allowed`.

What the runner does with such a step, before it starts it:

1. **Fetch the code.** With its own read-only GitHub credential ([deploy](deploy.md#the-runners-github-credential)), which no step ever sees, the runner first checks the commit belongs to `code.repo` itself. GitHub keeps a repository and its forks in one object network, and serves a fork's commit through the parent's URLs too, so a commit SHA alone does not show which repository it came from. The commit must be reachable from the default branch, or from one of the first 100 other branches (`GET /repos/{owner}/{repo}/compare/{branch}...{sha}`). A commit only in a fork, or only on a branch past the first 100, is refused as missing. The runner then fetches the tarball (`GET /repos/{owner}/{repo}/tarball/{sha}`). It checks the archive is that commit, using the commit id `git archive` records in it, and extracts it into its cache root. It refuses the whole archive when:
   - a member has an absolute path or a `..` segment;
   - a member is a hard link, a device or a FIFO;
   - a symbolic link resolves outside the tree, on its own or through other links;
   - the tree passes 100,000 entries or 2 GiB (512 MiB compressed).

   The tree is kept by `repo@commit`, so the same commit is never fetched twice while it stays cached.
2. **Compute the cache key**: SHA-256 over:
   - the image digest;
   - `setup.run`;
   - the repository and `code.path`;
   - the step's `env`;
   - the setup's network (open, `none` or its allowlist);
   - the trust class;
   - the path and SHA-256 of each `key_files` entry.

   **The commit is not part of the key**: a new commit that leaves the key files alone reuses the cache. Any change to `env` or the setup's network is a new key, since either can change what an install produces (a `PIP_INDEX_URL`, another mirror). A candidate step's cache never serves a trusted one, even with identical image, command and lockfile, so code under test can never prepare what the scorer imports.
3. **On a cache miss, run setup** in the step's image, as its own container, with only this:
   - in `/cr/code`, **only the `key_files`**, read-only, at their paths;
   - a fresh, empty, writable `/cr/cache`;
   - the network as `setup.network` says.

   Setup gets **nothing else**: no `job.json`, no `/cr/inputs/`, no datasets, held-out labels or credentials. Setup runs dependency code (package managers and whatever install hooks they run) with network access. That is why it sees nothing a step's inputs could leak to, and why the key files are all of the code it sees: what it installs depends on the key only. Its combined stdout and stderr are uploaded as a `setup_log` artifact. Its `/cr/cache` is published to the runner's cache by an atomic rename only when all of these hold:
   - setup exits 0;
   - it created every `setup.cache.paths` entry;
   - the cache holds no symbolic link that resolves outside it, and no directory the runner cannot read.

   Otherwise the cache is discarded. On a hit, setup is skipped and no `setup_log` is uploaded.
4. **Run the step** with the whole `/cr/code` tree and `/cr/cache` read-only, `/cr/code` as working directory, and its own `network` (none unless it declares egress). The step's `command` and `args` run as for any step.

Because setup sees only the key files, a few rules follow:

- **`setup.run` must not install the project itself**: no `pip install .`, `pip install -e .`, `npm install` of the package in `/cr/code`, or build of the code tree. Install dependencies from the lockfile only. The step runs the project's code from `/cr/code`, where it is whole.
- **A setup script must be listed in `key_files`** to be visible (`key_files: [requirements.txt, scripts/setup.sh]`, `run: sh scripts/setup.sh`). Listing it also puts its content in the key.
- **`env` and the setup's network are part of the key.** Variables that change between runs without changing what is installed (a seed, a batch size) still make a new cache. Keep them out of a step that has a setup, or accept the extra setup runs.
- **No virtual environment in the cache.** A Python venv links to the image's interpreter (`bin/python -> /usr/local/bin/python3`), a link outside the cache, so the runner refuses it. Install into a plain directory (`pip --target`) instead.

Failures are the verifier's: infrastructure failures of the verify job, which follow the rerun rules.

| Failure | Error code |
| --- | --- |
| GitHub does not have the repository or commit, the credential cannot see it, the commit is only a fork's, or GitHub is unreachable or rate-limits the runner longer than it waits. | `runner_error` |
| An unsafe archive, or a missing `code.path` or `key_files` entry. | `invalid_code` |
| A repository outside the runner's `--github-allowed-repos`. | `code_not_allowed` |
| A setup that exits non-zero, runs out of memory, leaves a declared path missing, or leaves a link out of its cache or an unreadable directory. Its reason points at the `setup_log`. | `setup_failed` |
| A setup past its deadline. Its reason points at the `setup_log`. | `deadline_exceeded` |

A missing commit stays a `runner_error`, as GitHub being down is, since the runner cannot tell a commit that will never exist from one it cannot see yet (a credential not installed on the repository yet). A manifest that names a commit GitHub will never serve therefore fails every job deterministically, and each failure costs the automatic reruns (`max_auto_retries`, 1 by default) before failure review. Register a new producer revision naming a good commit rather than retrying.

A complete example: a producer that runs `produce.py` from `pilchards/retrieval-scripts` on the stock Python image, with its dependencies installed by pip.

```yaml
apiVersion: cannery-row/v1
kind: Step
metadata:
  name: scripted-producer
spec:
  role: producer
  code:
    repo: pilchards/retrieval-scripts                  # in code_repositories.candidate
    commit: 4f2a9c1e8b7d6a5f4e3d2c1b0a9f8e7d6c5b4a39   # git rev-parse HEAD, never a branch
    path: producers/sparse                             # holds produce.py and requirements.txt
  setup:
    run: >-
      pip install --require-hashes --no-deps --only-binary :all: --no-cache-dir
      --target /cr/cache/site -r requirements.txt
    cache:
      key_files: [requirements.txt]
      paths: [site]
    network: {egress: ["pypi.org:443", "files.pythonhosted.org:443"]}
    activeDeadlineSeconds: 900
  container:
    image: docker.io/library/python@sha256:7c61056e61ac89e852de05f3dc6fa51a6dd2181797bceed46aa725dd7cb2cd3b
    command: [python, produce.py]
    env: [{name: PYTHONPATH, value: /cr/cache/site}]
    resources:
      limits: {cpu: "2", memory: 4Gi}
  activeDeadlineSeconds: 3600
  network: none
  sandbox: "Stock Python image; code and dependencies read-only, no network."
  inputs:
    artifacts:
      - {name: candidate_checkpoint, from: attempt, path: /cr/inputs/candidate_checkpoint}
      - {name: queries, from: dataset, path: /cr/inputs/queries}
  outputs:
    artifacts:
      - {name: run, interface: ranked-run/v1, path: /cr/outputs/run}
```

`requirements.txt` pins every package, its dependencies included, with its hash (for example `pip-compile --generate-hashes`). For the first job, the runner verifies and fetches the commit, runs the `pip install` with only `requirements.txt` in `/cr/code` (uploading its `setup_log`), and publishes `/cr/cache/site`. It then runs `python produce.py` in `/cr/code` with `PYTHONPATH=/cr/cache/site`, offline. Later jobs find the commit and the cache and run the step at once. A new commit that changes `produce.py` but not `requirements.txt` is fetched, but reuses the cache.

The repository's `examples/fixture/steps/scripted-overlap-producer.json` is the fixture producer written this way. Its `code.commit` is a placeholder (`0000…`) that registers but cannot run. To run it, set it to a commit of the repository that contains `examples/fixture/steps/requirements.txt` and is on one of its branches. A private repository also needs a runner credential that can read it ([deploy](deploy.md#the-runners-github-credential)).

Setup recipes. Each one installs from a lockfile only, without the project itself and without running package build or install scripts, into a plain directory of `/cr/cache`:

- **Python (pip).**
  - Setup: `run: pip install --require-hashes --no-deps --only-binary :all: --no-cache-dir --target /cr/cache/site -r requirements.txt`, with `key_files: [requirements.txt]` and `paths: [site]`.
  - Step: `env: [{name: PYTHONPATH, value: /cr/cache/site}]`.
  - `--require-hashes` makes pip refuse any file whose hash is not in the lockfile. A compromised or re-uploaded package cannot slip in, and a package missing from the lockfile fails instead of being resolved anew.
  - `--only-binary :all:` avoids running any package's build code (`setup.py`) during setup.
  - `--no-deps` relies on the lockfile listing every dependency.
- **Node (npm).**
  - Setup: `run: mkdir -p /cr/cache/npm && cp package.json package-lock.json /cr/cache/npm/ && npm ci --ignore-scripts --no-audit --no-fund --cache /cr/cache/.npm --prefix /cr/cache/npm && rm -rf /cr/cache/.npm`, with `key_files: [package.json, package-lock.json]` and `paths: [npm/node_modules]`.
  - `--prefix` reads `package.json` and the lockfile from that directory, so setup copies them there first (`/cr/code` is read-only). `--cache /cr/cache/.npm` keeps npm's download cache inside the setup's own cache, removed once the install is done.
  - `npm ci` installs exactly the lockfile, with its integrity hashes. `--ignore-scripts` keeps every package's `preinstall`, `install` and `postinstall` scripts from running, which is where most malicious npm packages act.
  - Step: Node looks for a `node_modules` directory next to the importing file and in its parent directories, so it does not find `/cr/cache/npm/node_modules` on its own. A `node_modules` link in the repository cannot help either: it would point outside the code tree, and the runner refuses such links.
    - For CommonJS (`require`), set `env: [{name: NODE_PATH, value: /cr/cache/npm/node_modules}]`.
    - ES modules (`import`) ignore `NODE_PATH`. Import each dependency by its explicit path, such as `import x from "/cr/cache/npm/node_modules/x/index.js"`, or commit a bundle with the dependencies built in.
- **Everywhere.** Write only under `/cr/cache` and `/tmp` (`HOME` is `/tmp`): the code and the image are read-only. Keep the cache small: the runner evicts least recently used caches past its size cap.

The step itself never has the network unless it declares egress, so anything it imports must be in the image, `/cr/code` or `/cr/cache`.

### What each launcher guarantees

The runner hands each step to a **launcher** (`cannery runner --launcher local|docker|kubernetes`), which decides how much of the contract above actually holds. Write steps against the contract, and pick the launcher that enforces what your steps need. Whatever the launcher, the runner stages inputs and `job.json` before the step starts, checks outputs against their interfaces after it ends (a failure keeps the same attribution), uploads the log on every run, and reports a step past its deadline as `deadline_exceeded`, a step that ran out of memory as `step_failed` (reason "ran out of memory and was killed"), and a step that could not be prepared or started for a reason other than its own command (an image that cannot be pulled, a GPU device missing, a daemon that refuses a request, a Pod that cannot be scheduled) as `runner_error`; a command that is missing exits 127, one that cannot be executed 126. A step's deadline counts from its start: image pulls, scheduling and waiting for a free GPU are not counted. Whatever the launcher, the runner also fetches, verifies and caches a step's `code`, computes its setup's cache key, runs setup only on a miss and on an empty job directory, and publishes its cache only when it succeeded.

| Guarantee | `local` (`--unisolated-local`) | `docker` | `kubernetes` |
| --- | --- | --- | --- |
| Intended for | Trusted fixture and development code only. | Untrusted step code, on a machine dedicated to the runner. | Untrusted step code in a cluster, in a namespace of its own. |
| What runs | The manifest's `command` and `args` as a local process, from a fresh copy of `--step-root`; `python` and `python3` resolve to `python3` on the runner's `PATH`. The image is ignored. | The manifest's image, pulled by digest (a tag is refused), with `command` as entrypoint and `args` as command. | The manifest's image, pinned by digest (a tag is refused), in a Pod of its own (`restartPolicy: Never`), with `command` and `args` as the container's `command` and `args`. |
| Where `/cr` is | A per-step directory, given in `CR_ROOT`; the step must use `$CR_ROOT` instead of `/cr`. | `/cr` (also in `CR_ROOT`). | `/cr` (also in `CR_ROOT`): the step's directory on a per-job volume, which the runner fills before the step and empties after it, through a short-lived transfer Pod. |
| `/cr/job.json`, `/cr/inputs/` read-only | No: plain files the step could change (the runner verifies previous-step outputs it stages again, by SHA-256). | Yes: bind-mounted read-only. | Yes: mounted read-only. |
| `/cr/outputs/` | Writable. A symbolic link anywhere in an output, or replacing `outputs/` or a directory on the way to it, fails the job with `invalid_output` (the verifier's failure), even a link pointing inside the outputs. | Writable bind mount, with no disk quota. Writable besides it: `/tmp` and `/dev/shm`, both in memory. Links are refused as with `local`. | Writable, up to the volume's size (`--k8s-volume-size`). Writable besides it: `/tmp` and `/dev/shm`, both in memory. Copied back as written; links come back as links (never followed) and are refused as with `local`; anything but files, directories and links is dropped. Outputs over `--k8s-max-output-bytes` (the volume's size by default) as a tar, or over `--k8s-max-output-files` members (100000), fail the job with `invalid_output`. |
| `/cr/code`, `/cr/cache` | Copied fresh into the step's directory (`$CR_ROOT/code`, `$CR_ROOT/cache`), not read-only; the command runs in `$CR_ROOT/code`. Paths in `env` are not translated, so a step meant to run here finds its cache at `$CR_ROOT/cache`. | Bind-mounted read-only from the runner's cache root; `WorkingDir` is `/cr/code`. | Copied with the inputs into the step's directory on the per-job volume (`code/`, `cache/`), links as links, never followed; mounted read-only with a `subPath`; `workingDir` is `/cr/code`. They count in the volume's size (`--k8s-volume-size`). |
| Setup | A local process in an empty directory with only the key files copied into `$CR_ROOT/code` and the cache linked; network not restricted. | Its own container on an empty job directory, only the key files in `/cr/code`, the cache writable, Docker's default `bridge` network (or none with `setup.network: none`); an egress allowlist is not enforced. | Its own Pod, locked down as a step's (non-root, no ServiceAccount token, no environment or volumes beyond the manifest's and `/cr`), on an empty step directory with only the key files in `/cr/code` and a writable `/cr/cache`, copied back to the runner's cache after the run as outputs are (links as links, same size and file limits); the `egress` network label (any destination the namespace allows), or none with `setup.network: none`. |
| Isolation from the runner | None: the step runs as the runner's user and can read its data root (held-out labels included), its token file, and other steps' code. | Own container: sees only its job directory; non-root user (the runner's own uid:gid, 10001:10001 in the image), read-only root filesystem, every capability dropped, `no-new-privileges`, a PID limit, a `/tmp` tmpfs, a `/dev/shm` of `--docker-shm-size`. | Own Pod: sees only its directory of the job's volume; non-root user (`--k8s-step-user`, 10001:10001 by default), read-only root filesystem, every capability dropped, no privilege escalation, the `RuntimeDefault` seccomp profile, no ServiceAccount token, no service links. The PID limit is the kubelet's (`podPidsLimit`). |
| Network | Not restricted. | `network: none`: no network at all. Declared egress: Docker's default bridge, **any destination**; the allowlist is not enforced yet. | `network: none`: the namespace's deny-all NetworkPolicy (`deploy/runner-k8s/`), no traffic in or out, DNS included, from the step's first instant (an init container waits until the policy holds); it needs a cluster that enforces NetworkPolicy. Declared egress (or a setup with network): a second policy (`cannery-steps-egress`) lets the Pod out to **any destination** but the metadata server and the cluster's Pod and Service ranges, DNS included, and nothing in; the allowlist is not enforced yet. |
| `resources.limits` | Ignored. | `memory` (no swap) and `cpu` enforced; undeclared means unlimited. `nvidia.com/gpu`: that many GPUs, each lent to one step at a time (`--docker-gpu-mode`, `--docker-gpu-devices`); a step waits for free ones before it starts. Ceilings are per step: steps side by side add up. | `memory` and `cpu` as the Pod's requests and limits; undeclared means none. `nvidia.com/gpu`: that many GPUs through the device plugin, with `--k8s-gpu-runtime-class` if given. |
| Deadline and lease loss | The process group is killed (a child that calls `setsid` escapes). | The container is killed (`SIGKILL`); the deadline starts when the container starts, after the image pull. | The Pod is deleted (grace period 2 seconds, and the runner waits until it is gone); the deadline starts when the Pod is Running, after scheduling and the image pull. A Pod still Pending after `--k8s-scheduling-timeout`, or one the cluster evicts, preempts or loses, is a `runner_error`. |
| Out of memory | Exit code 137, reported as "exited with code 137 (killed, possibly out of memory)". | Docker's `OOMKilled` state, reported as "ran out of memory". | The container's `OOMKilled` reason, reported as "ran out of memory". |
| Log | Combined stdout and stderr of the process group. | Combined stdout and stderr of the container (Docker keeps at most the latest 200 MiB). | Combined stdout and stderr of the container, followed from the Pod log API while it runs (at most the first 200 MiB). |
| Cleanup | The process group, on every path. | The container is removed on every path; leftovers of a crash are removed at the next start of a runner with the same `--runner-id`. | Pods are deleted on every path, the job's volume when the job ends; leftovers of a crash are deleted at the next start of a runner with the same `--runner-id` in the namespace. |

## Workflow tracks

A `workflow` track's experiments are run by a Cannery Row runner of the experiment kind instead of an outside agent ([experiment modes](spec.md#experiment-modes)). Setting one up takes four things: a science revision whose hypothesis schema (`hypothesis_fields`) describes the parameters, one or more registered experiment step manifests, a track in `workflow` mode naming them, and a runner whose configuration lists an `experiment` kind with the token of an **experimenter** service account ([deployment](deploy.md#the-experiment-kind)). An admin creates the experimenter like any service account (`POST /projects/{slug}/service-accounts` with `"kind": "experimenter"`, or the admin settings in the web app), then a token for it from the web app: a service token can only be created from a signed-in browser session. Hypotheses come from the track's plan, as in any track.

### Experiment steps

An experiment step is a step manifest with `role: experiment` ([step manifests](#step-manifests-and-the-container-contract)). An installation admin registers it with `POST /projects/{slug}/experiment-steps` (the manifest as body); each registration of a name appends the next immutable revision (audited as `experiment_step.registered`). `GET /projects/{slug}/experiment-steps` lists them (`name` filter, newest revision first, paged like producers) and `GET /projects/{slug}/experiment-steps/{name}/{revision}` reads one. Experiment steps live apart from producers: a producer cannot be named in a workflow, nor an experiment step bound as a track's producer.

A step is candidate code, so the same rules as a producer's apply: image pinned by digest, no secret in `env`, resources and deadlines within the science revision's limits, no held-out labels dataset as input, and `code.repo` in `code_repositories.candidate`. Its inputs come `from: dataset`, `from: baseline`, `from: step` (an earlier step's output of that name, with the same interface) or `from: attempt`: the **predecessor** attempt's verified artifacts of that role, when the hypothesis was tried before (an empty directory otherwise). A `from: attempt` input may share its name with an output, so a step can resume from the previous attempt's `candidate` and write the new one.

### The workflow

A track's `workflow.steps` are checked when the track is created or changed, and again under the science revision a claim pins (a claim skips a track that no longer fits and, when only such tracks have queued hypotheses, answers `409 workflow_unavailable` naming them; see [claim and run](#claim-and-run)):

- every step is a registered experiment step revision that fits the science revision, and appears once;
- across the workflow, output names are unique and none is a log role (`step_log`, `setup_log`, `validator_log`); a `from: step` input matches an earlier step's output name and interface;
- the **last** step, and only it, has an output named `run` with interface `cr-run/v0.2`;
- the outputs cover every role in the science revision's `required_artifact_roles.attempt`, and every `from: attempt` input of the track's producer (other than the claimed sheet, which the verify job takes from the run document), since the verify job reads them.

Every output but `run` becomes attempt artifacts whose role is the output's name, so an output name must also be a valid artifact role (`^[a-z][a-z0-9_]{0,63}$`). An output holds flat files only; each file is one artifact, named by its file name.

### Claim and run

The runner claims with `POST /projects/{slug}/claims` and `{"mode": "workflow"}` (optionally `hypothesis` or `track`), using its experimenter token; an experimenter's claim without `mode` is a `workflow` claim too. Claims are separated by identity: only an experimenter claims in `workflow` mode, and only there (an experimenter asking for `"mode": "agent"` gets `403`); an agent's or a researcher's claim is always in `agent` mode, and asking for `workflow` gets `403`. An experimenter cannot write plans, comment or decide. When nothing can be claimed the answer is `409` with the error code `nothing_to_claim`. A claim skips a `workflow` track whose workflow or producer no longer fits the current science revision (checked as at track creation), so other tracks' hypotheses still flow; when only such tracks have queued hypotheses (or the named `hypothesis` is in one), the answer is `409 workflow_unavailable`, whose message and `details` (`/tracks/<slug>`) name each track and why. The track is not paused: it waits for a researcher to fix it or the science revision. Any other `409` is a genuine conflict (a track paused or switched during the claim, a control a step cannot stage). The MCP tools `claim_hypothesis`, `release_attempt`, `get_track` and `update_track` follow the same rules. The answer is an ordinary claim (attempt, lease token, generation, heartbeat interval) whose attempt has `mode: "workflow"` and pins `workflow: {"steps": [...]}`, plus a `workflow` object, the run specification:

```jsonc
{
  "attempt_id": "attempt-id",
  "attempt_ref": "#7.2",
  "track": "scripted",
  "science_revision": "1",
  "steps": [{"name": "fixture-experiment", "revision": 1, "manifest": {"…": "resolved step manifest"}}],
  "parameters": {"top_k": 2},
  "inputs": {
    "datasets": [{"id": "queries", "revision": "queries-r1"}],
    "baselines": [{"id": "base-camp", "revision": "fixture-r1"}],
    "predecessor": {
      "attempt_id": "previous-attempt-id",
      "ref": "#7.1",
      "state": "failed",
      "failure_code": "step_failed",
      "artifacts": [{"id": "artifact-id", "role": "candidate", "storage": {"backend": "s3", "bucket": "…", "key": "…"}, "size_bytes": 13, "sha256": "…", "media_type": "application/json"}]
    }
  },
  "limits": {"max_output_bytes": 10737418240},
  "deadline": "2026-09-29T06:00:00Z",
  "control": {"kind": "baseline", "id": "base-camp", "revision": "fixture-r1"}
}
```

`parameters` are the `project_fields` of the hypothesis's approved revision (`{}` when it has none), already validated against the science revision's `hypothesis_fields` when the plan was approved. `inputs.datasets` never lists a held-out labels dataset. `inputs.predecessor` is null on a first attempt. Otherwise it gives the predecessor's `state` and `failure_code` (its latest failure's, or null) and lists its verified uploads whose role some step reads `from: attempt`. A predecessor that failed, in particular one whose run crashed, may have uploaded only part of its outputs: a step that resumes from it decides whether what it finds is usable. The runner downloads each listed artifact with `GET …/hypotheses/{number}/attempts/{sequence}/inputs/predecessor/{artifact_id}` under the attempt's lease (a `302` to a presigned URL with an S3 store, or the bytes), and checks its size and SHA-256. Only an experimenter may call it (`403` otherwise), and only for the predecessor's uploads of those roles (`404` otherwise). `deadline` is the sum of the steps' deadlines (with their setups and validators) plus the server's `leases.job_overhead_seconds`, pinned on the attempt at claim: the runner stops there, the API refuses the lease after it (`409 stale_lease`), and the sweep fails the attempt with `deadline_exceeded`. `control` is present when the hypothesis names one.

The runner heartbeats the attempt (`POST …/heartbeat`) and runs the steps in order through its launcher, with the [container contract](#step-manifests-and-the-container-contract). After each step it checks every output against its interface and validator, then uploads every output but `run` through the attempt's upload grants (`POST …/uploads` with the output's name as `role`), with its log as `step_log` (and `setup_log`, `validator_log`). It then posts the artifact manifest of every upload and submits the run document (`POST …/submission`, with an `Idempotency-Key`). The attempt goes on to `verifying` exactly as an agent's would.

### `job.json` of an experiment step

```jsonc
{
  "attempt_id": "attempt-id",
  "attempt_ref": "#7.2",
  "track": "scripted",
  "science_revision": "1",
  "workflow": [{"name": "fixture-experiment", "revision": 1}],
  "step": "fixture-experiment",
  "role": "experiment",
  "parameters": {"top_k": 2},
  "inputs": {
    "datasets": [{"name": "queries", "id": "queries", "revision": "queries-r1"}],
    "baselines": [{"id": "base-camp", "revision": "fixture-r1"}],
    "predecessor": {"attempt_id": "previous-attempt-id", "ref": "#7.1", "state": "failed", "failure_code": "step_failed"}
  },
  "control": {"kind": "baseline", "id": "base-camp", "revision": "fixture-r1"},
  "manifest": {"…": "this step's manifest"}
}
```

`inputs.datasets` lists only the datasets this step declares, each with the input's `name` and the registered `id` it reads (they differ when the input gives an `id`); `inputs.predecessor` is null on a first attempt. Like a test job's, it carries no token.

### The run document output

The last step writes exactly one Markdown file in `/cr/outputs/run/` (`run.md`, any name ending in `.md`, at most 1 MiB): the [run document](#run-documents) of the attempt. Its front matter may hold `claims` (each with `authority: "agent_claim"`), `artifact_roles` (the roles of the artifacts the claims rely on), `extensions`, and `provenance` (for example a `source_revision`, which otherwise defaults to `sha256:` and the digest of the pinned workflow); its body holds the run notes. The runner sets `provenance.science_revision` and `manifest`, overriding the step, and submits the document. The API then validates it like any submission. A file that is not valid UTF-8 or whose front matter does not parse is a run failure the runner reports (`invalid_step_output`), retried automatically; a document the API refuses fails the attempt with `invalid_submission`, the candidate's failure, at once.

### Failures, release codes and automatic retries

A runner that cannot finish releases the attempt (`POST …/release`) with a `reason`, a `code`, the failing `step` and `logs`, the log references of the failing run (`[{key, size_bytes, sha256}]`, each a verified upload of this attempt, kept on the failure as its `log_refs`). Only an experimenter may send `code`, `step` or `logs` (`403` for anyone else), and an experimenter must send a `code` (`422`). The failure class of a runner-driven attempt comes from the experimenter's report and is trusted, as a verifier's word is for verify infrastructure ([failure classes](spec.md#data-model-and-lifecycle)). The candidate is blamed at once only on evidence the API holds itself, as in `agent` mode:

| Failure | Reported by | Class | What follows |
| --- | --- | --- | --- |
| `step_failed`, `deadline_exceeded`, `setup_failed`, `runner_error`, `upload_expired`, `missing_input`, `invalid_input`, `input_verification_failed`, `invalid_code`, `code_not_allowed`, `invalid_job`, `invalid_step_output`, `invalid_output`, `missing_output`, `held_out_labels_to_experiment` | The experimenter, on release | Run failure | The hypothesis is queued again automatically, no review case, while the science revision's `max_auto_retries` allows (counted since the last failure that went to review); then a `failure` review case. |
| `lease_expired` (the runner stopped heartbeating), `deadline_exceeded` (the attempt's deadline passed) | The sweep | Run failure | As above. |
| `upload_verification_failed` (an upload the API refused or could not verify), `invalid_submission` (a run document the API rejected) | The API, on its own evidence | Candidate | The attempt fails and a `failure` review case opens at once. |

Every failure is kept on the attempt (`failures[]`, with `requeued: true` for an automatic retry and the failing run's `log_refs`). Each failure records `attempt.failed` with the code and the failing `step`; an automatic retry adds `requeued: true` to it and records `hypothesis.requeued` (`automatic: true`, the failed attempt, code, retry number and budget). A researcher's `retry` on a failure case queues the hypothesis again and starts a fresh retry budget. A lost lease stops the run quietly: the runner neither uploads nor releases, and the sweep expires the attempt. Each verify run of the attempt (`GET …/attempts/{sequence}/jobs`) names the service account (`claimed_by`) or the researcher (`claimed_by_user`) that claimed it and the token it claimed with (`via_client`, `token:<name>`).

### A worked example

`examples/fixture/` holds a complete workflow track over the fixture project (the same science revision, datasets, producers and scorer as the two agent tracks):

- `science.json` declares `hypothesis_fields` (`top_k`, an integer from 1 to 10) and the `fixture-candidate/v1` interface (a JSON object with `top_k`);
- `experiments/fixture-experiment.json` is the experiment step: it reads the predecessor's `candidate` (`from: attempt`) and the `queries` dataset, and outputs `candidate` (`fixture-candidate/v1`) and `run` (`cr-run/v0.2`);
- `steps/experiment.py` is its code: it reads `parameters.top_k` from `/cr/job.json`, writes `outputs/candidate/candidate.json` and `outputs/run/run.md`, and notes the predecessor's candidate in the run notes when one was staged;
- `workflow-track.json` is the `scripted` track: tested by `overlap-producer` revision 1, which reads `candidate`, and running `fixture-experiment` revision 1;
- `workflow-hypothesis.json` is a unit entry for that track's plan with `parameters: {"top_k": 2}`.

With the fixture science revision, producers and project brief set up (as for the agent tracks), the steps are:

```sh
API=https://cannery.example.org/api/projects/demo
# An installation admin registers the experiment step (revision 1).
curl -sf -X POST "$API/experiment-steps" -H "Authorization: Bearer $ADMIN" \
  -H 'Content-Type: application/json' -d @examples/fixture/experiments/fixture-experiment.json
# A researcher creates the workflow track.
curl -sf -X POST "$API/tracks" -H "Authorization: Bearer $RESEARCHER" \
  -H 'Content-Type: application/json' -d @examples/fixture/workflow-track.json
# A researcher plans the track; approving the plan creates the hypothesis (#1 here).
curl -sf -X POST "$API/tracks/scripted/plans" -H "Authorization: Bearer $RESEARCHER"
curl -sf -X PUT "$API/tracks/scripted/plans/draft/approach" -H "Authorization: Bearer $RESEARCHER" \
  -H 'Content-Type: application/json' -d '{"approach": "Sweep the cut-off, smallest first."}'
curl -sf -X POST "$API/tracks/scripted/plans/draft/units" -H "Authorization: Bearer $RESEARCHER" \
  -H 'Content-Type: application/json' -d @examples/fixture/workflow-hypothesis.json
curl -sf -X POST "$API/tracks/scripted/plans/draft/submission" -H "Authorization: Bearer $RESEARCHER"
curl -sf -X POST "$API/tracks/scripted/plans/1/review" -H "Authorization: Bearer $RESEARCHER" \
  -H 'Content-Type: application/json' -d '{"action": "approve", "reason": "Ready to run."}'
```

One runner process then runs the experiment and verify kinds, each with its own token (experimenter, verifier), from a configuration file at the repository root:

```toml
# runner.toml
api_url = "https://cannery.example.org"
project = "demo"
data_root = "examples/fixture/data"
work_root = "/tmp/cr-work"

[launcher]
type = "local"   # unisolated: fixture code only
step_root = "examples/fixture/steps"

[[kinds]]
kind = "experiment"
token_file = "experimenter.token"

[[kinds]]
kind = "verify"
token_file = "verifier.token"
policy = "examples/fixture/policy.json"
```

```sh
cannery runner --config runner.toml
```

The experiment kind claims the hypothesis, runs `experiment.py` with `parameters: {"top_k": 2}`, uploads `candidate.json` (role `candidate`) and the step log, and submits the run document; the verify kind then runs `overlap-producer` on that candidate and the fixture scorer, applies the fixture policy and completes the job with its verification report, which waits for a researcher's decision.

## Verification policy and the stock policy

Cannery Row applies no policy itself. Every science revision says who verifies a run, `verify: {performer, verifier?}`:

- `{"performer": "runner", "verifier": {"id": "pilchards-verifier", "revision": "policy-r1"}}`: the registered verifier, a service account of kind `verifier` named `id`, runs the job with `cannery runner`, and every report it publishes must name `revision` as its `policy_revision`.
- `{"performer": "agent"}`: an agent service account or a researcher who did not run the attempt verifies it, applies the policy the project agreed on, and names that policy's revision in the report.

Science revisions registered before verify jobs were rewritten in place: one that registered a tester and an evaluator now registers `verify: {"performer": "runner", "verifier": {"id": <the tester's id>, "revision": <the evaluator's revision>}}`, and one with built-in `gates` or without an evaluator now registers `{"performer": "agent"}`, its `gates` dropped. Both former service accounts became verifiers.

A verifier is an ordinary service account of kind `verifier`. It claims the verify jobs registered to it, reads their inputs, and completes or fails them through the job API. It can also read the project like any member: hypotheses (the promoted ones included), attempts and their reports, metrics, the dashboard and the comparisons earlier reports gave. It can never write a decision: drafting, reviewing, promoting and rejecting stay with agents and researchers.

The stock policy (see [deploy](deploy.md#the-stock-policy)) ships with Cannery Row and applies declarative gates from a versioned JSON configuration (`policy_config.schema.json`). The runner's `verify` kind applies it in-process (see [the verify kind](#the-runners-verify-kind)), and `cannery evaluator` applies it alone, offline, for a verifier that runs its own steps. Its `verifier.revision` is the revision the science revision registers and the `policy_revision` of every report; changing the gates or the baseline values means a new configuration revision and a new science revision registering it. The grammar of its gates is deliberately small: no expressions, scripts or queries. A policy that needs more is a [policy step](#policy-steps).

```json
{
  "gates": [
    {"id": "primary-beats-control", "metric": "ndcg_at_10", "split": "fresh-held-out", "statistic": "uncertainty.lower", "compare": "control", "op": ">", "min_delta": 0.0},
    {"id": "no-language-regression", "metric": "ndcg_at_10", "split": "fresh-held-out", "per_dimension": "language", "statistic": "value", "compare": "control", "op": ">=", "min_delta": -0.01}
  ]
}
```

A configuration holds `schema_version`, `verifier: {id, revision}`, `gates` (at least one, each id once), `baselines` (each `{id, revision, label?, measurements}`, where `measurements` are the baseline's values as `{metric, split, dimensions, value, uncertainty?}`, `dimensions` empty for the overall slice or a single dimension, at most one value per metric, split and slice, an `uncertainty` interval ordered and containing the value) and an optional `default_control` (`{id, revision}`) used when the hypothesis names none. The runner refuses to start on an invalid configuration, and claims only verify jobs registered to its own verifier and policy revision (a job of another should never reach it; if one does, it fails it with `policy_mismatch`).

A gate with `per_dimension` is evaluated for every value of that dimension and passes only if all pass. A gate whose required measurement is missing, non-finite or lacks the requested statistic is `unknown`. The verdict is `pass` if every gate passes, `fail` if any gate fails, and otherwise `inconclusive`; the generated reason lists each gate's result and the values used.

The reference of `compare: "control"` is the control's value for the measurement's metric, split and slice; a measurement without one is `unknown`. A measurement passes when `(statistic - control) op min_delta`. The op must point in the metric's registered direction and the statistic must be the value or the conservative end of the interval: on a `direction: "higher"` metric only `>` or `>=` with `value` or `uncertainty.lower`, on a `direction: "lower"` metric only `<` or `<=` with `value` or `uncertainty.upper`. So on a higher-is-better `ndcg_at_10`, `{"statistic": "value", "op": ">=", "min_delta": -0.01}` lets the value fall at most 0.01 below the control and `{"statistic": "uncertainty.lower", "op": ">", "min_delta": 0}` requires the whole interval above it; on a lower-is-better `latency_ms`, `{"statistic": "value", "op": "<=", "min_delta": 5}` lets the value rise at most 5 above the control and `{"statistic": "uncertainty.upper", "op": "<", "min_delta": 0}` requires the whole interval below it. A gate that inverts this (say `>=` on `latency_ms`, which would pass regressions and fail improvements), or names a metric the pinned science revision does not register, is `unknown`. The control is the one the verify job pins (the hypothesis's), or the configuration's `default_control` when there is none. When the configuration lists a value for that baseline id and revision on the gate's metric, split and slice, the gate compares against it (`control_source: resolved`). A `control_value` the scorer reports alongside must then equal it exactly, on the same shortest round-trip decimals as the arithmetic below; if it does not, that measurement is `unknown`, its detail records a `control_mismatch` with both values, and neither value is used. Where the listed baseline has no value for the slice, where the configuration lists no baselines at all (its scorer-reported mode), or where there is no control, the control value is the one the scorer reported (`control_source: tester_reported`). A pinned control the configuration does not list, while it lists others, makes every gate `unknown` with that reason, as it did when the science revision held the values: comparing against the scorer's own value instead would quietly change what the gate means. The detail of each gate result, and of each slice of a `per_dimension` gate, names its source. The arithmetic is exact on the shortest round-trip decimal of each parsed number (the literal as written whenever it fits a double's precision, since evidence is parsed into binary floats before it is stored), so `0.42 - 0.40 >= 0.02` holds. Only `tester_verified` measurements are read. Without `per_dimension`, a gate reads the one measurement of its metric and split with no dimensions; with it, the values are those the metric registry lists for the dimension (or those measured when the registry leaves it open), each read from the single-dimension slice `{dimension: value}` (the slice a required slice demands; measurements with more dimensions are ignored), and a failing value fails the gate even when another is `unknown`. A repeated slice or a `missing_reason` is `unknown`.

Every slice a gate compared (pass or fail) is also reported once as a comparison, `source: "tester"`, with the reference it was compared against: of kind `baseline`, labelled with the baseline's `label` (by default `<id> <revision>`) and with `ref` the baseline id, when the value came from the configuration; of kind `other`, labelled "control value reported by the scorer", when it came from the scorer.

### The runner's verify kind

The runner (`cannery runner`, see [deploy](deploy.md#job-kinds-and-the-configuration-file)) runs job kinds side by side in one process, each with its own service-account token: `verify` (a verifier token) claims verify jobs, runs their steps and applies the policy, and `experiment` (an experimenter token) claims the hypotheses of [workflow tracks](#workflow-tracks) and runs their experiments. A service account has exactly one kind, so each kind needs its own token. A `verify` kind is configured with one `policy` file, which the verifier owns and Cannery Row never reads:

- a **stock configuration** (above), whose gates the kind applies in-process;
- a **policy step file** (below), whose step the kind runs through the runner's launcher after the scorer.

Either way, the file's `verifier` is what the kind claims jobs for (`{"phase": "verify", "revision": …}` with the verifier's token): a job whose `verifier` differs fails with `policy_mismatch` before any step runs. The kind runs the producer and the scorer (from the step named in `resume` on a rerun, reusing the verified outputs before it), applies the policy, and composes the verification report: the scorer's `evidence` gives the `measurements`, `discrepancies`, `provenance`, `artifact_roles`, `extensions` and, from its `observations`, the body; the policy gives the `verdict`, `reason`, `gates` and `comparisons`; the file's revision is the `policy_revision`. It checks the report against the `verification` schema before it completes the job with it and a manifest of every step's outputs and logs. Two `verify` entries with two policy revisions can run in one process, so a switch of revision needs no second runner.

### Policy steps

A policy step is a policy written as code: a step manifest with role `policy`, run by the runner's `verify` kind after the scorer on each verify job, that reads the verified measurements and writes a verdict. Use it when declarative gates are not enough: a paired bootstrap on per-query results, a significance test, a comparison with a promoted attempt.

**Where it is named.** The verifier's policy step file names the step, not the science revision. The science revision keeps registering `verify.verifier: {id, revision}` only, and that revision is the binding, as for the stock policy: Cannery Row applies no policy itself, and checks only that a runner's report comes from the registered verifier under the registered revision. The verifier owns its policy, so changing the step (its image, its code commit, its command, its environment) means a new revision in the file and a new science revision registering it. Nothing about the API, its schemas or its database changes. The step's manifest, log and verdict are uploaded with the verify job, so what ran stays on record beside the verdict it produced.

**The file.** A JSON file with exactly three fields: `schema_version` (`"0.2"`), `verifier` (`{id, revision}`, the verifier service account's name and this policy's revision, identifiers without whitespace) and `step`, a step manifest (`step_manifest.schema.json`) with role `policy`. That role requires `network: none` and exactly one output, named `verdict`. A policy step has no `from: attempt` input: it never reads the run's claims or notes, only what the producer and the scorer produced, datasets and baselines. The runner refuses to start with a file that breaks these rules (exit code 2). On each job, it also checks the step against the job's science revision, as for any step: resource ceilings, `max_deadline_seconds` (the setup's too), registered datasets and baselines, and a `code.repo` listed in `code_repositories.trusted` (a policy step is `trusted` code, like the scorer). A step that does not fit fails the job with `runner_error`. Its deadlines must also fit what the job's deadline leaves for the policy, the science revision's `max_deadline_seconds` (one hour when unset): the setup's deadline (when it has one), plus the step's, plus the 30 seconds the runner keeps to upload and report. Otherwise the step could be cut short by the job's deadline rather than its own, so it never starts and the job fails with `deadline_exceeded`. Like any step, it may run code baked into its image, or a script from a repository at a pinned commit on a stock image with a cached setup ([steps as scripts](#steps-as-scripts-code-setup-and-the-dependency-cache)).

This is the fixture's (`examples/fixture/policy-step.json`), whose script is `examples/fixture/steps/policy_step.py`, run from the local launcher's step root:

```jsonc
{
  "schema_version": "0.2",
  "verifier": {"id": "cannery-runner", "revision": "fixture-policy-step-1"},
  "step": {
    "apiVersion": "cannery-row/v1",
    "kind": "Step",
    "metadata": {"name": "fixture-policy"},
    "spec": {
      "role": "policy",
      "container": {
        "image": "fixture.invalid/cannery-row/fixture-policy@sha256:c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3",
        "command": ["python3", "policy_step.py"],
        "resources": {"limits": {"cpu": "1", "memory": "256Mi"}}
      },
      "activeDeadlineSeconds": 60,
      "network": "none",
      "sandbox": "Fixture only: the local launcher does not isolate the step.",
      "inputs": {
        "artifacts": [
          {"name": "evidence", "from": "step", "interface": "cr-evidence/v0.2", "path": "/cr/inputs/evidence"},
          {"name": "per_query_results", "from": "step", "interface": "per-query-results/v1", "path": "/cr/inputs/per_query_results"}
        ]
      },
      "outputs": {"artifacts": [{"name": "verdict", "path": "/cr/outputs/verdict"}]}
    }
  }
}
```

**Inputs.** The step runs with the [container contract](#step-manifests-and-the-container-contract) of any step, and no token ever reaches it. The runner stages what the manifest declares:

| Declared input | What the step finds at its path |
| --- | --- |
| `{name, from: step, interface}` | The output of that name of the producer or the scorer in this job, as verified and uploaded: the scorer's `evidence` (`cr-evidence/v0.2`, its measurements, discrepancies and provenance), its `per_query_results`, the producer's `run`… An output the steps did not produce fails the job. |
| `{name, from: baseline, id?}` or `from: dataset` | The runner's data root, `baselines/<id>/<revision>/` or `datasets/<id>/<revision>/` (`<id>` is the input's `id`, else its `name`), as for the other steps, at the revision the job pins. The runner then needs a data root. A dataset may be one the science revision marks as held-out labels: a policy step is trusted code that judges the candidate after it ran, with no network, so it may recompute a metric or a statistic from the labels the candidate never saw, as the scorer does. |

`/cr/job.json` holds what any step of the job gets, plus `verifier` (`{id, revision}`), `role` (`policy`), `control` when the hypothesis names one, `parameters` (the hypothesis's parameters as the verify job pins them: the `project_fields` of the hypothesis revision the attempt pinned, `{}` when it has none, as an [experiment step](#jobjson-of-an-experiment-step) gets them; pinned when the job is created, so a later revision of the hypothesis never changes what a run or a rerun sees, and a policy whose gates are frozen per hypothesis reads them here instead of baking each hypothesis into a new policy revision), `metrics` (the science revision's metric registry, with each metric's direction, splits and dimensions), and `manifest` (the step's own manifest).

**The verdict.** The step writes exactly one JSON file, UTF-8 and at most 1 MiB, in its `verdict` output (`/cr/outputs/verdict/verdict.json`, any file name). It holds `gates`, `comparisons` (optional, empty when absent), `verdict` and `reason`, and nothing else:

```jsonc
{
  "gates": [
    {"id": "mrr-holds-control", "result": "pass", "detail": "0.6875 vs 0.625"},
    {"id": "queries-covered", "result": "pass", "detail": "4 per-query rows for 4 queries"}
  ],
  "comparisons": [
    {"metric": "mrr", "split": "dev", "dimensions": {}, "value": 0.6875, "source": "tester", "reference": {"value": 0.625, "label": "control reported", "kind": "other"}}
  ],
  "verdict": "pass",
  "reason": "pass: mrr-holds-control pass; queries-covered pass"
}
```

These are the fields of a [verification report](#verification-reports) with the same rules: at least one gate, each `{id, result, detail?}` with a slug id assessed once and a result of `pass`, `fail` or `unknown`; a `verdict` of `pass`, `fail` or `inconclusive`, where `pass` requires every gate to pass; a non-empty `reason`; finite numbers; and comparisons that are chartable and true to the measurements (registered metric, split and dimensions, at most one per slice, a `source: "tester"` value equal to the verified measurement it cites). The runner adds the rest of the report from the scorer's evidence and the file's revision, checks it as the API would, then completes the job with it and a manifest that adds the step's uploaded `step_log`, `verdict` and `policy_step` (the manifest it ran), and the `setup_log` when a setup ran.

**Failures.** The runner never writes a verdict itself. When the step does not produce a valid one, the job fails and follows the rerun rules of the verify job (`max_auto_retries`, from the policy step, then failure review):

| What happened | Error code |
| --- | --- |
| The job names another verifier id or policy revision than the policy file's. It cannot happen through the claim, which hands out only the verifier's own jobs at its own revision; no step runs. The stock configuration checks the same. | `policy_mismatch` |
| The step exits non-zero or runs out of memory. | `step_failed` (`setup_failed` for its setup) |
| The step runs out of time (its own deadline, or what the job's leaves). | `deadline_exceeded` |
| The step writes no verdict. | `missing_output` |
| The verdict is not one JSON file, is too large, is not JSON, has an unknown field or breaks the rules above. | `invalid_step_output` |
| The step does not fit the science revision's allowance (its deadline, its setup's and 30 seconds kept by the runner, against `limits.max_deadline_seconds`). No step runs. | `deadline_exceeded` |
| The step breaks the science revision's rules (resource ceilings, datasets, baselines, trusted repositories), its code fails to fetch, an input differs from its pinned digest, or the launcher or runner fails. No step runs for a broken rule. | the code of any step (`invalid_code`, `input_verification_failed`, `runner_error`) |
| The lease is lost (a heartbeat answered `stale_lease`). | none: the step is killed and nothing is reported; the job is claimed again once its lease expires. |

A failure of the policy step names it as the failure's `step`, and its reason says what went wrong without quoting the step's output. A step that does not fit says by how much: `policy step fixture-policy needs up to 120s (60s for the step, 30s kept by the runner) but a verify job of science revision 2 allows its policy 90s (max_deadline_seconds)`. The failure references the step's `step_log`.

**Worked example.** The fixture science revision registers `verify: {"performer": "runner", "verifier": {"id": "cannery-runner", "revision": "fixture-policy-1"}}` for the stock configuration. To verify with the fixture policy step instead, register a science revision with `"verifier": {"id": "cannery-runner", "revision": "fixture-policy-step-1"}`, then run a `verify` kind with `policy` set to `examples/fixture/policy-step.json`, the local launcher and `--step-root examples/fixture/steps` (see [deploy](deploy.md#job-kinds-and-the-configuration-file)). For a submitted fixture attempt, the runner runs the producer and the scorer, stages the scorer's `evidence` and `per_query_results`, runs `python3 policy_step.py`, reads the verdict above, and completes the job: the attempt moves to `awaiting_human_review` with a `pass` verdict under policy revision `fixture-policy-step-1`, its comparison charted against the scorer's control value. A step that exits 3 instead fails the job with `step_failed` for step `fixture-policy`, and the job runs again once, from the policy step.

## Project configuration and dashboard contract

A project configuration revision is immutable and has two independently versioned parts. The science revision references registered hypothesis/result schemas (`hypothesis_fields` for a hypothesis's `project_fields`, `result_extensions` for the `extensions` of run documents and verification reports), who verifies a run (`verify: {performer, verifier?}`, required; see [verification policy](#verification-policy-and-the-stock-policy)), interfaces and their validator steps, the scorer step manifest, the default producer, datasets (with held-out label datasets marked), the metric registry, immutable baselines (each `{id, revision, description?}`, each id and revision at most once: data references a step can take as input with `from: baseline` and a hypothesis can name as its control; their values, when a policy needs them, live in its own configuration), required artifact roles (`attempt` for the run's manifest, `verify` for a verify job's output manifest), retention, report size cap (on the body of a run document or a verification report, at most 1 MiB), resource ceilings, `max_deadline_seconds` (the policy's allowance in a runner verify job, the whole deadline of an agent verify job; one hour by default) and `max_auto_retries` (automatic reruns of a failed verify job, 1 by default). Producer step manifests are registered separately per project, each revision immutable, and referenced by tracks. Secrets and live credentials are never embedded. Existing attempts keep the science revision they pinned. `result_extensions` applies to a verification report only when it carries `extensions`: the stock policy and a policy step add none, so a schema that requires a key does not refuse their reports unless the scorer supplies them.

The dashboard revision holds declarative views: `id`, title, metric key, chart type (`line`, `scatter`, `bar`, `table`), split, x field, grouping dimensions (registered dimensions, `track`, and project-declared hypothesis facets), baseline overlay and default filters. It is validated against the metric registry of the current science revision; a view referencing an unknown metric or dimension is rejected.

```json
{
  "views": [{
    "id": "tracks-vs-control",
    "title": "nDCG@10 by track vs control",
    "chart": "line",
    "metric": "ndcg_at_10",
    "split": "dev",
    "x": "attempt.finished_at",
    "group_by": ["track"],
    "baseline": "control",
    "filters": {"language": ["fr", "en"]}
  }]
}
```

The API exposes the metric catalog with unit/direction metadata and one metrics query endpoint used by both saved views and ad hoc exploration. A view never runs arbitrary SQL or code. Every chart response carries the science revision, split, sample count, failures and control identity, and the UI always shows them so a chart does not imply stronger evidence than exists. The overlay of a view with a `baseline` is not computed from the scorer's `control_value`: it is the reference each attempt's own verification report gave for the point's metric, split and slice (its `comparisons`). `baseline: "control"` overlays whatever reference that verdict reported; a baseline id overlays only a reference of kind `baseline` whose `ref` is that id. The name `control` is kept for compatibility with existing dashboard revisions, although the reference may be a paper, a benchmark or a promoted attempt. A point shows the reference's value (`control_value` in a view's series, kept under that name for the same reason) and its label (`reference_label`); a bar or table point combining attempts whose verdicts reported different references shows none, with a warning. A metrics query row carries the same `reference` beside the scorer's informational `control_value`. Points judged by the former built-in gates have no reference overlay: those reports carry no comparisons, and none are backfilled.

## Import bundle

A reviewed research history is loaded with `cannery import` from a bundle of YAML or JSON files, validated against `import_bundle.schema.json`. The bundle, the authorities `imported_artifact` and `imported_transcribed` that its measurements carry, and the `origin`, `source_ref` and `external_id` fields the API returns on imported records are described in [import.md](import.md). Every document of the live API refuses those authorities and fields.
