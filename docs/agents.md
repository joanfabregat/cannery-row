# Working as an agent

An agent, such as a Codex or Claude Code session, works on a Cannery Row project through MCP (`/mcp`, Streamable HTTP) or REST, with a bearer token. Its actor and `via` are taken from the token: an agent working for a researcher uses the researcher's personal token and is recorded as that person, with its channel and client in `via`; an agent running units on its own uses an `agent` service token. Plans and human decisions need a person with the `researcher` role, so a service token can run units but cannot write or approve a plan.

Before working, read the project brief (`get_brief`, or the resource `cannery-row://projects/{project}/brief`) and search for prior work (`search`).

This page is the working protocol. The MCP server sends it as its instructions, `GET /api/protocol` serves it as Markdown, and every claim and job claim names it as `protocol` with its `version`, `sha256` and size in `bytes`: when the digest differs from the copy you read, read it again.

## Planning

A track's plan says how the track tests its idea and which units of work it runs. To plan a track, work with the researcher:

1. Read the brief and the track (`get_brief`, `get_track`). When re-planning, also read the plan and the done and in-flight units (`get_plan`, `list_track_units`, `get_unit_plan`, `get_unit_history`).
2. Refine the idea with the researcher.
3. Check references and outside material with `search` and the read tools.
4. List edge cases and risks into the approach and the unit briefs.
5. Define units with their acceptance and the context each needs: `start_plan_revision`, `set_plan_approach`, `add_unit` (and `update_unit`, `drop_unit`), `set_alignment` for every unit already done or in flight (`keep`, `obsolete` or `redo`, with a reason), and `answer_concern` for every open concern (`needs_answer`), saying how the revision answers it.
6. Run `check_plan` until it reports nothing, then `submit_plan`.
7. A researcher approves it (`review_plan`, or the track page in the web app), sends it back or declines it, with a reason. Approval creates the queued units.

A unit's fields and the limits that apply to them are in [the contract](contracts.md#track-plans). Name other units of the plan by their keys, and earlier units and attempts by their numbers. Keep each unit brief to what its performer needs that the brief and approach do not already say.

## Concerns

Raise a concern (`raise_concern`) when your work shows that the track's plan itself is wrong, whatever phase you are in: a wrong assumption the plan relies on, a better idea than the plan's for testing the track's idea, or a blocker that stops the plan from being carried out as written. The concern is Markdown with YAML front matter (`GET /api/schemas/concern`): its `kind` (`wrong_assumption`, `better_idea`, `blocker` or `other`) and, when it comes from one, the `unit` number and the `attempt` sequence, with the argument as the body (at most 16 KiB): what you saw, why it matters to the plan, and what you would change. A runner step raises one by writing `/cr/outputs/concern/concern.md`.

Otherwise, just continue: a failure of your own run is a failure report, a disagreement with a run's claims is the verification report, an unexpected result is the write-up, and a question about one unit is a [question](#questions). A concern holds up the whole track: while one is open, no new unit of the track can be claimed (`409 concern_open`); work already claimed continues through run, verify, document and decide, so finish what you hold.

A plan revision answers the concern (`answer_concern` in the draft; `check_plan` lists every open concern the draft does not answer, and approval closes those it answers), or a researcher dismisses it with a reason (`dismiss_concern`). `list_concerns` and `get_concern` read them.

## Questions

Ask a researcher (`ask` under an attempt's lease in agent mode, `ask_job` under a job's lease) when you cannot settle something about the unit you hold from the brief, the context bundle or the plan: an ambiguous acceptance criterion, a choice between two readings of the brief, a resource you need and do not have. A question concerns one unit and pauses at most its own attempt or job; a concern is about the track's plan and holds up the whole track. When the answer would change the plan rather than this unit, raise a concern instead.

Prefer proceeding on a stated default. Ask a non-blocking question (`blocking: false`) with the `default` you proceed on, a sentence a researcher can accept by not answering, then carry on as if it were the answer. Ask a blocking question (`blocking: true`) only when every default would waste the run or do harm: what you would build on is unknown, the step is irreversible or costly, or the brief contradicts itself. Ask one thing per question, say what you saw and what each answer would change, and keep it short (at most 16 KiB of Markdown).

A blocking question puts your attempt in `waiting_on_human`, or pauses your job, and stops its lease clock: the lease and the deadline do not run out while it waits. Do nothing else on the attempt meanwhile. Wait for the answer with `wait_for_answer`, which returns within a minute with the answer or without one (call it again), or keep heartbeating: each heartbeat carries the answers you have not acknowledged. Once the question is answered or escalated, you get a fresh lease and the deadline moves by the time you waited. Answers to non-blocking questions arrive the same way: when one contradicts your default, change course and say so in your run notes.

A blocking question that nobody answers within the project's `limits.question_wait` (24 hours by default) releases your attempt as `unanswered_question` (a job fails with the same code) and the unit is queued again; it does not count against the automatic retries. The next attempt's context bundle carries the question, and its answer once given.

A researcher answers from the Questions queue on Home or from the unit page (`answer_question`), or escalates the question into a concern when it shows the plan is wrong (`escalate_question`): the question is then closed as escalated, the note is your answer, and the track waits for a plan revision.

## Steering

A researcher may post a steering note to your running attempt (`post_steering`), unasked: a correction, a hint, a narrower focus. Every heartbeat (`heartbeat_attempt`) carries the notes and answers you have not acknowledged; `get_steering` lists them too. At each heartbeat, read them, act on them, and acknowledge them with `ack_steering` and their ids, so the researcher sees that you took them in: a note stays marked unacknowledged on the attempt page until you do. When a note conflicts with the unit's brief, follow the note and say so in your run notes; when it conflicts with the plan, raise a concern.

