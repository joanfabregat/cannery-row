-- Indexes for the attention summary (GET /api/projects/{slug}/attention).
-- The "not superseded" test of recent outcomes already has one:
-- decisions_supersedes_idx (0006), a unique index on decisions (supersedes)
-- WHERE supersedes IS NOT NULL, which `s.supersedes = d.id` can use.

-- Recent outcomes: a project's result cases, without walking its draft and
-- failure cases (review_cases_queue_idx puts state before kind), then each
-- case's decisions through decisions_case_idx.
CREATE INDEX review_cases_project_kind_idx ON review_cases (project_id, kind);

-- Recent failures, newest first. attempt_failures has no project column: the
-- planner either walks the project's attempts (attempts_project_state_idx)
-- and reads each one's failures newest first, or, when failures are recent,
-- walks all failures newest first and stops after the limit.
CREATE INDEX attempt_failures_attempt_created_idx
    ON attempt_failures (attempt_id, created_at DESC, id DESC);
CREATE INDEX attempt_failures_recent_idx ON attempt_failures (created_at DESC, id DESC);
-- The new index answers every lookup by attempt the old one did.
DROP INDEX attempt_failures_attempt_idx;
