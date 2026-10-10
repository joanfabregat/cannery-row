-- SPDX-License-Identifier: AGPL-3.0-only
-- Document and decide per hypothesis. An attempt ends at `verified`; the
-- hypothesis then waits in `documenting` for one write-up covering every
-- attempt, written through a document job that agent service accounts and
-- researchers claim, or skipped by a researcher with a reason. The write-up
-- (or the skip) moves it to `deciding` and opens its `decision` case, which
-- a researcher resolves with a decision document: front matter with the
-- outcome and the verification and write-up it cites, the reason as body.
-- A failure case is resolved with `retry` or `stop`; `stop` sends the
-- hypothesis to document, and its decision is then `failed`.
--
-- History is kept and rewritten into the new shape:
--   * attempts awaiting a decision or decided end at `verified`; a
--     hypothesis keeps the decision it had;
--   * a hypothesis whose result awaited a decision moves to `documenting`
--     with a pending document job, and its undecided result case goes, so
--     it is written up before it is decided;
--   * a hypothesis whose failure awaits review stays `active` until the
--     failure is resolved;
--   * result cases become decision cases, and `close_failed` decisions
--     become `stop`.

ALTER TABLE jobs DISABLE TRIGGER jobs_frozen;
ALTER TABLE decisions DISABLE TRIGGER decisions_immutable;

-- Write-ups: imported retrospective reports, and live write-ups submitted
-- by their documenter. Either is Markdown with front matter, and never empty.
ALTER TABLE phase_outputs DROP CONSTRAINT phase_outputs_writeup_check;
ALTER TABLE phase_outputs ADD CONSTRAINT phase_outputs_writeup_check CHECK (
    stage <> 'writeup'
    OR (status = 'completed' AND jsonb_typeof(front_matter) = 'object' AND body ~ '\S'
        AND octet_length(body) <= 1048576 AND sha256 ~ '^[0-9a-f]{64}$'));

-- Document jobs: one per hypothesis, on its last attempt, performed by an
-- agent service account or a researcher; a researcher may skip it with a
-- reason, kept in error_reason.
ALTER TABLE jobs DROP CONSTRAINT jobs_phase_check;
ALTER TABLE jobs ADD CONSTRAINT jobs_phase_check CHECK (phase IN ('verify', 'document'));
ALTER TABLE jobs DROP CONSTRAINT jobs_performer_check;
ALTER TABLE jobs ADD CONSTRAINT jobs_performer_check CHECK (
    performer IN ('runner', 'agent') AND (performer = 'runner') = (verifier_id IS NOT NULL)
    AND (phase = 'verify' OR performer = 'agent'));
ALTER TABLE jobs DROP CONSTRAINT jobs_state_check;
ALTER TABLE jobs ADD CONSTRAINT jobs_state_check
    CHECK (state IN ('pending', 'claimed', 'completed', 'failed', 'skipped'));
ALTER TABLE jobs DROP CONSTRAINT jobs_check6;
ALTER TABLE jobs ADD CONSTRAINT jobs_finished_check
    CHECK ((state IN ('completed', 'failed', 'skipped')) = (finished_at IS NOT NULL));
ALTER TABLE jobs ADD CONSTRAINT jobs_skipped_check CHECK (
    state <> 'skipped'
    OR (phase = 'document' AND claimed_by_user IS NOT NULL AND claimed_by_service IS NULL
        AND error_code IS NULL AND error_reason ~ '\S'));
ALTER TABLE jobs DROP CONSTRAINT jobs_claimant_check;
ALTER TABLE jobs ADD CONSTRAINT jobs_claimant_check CHECK (
    num_nonnulls(claimed_by_service, claimed_by_user) <= 1
    AND (state IN ('pending', 'skipped') OR error_code = 'superseded'
         OR (num_nonnulls(claimed_by_service, claimed_by_user) = 1 AND deadline IS NOT NULL)));

CREATE OR REPLACE FUNCTION jobs_freeze_spec() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'jobs are never deleted';
    END IF;
    IF (NEW.project_id, NEW.attempt_id, NEW.phase, NEW.run_number, NEW.science_revision,
        NEW.performer, NEW.verifier_id, NEW.spec, NEW.deadline_seconds, NEW.created_at,
        NEW.origin, NEW.previous_run_id)
       IS DISTINCT FROM
       (OLD.project_id, OLD.attempt_id, OLD.phase, OLD.run_number, OLD.science_revision,
        OLD.performer, OLD.verifier_id, OLD.spec, OLD.deadline_seconds, OLD.created_at,
        OLD.origin, OLD.previous_run_id) THEN
        RAISE EXCEPTION 'a job''s spec is immutable';
    END IF;
    IF OLD.state IN ('completed', 'failed', 'skipped') THEN
        RAISE EXCEPTION 'a finished job is immutable';
    END IF;
    IF NEW.lease_generation < OLD.lease_generation THEN
        RAISE EXCEPTION 'a job''s lease generation never decreases';
    END IF;
    -- A claimed job keeps its claimant until it finishes or returns to the
    -- queue; a researcher who skips it becomes the one who finished it.
    IF OLD.state = 'claimed' AND NEW.state NOT IN ('pending', 'skipped')
       AND (NEW.claimed_by_service, NEW.claimed_by_user)
           IS DISTINCT FROM (OLD.claimed_by_service, OLD.claimed_by_user) THEN
        RAISE EXCEPTION 'a claimed job''s claimant cannot change';
    END IF;
    RETURN NEW;
