# Working as an agent

Cannery Row runs research as tracks of units: a track's plan defines its units, agents run each unit as attempts, and each submitted attempt is verified, written up and decided. You work through MCP (`/mcp`) or REST with a bearer token, which sets your actor: a researcher's personal token, or an `agent` service token, which runs units but cannot write or approve a plan.

Call `get_protocol` before you start: it returns this whole guide, of which the MCP server sends only this opening as its instructions. Every claim names it as `protocol` with its `version` and `sha256`: when the digest differs from your copy, read it again.

The main loop, per role:

- Planning, with a researcher: `get_brief`, `search`, then `start_plan_revision`, `add_unit`, `check_plan` and `submit_plan`.
- Running a unit: `claim_unit`, then `get_context` with the claim's `context.arguments` for the context bundle, whose Submitting section says what to upload and submit; then `heartbeat_attempt`, `create_upload`, `post_manifest` and `submit_attempt`.
- Verifying, documenting or deciding: `claim_job` with the phase, `get_context` with the claim's arguments, `get_job_input`, then `complete_job` or `fail_job`.

Every document the API takes has a contract schema: `list_schemas` names them and `get_schema` returns one. A refusal names the failing field and what it expected.

## Getting started

An agent working for a researcher uses the researcher's personal token and is recorded as that person, with its channel and client in `via`; an agent running units on its own uses an `agent` service token. Plans and human decisions need a person with the `researcher` role.

Before working, read the project brief (`get_brief`, or the resource `cannery-row://projects/{project}/brief`) and search for prior work (`search`). `list_units` and `get_unit` say whether each unit can be claimed now (`claimable`) and, when not, why (`claimable_reason`: the unit is not queued, its track is not active, or a concern is open on the track).

The REST paths in this guide are relative to the server's base URL, which `get_protocol` returns as `base_url` and the MCP server's instructions end with. Every REST request carries the same `Authorization: Bearer <token>` header as MCP, downloads included: a `download_url` from `get_job_input` or `get_artifact` too. An S3-backed server answers a download with a redirect to a presigned URL; follow it without the header.

This page is the working protocol. `get_protocol` and `GET /api/protocol` serve it as Markdown, and every claim and job claim names it as `protocol` with its `version`, `sha256` and size in `bytes`. The MCP server's instructions are its opening, up to this section.

Where to learn what to submit: the attempt's context bundle (`get_context`) has a Submitting section built from the science revision the attempt pinned (the artifact roles the manifest needs, the metrics you may claim with their splits and slices, the datasets and interfaces, an example manifest and an example run document). The schemas themselves are served by name: `get_schema` (`GET /api/schemas/{name}`), for example `unit`, `artifact_manifest`, `run`, `verification`, `writeup`, `decision`, `concern` and `transcript`.

## Planning

A track's plan says how the track tests its idea and which units of work it runs. To plan a track, work with the researcher:

1. Read the brief and the track (`get_brief`, `get_track`). A track with no plan yet answers `get_plan` with `not_found`: start its first revision with `start_plan_revision`. When re-planning, also read the plan and the done and in-flight units (`get_plan`, `list_track_units`, `get_unit_plan`, `get_unit_history`).
2. Refine the idea with the researcher.
3. Check references and outside material with `search` and the read tools.
4. List edge cases and risks into the approach and the unit briefs.
5. Define units with their acceptance and the context each needs: `start_plan_revision`, `set_plan_approach`, `add_unit` (and `update_unit`, `drop_unit`), `set_alignment` for every unit already done or in flight (`keep`, `obsolete` or `redo`, with a reason), and `answer_concern` for every open concern (`needs_answer`), saying how the revision answers it.
6. Run `check_plan` until it reports nothing, then `submit_plan`.
7. A researcher approves it (`review_plan`, or the track page in the web app), sends it back or declines it, with a reason. Approval creates the queued units.

A unit's fields and the limits that apply to them are in [the contract](contracts.md#track-plans). Name other units of the plan by their keys, and earlier units and attempts by their numbers: a unit gets its number when the plan is approved, so a new unit's `number` is null in the draft. A relation's `kind` is `derived_from`, `supersedes` or `related_to`. `get_schema` `unit` returns the full shape, for example `acceptance.compute_budget`, an object of `<resource>_max` numbers such as `{"gpu_hours_max": 4}` (the conventional names are `gpu_hours_max`, `cpu_hours_max`, `wall_clock_hours_max` and `cost_usd_max`; any other snake-case `<resource>_max` is accepted), and `acceptance.required_slices`, bare dimension names (the metric catalog lists each dimension with its values). Keep each unit brief to what its performer needs that the brief and approach do not already say.

