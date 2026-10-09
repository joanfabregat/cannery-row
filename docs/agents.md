# Working as an agent

An agent, such as a Codex or Claude Code session, works on a Cannery Row project through MCP (`/mcp`, Streamable HTTP) or REST, with a bearer token. Its actor and `via` are taken from the token: an agent working for a researcher uses the researcher's personal token and is recorded as that person, with its channel and client in `via`; an agent running units on its own uses an `agent` service token. Plans and human decisions need a person with the `researcher` role, so a service token can run units but cannot write or approve a plan.

Before working, read the project brief (`get_brief`, or the resource `cannery-row://projects/{project}/brief`) and search for prior work (`search`).

## Planning

A track's plan says how the track tests its idea and which units (hypotheses) it runs. To plan a track, work with the researcher:

1. Read the brief and the track (`get_brief`, `get_track`). When re-planning, also read the plan and the done and in-flight units (`get_plan`, `list_units`, `get_unit`, `get_unit_history`).
2. Refine the idea with the researcher.
3. Check references and outside material with `search` and the read tools.
4. List edge cases and risks into the approach and the unit briefs.
5. Define units with their acceptance and the context each needs: `start_plan_revision`, `set_plan_approach`, `add_unit` (and `update_unit`, `drop_unit`), and `set_alignment` for every unit already done or in flight (`keep`, `obsolete` or `redo`, with a reason).
6. Run `check_plan` until it reports nothing, then `submit_plan`.
7. A researcher approves it (`review_plan`, or the track page in the web app), sends it back or declines it, with a reason. Approval creates the queued hypotheses.

A unit's fields and the limits that apply to them are in [the contract](contracts.md#track-plans). Name other units of the plan by their keys, and earlier units and attempts by their numbers. Keep each unit brief to what its performer needs that the brief and approach do not already say.

## Context

A claim (`claim_hypothesis`) in a planned track names the attempt's context bundle as `context`, with its `ref` and size in `bytes`. Read it before anything else: it holds the brief, the plan's approach, the unit's fields and brief, a one-line index of the track's other units, and a summary line and reference for each context item and each output of the units it derives from. It is assembled from the revisions the claim pinned, so it does not change while the attempt runs. Read it as `context.md` at its `ref`, or as the MCP resource `cannery-row://projects/{project}/hypotheses/{number}/attempts/{sequence}/context`; append `/compact` (or `?detail=compact`) for a version under 16 KiB. Follow a reference only when the summary line is not enough.

Then run the unit as the [guide](guide.md#agent-mode) describes: heartbeat, upload, record the manifest and submit under the lease you were given.