END;
$$;

-- Decision cases: one per hypothesis and pending at most once, citing its
-- write-up when it has one and the verification report it decides on.
DROP INDEX review_cases_one_pending_result_idx;
ALTER TABLE review_cases DROP CONSTRAINT review_cases_kind_check;
ALTER TABLE review_cases DROP CONSTRAINT review_cases_result_check;
ALTER TABLE review_cases ADD COLUMN writeup_id uuid REFERENCES phase_outputs (id);

-- Undecided results are written up first: their cases go, and the
-- hypotheses wait for a write-up.
CREATE TEMPORARY TABLE undecided_results ON COMMIT DROP AS
SELECT c.id AS case_id, c.project_id, c.hypothesis_id, c.attempt_id, h.state AS hypothesis_state,
       a.state AS attempt_state, a.science_revision, t.slug AS track
FROM review_cases c
JOIN hypotheses h ON h.id = c.hypothesis_id
JOIN attempts a ON a.id = c.attempt_id
JOIN tracks t ON t.id = a.track_id
WHERE c.kind = 'result' AND c.state = 'pending'
  AND NOT EXISTS (SELECT 1 FROM decisions d WHERE d.review_case_id = c.id);
DELETE FROM review_cases c USING undecided_results u WHERE c.id = u.case_id;

UPDATE review_cases SET kind = 'decision' WHERE kind = 'result';
ALTER TABLE review_cases ADD CONSTRAINT review_cases_kind_check
    CHECK (kind IN ('plan', 'decision', 'failure'));
ALTER TABLE review_cases ADD CONSTRAINT review_cases_decision_check
    CHECK (kind = 'decision' OR (evidence_id IS NULL AND writeup_id IS NULL));
CREATE UNIQUE INDEX review_cases_one_pending_decision_idx ON review_cases (hypothesis_id)
    WHERE state = 'pending' AND kind = 'decision';

-- Decisions: a decision case is decided with a decision document, whose
-- front matter is kept beside the reason (its body). A failure case is
-- resolved with retry or stop.
ALTER TABLE decisions DROP CONSTRAINT decisions_action_check;
UPDATE decisions SET action = 'stop' WHERE action = 'close_failed';
UPDATE search_documents SET title = 'stop', updated_at = now()
WHERE kind = 'decision_reason' AND title = 'close_failed';
ALTER TABLE decisions ADD CONSTRAINT decisions_action_check CHECK (action IN (
    'approve', 'send_back', 'decline', 'promote', 'reject', 'inconclusive', 'failed', 'retry',
    'stop'));
ALTER TABLE decisions ADD COLUMN front_matter jsonb;
ALTER TABLE decisions ADD COLUMN sha256 text;
ALTER TABLE decisions ADD CONSTRAINT decisions_document_check CHECK (
    (front_matter IS NULL) = (sha256 IS NULL)
    AND (front_matter IS NULL OR (jsonb_typeof(front_matter) = 'object'
                                  AND sha256 ~ '^[0-9a-f]{64}$'
                                  AND action IN ('promote', 'reject', 'inconclusive', 'failed'))));

-- States.
ALTER TABLE hypotheses DROP CONSTRAINT hypotheses_state_check;
ALTER TABLE attempts DROP CONSTRAINT attempts_state_check;
DROP INDEX attempts_one_open_idx;

UPDATE attempts SET state = 'verified', finished_at = coalesce(finished_at, now())
WHERE state IN ('awaiting_human_review', 'promoted', 'rejected', 'inconclusive');

INSERT INTO audit_events (project_id, actor_kind, via_channel, action, subject_type, subject_id,
                          prior_state, new_state, reason)
SELECT h.project_id, 'system', 'system', 'hypothesis.state_changed', 'hypothesis', h.id::text,
       jsonb_build_object('state', h.state), jsonb_build_object('state', 'active'),
       'A hypothesis now waits for its failure to be reviewed while active.'
FROM hypotheses h
WHERE h.state = 'awaiting_human_review'
  AND NOT EXISTS (SELECT 1 FROM undecided_results u WHERE u.hypothesis_id = h.id)
ORDER BY h.project_id, h.number;
UPDATE hypotheses h SET state = 'active', updated_at = now()
WHERE h.state = 'awaiting_human_review'
  AND NOT EXISTS (SELECT 1 FROM undecided_results u WHERE u.hypothesis_id = h.id);