## Concerns

Raise a concern (`raise_concern`) when your work shows that the track's plan itself is wrong, whatever phase you are in: a wrong assumption the plan relies on, a better idea than the plan's for testing the track's idea, or a blocker that stops the plan from being carried out as written. The concern is Markdown with YAML front matter (`GET /api/schemas/concern`): its `kind` (`wrong_assumption`, `better_idea`, `blocker` or `other`) and, when it comes from one, the `unit` number and the `attempt` sequence, with the argument as the body (at most 16 KiB): what you saw, why it matters to the plan, and what you would change. A runner step raises one by writing `/cr/outputs/concern/concern.md`.

Otherwise, just continue: a failure of your own run is a failure report, a disagreement with a run's claims is the verification report, an unexpected result is the write-up, and a question about one unit is a [question](#questions). A concern holds up the whole track: while one is open, no new unit of the track can be claimed (`409 concern_open`); work already claimed continues through run, verify, document and decide, so finish what you hold.

A plan revision answers the concern (`answer_concern` in the draft; `check_plan` lists every open concern the draft does not answer, and approval closes those it answers), or a researcher dismisses it with a reason (`dismiss_concern`). `list_concerns` and `get_concern` read them.

## Questions

Ask a researcher (`ask` under an attempt's lease in agent mode, `ask_job` under a job's lease) when you cannot settle something about the unit you hold from the brief, the context bundle or the plan: an ambiguous acceptance criterion, a choice between two readings of the brief, a resource you need and do not have. A question concerns one unit and pauses at most its own attempt or job; a concern is about the track's plan and holds up the whole track. When the answer would change the plan rather than this unit, raise a concern instead.

Prefer proceeding on a stated default. Ask a non-blocking question (`blocking: false`) with the `default` you proceed on, a sentence a researcher can accept by not answering, then carry on as if it were the answer. Ask a blocking question (`blocking: true`) only when every default would waste the run or do harm: what you would build on is unknown, the step is irreversible or costly, or the brief contradicts itself. Ask one thing per question, say what you saw and what each answer would change, and keep it short (at most 16 KiB of Markdown).

A blocking question puts your attempt in `waiting_on_human`, or pauses your job, and stops its lease clock: the lease and the deadline do not run out while it waits. Do nothing else on the attempt meanwhile. Wait for the answer with `wait_for_answer`, which waits up to 20 seconds by default (at most 25, under common MCP client timeouts) and returns with the answer or without one: call it again in a loop until the answer is set or the question is no longer open. Or keep heartbeating: each heartbeat carries the answers you have not acknowledged. An answer `wait_for_answer` returns is acknowledged as it is returned. Once the question is answered or escalated, you get a fresh lease and the deadline moves by the time you waited. Answers to non-blocking questions arrive the same way: when one contradicts your default, change course and say so in your run notes.

A blocking question that nobody answers within the project's `limits.question_wait` (24 hours by default) releases your attempt as `unanswered_question` (a job fails with the same code) and the unit is queued again; it does not count against the automatic retries. The next attempt's context bundle carries the question, and its answer once given.

A researcher answers from the Questions queue on Home or from the unit page (`answer_question`), or escalates the question into a concern when it shows the plan is wrong (`escalate_question`): the question is then closed as escalated, the note is your answer, and the track waits for a plan revision.

## Steering

A researcher may post a steering note to your running attempt (`post_steering`), unasked: a correction, a hint, a narrower focus. Every heartbeat (`heartbeat_attempt`) carries the notes and answers you have not acknowledged; `get_steering` lists them too. At each heartbeat, read them, act on them, and acknowledge them with `ack_steering` and their ids, so the researcher sees that you took them in: a note stays marked unacknowledged on the attempt page until you do. The response lists as `acknowledged` every id you sent, and as `already_acknowledged` those that were before the call (an answer `wait_for_answer` returned, or an earlier acknowledgement), so acknowledging twice is harmless. An answer replies to one of your questions; a steering note is a researcher's unasked message to your attempt. When a note conflicts with the unit's brief, follow the note and say so in your run notes; when it conflicts with the plan, raise a concern.

## Transcripts

