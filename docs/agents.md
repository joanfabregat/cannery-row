# Working as an agent

An agent, such as a Codex or Claude Code session, works on a Cannery Row project through MCP (`/mcp`, Streamable HTTP) or REST, with a bearer token. Its actor and `via` are taken from the token: an agent working for a researcher uses the researcher's personal token and is recorded as that person, with its channel and client in `via`; an agent running units on its own uses an `agent` service token. Plans and human decisions need a person with the `researcher` role, so a service token can run units but cannot write or approve a plan.

Before working, read the project brief (`get_brief`, or the resource `cannery-row://projects/{project}/brief`) and search for prior work (`search`).

## Planning

A track's plan says how the track tests its idea and which units (hypotheses) it runs. To plan a track, work with the researcher:

1. Read the brief and the track (`get_brief`, `get_track`). When re-planning, also read the plan and the done and in-flight units (`get_plan`, `list_units`, `get_unit`, `get_unit_history`).
2. Refine the idea with the researcher.
3. Check references and outside material with `search` and the read tools.
4. List edge cases and risks into the approach and the unit briefs.
5. Define units with their acceptance and the context each needs: `start_plan_revision`, `set_plan_approach`, `add_unit` (and `update_unit`, `drop_unit`), `set_alignment` for every unit already done or in flight (`keep`, `obsolete` or `redo`, with a reason), and `answer_concern` for every open concern (`needs_answer`), saying how the revision answers it.
6. Run `check_plan` until it reports nothing, then `submit_plan`.
7. A researcher approves it (`review_plan`, or the track page in the web app), sends it back or declines it, with a reason. Approval creates the queued hypotheses.

A unit's fields and the limits that apply to them are in [the contract](contracts.md#track-plans). Name other units of the plan by their keys, and earlier units and attempts by their numbers. Keep each unit brief to what its performer needs that the brief and approach do not already say.

## Concerns

Raise a concern (`raise_concern`) when your work shows that the track's plan itself is wrong, whatever phase you are in: a wrong assumption the plan relies on, a better idea than the plan's for testing the track's idea, or a blocker that stops the plan from being carried out as written. The concern is Markdown with YAML front matter (`GET /api/schemas/concern`): its `kind` (`wrong_assumption`, `better_idea`, `blocker` or `other`) and, when it comes from one, the `hypothesis` number and the `attempt` sequence, with the argument as the body (at most 16 KiB): what you saw, why it matters to the plan, and what you would change. A runner step raises one by writing `/cr/outputs/concern/concern.md`.

Otherwise, just continue: a failure of your own run is a failure report, a disagreement with a run's claims is the verification report, an unexpected result is the write-up, and a question about one hypothesis is a comment. A concern holds up the whole track: while one is open, no new hypothesis of the track can be claimed (`409 concern_open`); work already claimed continues through run, verify, document and decide, so finish what you hold.

A plan revision answers the concern (`answer_concern` in the draft; `check_plan` lists every open concern the draft does not answer, and approval closes those it answers), or a researcher dismisses it with a reason (`dismiss_concern`). `list_concerns` and `get_concern` read them.

## Context

A claim (`claim_hypothesis`) in a planned track names the attempt's context bundle as `context`, with its `ref` and size in `bytes`. Read it before anything else: it holds the brief, the plan's approach, the unit's fields and brief, a one-line index of the track's other units, and a summary line and reference for each context item and each output of the units it derives from. It is assembled from the revisions the claim pinned, so it does not change while the attempt runs. Read it as `context.md` at its `ref`, or as the MCP resource `cannery-row://projects/{project}/hypotheses/{number}/attempts/{sequence}/context`; append `/compact` (or `?detail=compact`) for a version under 16 KiB. Follow a reference only when the summary line is not enough.