UPDATE hypotheses h SET state = 'documenting', updated_at = now()
FROM undecided_results u WHERE h.id = u.hypothesis_id;

CREATE TEMPORARY TABLE document_jobs ON COMMIT DROP AS
SELECT gen_random_uuid() AS id, u.*, (
           SELECT h.number FROM hypotheses h WHERE h.id = u.hypothesis_id) AS number,
       300 + coalesce((SELECT (r.content #>> '{limits,max_deadline_seconds}')::integer
                       FROM config_revisions r
                       WHERE r.project_id = u.project_id AND r.kind = 'science'
                         AND r.revision = u.science_revision), 3600) AS deadline_seconds
FROM undecided_results u;
INSERT INTO jobs (id, project_id, attempt_id, phase, run_number, science_revision, performer,
                  spec, deadline_seconds, origin)
SELECT j.id, j.project_id, j.attempt_id, 'document',
       1 + coalesce((SELECT max(run_number) FROM jobs o
                     WHERE o.attempt_id = j.attempt_id AND o.phase = 'document'), 0),
       j.science_revision, 'agent',
       jsonb_build_object('performer', 'agent', 'track', j.track, 'hypothesis', j.number,
                          'steps', '[]'::jsonb, 'parameters', '{}'::jsonb,
                          'output_prefix', format('projects/%s/attempts/%s/document-runs/%s/',
                                                  j.project_id, j.attempt_id, j.id)),
       j.deadline_seconds, 'submission'
FROM document_jobs j ORDER BY j.project_id, j.number;

INSERT INTO audit_events (project_id, actor_kind, via_channel, action, subject_type, subject_id,
                          prior_state, new_state, reason)
SELECT j.project_id, 'system', 'system', 'hypothesis.documenting', 'hypothesis',
       j.hypothesis_id::text,
       jsonb_build_object('state', j.hypothesis_state, 'review_case_id', j.case_id::text),
       jsonb_build_object('state', 'documenting', 'job_id', j.id::text,
                          'attempt_id', j.attempt_id::text),
       'Results are now written up before they are decided; the undecided result case was '
       || 'withdrawn and a document job queued.'
FROM document_jobs j ORDER BY j.project_id, j.number;

ALTER TABLE hypotheses ADD CONSTRAINT hypotheses_state_check CHECK (state IN (
    'queued', 'active', 'documenting', 'deciding', 'promoted', 'rejected', 'inconclusive',
    'failed', 'cancelled'));
ALTER TABLE attempts ADD CONSTRAINT attempts_state_check CHECK (state IN (
    'claimed', 'running', 'verifying', 'verified', 'failed', 'cancelled', 'unreviewed'));
CREATE UNIQUE INDEX attempts_one_open_idx ON attempts (hypothesis_id)
    WHERE state IN ('claimed', 'running', 'verifying');

-- Search: a write-up is one document, its summary as the title.
ALTER TABLE search_documents DROP CONSTRAINT search_documents_kind_check;
ALTER TABLE search_documents ADD CONSTRAINT search_documents_kind_check CHECK (kind IN (
    'track', 'hypothesis', 'attempt', 'report', 'verification', 'writeup', 'decision_reason',
    'comment'));

CREATE OR REPLACE FUNCTION search_index_evidence(e phase_outputs) RETURNS void
LANGUAGE plpgsql AS $$
DECLARE
    hypothesis uuid;
    body text;
BEGIN
    IF e.stage NOT IN ('verification', 'writeup') THEN
        RETURN;
    END IF;
    SELECT hypothesis_id INTO hypothesis FROM attempts WHERE id = e.attempt_id;
    IF e.stage = 'writeup' THEN
        PERFORM search_put(e.project_id, 'writeup', e.id, NULL, hypothesis, e.attempt_id,
                           e.producer_user, e.producer_service,
                           coalesce(e.front_matter ->> 'summary', ''), e.body, e.created_at);
        RETURN;
    END IF;
    body := concat_ws(E'\n', e.front_matter ->> 'reason', nullif(e.body, ''), (
        SELECT string_agg(d ->> 'description', E'\n')
        FROM jsonb_array_elements(
            CASE WHEN jsonb_typeof(e.front_matter -> 'discrepancies') = 'array'
                 THEN e.front_matter -> 'discrepancies' ELSE '[]'::jsonb END) AS d));
    IF body ~ '\S' OR e.front_matter ? 'verdict' THEN
        PERFORM search_put(e.project_id, 'verification', e.id, NULL, hypothesis, e.attempt_id,
                           e.producer_user, e.producer_service,
                           coalesce(e.front_matter ->> 'verdict', ''), body, e.created_at);
    END IF;
END;
$$;

SELECT search_index_evidence(p) FROM phase_outputs p
WHERE p.stage = 'writeup' ORDER BY p.created_at, p.id;

ALTER TABLE decisions ENABLE TRIGGER decisions_immutable;
ALTER TABLE jobs ENABLE TRIGGER jobs_frozen;