In agent mode, append your transcript as you work (`append_transcript`), in batches at least as often as you heartbeat: one event per line of JSON Lines with its `kind` (`assistant`, `user`, `tool_call`, `tool_result`, `note`, `question`, `answer` or `steer`) and its `content`, as [`transcript.schema.json`](../contracts/schemas/transcript.schema.json) defines (`get_schema` `transcript`). The time `ts` is optional: the server stamps every event with `received_at` as it stores it, so leave `ts` out rather than guess one. Record the questions you ask, the answers and the steering notes you read as `question`, `answer` and `steer` events with their `message` id. Each event is validated, a batch holds at most 1000, and an append that would take the transcript over the project's `limits.transcript_max_bytes` (64 MiB by default) is refused with the location, the size and the limit: summarize long tool output rather than append it whole.

Redacting is your job: Cannery Row stores what you append as is and shows it to every member of the project. Leave out secrets, tokens, credentials and personal data before you append. Researchers follow the transcript live on the attempt's timeline page (`get_transcript` reads it). Submitting the attempt seals it as the attempt's `transcript` artifact, and no append is accepted after that. `transcript` is a system role: the server adds that artifact itself, so it appears in `get_attempt` besides the roles of your manifest; do not upload one.

## Context

A claim (`claim_unit`) in a planned track names the attempt's context bundle as `context`: its `ref` and size in `bytes`, the MCP `tool` that reads it (`get_context`) with its `arguments`, and the MCP `resource`. Read it before anything else: it holds the brief, the plan's approach, the unit's fields and brief, what to submit, a one-line index of the track's other units, a summary line and reference for each context item and each output of the units it derives from, and, for a later attempt, how the earlier attempts ended. It is assembled from the revisions the claim pinned, so it does not change while the attempt runs. Read it with `get_context`, as `context.md` at its `ref`, or as the MCP resource `cannery-row://projects/{project}/units/{number}/attempts/{sequence}/context`; append `/compact` (or `?detail=compact`, or `detail: "compact"`) for a version under 16 KiB. Follow a reference only when the summary line is not enough.

Its Submitting section is built from the science revision the attempt pinned: the artifact roles the manifest must include, with what goes in each when the science revision describes it, the metrics you may claim (keys, units, directions, splits, dimensions and required slices), the datasets and interfaces, an example `post_manifest` document and an example run document with the exact `provenance.science_revision`. A submission the server refuses fails the attempt with `invalid_submission` and the reason (for missing roles, their names), which the failure review case carries; check the document against that section first.

The section on earlier attempts lists, as they stood at your claim, each earlier attempt's failures with their codes and reasons, the researchers' decisions on it with their reasons (a failure case decided `retry` says what to do differently), and the steering notes posted to it; the questions those attempts asked follow, with the answers given before your claim.

Then run the unit as the [guide](guide.md#agent-mode) describes: heartbeat, upload, record the manifest and submit the run document under the lease you were given: front matter with the claims, provenance and verified manifest (`get_schema` `run`), run notes as the body. A run that failed releases the attempt with a failure report instead. Artifacts of earlier attempts that your unit names as context items are yours to download (`get_artifact`) while the attempt is in progress.

## Verify

A submitted run is verified before a researcher decides on it. When the science revision's `verify` performer is `agent`, an agent service account or a researcher who did not run the attempt verifies it; a claim never hands out a job of an attempt you ran yourself.

1. Claim a verify job (`claim_job` with `{"phase": "verify"}`). The answer names the job, the attempt, the brief, the plan and the verifier's context bundle (`phase: "verify"`), which holds the unit and, in place of Submitting, what you produce. Its `output` says what `complete_job` takes: `required_roles`, the roles your output manifest must list, with what each holds, and `expected`, the values your report must name.
2. Read the bundle. As an agent verifier you run none of the job's `steps`: they are what a runner verifier would run, and their containers and outputs are not yours to produce. You check the run's claims your own way and write the verification report.
3. Read the inputs (`get_job_input`): `run`, the run document's front matter with its claims and provenance, `manifest`, the run's verified manifest (the `run` input's `manifest.ref` is this manifest's id, not an artifact id), and `artifacts`, each input artifact's `artifact_id`, role and `download_url`. Download them at that URL with your `Authorization: Bearer` header (`get_artifact`), or by storage key with `object`: as the job's holder you may, for as long as its lease runs, and nothing else. The run notes are not an input: judge the claims against the artifacts, not the narrative.
4. Heartbeat (`heartbeat_job`) and upload what you produce (`create_job_upload`), one upload for each required output role at least.
5. Complete the job (`complete_job`) with `{"schema_version": "0.2", "job_id", "document", "manifest"}`: the verification report as `document` and the manifest of your uploads as `manifest`, listing every required role. Only the uploads the manifest lists are the job's outputs. The report's front matter (`GET /api/schemas/verification`) holds the verdict (`pass`, `fail` or `inconclusive`), the reason, the gates, your verified measurements, discrepancies, comparisons, the provenance and `artifact_roles`; your observations go in an optional body. `policy_revision` is `output.expected.policy_revision`: for an agent verifier, the pinned science revision. `provenance` names `output.expected`'s `science_revision`, `source_revision`, `control_revision` (the control's bare revision, such as `r1`, when the unit names one) and one of its `dataset_revisions` (left out when there are none). `measurements` holds one measurement of each required slice the bundle lists, whose only dimension is the slice's (with a `value`, or a `missing_reason` when you could not measure it). A `pass` needs every gate passed, and a comparison cites a measurement of the same report. An invalid report is refused with the details, each giving the expected value, and the lease is kept: correct it and complete again. A valid one marks the attempt verified, and the unit waits for its write-up.

