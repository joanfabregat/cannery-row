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

The server assigns `id`, state (`active` on creation), actor, via, timestamps and revision. `slug` is unique within the project. `producer` references a registered producer step manifest revision; omitted, the project default applies. A track declares no gates: the evaluation policy belongs to the evaluator the science revision registers (see [evaluation policy](#evaluation-policy-and-the-stock-evaluator)). A state change supplies `to_state` (`active`, `paused`, `archived`), the expected revision and a non-empty `reason`; the server enforces the transitions in [the spec](spec.md#tracks).

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

The server assigns `id`, the project-local sequential `number` (displayed `#123`), actor, via, timestamps, state and revision. Draft creation takes an `Idempotency-Key` header. A `relations[].hypothesis` is a number in the same project, or `{"project": "slug", "number": 42}` for another project the caller can read. Draft edits increment revision; draft approval freezes that revision before queueing. The draft review records `approve`, `request_revision`, or `decline` with a non-empty reason and authenticated researcher. Free-text criteria help humans but do not replace the executable pinned evaluation policy. The project schema may require additional typed fields; it cannot remove the track, question, plan or budget. `control` is optional: an opaque label `{kind, id, revision}` naming what the hypothesis is compared against, which must be a registered baseline of the current science revision when the draft is written. Cannery Row does not interpret it: when the hypothesis names one, the test and evaluation jobs pin it as `control`, the tester reports its revision as `control_revision`, and the evaluator decides what it means. The test job stages it as a baseline input only while the pinned science revision registers it. A claim keeps a control a later science revision stopped registering (pinned, not staged), and is refused with 409 only when a step takes that baseline id as input (`from: baseline`), which the tester could not stage. A test job queued before migration 0010 has no `control` key; its control is its first pinned baseline. Without one, no `control_revision` is expected.

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

## Evidence envelope

Agent, tester and evaluator all use the same versioned evidence envelope. Role-based validation controls which stages they may publish. The agent's claimed result sheet includes its claimed measurements and report, but those measurements are labeled `agent_claim` and never drive promotion. The tester receives that frozen sheet plus the artifact manifest, independently checks them, and publishes `tester_verified` measurements and explicit discrepancies. The evaluator assesses only verified evidence against the pinned policy. The framework derives final scientific status only after the separate, reasoned human decision.

```json
{
  "schema_version": "0.2",
  "attempt_id": "attempt-id",
  "stage": "tester",
  "status": "completed",
  "producer": {"kind": "service", "id": "tester-identity"},
  "started_at": "2026-09-29T00:00:00Z",
  "finished_at": "2026-09-29T01:00:00Z",
  "provenance": {
    "source_revision": "git-commit-or-content-digest",
    "tester_revision": "immutable-tester-digest",
    "dataset_revision": "immutable-dataset-id",
    "control_revision": "immutable-control-id",
    "science_revision": "science-revision",
    "seed": 1
  },
  "observations": "Full test completed with no missing query rows.",
  "measurements": [{
    "metric": "ndcg_at_10",
    "value": 0.42,
    "authority": "tester_verified",
    "unit": "ratio",
    "direction": "higher",
    "split": "dev",
    "dimensions": {"language": "fr"},
    "sample_count": 100,
    "control_value": 0.40,
    "uncertainty": {"method": "cluster-bootstrap", "lower": 0.39, "upper": 0.45}
  }],
  "discrepancies": [],
  "artifact_roles": ["raw_results", "test_log"],
  "extensions": {}
}
```

Numbers must be finite; missing/unsupported metrics are explicit with a reason rather than zero or omitted where required. A metric registry defines key, unit, direction, aggregation, permitted dimensions, required slices and applicable splits. A required slice `{dimension: value}` is covered only by a measurement whose `dimensions` are exactly that one pair (with a value or a `missing_reason`); a measurement with several dimensions, such as `{"language": "fr", "task": "retrieval"}`, may be reported as well but covers no required slice and is never read by a stock evaluator gate. `control_value` is the value of the pinned control the tester reports for the same slice: informational, for the evaluator to use or cross-check (see [the stock evaluator](#evaluation-policy-and-the-stock-evaluator)), and never a chart overlay. Any value used for a decision includes source data identity. Store per-query results as verified artifacts and summarized values as queryable rows. `status` records stage completion/failure; it is not the promotion decision. Producer identity, timestamps, configuration revision and measurement authority are server-validated or server-assigned, not trusted merely because a JSON field claims them.

An agent-stage record is the **claimed result sheet**. It requires a structured `report` with `what_was_tried`, `configuration`, `observations`, `findings`, `limitations`, `next_question`, `elapsed_seconds`, and `body_markdown`, the full report. The report is stored in the database and indexed for search; images it embeds are `report_asset` artifacts referenced by role and key. The sheet also links the verified input artifact manifest as `manifest` (`ref` and `sha256`). `configuration` includes code/data/model identities and distinguishes inherited quality from changes learned in this attempt. Agent measurements are allowed only with `authority: "agent_claim"`; the tester must never silently adopt them as verified. Empty strings do not satisfy required fields. The server records measured timestamps and duration independently of the agent's reported duration. A failed or abandoned attempt still requires a short report or a framework-generated failure record stating why none was provided.

An evaluator record adds `assessment`: policy revision, each gate's `pass`/`fail`/`unknown` result, optional comparisons, evidence references, verdict (`pass`, `fail`, or `inconclusive`), and a non-empty reason. It is an auditable gate, not the final decision. Cannery Row enforces only that a `pass` verdict reports every gate as `pass`; which gates exist, and that missing required evidence never yields `pass`, are the evaluator's responsibility. The registered evaluator publishes it with its service identity; its `policy_revision` must be the registered evaluator revision, and its record's `source_revision`, `dataset_revision` and `control_revision` must be those of the tester evidence it assesses (no `control_revision` when the tester reported none). Records published by the former built-in evaluator (producer `{"kind": "builtin", "id": "builtin-evaluator"}`) stay readable.

```json
{
  "schema_version": "0.2",
  "attempt_id": "attempt-id",
  "stage": "evaluator",
  "status": "completed",
  "producer": {"kind": "service", "id": "stock-evaluator"},
  "started_at": "2026-09-29T01:00:01Z",
  "finished_at": "2026-09-29T01:00:02Z",
  "provenance": {"source_revision": "git-commit-or-content-digest", "science_revision": "science-revision", "dataset_revision": "immutable-dataset-id", "control_revision": "immutable-control-id"},
  "assessment": {
    "policy_revision": "policy-r1",
    "gates": [{"id": "primary-beats-control", "result": "pass", "detail": "ndcg_at_10 on dev: uncertainty.lower 0.41 - control 0.4 = 0.01 > 0"}],
    "comparisons": [
      {"metric": "ndcg_at_10", "split": "dev", "dimensions": {"language": "fr"}, "value": 0.42, "source": "tester", "reference": {"value": 0.40, "label": "Base camp", "kind": "baseline", "ref": "base-camp"}},
      {"metric": "ndcg_at_10", "split": "dev", "dimensions": {"language": "en"}, "value": 0.44, "source": "evaluator", "reference": {"value": 0.45, "label": "Best promoted attempt", "kind": "promoted_attempt", "ref": "#12.3"}}
    ],
    "evidence": [{"ref": "evidence-id", "sha256": "2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae"}],
    "verdict": "pass",
    "reason": "Pass: all 1 gates pass on tester-verified measurements."
  }
}
```

`comparisons` says what the verdict compared, so the metrics overlay and the per-track history can show it: each entry is `{metric, split, dimensions, value, source, reference}`, where `reference` is `{value, label, kind, ref?}` and `kind` one of `paper`, `benchmark`, `promoted_attempt`, `baseline`, `manual` or `other` (`ref` is free text: a baseline id, an attempt reference, a URL or a DOI). Cannery Row does not judge a comparison, but refuses (as an invalid output) one it cannot chart or that misquotes the evidence: numbers must be finite; the metric, split and each dimension and value must be registered in the pinned science revision; at most one entry per metric, split and slice; and `source: "tester"` cites a `tester_verified` measurement of the assessed evidence for exactly that slice, whose value it must equal as an exact decimal. `source: "evaluator"` is a value the evaluator derived. Accepted comparisons are stored with the record's verdict and policy revision and are read through `GET /projects/{slug}/comparisons` (filters: `metric`, `split`, `dimensions` for an exact slice, `overall`, `filter=dimension:value`, `track`, `attempt_state`, `verdict`, `since`, `until`; ordered by id, newest first, like `/metrics`, and paged by `before` with `{items, next_before}`), and through the MCP tool `query_comparisons`. Every completed evaluation leaves the attempt awaiting human review, regardless of verdict. A `failed` evaluator record carries no `assessment`; a failure before a valid verdict follows the rerun rules and then opens a `failure` review case with stage, error code, sanitized details and artifact/log references.

A human decision always supplies `review_case_id`, `evidence_revision`, `action`, and a non-empty `reason`. Draft actions are `approve`, `request_revision`, `decline`; result actions are `promote`, `reject`, `inconclusive`; failure actions are `retry`, `close_failed`. The server derives actor, via and timestamp from authentication and binds the decision to the exact draft/evaluator/failure evidence revision. `promote` is valid only when the evaluator verdict is `pass`. A later correction is another linked, reasoned decision record; it never edits the evaluator verdict or original human decision. This preserves the distinction between measurements, evaluator judgment, and human validation.

## Test and evaluation jobs

Testers and external evaluators pull work. A claim returns one job, a lease token, and its generation. An evaluation claim names the policy revision the evaluator applies (`{"stage": "evaluator", "revision": "policy-r1"}`) and is handed only evaluation jobs pinned to that revision, so evaluators of different revisions can run side by side and a switch of revision never fails a job; a test claim names none. Heartbeats extend the lease; completion or failure requires the current token. The job lease token authorizes only reading the listed inputs and writing under `output_prefix`.

```json
{
  "schema_version": "0.2",
  "job_id": "test-run-id",
  "stage": "tester",
  "attempt_id": "attempt-id",
  "tester": {"id": "tester-identity"},
  "track": "compact-sparse",
  "control": {"id": "base-camp", "revision": "immutable-revision"},
  "steps": [
    {"name": "sparse-producer", "revision": 3, "manifest": {"…": "resolved step manifest"}},
    {"name": "scorer", "revision": "science-revision", "manifest": {"…": "resolved step manifest"}}
  ],
  "science_revision": "science-revision",
  "inputs": {
    "claimed_sheet": {"ref": "evidence-id", "sha256": "…"},
    "manifest": {"ref": "manifest-id", "sha256": "…"},
    "baselines": [{"id": "base-camp", "revision": "immutable-revision"}],
    "datasets": [{"id": "dataset-id", "revision": "immutable-revision"}]
  },
  "output_prefix": "projects/pilchards/attempts/attempt-id/test-runs/test-run-id/",
  "deadline": "2026-09-29T06:00:00Z",
  "limits": {"max_output_bytes": 10737418240},
  "lease": {"token": "opaque", "generation": 1, "expires_at": "2026-09-29T00:10:00Z"}
}
```

A job completes with a tester-stage envelope plus an output artifact manifest, or fails with an error code, sanitized reason and log references, never invented metrics. Cannery Row checks output identity, completeness, object presence, bytes and SHA-256 before ingestion. A second completion of the same job is idempotent and cannot publish a second result. `steps` is present when the job is executed by the Cannery Row runner; a self-hosted tester ignores it. `control` is present when the hypothesis names one: the tester reports its revision as `control_revision`. `inputs.baselines` lists the baselines the steps may stage: the control first, then each baseline a step takes as input (`from: baseline`), by id from the pinned science revision. An evaluation job (`stage: "evaluator"`) names the registered `evaluator` (`id` and policy `revision`) instead of a tester and steps, carries the test job's `control`, baselines and datasets, pins the hypothesis's `parameters` (the `project_fields` of the hypothesis revision the attempt pinned, `{}` when it has none), and lists the verified tester evidence and output manifest as inputs; the frozen claimed sheet is not one. `parameters` are pinned when the evaluation job is created: a later revision of the hypothesis never changes what a running evaluation, or a rerun of it, sees. A test job has no `parameters`.

## Step manifests and the container contract

A step manifest describes one container run. Field names follow Argo Workflows templates; `from`, `interface`, `network`, `sandbox`, `code` and `setup` are Cannery Row extensions. Manifests are authored as YAML or JSON and stored as JSON. A step either runs code baked into its image, as below, or runs a script from a repository at a pinned commit on a stock image (see [Steps as scripts](#steps-as-scripts-code-setup-and-the-dependency-cache)).

```yaml
apiVersion: cannery-row/v1
kind: Step
metadata:
  name: sparse-producer
spec:
  role: producer                  # producer | scorer | validator | experiment | evaluator
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

The scorer manifest has the same shape with `role: scorer`. Its inputs typically are the producer's `run` (`from: step`), `qrels` (`from: dataset`) and `claimed_sheet` (`from: attempt`); its outputs must include `evidence` (the tester envelope, `interface: cr-evidence/v0.2`) and may include further roles such as `per_query_results`. Input sources are `attempt` (verified submission artifacts and the frozen claimed sheet), `dataset`, `baseline` and `step` (a previous step's output of the same name). Each entry of `inputs.artifacts` has these fields:

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
- A producer (or an experiment) must not declare a dataset input, under whatever name, whose id the science revision marks as held-out labels; they run before the test, as the candidate. Only trusted steps that judge it may receive them: the scorer, and an evaluator [policy step](#policy-steps).
- The producer's output interface must equal the scorer's `from: step` input interface.
- Names are unique within a step; paths live under `/cr/inputs/` or `/cr/outputs/`.
- `code.commit` is a full 40-character commit SHA; `code.repo` is listed in the science revision's `code_repositories` for the step's trust class (`candidate` for a producer or experiment, `trusted` otherwise); `code.path` and each `setup.cache.key_files` entry are relative paths without `..`; `setup` requires `code`; the setup's deadline, like the step's, fits the science revision's `max_deadline_seconds`.

The `role` is one of:

| Role | Where it runs | Rules |
| --- | --- | --- |
| `producer` | First step of a test job, from the track's binding. | At least one output, each naming its interface; never a held-out labels dataset; no `from: step` input. |
| `scorer` | Last step of a test job, from the science revision. | Outputs `evidence` (`cr-evidence/v0.2`). |
| `validator` | On a step output, from the interface naming it (see below). | One `from: step` input, no outputs, `network: none`. |
| `experiment` | A step of a `workflow` track's experiment, run by the runner's experiment kind ([workflow tracks](#workflow-tracks)); registered at `/projects/{slug}/experiment-steps`. A test job refuses it (`invalid_job`). | At least one output, each naming its interface; never a held-out labels dataset; `code.repo` in the candidate repositories. A `from: attempt` input reads the predecessor attempt's artifacts of that role, so it may share its name with an output. |
| `evaluator` | A policy step: the evaluation policy of the runner's `eval` kind, named in the evaluator's own policy step document (see [policy steps](#policy-steps)), never registered through an endpoint; a test job refuses it (`invalid_job`). | Exactly one output, `verdict`; `network: none` for the step itself; no `from: step` input. Its `setup`, if any, may have network like any setup: it sees no inputs. |

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

Other formats get the checks above only; a project can register a **validator step** for anything more. It is a step manifest with `role: validator` in the science revision's `validators`: exactly one input, `from: step`, whose `interface` is the interface that names it, no outputs, and `network: none`. The runner runs it like any other step (its own directory and copy of the step code, its own deadline and resource limits) with the output mounted read-only at its input path, and uploads its log as a `validator_log`. A test job's deadline includes the deadlines of the validator steps its outputs go through. The validator's exit code is its answer:

| Exit | Meaning | Outcome |
| --- | --- | --- |
| `0` | The output is accepted. | The job goes on. |
| `1` | The output is rejected: the only content verdict. The validator says why in its log. | `invalid_step_output`, a tester-side failure (the API holds no evidence of its own, see below). |
| Anything else: another code, a signal, `137` (killed, as by the out-of-memory killer), a deadline | The validator failed; that says nothing about the output. | `step_failed` (or `deadline_exceeded`), a tester-side failure. |

Either way the failure reason names the validator and points at its `validator_log` artifact; it never quotes the log, which may quote the output. A validator must catch its own errors and exit with a code other than 1 when it cannot check: an uncaught Python exception exits 1, which would read as a rejection. The scorer's `evidence` (`cr-evidence/v0.2`) is not a registered interface and keeps its own checks at completion.

A rejected output fails the job with `invalid_step_output` and a reason naming the step, output, file, interface and version, JSON Pointer and line, such as `producer overlap-producer output "run" (run.json) does not match ranked-run/v1: /queries/q1/1: must be of type string`. Who is at fault decides what follows, and the API decides it on evidence it holds itself, never on a tester's word. A producer's output that the API itself refused (see below) is the candidate's fault: the failure is agent-side, so the attempt fails at once and a `failure` review case opens with stage `agent`, with no automatic rerun (rerunning the same candidate would fail the same way); a researcher's `retry` requeues the hypothesis. The Cannery Row runner makes this the normal path: when a producer's output fails its checks, it first uploads the failing file naming its interface, the API checks it and refuses it, and the runner then reports `invalid_step_output` naming the step. Everything else is the tester's: a scorer's output, a validator's rejection, a file the API could not check (over its validation cap, or `validate: false`), or a report the API has no refusal on record for. That is an infrastructure failure of the test stage: it follows the rerun rules, then failure review with the reason preserved, where a researcher can retry.

A self-hosted tester runs its own checks, but the API checks too when it can: a job output upload may name its step output's `interface` (`POST …/jobs/{id}/uploads` with `interface`, which must be registered in the job's science revision). The bytes are checked against the interface as they stream in, or, sent directly to an S3 store, when the upload finishes (never on the declared size alone): size, emptiness and leading bytes always, and for JSON or JSON Lines no larger than the server's `storage.validate_json_max_bytes` (64 MiB by default), the content too, line by line for JSON Lines. At most `storage.max_concurrent_validations` uploads (2 by default) have their content validated at once per API process; the others wait for a slot, up to `storage.max_stream_seconds`, then get `503 unavailable` and may retry the PUT. A file that does not match is refused with the error code `invalid_content` and its problems as details, as soon as a fatal problem shows (wrong leading bytes, over `max_bytes`, five problems), without reading the rest of the body. Nothing of it is kept, the grant is spent and records the refusal (and an `artifact.refused` audit event), and the job keeps its lease, so the tester reports the job's failure (`invalid_step_output`, naming the step). That record is the evidence: a report of `invalid_step_output` naming a producer step is agent-side only when the API refused, in the same job, an upload under that step's output path (`<step>/<output>/…`) with that output's role and declared interface. An accepted output records its `interface` and `content_validated`: `false` when only size and leading bytes were checked (binary content, a file over the server's cap, or `validate: false`). The Cannery Row runner names the interface of every output it uploads.

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

The runner completes the job with the scorer's `evidence` output plus a manifest of every step's outputs and logs, or fails it with the failing step, error code, sanitized reason and log references.

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
| `setup.activeDeadlineSeconds` | Setup's own deadline, 600 seconds when absent. A test job's deadline includes it. |

`setup` requires `code`: the cache key is derived from the code's repository and key files, so a setup without code would have nothing pinning what it installs. A step can have `code` without `setup` (a script with no dependencies beyond its image).

**Trust classes and allowed repositories.** A step's role gives its code a trust class: `candidate` for a `producer` or an `experiment` (the code under test), `trusted` for the `scorer`, a `validator` or an `evaluator` (the code that judges it). The science revision lists the repositories each class may run code from:

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

Failures are the tester's: infrastructure failures of the test stage, which follow the rerun rules.

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
| `/cr/outputs/` | Writable. A symbolic link anywhere in an output, or replacing `outputs/` or a directory on the way to it, fails the job with `invalid_output` (the tester's failure), even a link pointing inside the outputs. | Writable bind mount, with no disk quota. Writable besides it: `/tmp` and `/dev/shm`, both in memory. Links are refused as with `local`. | Writable, up to the volume's size (`--k8s-volume-size`). Writable besides it: `/tmp` and `/dev/shm`, both in memory. Copied back as written; links come back as links (never followed) and are refused as with `local`; anything but files, directories and links is dropped. Outputs over `--k8s-max-output-bytes` (the volume's size by default) as a tar, or over `--k8s-max-output-files` members (100000), fail the job with `invalid_output`. |
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

A `workflow` track's experiments are run by a Cannery Row runner of the experiment kind instead of an outside agent ([experiment modes](spec.md#experiment-modes)). Setting one up takes four things: a science revision whose hypothesis schema (`hypothesis_fields`) describes the parameters, one or more registered experiment step manifests, a track in `workflow` mode naming them, and a runner whose configuration lists an `experiment` kind with the token of an **experimenter** service account ([deployment](deploy.md#the-experiment-kind)). An admin creates the experimenter like any service account (`POST /projects/{slug}/service-accounts` with `"kind": "experimenter"`, or the admin settings in the web app), then a token for it from the web app: a service token can only be created from a signed-in browser session. Hypotheses are drafted, approved and reviewed as in any track.

### Experiment steps

An experiment step is a step manifest with `role: experiment` ([step manifests](#step-manifests-and-the-container-contract)). An installation admin registers it with `POST /projects/{slug}/experiment-steps` (the manifest as body); each registration of a name appends the next immutable revision (audited as `experiment_step.registered`). `GET /projects/{slug}/experiment-steps` lists them (`name` filter, newest revision first, paged like producers) and `GET /projects/{slug}/experiment-steps/{name}/{revision}` reads one. Experiment steps live apart from producers: a producer cannot be named in a workflow, nor an experiment step bound as a track's producer.

A step is candidate code, so the same rules as a producer's apply: image pinned by digest, no secret in `env`, resources and deadlines within the science revision's limits, no held-out labels dataset as input, and `code.repo` in `code_repositories.candidate`. Its inputs come `from: dataset`, `from: baseline`, `from: step` (an earlier step's output of that name, with the same interface) or `from: attempt`: the **predecessor** attempt's verified artifacts of that role, when the hypothesis was tried before (an empty directory otherwise). A `from: attempt` input may share its name with an output, so a step can resume from the previous attempt's `candidate` and write the new one.

### The workflow

A track's `workflow.steps` are checked when the track is created or changed, and again under the science revision a claim pins (a claim skips a track that no longer fits and, when only such tracks have queued hypotheses, answers `409 workflow_unavailable` naming them; see [claim and run](#claim-and-run)):

- every step is a registered experiment step revision that fits the science revision, and appears once;
- across the workflow, output names are unique and none is a log role (`step_log`, `setup_log`, `validator_log`); a `from: step` input matches an earlier step's output name and interface;
- the **last** step, and only it, has an output named `claimed_sheet` with interface `cr-evidence/v0.2`;
- the outputs cover every role in the science revision's `required_artifact_roles.attempt`, and every `from: attempt` input of the track's producer (other than the claimed sheet), since the test job reads them.

Every output but `claimed_sheet` becomes attempt artifacts whose role is the output's name, so an output name must also be a valid artifact role (`^[a-z][a-z0-9_]{0,63}$`). An output holds flat files only; each file is one artifact, named by its file name.

### Claim and run

The runner claims with `POST /projects/{slug}/claims` and `{"mode": "workflow"}` (optionally `hypothesis` or `track`), using its experimenter token; an experimenter's claim without `mode` is a `workflow` claim too. Claims are separated by identity: only an experimenter claims in `workflow` mode, and only there (an experimenter asking for `"mode": "agent"` gets `403`); an agent's or a researcher's claim is always in `agent` mode, and asking for `workflow` gets `403`. An experimenter cannot create or revise drafts, comment or decide. When nothing can be claimed the answer is `409` with the error code `nothing_to_claim`. A claim skips a `workflow` track whose workflow or producer no longer fits the current science revision (checked as at track creation), so other tracks' hypotheses still flow; when only such tracks have queued hypotheses (or the named `hypothesis` is in one), the answer is `409 workflow_unavailable`, whose message and `details` (`/tracks/<slug>`) name each track and why. The track is not paused: it waits for a researcher to fix it or the science revision. Any other `409` is a genuine conflict (a track paused or switched during the claim, a control a step cannot stage). The MCP tools `claim_hypothesis`, `release_attempt`, `get_track` and `update_track` follow the same rules. The answer is an ordinary claim (attempt, lease token, generation, heartbeat interval) whose attempt has `mode: "workflow"` and pins `workflow: {"steps": [...]}`, plus a `workflow` object, the run specification:

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

`parameters` are the `project_fields` of the hypothesis's approved revision (`{}` when it has none), already validated against the science revision's `hypothesis_fields` when the draft was written. `inputs.datasets` never lists a held-out labels dataset. `inputs.predecessor` is null on a first attempt. Otherwise it gives the predecessor's `state` and `failure_code` (its latest failure's, or null) and lists its verified uploads whose role some step reads `from: attempt`. A predecessor that failed, in particular one whose run crashed, may have uploaded only part of its outputs: a step that resumes from it decides whether what it finds is usable. The runner downloads each listed artifact with `GET …/hypotheses/{number}/attempts/{sequence}/inputs/predecessor/{artifact_id}` under the attempt's lease (a `302` to a presigned URL with an S3 store, or the bytes), and checks its size and SHA-256. Only an experimenter may call it (`403` otherwise), and only for the predecessor's uploads of those roles (`404` otherwise). `deadline` is the sum of the steps' deadlines (with their setups and validators) plus the server's `leases.job_overhead_seconds`, pinned on the attempt at claim: the runner stops there, the API refuses the lease after it (`409 stale_lease`), and the sweep fails the attempt with `deadline_exceeded`. `control` is present when the hypothesis names one.

The runner heartbeats the attempt (`POST …/heartbeat`) and runs the steps in order through its launcher, with the [container contract](#step-manifests-and-the-container-contract). After each step it checks every output against its interface and validator, then uploads every output but `claimed_sheet` through the attempt's upload grants (`POST …/uploads` with the output's name as `role`), with its log as `step_log` (and `setup_log`, `validator_log`). It then posts the artifact manifest of every upload and submits the claimed sheet (`POST …/submission`, with an `Idempotency-Key`). The attempt goes on to `testing` exactly as an agent's would.

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

### The claimed sheet output

The last step writes exactly one JSON file in `/cr/outputs/claimed_sheet/` (any name ending in `.json`): one JSON object holding the parts of the [claimed result sheet](#evidence-envelope) only the experiment knows. It must have `report`; it may have `measurements` (each with `authority: "agent_claim"`), `observations`, `artifact_roles` (the roles of the artifacts the report relies on), `extensions`, and `provenance` (for example a `source_revision`, which otherwise defaults to `sha256:` and the digest of the pinned workflow). The runner sets everything else, overriding the step: `schema_version`, `attempt_id`, `stage: "agent"`, `status: "completed"`, `producer: {"kind": "agent", "id": "cannery-runner"}`, `started_at`, `finished_at`, `provenance.science_revision` and `manifest`. The API then validates the result like any submission. A sheet that is not exactly one JSON object is a run failure the runner reports (`invalid_step_output`), retried automatically; a sheet the API refuses fails the attempt with `invalid_submission`, the candidate's failure, at once.

### Failures, release codes and automatic retries

A runner that cannot finish releases the attempt (`POST …/release`) with a `reason`, a `code`, the failing `step` and `logs`, the log references of the failing run (`[{key, size_bytes, sha256}]`, each a verified upload of this attempt, kept on the failure as its `log_refs`). Only an experimenter may send `code`, `step` or `logs` (`403` for anyone else), and an experimenter must send a `code` (`422`). The failure class of a runner-driven attempt comes from the experimenter's report and is trusted, as a tester's word is for test infrastructure ([failure classes](spec.md#data-model-and-lifecycle)). The candidate is blamed at once only on evidence the API holds itself, as in `agent` mode:

| Failure | Reported by | Class | What follows |
| --- | --- | --- | --- |
| `step_failed`, `deadline_exceeded`, `setup_failed`, `runner_error`, `upload_expired`, `missing_input`, `invalid_input`, `input_verification_failed`, `invalid_code`, `code_not_allowed`, `invalid_job`, `invalid_step_output`, `invalid_output`, `missing_output`, `held_out_labels_to_experiment` | The experimenter, on release | Run failure | The hypothesis is queued again automatically, no review case, while the science revision's `max_auto_retries` allows (counted since the last failure that went to review); then a `failure` review case. |
| `lease_expired` (the runner stopped heartbeating), `deadline_exceeded` (the attempt's deadline passed) | The sweep | Run failure | As above. |
| `upload_verification_failed` (an upload the API refused or could not verify), `invalid_submission` (a claimed sheet the API rejected) | The API, on its own evidence | Candidate | The attempt fails and a `failure` review case opens at once. |

Every failure is kept on the attempt (`failures[]`, with `requeued: true` for an automatic retry and the failing run's `log_refs`). Each failure records `attempt.failed` with the code and the failing `step`; an automatic retry adds `requeued: true` to it and records `hypothesis.requeued` (`automatic: true`, the failed attempt, code, retry number and budget). A researcher's `retry` on a failure case queues the hypothesis again and starts a fresh retry budget. A lost lease stops the run quietly: the runner neither uploads nor releases, and the sweep expires the attempt. Each test or evaluation run of the attempt (`GET …/attempts/{sequence}/jobs`) names the service account that claimed it (`claimed_by`) and the token it claimed with (`via_client`, `token:<name>`).

### A worked example

`examples/fixture/` holds a complete workflow track over the fixture project (the same science revision, datasets, producers and scorer as the two agent tracks):

- `science.json` declares `hypothesis_fields` (`top_k`, an integer from 1 to 10) and the `fixture-candidate/v1` interface (a JSON object with `top_k`);
- `experiments/fixture-experiment.json` is the experiment step: it reads the predecessor's `candidate` (`from: attempt`) and the `queries` dataset, and outputs `candidate` (`fixture-candidate/v1`) and `claimed_sheet` (`cr-evidence/v0.2`);
- `steps/experiment.py` is its code: it reads `parameters.top_k` from `/cr/job.json`, writes `outputs/candidate/candidate.json` and `outputs/claimed_sheet/claimed_sheet.json`, and notes the predecessor's candidate when one was staged;
- `workflow-track.json` is the `scripted` track: tested by `overlap-producer` revision 1, which reads `candidate`, and running `fixture-experiment` revision 1;
- `workflow-hypothesis.json` is a hypothesis of that track with `project_fields: {"top_k": 2}`.

With the fixture science revision and producers set up (as for the agent tracks), the steps are:

```sh
API=https://cannery.example.org/api/projects/demo
# An installation admin registers the experiment step (revision 1).
curl -sf -X POST "$API/experiment-steps" -H "Authorization: Bearer $ADMIN" \
  -H 'Content-Type: application/json' -d @examples/fixture/experiments/fixture-experiment.json
# A researcher creates the workflow track.
curl -sf -X POST "$API/tracks" -H "Authorization: Bearer $RESEARCHER" \
  -H 'Content-Type: application/json' -d @examples/fixture/workflow-track.json
# Anyone allowed drafts the hypothesis; a researcher approves it (draft-review).
curl -sf -X POST "$API/hypotheses" -H "Authorization: Bearer $AGENT" \
  -H 'Content-Type: application/json' -d @examples/fixture/workflow-hypothesis.json
```

One runner process then runs the experiment, test and eval kinds, each with its own token (experimenter, tester, evaluator), from a configuration file at the repository root:

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
kind = "test"
token_file = "tester.token"

[[kinds]]
kind = "eval"
token_file = "evaluator.token"
policy = "examples/fixture/evaluator.json"
```

```sh
cannery runner --config runner.toml
```

The experiment kind claims the hypothesis, runs `experiment.py` with `parameters: {"top_k": 2}`, uploads `candidate.json` (role `candidate`) and the step log, and submits the sheet; the test runner then runs `overlap-producer` on that candidate and the fixture scorer, and the evaluator gives its verdict, which waits for a researcher's decision. The conformance test `native_experiment_worker_submits_and_tester_consumes` (`crates/conformance/tests/native_workers.rs`) runs this end to end with the local launcher.

## Evaluation policy and the stock evaluator

Cannery Row applies no evaluation policy itself. Every science revision registers an evaluator, `evaluator: {id, revision}`: the evaluator service account's name and the policy revision its verdicts must report. When a test job completes, Cannery Row queues an evaluation job for that evaluator; the attempt stays `evaluating` until the evaluator completes it (or fails, following the rerun rules). An attempt left in `evaluating` with no evaluation job and no evaluator record is picked up by the background sweep, which queues the evaluation job again. A science revision registered before evaluators were required (with built-in `gates`, or without an `evaluator`) stays readable, and attempts that pinned one keep their records, but registering one is refused and a claim that would pin one is refused (409): "science revision N uses built-in gates, which moved to the stock evaluator; register a new revision with an evaluator". An attempt already `evaluating` under such a revision stays there: the sweep leaves it alone, since there is no evaluator to ask.

An evaluator is an ordinary service account of kind `evaluator`. It claims evaluation jobs, reads their inputs, and completes or fails them through the job API. It can also read the project like any member: hypotheses (the promoted ones included), attempts and their reports and evidence, metrics, the dashboard and the comparisons earlier verdicts reported. It can never write a decision: drafting, reviewing, promoting and rejecting stay with agents and researchers.

The stock evaluator (see [deploy](deploy.md#the-stock-evaluator)) is an evaluator that ships with Cannery Row and applies declarative gates from a versioned JSON configuration (`evaluator_config.schema.json`). It is the runner's `eval` kind under that configuration (see [the eval kind](#the-runners-eval-kind)), which `cannery evaluator` runs alone. It uses only the HTTP API, like any other evaluator. Its `evaluator.revision` is the revision the science revision registers and the `policy_revision` of every verdict it publishes; changing the gates or the baseline values means a new configuration revision and a new science revision registering it. The grammar of its gates is deliberately small: no expressions, scripts or queries. A policy that needs more is a [policy step](#policy-steps).

```json
{
  "gates": [
    {"id": "primary-beats-control", "metric": "ndcg_at_10", "split": "fresh-held-out", "statistic": "uncertainty.lower", "compare": "control", "op": ">", "min_delta": 0.0},
    {"id": "no-language-regression", "metric": "ndcg_at_10", "split": "fresh-held-out", "per_dimension": "language", "statistic": "value", "compare": "control", "op": ">=", "min_delta": -0.01}
  ]
}
```

A configuration holds `schema_version`, `evaluator: {id, revision}`, `gates` (at least one, each id once), `baselines` (each `{id, revision, label?, measurements}`, where `measurements` are the baseline's values as `{metric, split, dimensions, value, uncertainty?}`, `dimensions` empty for the overall slice or a single dimension, at most one value per metric, split and slice, an `uncertainty` interval ordered and containing the value) and an optional `default_control` (`{id, revision}`) used when the hypothesis names none. The evaluator refuses to start on an invalid configuration, and claims only evaluation jobs pinned to its own policy revision (a job of another revision should never reach it; if one does, it fails it with `policy_mismatch`).

A gate with `per_dimension` is evaluated for every value of that dimension and passes only if all pass. A gate whose required measurement is missing, non-finite or lacks the requested statistic is `unknown`. The verdict is `pass` if every gate passes, `fail` if any gate fails, and otherwise `inconclusive`; the generated reason lists each gate's result and the values used.

The reference of `compare: "control"` is the control's value for the measurement's metric, split and slice; a measurement without one is `unknown`. A measurement passes when `(statistic - control) op min_delta`. The op must point in the metric's registered direction and the statistic must be the value or the conservative end of the interval: on a `direction: "higher"` metric only `>` or `>=` with `value` or `uncertainty.lower`, on a `direction: "lower"` metric only `<` or `<=` with `value` or `uncertainty.upper`. So on a higher-is-better `ndcg_at_10`, `{"statistic": "value", "op": ">=", "min_delta": -0.01}` lets the value fall at most 0.01 below the control and `{"statistic": "uncertainty.lower", "op": ">", "min_delta": 0}` requires the whole interval above it; on a lower-is-better `latency_ms`, `{"statistic": "value", "op": "<=", "min_delta": 5}` lets the value rise at most 5 above the control and `{"statistic": "uncertainty.upper", "op": "<", "min_delta": 0}` requires the whole interval below it. A gate that inverts this (say `>=` on `latency_ms`, which would pass regressions and fail improvements), or names a metric the pinned science revision does not register, is `unknown`. The control is the one the evaluation job pins (the hypothesis's), or the configuration's `default_control` when there is none. When the configuration lists a value for that baseline id and revision on the gate's metric, split and slice, the gate compares against it (`control_source: resolved`). A `control_value` the tester reports alongside must then equal it exactly, on the same shortest round-trip decimals as the arithmetic below; if it does not, that measurement is `unknown`, its detail records a `control_mismatch` with both values, and neither value is used. Where the listed baseline has no value for the slice, where the configuration lists no baselines at all (its tester-reported mode), or where there is no control, the control value is the one the tester reported (`control_source: tester_reported`). A pinned control the configuration does not list, while it lists others, makes every gate `unknown` with that reason, as it did when the science revision held the values: comparing against the tester's own value instead would quietly change what the gate means. The detail of each gate result, and of each slice of a `per_dimension` gate, names its source. The arithmetic is exact on the shortest round-trip decimal of each parsed number (the literal as written whenever it fits a double's precision, since evidence is parsed into binary floats before it is stored), so `0.42 - 0.40 >= 0.02` holds. Only `tester_verified` measurements are read. Without `per_dimension`, a gate reads the one measurement of its metric and split with no dimensions; with it, the values are those the metric registry lists for the dimension (or those measured when the registry leaves it open), each read from the single-dimension slice `{dimension: value}` (the slice a required slice demands; measurements with more dimensions are ignored), and a failing value fails the gate even when another is `unknown`. A repeated slice or a `missing_reason` is `unknown`.

Every slice a gate compared (pass or fail) is also reported once as a comparison, `source: "tester"`, with the reference it was compared against: of kind `baseline`, labelled with the baseline's `label` (by default `<id> <revision>`) and with `ref` the baseline id, when the value came from the configuration; of kind `other`, labelled "control value reported by the tester", when it came from the tester.

### The runner's eval kind

The runner (`cannery runner`, see [deploy](deploy.md#job-kinds-and-the-configuration-file)) runs job kinds side by side in one process, each with its own service-account token: `test` (a tester token) claims test jobs and runs their steps, `eval` (an evaluator token) claims evaluation jobs and publishes verdicts, and `experiment` (an experimenter token) claims the hypotheses of [workflow tracks](#workflow-tracks) and runs their experiments. A service account has exactly one kind, so each kind needs its own token. An `eval` kind is configured with one `policy` file, which the evaluator owns and Cannery Row never reads:

- a **stock configuration** (above), whose gates the kind applies in-process. `cannery evaluator` is this kind alone, with the same flags and exit codes;
- a **policy step document** (below), whose step the kind runs through the runner's launcher.

Either way, the file's `evaluator.revision` is the revision the kind claims jobs for (`{"stage": "evaluator", "revision": …}`), and the `policy_revision` of every verdict it publishes. The kind completes a job with an evaluator record built the same way for both: the producer is the evaluator's service account (`{"kind": "service", "id": evaluator.id}`), the provenance echoes the tested evidence's `source_revision`, `dataset_revision` and `control_revision` (the first evidence record's) with the job's `science_revision`, and `assessment.evidence` lists the job's evidence refs. Each evidence record is checked against the SHA-256 its ref pins before anything reads it. Two `eval` entries with two policy revisions can run in one process, so a switch of revision needs no second runner.

### Policy steps

A policy step is an evaluation policy written as code: a step manifest with role `evaluator`, run by the runner's `eval` kind on each evaluation job, that reads the verified evidence and writes a verdict. Use it when declarative gates are not enough: a paired bootstrap on per-query results, a significance test, a comparison with a promoted attempt.

**Where it is named.** The evaluator's policy step document names the step, not the science revision. The science revision keeps registering `evaluator: {id, revision}` only, and that revision is the binding, as for the stock evaluator: Cannery Row applies no policy itself, and checks only that a verdict comes from the registered evaluator under the registered revision. The evaluator owns its policy, so changing the step (its image, its code commit, its command, its environment) means a new revision in the document and a new science revision registering it. Nothing about the API, its schemas or its database changes. The step's manifest, log and verdict are uploaded with the evaluation job, so what ran stays on record beside the verdict it produced.

**The document.** A JSON file with exactly three fields: `schema_version` (`"0.2"`), `evaluator` (`{id, revision}`, the evaluator service account's name and this policy's revision, identifiers without whitespace) and `step`, a step manifest (`step_manifest.schema.json`) with role `evaluator`. That role requires `network: none` and exactly one output, named `verdict`. A policy step has no `from: step` input and does not read the claimed sheet. The runner refuses to start with a document that breaks these rules (exit code 2). On each job, it also checks the step against the job's science revision, as for any step: resource ceilings, `max_deadline_seconds` (the setup's too), registered datasets and baselines, and a `code.repo` listed in `code_repositories.trusted` (an evaluator step is `trusted` code, like the scorer). A step that does not fit fails the job with `evaluator_error`. Its deadlines must also fit the evaluation job's: the setup's deadline (when it has one), plus the step's, plus the 30 seconds the runner keeps to upload and report, must not exceed the job's deadline, which the server sets to the science revision's `max_deadline_seconds` (one hour when unset). Otherwise the step could be cut short by the job's deadline rather than its own, so it never starts and the job fails with `evaluator_error` (`policy step fixture-policy needs up to 620s (590s for the step, 30s kept by the runner) but an evaluation job of science revision 1 has 600s (max_deadline_seconds)`). Like any step, it may run code baked into its image, or a script from a repository at a pinned commit on a stock image with a cached setup ([steps as scripts](#steps-as-scripts-code-setup-and-the-dependency-cache)).

This is the fixture's (`examples/fixture/policy-step.json`), whose script is `examples/fixture/steps/policy_step.py`, run from the local launcher's step root:

```jsonc
{
  "schema_version": "0.2",
  "evaluator": {"id": "stock-evaluator", "revision": "fixture-policy-step-1"},
  "step": {
    "apiVersion": "cannery-row/v1",
    "kind": "Step",
    "metadata": {"name": "fixture-policy"},
    "spec": {
      "role": "evaluator",
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
          {"name": "evidence", "from": "attempt", "path": "/cr/inputs/evidence"},
          {"name": "manifest", "from": "attempt", "path": "/cr/inputs/manifest"},
          {"name": "per_query_results", "from": "attempt", "path": "/cr/inputs/per_query_results"}
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
| `{name: evidence, from: attempt}` | `evidence.json`: `[{"ref", "sha256", "record"}, …]`, the job's verified tester evidence records in the job's order, each checked against the SHA-256 its ref pins. |
| `{name: manifest, from: attempt}` | `manifest.json`: the tester's verified output manifest, checked against the job's digest. |
| `{name: <role>, from: attempt}`, any other name | The objects of that role among the tester's outputs (`per_query_results`, `step_log`, the producer's `run`…), each downloaded and checked against the manifest's size and SHA-256. A role the tester did not output fails the job. |
| `{name, from: baseline, id?}` or `from: dataset` | The runner's data root, `baselines/<id>/<revision>/` or `datasets/<id>/<revision>/` (`<id>` is the input's `id`, else its `name`), as for a test job, at the revision the job pins. The runner then needs a data root. A dataset may be one the science revision marks as held-out labels: an evaluator step is trusted code that judges the candidate after its test, with no network, so it may recompute a metric or a statistic from the labels the candidate never saw, as the scorer does. |

`/cr/job.json` always holds `job_id`, `attempt_id`, `attempt_ref`, `track`, `science_revision`, `evaluator` (`{id, revision}`), `step`, `role` (`evaluator`), `inputs` (the `evidence` refs, the `manifest` ref, the datasets the step declares, each `{name, id, revision}`, and the pinned `baselines`), `control` when the hypothesis names one, `parameters` (the hypothesis's parameters as the evaluation job pins them: the `project_fields` of the hypothesis revision the attempt pinned, `{}` when it has none, as an [experiment step](#jobjson-of-an-experiment-step) gets them; pinned when the job is created, so a later revision of the hypothesis never changes what a run or a rerun sees, and a policy whose gates are frozen per hypothesis reads them here instead of baking each hypothesis into a new policy revision), `metrics` (the science revision's metric registry, with each metric's direction, splits and dimensions), and `manifest` (the step's own manifest).

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

These are the fields of an [evaluator record's](#evidence-envelope) `assessment`, with the same rules: at least one gate, each `{id, result, detail?}` with a slug id assessed once and a result of `pass`, `fail` or `unknown`; a `verdict` of `pass`, `fail` or `inconclusive`, where `pass` requires every gate to pass; a non-empty `reason`; finite numbers; and comparisons that are chartable and true to the evidence (registered metric, split and dimensions, at most one per slice, a `source: "tester"` value equal to the verified measurement it cites). The runner adds the rest of the record: `policy_revision`, the evidence refs, the producer, the provenance and the times. It checks the record as the API would before it completes the job, then completes it with the record and a manifest of the step's uploaded `step_log`, `verdict` and `policy_step` (the manifest it ran), and the `setup_log` when a setup ran.

**Failures.** The runner never writes a verdict itself. When the step does not produce a valid one, the job fails and follows the rerun rules of the `evaluator` stage (`max_auto_retries`, then failure review):

| What happened | Error code |
| --- | --- |
| The job is pinned to another evaluator id or policy revision than the policy file's. It cannot happen through the claim, which hands out only the evaluator's own jobs at its own revision; the step does not run. The stock configuration checks the same. | `policy_mismatch` |
| An evidence record or the manifest does not match the digest the job pins. | `input_verification_failed` |
| The step exits non-zero, runs out of memory or time (its own deadline, or what the job's leaves), writes no verdict, or writes one that is not one JSON file or is too large (both checked before the verdict is uploaded), is not JSON, has an unknown field or breaks the rules above. The step does not fit the science revision or the job's deadline, its code or setup fails, an input is missing, or the launcher or runner fails. | `evaluator_error` |
| The lease is lost (a heartbeat answered `stale_lease`). | none: the step is killed and nothing is reported; the job is claimed again once its lease expires. |

An `evaluator_error` reason says what went wrong without quoting the step's output. It starts with the underlying code when there is one (`step_failed: step fixture-policy exited with code 3`, `deadline_exceeded: step fixture-policy exceeded its 60s deadline`, `setup_failed: …`). For an invalid verdict, it names each problem by its JSON Pointer in the verdict, worded from the schema's side: `policy step fixture-policy wrote an invalid verdict: /gates/2/result: must equal "pass"`. The failure references the step's `step_log` and, when the step wrote one that could be uploaded (one file, at most 1 MiB), its `verdict`.

**Worked example.** The fixture science revision registers `evaluator: {"id": "stock-evaluator", "revision": "fixture-policy-1"}` for the stock configuration. To evaluate with the fixture policy step instead, register a science revision with `"evaluator": {"id": "stock-evaluator", "revision": "fixture-policy-step-1"}`, then run an `eval` kind with `policy` set to `examples/fixture/policy-step.json`, the local launcher and `--step-root examples/fixture/steps` (see [deploy](deploy.md#job-kinds-and-the-configuration-file)). For a tested fixture attempt, the runner stages `evidence.json`, `manifest.json` and the tester's `per_query_results`, runs `python3 policy_step.py`, reads the verdict above, and completes the job: the attempt moves to `awaiting_human_review` with a `pass` verdict under policy revision `fixture-policy-step-1`, its comparison charted against the tester's control value. A step that exits 3 instead fails the job with `evaluator_error` and the reason `step_failed: step fixture-policy exited with code 3`, and the evaluator stage runs again once.

## Project configuration and dashboard contract

A project configuration revision is immutable and has two independently versioned parts. The science revision references registered hypothesis/result schemas (`hypothesis_fields` for a hypothesis's `project_fields`, `result_extensions` for the `extensions` of result evidence), the tester identity, interfaces and their validator steps, the scorer step manifest, the default producer, datasets (with held-out label datasets marked), the metric registry, immutable baselines (each `{id, revision, description?}`, each id and revision at most once: data references a step can take as input with `from: baseline` and a hypothesis can name as its control; their values, when an evaluator needs them, live in its own configuration), the evaluator registration (`evaluator: {id, revision}`, required; built-in `gates` are refused), required artifact roles, retention, report size cap (on `body_markdown`, at most 1 MiB), resource ceilings and `max_auto_retries`. Producer step manifests are registered separately per project, each revision immutable, and referenced by tracks. A self-hosted tester supplies an immutable implementation revision instead of step manifests. Secrets and live credentials are never embedded. Existing attempts keep the science revision they pinned. `result_extensions` applies to the tester's evidence, whose `extensions` must match it when a test job completes, and to an evaluator record only when the record carries `extensions`: the stock evaluator, the runner's eval kind and a policy step write none, so a schema that requires a key does not refuse their verdicts.

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

The API exposes the metric catalog with unit/direction metadata and one metrics query endpoint used by both saved views and ad hoc exploration. A view never runs arbitrary SQL or code. Every chart response carries the science revision, split, sample count, failures and control identity, and the UI always shows them so a chart does not imply stronger evidence than exists. The overlay of a view with a `baseline` is not computed from the tester's `control_value`: it is the reference each attempt's own evaluator verdict reported for the point's metric, split and slice (`assessment.comparisons`). `baseline: "control"` overlays whatever reference that verdict reported; a baseline id overlays only a reference of kind `baseline` whose `ref` is that id. The name `control` is kept for compatibility with existing dashboard revisions, although the reference may be a paper, a benchmark or a promoted attempt. A point shows the reference's value (`control_value` in a view's series, kept under that name for the same reason) and its label (`reference_label`); a bar or table point combining attempts whose verdicts reported different references shows none, with a warning. A metrics query row carries the same `reference` beside the tester's informational `control_value`. Points evaluated before migration 0010 have no reference overlay: records of the former built-in evaluator carry no comparisons, and none are backfilled.

## Import bundle

A reviewed research history is loaded with `cannery import` from a bundle of YAML or JSON files, validated against `import_bundle.schema.json`. The bundle, the authorities `imported_artifact` and `imported_transcribed` that its measurements carry, and the `origin`, `source_ref` and `external_id` fields the API returns on imported records are described in [import.md](import.md). Every document of the live API refuses those authorities and fields.