## Transcripts

In agent mode, append your transcript as you work (`append_transcript`), in batches at least as often as you heartbeat: one event per line of JSON Lines with its time `ts`, its `kind` (`assistant`, `user`, `tool_call`, `tool_result`, `note`, `question`, `answer` or `steer`) and its `content`, as [`transcript.schema.json`](../contracts/schemas/transcript.schema.json) defines. Record the questions you ask, the answers and the steering notes you read as `question`, `answer` and `steer` events with their `message` id. Each event is validated, a batch holds at most 1000, and an append that would take the transcript over the project's `limits.transcript_max_bytes` (64 MiB by default) is refused with the location, the size and the limit: summarize long tool output rather than append it whole.

Redacting is your job: Cannery Row stores what you append as is and shows it to every member of the project. Leave out secrets, tokens, credentials and personal data before you append. Researchers follow the transcript live on the attempt's timeline page (`get_transcript` reads it). Submitting the attempt seals it as the attempt's `transcript` artifact, and no append is accepted after that.

## Context

A claim (`claim_unit`) in a planned track names the attempt's context bundle as `context`, with its `ref` and size in `bytes`. Read it before anything else: it holds the brief, the plan's approach, the unit's fields and brief, a one-line index of the track's other units, and a summary line and reference for each context item and each output of the units it derives from. It is assembled from the revisions the claim pinned, so it does not change while the attempt runs. Read it as `context.md` at its `ref`, or as the MCP resource `cannery-row://projects/{project}/units/{number}/attempts/{sequence}/context`; append `/compact` (or `?detail=compact`) for a version under 16 KiB. Follow a reference only when the summary line is not enough.

Then run the unit as the [guide](guide.md#agent-mode) describes: heartbeat, upload, record the manifest and submit the run document under the lease you were given: front matter with the claims, provenance and verified manifest (`GET /api/schemas/run`), run notes as the body. A run that failed releases the attempt with a failure report instead.

## Verify

A submitted run is verified before a researcher decides on it. When the science revision's `verify` performer is `agent`, an agent service account or a researcher who did not run the attempt verifies it; a claim never hands out a job of an attempt you ran yourself.

1. Claim a verify job (`claim_job` with `{"phase": "verify"}`). The answer names the job, the attempt, the brief, the plan and the context bundle, which holds the unit.
2. Read the inputs (`get_job_input`): `run`, the run document's front matter with its claims and provenance, and `manifest`, the run's verified artifacts (download them with `object`). The run notes are not an input: judge the claims against the artifacts, not the narrative.
3. Heartbeat (`heartbeat_job`) and upload what you produce (`create_job_upload`).
4. Complete the job (`complete_job`) with the verification report: front matter with the verdict (`pass`, `fail` or `inconclusive`), reason, policy revision, gates, verified measurements, discrepancies, comparisons and provenance (`GET /api/schemas/verification`), and your observations as an optional body. A `pass` needs every gate passed, and a comparison cites a measurement of the same report. An invalid report is refused with the details and the lease is kept: correct it and complete again. A valid one marks the attempt verified, and the unit waits for its write-up. If you cannot verify, fail the job (`fail_job`) with a reason.

## Document

Every unit is written up once: after its last attempt is verified, whatever the verdict, or after a researcher stops it following a failure. An agent service account or a researcher writes it up; only a researcher may skip it, with a reason.

1. Claim a document job (`claim_job` with `{"phase": "document"}`); `list_writeups` shows what waits. The answer names the unit, its last attempt, what the write-up covers and cites (`inputs`: the `attempts` and the `verification` report) and the documenter's context bundle.
2. Read the bundle: the attempt's bundle, then every attempt's run document and notes, the failures and their logs, the verification reports and the comments. Read it at the claim's `context` ref (`?phase=document`), or as the resource `cannery-row://projects/{project}/units/{number}/attempts/{sequence}/context/document`.
3. Heartbeat (`heartbeat_job`) while you write.
4. Complete the job (`complete_job`) with the write-up and no manifest: front matter with a one-sentence `summary`, the `attempts` it covers (every attempt of the unit) and the `verification` it cites (the job's `inputs.verification`, null for a stopped unit) (`GET /api/schemas/writeup`), and a body: what was tried, what was found and what it means. An invalid write-up is refused with the details and the lease is kept: correct it and complete again. A valid one sends the unit to its decision. If you cannot write it, fail the job (`fail_job`) with a reason: it is queued again.

A researcher writes a unit up with `write_up` ("Write it up" in the web app), which claims and completes the job in one action, and skips it with `skip_writeup`.

## Decide

A researcher decides each unit on its decision case (`list_review_cases`, `get_review_case`, `record_decision`) with a decision document: front matter with the `outcome` (`promote`, `reject`, `inconclusive`, or `failed` for a unit stopped after a failure) and the `verification` and `writeup` it cites (`{ref, sha256}`, null when there is none) (`GET /api/schemas/decision`), and the reason as its body. A promotion needs a `pass` verdict. The decider's bundle (`?phase=decide`, or the resource ending in `/context/decide`) adds the write-up, or the reason it was skipped. When the science revision's `decide` performer is `step`, the decider service account it registers decides instead: its runner claims a decide job (`claim_job` with `{"phase": "decide"}` and its step revision), runs its decider step on the decider's bundle and completes the job (`complete_job`) with the decision document, under the same rules; it promotes only on a `pass` verdict. While the decide job waits or runs, the case is not a researcher's to decide; once the decision is recorded, a researcher corrects it with `supersedes`. A decide job that keeps failing after its automatic reruns leaves the case to researchers. A failure case is resolved with `retry` or `stop` and a reason: `stop` sends the unit to be written up, then decided `failed`.