If you cannot verify, fail the job (`fail_job`) with a code and a reason. The job is then run again, by you or another verifier, while the science revision's `max_auto_retries` allows (1 by default); after that the attempt fails with a `verify` failure and a failure review case opens, where a researcher retries the verification or stops the unit.

## Document

Every unit is written up once: after its last attempt is verified, whatever the verdict, or after a researcher stops it following a failure. An agent service account or a researcher writes it up; only a researcher may skip it, with a reason.

1. Claim a document job (`claim_job` with `{"phase": "document"}`); `list_writeups` shows what waits. The answer names the unit, its last attempt, what the write-up covers and cites (`inputs`: the `attempts` and the `verification` report) and the documenter's context bundle.
2. Read the bundle: the attempt's bundle, then every attempt's run document and notes, the failures and their logs, the verification reports and the comments. Read it with `get_context` and the claim's `context.arguments` (`phase: "document"`), at the claim's `context` ref (`?phase=document`), or as the resource `cannery-row://projects/{project}/units/{number}/attempts/{sequence}/context/document`.
3. Heartbeat (`heartbeat_job`) while you write.
4. Complete the job (`complete_job`) with `{"schema_version": "0.2", "job_id", "document"}` and no manifest. The document is the write-up, the `writeup` schema's first variant, `Write-up` (the second, `Imported write-up`, is only for imports): front matter with a one-sentence `summary`, the `attempts` it covers (every attempt of the unit) and the `verification` it cites (the job's `inputs.verification`, null for a stopped unit) (`GET /api/schemas/writeup`), and a body: what was tried, what was found and what it means. For example:

   ```markdown
   ---
   summary: Sparse retrieval beats the control on French queries but not on English ones.
   attempts: [1, 2]
   verification: {ref: <inputs.verification.ref>, sha256: <inputs.verification.sha256>}
   ---
   What was tried, what was found and what it means.
   ```

   An invalid write-up is refused with the details and the lease is kept: correct it and complete again. A valid one sends the unit to its decision. If you cannot write it, fail the job (`fail_job`) with a reason: it is queued again.

A researcher writes a unit up with `write_up` ("Write it up" in the web app), which claims and completes the job in one action with the same write-up as `document`, and skips it with `skip_writeup`.

## Decide

A researcher decides each unit on its decision case (`list_review_cases`, `get_review_case`, `record_decision`) with a decision document: front matter with the `outcome` (`promote`, `reject`, `inconclusive`, or `failed` for a unit stopped after a failure) and the `verification` and `writeup` it cites (`{ref, sha256}`, null when there is none) (`GET /api/schemas/decision`), and the reason as its body. A promotion needs a `pass` verdict. The citations are the case's own: `get_review_case` states them as `cites`, `{"verification", "writeup"}`, each `{ref, sha256}` or null, and the decider's bundle (`?phase=decide`, or the resource ending in `/context/decide`), which adds the write-up or the reason it was skipped, has a Deciding section with the exact front matter to cite. When the science revision's `decide` performer is `step`, the decider service account it registers decides instead: its runner claims a decide job (`claim_job` with `{"phase": "decide"}` and its step revision), runs its decider step on the decider's bundle and completes the job (`complete_job`) with the decision document, under the same rules; it promotes only on a `pass` verdict. While the decide job waits or runs, the case is not a researcher's to decide; once the decision is recorded, a researcher corrects it with `supersedes`. A decide job that keeps failing after its automatic reruns leaves the case to researchers. A failure case is resolved with `retry` or `stop` and a reason: `stop` sends the unit to be written up, then decided `failed`. `record_decision` takes one of two input sets: a decision document for a decision case, or `action`, `evidence_revision` and `reason` for a failure case, where `evidence_revision` is the case's `subject_revision`. A `retry` reason reaches the next attempt's context bundle, with the earlier attempts' failures, questions, answers and steering notes.
