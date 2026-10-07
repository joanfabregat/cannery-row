# Importing a research history

`cannery import` loads a research history that was kept somewhere else (lab notebooks, reports, run artifacts in a bucket) into a Cannery Row project, after a human has reviewed it as a bundle of files. The imported hypotheses, attempts, measurements, verdicts and decisions keep their historical dates and are marked as imported everywhere: they can be read, searched, charted and compared with live work, but they never pass for work done through Cannery Row. A draft or a result still awaiting review in the history continues in the live workflow after the import.

This page is the whole contract for writing a bundle. The bundle's JSON Schema is `contracts/schemas/import_bundle.schema.json`, and `examples/import/` is a complete small bundle (imported by the test suite) that shows every state.

**Iterate before the real import.** Once a file is imported, any edit to it is refused (see [Idempotency](#idempotency-and-updates)): imported records are never updated. Work on a bundle with `--dry-run`, which runs every check and rolls back, or import it into a throwaway project, and import into the real project only when the bundle is final.

## Guarantees

- **Honest marks.** Every record the import creates has `origin: imported` and a `source_ref` saying where it comes from (a document location, an artifact URI, or the bundle file). A live record never has either. The API returns both on hypotheses, attempts, decisions, artifacts, reports and comparisons; imported hypotheses also return their `external_id`, the id they had in the source history.
- **No promotion by import.** Imported measurements have their own authorities, `imported_artifact` and `imported_transcribed`, never `tester_verified` or `agent_claim`. The live API refuses both authorities and the `origin`, `source_ref` and `external_id` fields in every document it accepts, so only `cannery import` can create imported records.
- **Historical dates.** Records keep the dates the bundle gives: hypotheses, attempts, measurements (the attempt's end), verdicts, review cases, decisions and reports. Charts and time filters place imported values at those dates. Only the audit events are dated now, because they record the import itself.
- **One transaction.** The import writes everything or nothing. A dry run does all the checks and all the writes, then rolls back.
- **Idempotent.** Re-running the same bundle is a no-op. A changed entry is refused with the JSON Pointers that differ, and an entry missing from the bundle is refused unless `--allow-missing`; nothing is updated or removed.
- **No invention.** The bundle states each human decision with the user who took it, its date, reason and source; the import never creates a decision the history does not record. Fields the live schema requires but the history does not hold are filled by the documented rules below, never guessed.
- **No values in errors.** Problems name a bundle file and a JSON Pointer into it, and say what is wrong; they do not repeat the bundle's values.

## Running it

```sh
cannery import --bundle DIR --project SLUG [--dry-run] [--allow-missing] [--science FILE] [--statement-timeout-seconds 30]
```

It connects with the server's settings (`--settings` or `CANNERY_SETTINGS`, and `CANNERY_DATABASE_URL` or a managed database), like `cannery migrate`. With the container image:

```sh
podman run --rm -v ./bundle:/bundle:ro -v ./science.json:/science.json:ro -e CANNERY_DATABASE_URL \
  ghcr.io/joanfabregat/cannery-row:<tag> import --bundle /bundle --project my-project --science /science.json --dry-run
```

- `--project` must equal `slug` in `project.yaml`. The project is created when it does not exist, by the user `created_by` names; an existing project keeps its title and description.
- `--science` is a science revision document (the same JSON `POST /api/projects/{slug}/config/science` takes). It is required when the project has no science revision and is then registered, with the same checks as the API. When the project has one, `--science` may be omitted, or must be identical to the current revision; to change the science, register a new revision through the API first.
- `--dry-run` prints the plan and writes nothing.
- `--allow-missing` accepts a bundle that lacks files imported before into the project: they stay as they were imported, and the plan lists them. Without it such a bundle is refused, with the missing ids (see [Idempotency](#idempotency-and-updates)).
- `--statement-timeout-seconds` (1 to 300, 30 by default) bounds each PostgreSQL statement of the import, including the wait for the project's import lock.

The command prints the plan: what is (or would be) created, the hypotheses per track and per state, the measurements per authority, and the gaps (attempts without an artifact, a measurement or a verdict, and measurements with a missing value). It exits 0 on success and 1 otherwise. A refused bundle prints each problem on its own line on standard error, as `cannery import: ` followed by a JSON object with exactly `file`, `pointer` and `message`. Command-line usage errors exit 2.

Every user the bundle names (`created_by`, each `decided_by`) must already exist with that email verified by the identity provider, and the email must name one user only. Sign the users in once before importing.

Membership follows the decisions. When the import creates the project, it makes `created_by` and every `decided_by` user researchers of it (audit action `import.membership`). When the project exists, each `decided_by` of a newly imported hypothesis must already be a researcher of it, as for a live decision; grant the role through the API first. `created_by` needs no role in an existing project.

## Layout

```
bundle/
  project.yaml
  policies/<id>.yaml
  tracks/<slug>.yaml
  hypotheses/<id>.yaml
  reports/<path>.md
```

Each file outside `reports/` is YAML (`.yaml` or `.yml`) or JSON (`.json`), and its name (without the extension) equals its `id` or `slug`. `reports/` holds the attempts' Markdown reports (`.md` only), in any subdirectories; `reports/<hypothesis id>/<attempt label>.md` is the usual layout (see [Reports](#reports)). Any other file or directory is refused, except hidden ones (starting with `.`), here and in `reports/`, and so is a symbolic link anywhere in the bundle: a bundle is never read through a link. YAML is read strictly: a duplicate key is an error, anchors, aliases and merge keys are refused, dates stay text (they are checked as dates), and only `true` and `false` are booleans, so `no` or `on` stay strings. Files are read in name order. A bundle holds at most 20000 files (reports included) of at most 8 MiB each, and a report is at most 256 KiB.

Dates are either a calendar date (`2025-01-08`) or an RFC 3339 instant with an offset (`2025-01-09T14:00:00Z`). Give only what the source states: a date when only the day is known.

The order of events is checked: a hypothesis is not created before its track, an attempt does not start before its hypothesis or finish before it starts, a verdict is not reached before its attempt finished, and a decision is not taken before the verdict it decides on (`close_failed`: before the attempt finished; `decline`: before the hypothesis was created). A review case is opened when its verdict was reached (a failure case when the attempt finished; a draft case when the hypothesis was created) and resolved when it was decided, so it is never resolved before it was opened. Two instants compare as instants. A date compared with an instant compares by its UTC day, so a decision dated `2025-01-09` may follow a verdict reached at `2025-01-09T14:00:00Z`, but not one reached on the 10th. A date is stored at midnight UTC, or, when it follows an instant of the same day, at that instant: the decision above is stored at `2025-01-09T14:00:00Z`.

A `source` is one token without spaces: a document location `<path>:<line>[-<line>]@<commit>`, an artifact URI with a JSON Pointer (`gs://bucket/run/metrics.json#/mrr/en`), or, where the schema allows any source, a plain path or URL.

## project.yaml

| Field | Required | Meaning |
| --- | --- | --- |
| `slug` | yes | The project, equal to `--project`. |
| `title` | yes | Its title, used when the project is created. |
| `description` | no | Its description, used when the project is created. |
| `created_by` | yes | Email of the user who creates the project; also recorded as the author of imported hypotheses and the claimant of imported attempts. |

## policies/&lt;id&gt;.yaml

The evaluator policies the historical verdicts were reached under. They are recorded as written and never run; the live evaluator keeps its own policy.

| Field | Required | Meaning |
| --- | --- | --- |
| `id` | yes | Slug, equal to the file name. Verdicts name it. |
| `revision` | yes | The policy's version label. Imported verdicts record `policy_revision` as `<id>@<revision>`. |
| `title` | yes | A short name. |
| `description` | no | Free text. |
| `gates` | yes | At least one `{id, description, definition?}`: the gate's slug, its rule in words, and optionally its definition in any structure, as written in the source. |
| `source` | yes | Where the policy is written. |
| `sources` | no | Other documents it comes from. |

## tracks/&lt;slug&gt;.yaml

| Field | Required | Meaning |
| --- | --- | --- |
| `slug` | yes | Equal to the file name. A track that already exists in the project, and was not created by an earlier import of the same entry, is refused. |
| `title` | yes | |
| `description` | no | |
| `state` | yes | `active`, `paused` or `archived`. An archived track holds no draft and nothing awaiting review. |
| `created_at` | yes | |
| `archived_at` | no | Only with `state: archived`. |

## hypotheses/&lt;id&gt;.yaml

| Field | Required | Meaning |
| --- | --- | --- |
| `id` | yes | The id in the source history, kept as the hypothesis's `external_id` (unique per project) and equal to the file name. |
| `track` | yes | A track of the bundle. |
| `title` | yes | |
| `kind` | yes | `experiment`, `diagnostic` or `infrastructure`. |
| `claim` | yes | The hypothesis as stated before it ran. |
| `control` | no | `{baseline: <id>}` or `{hypothesis: <id of the bundle>}`. |
| `relations` | no | `[{type, to}]`, with `type` one of `derived_from`, `supersedes`, `related_to`, and `to` a hypothesis of the bundle (or one imported earlier). |
| `created_at` | yes | |
| `state` | yes | One of the states below. |
| `document` | for a draft | The full hypothesis document a live draft has: `question`, `rationale`, `intervention`, `plan`, and optionally `control` (`{kind: baseline, id, revision}`, a registered baseline) and `project_fields`. It is checked against the science revision exactly like a live draft. Other states may give it; it is then checked against the schema only. |
| `attempts` | no | The runs, in the order they happened (below). |
| `decision` | per state | The human decision (below). |
| `sources` | yes | At least one document the hypothesis comes from. |
| `notes` | no | Caveats, contradictions resolved, run ids that produced no number. |

The fields of the history (`id`, `kind`, `claim`, `control`, `sources`, `notes`) are kept as given on the hypothesis, under `imported`, whether or not it has a `document`. The `document` is what revision 1 holds and what a researcher edits live; it is not where the history's fields are kept, and a live revision never changes them.

Hypotheses get the project's next numbers in import order: within one import, by `created_at` then `id`. A file added to the bundle later is imported later and gets the next free number, after every hypothesis already in the project, whatever its `created_at`.

### Attempts

| Field | Required | Meaning |
| --- | --- | --- |
| `label` | yes | Unique within the hypothesis: a seed, a run id. |
| `started_at` | yes | |
| `finished_at` | no | |
| `source_revision` | no | The code revision the run used. |
| `config` | no | The configuration it ran with (a path). |
| `status` | yes | `completed` or `failed`. |
| `failure` | when failed | `{stage?, code, reason}`: `stage` is `agent` (the run itself, the default), `tester` (its scoring) or `evaluator`; `code` is snake case. |
| `artifacts` | no | Objects outside the store: `{role, uri, sha256, size, media_type?}`. The URI has no fragment. They are referenced, never copied. |
| `measurements` | no | The run's values (below). |
| `verdict` | no | The historical evaluator verdict (below). A failed attempt has none. |
| `report` | no | The run's report, a Markdown file of the bundle (below). |
| `source` | no | Where the run is recorded; by default the attempt's place in the bundle. |
| `notes` | no | Shown as the attempt's notes. |

`label`, `config`, `notes` and `source_revision` are kept on the attempt, under `imported`, so a failed run without measurements keeps them too.

### Reports

An attempt may have a report: the narrative of the run, written as Markdown in a file of `reports/`.

```yaml
report:
  path: reports/H-001/seed-1.md
  kind: retrospective
  author: "A research agent, from the notebook and the run metrics; reviewed by ana@example.org"
  written_at: 2026-09-28
```

| Field | Required | Meaning |
| --- | --- | --- |
| `path` | yes | The file, relative to the bundle: under `reports/`, ending in `.md`, with no hidden segment and no `..`. |
| `kind` | yes | `retrospective`: written after the fact, from the run's sources. Only this kind exists for now; the field lets a report written at the time be told apart later. |
| `author` | yes | Free text: who wrote it (an agent, a person) and who reviewed it. |
| `written_at` | yes | When it was written: a date or an instant, kept and shown as given (a date gets no time). Not before the attempt finished; a date compares by its UTC day. |

- A report is UTF-8 text, at most 256 KiB, not empty.
- Each file of `reports/` is the report of exactly one attempt: a path two attempts reference, a path with no file, and a file no attempt references are refused.
- The report's content is part of its hypothesis entry, by its SHA-256: a re-run with the same file is a no-op, and a changed file is refused like any changed entry (see [Idempotency](#idempotency-and-updates)), with the attempt's `report` as the JSON Pointer.
- It is stored on the imported attempt as history, with `origin: imported` and its path as `source_ref`. It is never a claimed result sheet, an agent report or evidence: it holds no measurement, it is not listed among the project's reports, and nothing is evaluated from it.

### Measurements and their authority

| Field | Required | Meaning |
| --- | --- | --- |
| `metric` | yes | A metric of the project's science revision. Its unit and direction come from the registry. |
| `split` | yes | One of the metric's splits. |
| `dimensions` | no | A slice: each name and value registered for the metric. At most one measurement per metric, split and slice in an attempt. |
| `value` or `missing_reason` | one of them | The value, or why there is none. |
| `control_value`, `uncertainty`, `sample_count` | no | As in a live tester record: `uncertainty` is `{method, lower, upper}`. |
| `authority` | yes | `imported_artifact` or `imported_transcribed`. |
| `source` | yes | Where the value was read (see below). |

- `imported_artifact`: the value was read from a run artifact. `source` is `<uri>#<JSON Pointer>`, and the URI must be an artifact of the bundle, with its SHA-256, so a reader can fetch the object and check it.
- `imported_transcribed`: the value was copied from a document. `source` is `<path>:<line>[-<line>]@<commit>`.

Prefer `imported_artifact` whenever the artifact exists. Both authorities are shown as imported, with their source, wherever an authority is shown; neither is ever presented as verified.

### Verdicts

| Field | Required | Meaning |
| --- | --- | --- |
| `policy` | yes | A policy of the bundle (or imported earlier). |
| `result` | yes | `pass`, `fail` or `inconclusive`. A `pass` needs every gate to pass. |
| `gates` | yes | `[{id, result, detail?}]`: each gate is a gate of the policy; `result` is `pass`, `fail` or `unknown`. |
| `comparisons` | no | What the verdict compared, in the shape of a live evaluator's comparisons. A `source: tester` comparison must cite one of the attempt's measurements (same metric, split and slice) with the same value. |
| `reason` | yes | |
| `evaluated_at` | no | When; not before the attempt's end, which is the default. |
| `source` | yes | Where the verdict is recorded. |

### Decisions

A hypothesis has at most one decision, and it is about its last attempt (or, for `decline`, about the draft).

| Field | Required | Meaning |
| --- | --- | --- |
| `action` | yes | `promote`, `reject`, `inconclusive`, `close_failed` or `decline`. |
| `decided_at` | yes | Not before the verdict it decides on (see the dates above). |
| `decided_by` | yes | Email of the user who decided; a researcher of the project (see membership above). |
| `reason` | yes | Quoted or closely summarized from the source, never invented. |
| `source` | yes | Where the decision is recorded. |

When the history closed several runs with one decision, give those runs as attempts in order: the last one is the decided attempt, and the earlier ones are imported as `unreviewed` (or `failed`), with the decision's reason free to mention them. One decision is never recorded on several attempts: a live correction of an imported decision then moves its attempt and the hypothesis together, exactly as it does for live work, and no other attempt is left holding the corrected outcome.

### States

The state must be consistent with the attempts and the decision:

| State | Attempts | Decision | What the import creates |
| --- | --- | --- | --- |
| `draft` | none | none | Revision 1 from `document`, and a pending draft review case: a researcher approves, revises or declines it live. |
| `declined` | none | `decline` | A draft review case resolved by the decision. |
| `awaiting_human_review` | the last one completed, with a verdict | none | A pending result case on that verdict: a researcher decides it live, and promotion needs a `pass`, as for any result. |
| `promoted`, `rejected`, `inconclusive` | the last one completed, with a verdict (`pass` for `promote`) | `promote`, `reject`, `inconclusive` | A result case on the last attempt, resolved by the decision. |
| `failed` | the last one failed | `close_failed` | A failure case on the last attempt, resolved by the decision. |

The decided attempt takes the outcome as its state. Any other attempt ends `failed` when it failed, and otherwise `unreviewed`: a finished run with no decision of its own (another run of the hypothesis was decided, or the hypothesis awaits review). `unreviewed` exists only for imported attempts. A failed attempt records its failure; it opens no failure case unless a `close_failed` decides it.

## What the live schema needs and the history does not have

- An imported hypothesis that left draft is recorded as approved at revision 1 on its `created_at`; no approval decision is created, since the history does not record one.
- Revision 1 of a hypothesis without a `document` holds only what the history states: the title, the claim as the question and the relations by number. It is never offered as a live draft. The history's own fields are on the hypothesis's `imported`, never in a revision.
- Imported attempts and revisions pin the science revision current at import.
- The project's `created_by` user is recorded as the author of imported hypotheses and the claimant of imported attempts; `via` is the `cli` channel with client `cannery import`.
- Measurements and verdicts are stored as tester and evaluator records with no producer (shown as produced by `import`). An imported record whose measurement names any other authority is refused by the database.
- An imported artifact is stored with backend `external`, its URI, size and SHA-256. Its `verified_at` is when the import recorded the reference: Cannery Row never had the bytes. Downloading it answers `409 conflict` with the URI.
- Historical policies are kept as documents (`historical_policies`), immutable and never run.

## Reading imported records

- Hypotheses, attempts, review cases, decisions, artifacts and reports carry `origin` and `source_ref`; hypotheses carry `external_id`; hypotheses and attempts carry `imported` (the history's own fields, above). Comparisons, search hits and the attention summary's pending reviews, recent outcomes and recent failures carry `origin`.
- The reports list (`GET /api/projects/{slug}/reports`) lists claimed result sheets. An imported attempt has none, so it is not listed there, even with a retrospective report; read it through the attempts of its hypothesis, its report, search or the results views.
- The report of an imported attempt (`GET …/attempts/{sequence}/report`) has no claimed measurements, the imported measurements as the tester section (each with its `source`), the verdict as the evaluation, and `author.kind: import`. Its `report` is the history's report when the bundle gives one: `kind`, `author`, `written_at` (a date or an instant, as the bundle gave it), `body_markdown`, `origin: imported` and `source_ref` (its path in the bundle); otherwise it is empty.
- `GET /api/projects/{slug}/metrics/query` returns tester-verified values by default; `authority=imported` returns both imported authorities (or name one), each row with its `authority` and `source_ref`. A dashboard view takes `authority=imported` the same way. Failed attempts are counted per origin.
- `GET /api/projects/{slug}/comparisons` (and the `query_comparisons` MCP tool) returns live comparisons by default, like the metrics; `origin=imported` returns the imported verdicts' comparisons at their historical dates, and `origin=all` both.
- The web app shows an "Imported" badge on hypotheses (in the list too), attempts, reports, decisions and the attention rows, with the source written out on detail pages; an imported attempt's report is shown on its page as "Retrospective report", rendered as Markdown with its author and date; it shows "Imported" in place of the author or claimant of an imported record, labels each imported measurement with its authority, and shows the imported history on request next to each result view.
- Audit events of an import use the actions `import.project_created`, `import.membership`, `import.science_registered`, `import.policy`, `import.track`, `import.hypothesis` and `import.completed`, with the system actor through the `cli` channel, dated when the import ran, and the bundle's SHA-256 in `new_state`.

## Idempotency and updates

Each file is recorded as an entry with its content (`import_entries`). On a re-run:

- an entry already recorded with the same content is skipped; when every entry is, the import writes nothing (not even an audit event) and says so;
- an entry recorded with a different content is refused, with the JSON Pointers that were changed, added or removed, and nothing is written;
- an entry not recorded yet is imported next to the earlier ones, so a history can grow by new files (numbered after the hypotheses already there);
- an entry recorded but no longer in the bundle is refused, with the list of missing ids, and nothing is written. With `--allow-missing` the import goes on: the records imported from that entry stay exactly as they were, and the plan lists the entry as missing. Removing a file never removes what was imported from it.

The bundle's SHA-256 is the digest of its canonical JSON, so the same content in YAML or JSON has the same hash. Updating or deleting imported records is out of scope: correct the source and import into a new project, or record a correction live (a superseding decision, a comment). This is why a bundle should be final before its real import (see the top of this page).

## What cannot be imported

- Live-only records: claimed result sheets, agent reports (a run's report is imported as history, see [Reports](#reports)), manifests, test or evaluation jobs, leases, uploads, comments.
- Decisions other than the five outcomes above: `approve`, `request_revision` and `retry`, and corrections that supersede a decision.
- Hypotheses `queued`, `active` or `cancelled`, and attempts in progress.
- Measurements with a live authority, or of a metric, split or slice the science revision does not register.
- Artifact bytes: an artifact is a reference with its SHA-256.
- Anything with a user who is not already a Cannery Row user with a verified email.

## Example

`examples/import/` is a complete bundle for the fixture science revision (`examples/fixture/science.json`), with two users, `ana@example.org` and `ben@example.org`:

```sh
cannery import --bundle examples/import --project retrieval-history --science examples/fixture/science.json --dry-run
```

It has one policy, two tracks (one archived) and a hypothesis in each state: `H-001` promoted on artifact values with a comparison and a retrospective report (`reports/H-001/seed-1.md`), `H-002` rejected after a failed run, on transcribed values, `H-003` inconclusive with a missing value, `H-004` failed, `H-005` declined, `H-006` awaiting review (derived from `H-001`), and `H-007` a draft with its full document. `hypotheses/H-006.yaml`, with one of its three measurements:

```yaml
id: H-006
track: lexical
title: Tune BM25 b on top of the new k1
kind: experiment
claim: With k1 at 0.9, b at 0.6 raises dev MRR further.
control: {hypothesis: H-001}
relations:
  - {type: derived_from, to: H-001}
created_at: 2025-03-03
state: awaiting_human_review
attempts:
  - label: seed-1
    started_at: 2025-03-04T08:00:00Z
    finished_at: 2025-03-04T08:50:00Z
    status: completed
    artifacts:
      - role: metrics
        uri: gs://retrieval-history/runs/h006-seed-1/metrics.json
        sha256: 9e94844d75a69554cf00689e0031bf404a46b5b38046c70238670562d27e1187
        size: 420
        media_type: application/json
    measurements:
      - metric: mrr
        split: dev
        value: 0.73
        authority: imported_artifact
        source: gs://retrieval-history/runs/h006-seed-1/metrics.json#/mrr/overall
    verdict:
      policy: release-gate
      result: pass
      gates:
        - {id: mrr-holds, result: pass}
        - {id: no-language-regression, result: pass}
      reason: Both gates pass; the gain over H-001 is 0.02.
      evaluated_at: 2025-03-05
      source: notebook/2025-03.md:10-14@3f9c2ab
sources:
  - notebook/2025-03.md:1-14@3f9c2ab
notes: Never reviewed before the notebook was retired; decide it in Cannery Row.
```