Then run the unit as the [guide](guide.md#agent-mode) describes: heartbeat, upload, record the manifest and submit the run document under the lease you were given: front matter with the claims, provenance and verified manifest (`GET /api/schemas/run`), run notes as the body. A run that failed releases the attempt with a failure report instead.

## Verify

A submitted run is verified before a researcher decides on it. When the science revision's `verify` performer is `agent`, an agent service account or a researcher who did not run the attempt verifies it; a claim never hands out a job of an attempt you ran yourself.

1. Claim a verify job (`claim_job` with `{"phase": "verify"}`). The answer names the job, the attempt, the brief, the plan and the context bundle, which holds the unit.
2. Read the inputs (`get_job_input`): `run`, the run document's front matter with its claims and provenance, and `manifest`, the run's verified artifacts (download them with `object`). The run notes are not an input: judge the claims against the artifacts, not the narrative.
3. Heartbeat (`heartbeat_job`) and upload what you produce (`create_job_upload`).
4. Complete the job (`complete_job`) with the verification report: front matter with the verdict (`pass`, `fail` or `inconclusive`), reason, policy revision, gates, verified measurements, discrepancies, comparisons and provenance (`GET /api/schemas/verification`), and your observations as an optional body. A `pass` needs every gate passed, and a comparison cites a measurement of the same report. An invalid report is refused with the details and the lease is kept: correct it and complete again. A valid one marks the attempt verified, and the hypothesis waits for its write-up. If you cannot verify, fail the job (`fail_job`) with a reason.

## Document

Every hypothesis is written up once: after its last attempt is verified, whatever the verdict, or after a researcher stops it following a failure. An agent service account or a researcher writes it up; only a researcher may skip it, with a reason.

1. Claim a document job (`claim_job` with `{"phase": "document"}`); `list_writeups` shows what waits. The answer names the hypothesis, its last attempt, what the write-up covers and cites (`inputs`: the `attempts` and the `verification` report) and the documenter's context bundle.
2. Read the bundle: the attempt's bundle, then every attempt's run document and notes, the failures and their logs, the verification reports and the comments. Read it at the claim's `context` ref (`?phase=document`), or as the resource `cannery-row://projects/{project}/hypotheses/{number}/attempts/{sequence}/context/document`.
3. Heartbeat (`heartbeat_job`) while you write.
4. Complete the job (`complete_job`) with the write-up and no manifest: front matter with a one-sentence `summary`, the `attempts` it covers (every attempt of the hypothesis) and the `verification` it cites (the job's `inputs.verification`, null for a stopped hypothesis) (`GET /api/schemas/writeup`), and a body: what was tried, what was found and what it means. An invalid write-up is refused with the details and the lease is kept: correct it and complete again. A valid one sends the hypothesis to its decision. If you cannot write it, fail the job (`fail_job`) with a reason: it is queued again.

A researcher writes a hypothesis up with `write_up` ("Write it up" in the web app), which claims and completes the job in one action, and skips it with `skip_writeup`.

## Decide

A researcher decides each hypothesis on its decision case (`list_review_cases`, `get_review_case`, `record_decision`) with a decision document: front matter with the `outcome` (`promote`, `reject`, `inconclusive`, or `failed` for a hypothesis stopped after a failure) and the `verification` and `writeup` it cites (`{ref, sha256}`, null when there is none) (`GET /api/schemas/decision`), and the reason as its body. A promotion needs a `pass` verdict. The decider's bundle (`?phase=decide`, or the resource ending in `/context/decide`) adds the write-up, or the reason it was skipped. When the science revision's `decide` performer is `step`, the decider service account it registers decides instead: its runner claims a decide job (`claim_job` with `{"phase": "decide"}` and its step revision), runs its decider step on the decider's bundle and completes the job (`complete_job`) with the decision document, under the same rules; it promotes only on a `pass` verdict. While the decide job waits or runs, the case is not a researcher's to decide; once the decision is recorded, a researcher corrects it with `supersedes`. A decide job that keeps failing after its automatic reruns leaves the case to researchers. A failure case is resolved with `retry` or `stop` and a reason: `stop` sends the hypothesis to be written up, then decided `failed`.
