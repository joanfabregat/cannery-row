-- Background sweeps, automatic stage reruns and failure review.

-- An expired grant the orphan sweep reconciled: its bytes, if any, were
-- deleted. Like a pending grant, it can be replaced for the same key, unless
-- the key has an artifact (the sweep then marks the grant failed instead).
ALTER TABLE uploads DROP CONSTRAINT uploads_state_check;
ALTER TABLE uploads ADD CONSTRAINT uploads_state_check
    CHECK (state IN ('pending', 'receiving', 'verified', 'failed', 'expired'));
CREATE INDEX uploads_open_expiry_idx ON uploads (expires_at)
    WHERE state IN ('pending', 'receiving');

-- When the stream currently receiving a grant began. A stream that began
-- before its grant expired may run past the expiry, up to the configured
-- maximum stream duration; only then is a receiving grant an orphan.
ALTER TABLE uploads ADD COLUMN receiving_since timestamptz;
UPDATE uploads SET receiving_since = created_at WHERE state = 'receiving';

-- The sweep finds claimed jobs whose lease expired or deadline passed.
CREATE INDEX jobs_claimed_lease_idx ON jobs (lease_expires_at) WHERE state = 'claimed';
CREATE INDEX jobs_claimed_deadline_idx ON jobs (deadline) WHERE state = 'claimed';

-- Why a job run exists: the submission, an automatic rerun after an
-- infrastructure failure, or a researcher's retry. A rerun names the failed
-- run it replaces and keeps its spec, except for its own output prefix.
ALTER TABLE jobs ADD COLUMN origin text NOT NULL DEFAULT 'submission'
    CHECK (origin IN ('submission', 'auto_retry', 'human_retry'));
ALTER TABLE jobs ADD COLUMN previous_run_id uuid REFERENCES jobs (id);
ALTER TABLE jobs ADD CONSTRAINT jobs_rerun_check
    CHECK ((origin = 'submission') = (previous_run_id IS NULL));

CREATE OR REPLACE FUNCTION jobs_freeze_spec() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'jobs are never deleted';
    END IF;
    IF (NEW.project_id, NEW.attempt_id, NEW.stage, NEW.run_number, NEW.science_revision,
        NEW.tester_id, NEW.spec, NEW.deadline_seconds, NEW.created_at, NEW.origin,
        NEW.previous_run_id)
       IS DISTINCT FROM
       (OLD.project_id, OLD.attempt_id, OLD.stage, OLD.run_number, OLD.science_revision,
        OLD.tester_id, OLD.spec, OLD.deadline_seconds, OLD.created_at, OLD.origin,
        OLD.previous_run_id) THEN
        RAISE EXCEPTION 'a job''s spec is immutable';
    END IF;
    IF OLD.state IN ('completed', 'failed') THEN
        RAISE EXCEPTION 'a finished job is immutable';
    END IF;
    IF NEW.lease_generation < OLD.lease_generation THEN
        RAISE EXCEPTION 'a job''s lease generation never decreases';
    END IF;
    -- A claimed job keeps its tester until it finishes or returns to the queue.
    IF OLD.state = 'claimed' AND NEW.state <> 'pending'
       AND NEW.claimed_by_service IS DISTINCT FROM OLD.claimed_by_service THEN
        RAISE EXCEPTION 'a claimed job''s tester cannot change';
    END IF;
    RETURN NEW;
END;
$$;

-- A failure case is about one recorded failure of its attempt.
ALTER TABLE review_cases ADD COLUMN failure_id uuid UNIQUE REFERENCES attempt_failures (id);
UPDATE review_cases c SET failure_id = (
    SELECT f.id FROM attempt_failures f WHERE f.attempt_id = c.attempt_id
    ORDER BY f.created_at DESC LIMIT 1
) WHERE c.kind = 'failure';
ALTER TABLE review_cases ADD CONSTRAINT review_cases_failure_check
    CHECK ((kind = 'failure') = (failure_id IS NOT NULL));
CREATE UNIQUE INDEX review_cases_one_pending_failure_idx ON review_cases (attempt_id)
    WHERE state = 'pending' AND kind = 'failure';

-- A case is resolved by exactly one decision; a later correction must name
-- the decision it supersedes, and a decision is superseded at most once.
CREATE UNIQUE INDEX decisions_one_per_case_idx ON decisions (review_case_id)
    WHERE supersedes IS NULL;
CREATE UNIQUE INDEX decisions_supersedes_idx ON decisions (supersedes)
    WHERE supersedes IS NOT NULL;
