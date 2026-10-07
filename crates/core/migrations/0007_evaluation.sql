-- Evaluation: track gates pinned at claim, built-in and external evaluator
-- verdicts, evaluation jobs, and result review cases.

-- The track gates an attempt is evaluated with, pinned at claim time like its
-- science revision and producer. Attempts claimed before this migration pin
-- their track's gates as they are now.
ALTER TABLE attempts ADD COLUMN track_gates jsonb;
UPDATE attempts a SET track_gates = t.gates FROM tracks t WHERE t.id = a.track_id;
ALTER TABLE attempts ALTER COLUMN track_gates SET NOT NULL;

-- Evidence of the built-in evaluator has no user or service account behind it;
-- every other record has exactly one.
ALTER TABLE evidence_records ADD CONSTRAINT evidence_records_producer_check CHECK (
    num_nonnulls(producer_user, producer_service) = 1
    OR (stage = 'evaluator' AND producer_user IS NULL AND producer_service IS NULL)
);

-- An external evaluator may complete its job with an output manifest; a
-- tester must. For an evaluation job, tester_id holds the registered
-- evaluator's id: the service account name that may claim it.
ALTER TABLE manifests DROP CONSTRAINT manifests_stage_check;
ALTER TABLE manifests ADD CONSTRAINT manifests_stage_check
    CHECK (stage IN ('agent', 'tester', 'evaluator'));
DO $$
DECLARE
    found text;
BEGIN
    SELECT conname INTO STRICT found FROM pg_constraint
    WHERE conrelid = 'jobs'::regclass AND contype = 'c'
      AND pg_get_constraintdef(oid) LIKE '%(manifest_id IS NOT NULL)%';
    EXECUTE format('ALTER TABLE jobs DROP CONSTRAINT %I', found);
END;
$$;
ALTER TABLE jobs ADD CONSTRAINT jobs_manifest_check CHECK (
    (manifest_id IS NULL OR state = 'completed')
    AND (stage = 'evaluator' OR (state = 'completed') = (manifest_id IS NOT NULL))
);

-- A result case is about one completed evaluator record, whatever its
-- verdict; an attempt has at most one pending result case.
ALTER TABLE review_cases ADD COLUMN evidence_id uuid UNIQUE REFERENCES evidence_records (id);
ALTER TABLE review_cases ADD CONSTRAINT review_cases_result_check
    CHECK ((kind = 'result') = (evidence_id IS NOT NULL));
CREATE UNIQUE INDEX review_cases_one_pending_result_idx ON review_cases (attempt_id)
    WHERE state = 'pending' AND kind = 'result';
