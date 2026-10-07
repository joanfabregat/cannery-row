-- Jobs: registered producer step manifests, test jobs with their own leases,
-- and job outputs uploaded under the job's prefix.

-- Producer step manifests, registered per project and revisioned per name.
-- Tracks bind one revision; the project scorer lives in the science revision.
CREATE TABLE producer_manifests (
    project_id uuid NOT NULL REFERENCES projects (id),
    name       text NOT NULL CHECK (name ~ '^[a-z0-9][a-z0-9-]{0,62}$'),
    revision   integer NOT NULL CHECK (revision > 0),
    content    jsonb NOT NULL,
    created_by uuid NOT NULL REFERENCES users (id),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, name, revision)
);
CREATE TRIGGER producer_manifests_immutable BEFORE UPDATE OR DELETE ON producer_manifests
    FOR EACH ROW EXECUTE FUNCTION forbid_mutation();

-- Test (and later evaluation) jobs. A job is created in the same transaction
-- as the attempt state change that needs it. Its spec (tester, track, steps,
-- inputs, output prefix, limits) is frozen; only its queue and lease state
-- change. Each claim, and each reissue of a claim's lease token, increments
-- the lease generation, which never decreases.
CREATE TABLE jobs (
    id                 uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id         uuid NOT NULL REFERENCES projects (id),
    attempt_id         uuid NOT NULL REFERENCES attempts (id),
    stage              text NOT NULL CHECK (stage IN ('tester', 'evaluator')),
    run_number         integer NOT NULL CHECK (run_number > 0),
    state              text NOT NULL DEFAULT 'pending'
                           CHECK (state IN ('pending', 'claimed', 'completed', 'failed')),
    science_revision   integer NOT NULL,
    tester_id          text NOT NULL,
    spec               jsonb NOT NULL,
    deadline_seconds   integer NOT NULL CHECK (deadline_seconds > 0),
    created_at         timestamptz NOT NULL DEFAULT now(),
    claimed_by_service uuid REFERENCES service_accounts (id),
    via_channel        text,
    via_client         text,
    lease_generation   integer NOT NULL DEFAULT 0,
    lease_token_hash   bytea UNIQUE,
    lease_expires_at   timestamptz,
    claimed_at         timestamptz,
    deadline           timestamptz,
    finished_at        timestamptz,
    evidence_id        uuid REFERENCES evidence_records (id),
    manifest_id        uuid REFERENCES manifests (id),
    error_step         text,
    error_code         text,
    error_reason       text,
    logs               jsonb NOT NULL DEFAULT '[]',
    UNIQUE (attempt_id, stage, run_number),
    -- Only a claimed job holds a lease.
    CHECK ((state = 'claimed') = (lease_token_hash IS NOT NULL)),
    CHECK ((lease_token_hash IS NULL) = (lease_expires_at IS NULL)),
    CHECK (state = 'pending' OR (claimed_by_service IS NOT NULL AND deadline IS NOT NULL)),
    CHECK ((state = 'completed') = (evidence_id IS NOT NULL)),
    CHECK ((state = 'completed') = (manifest_id IS NOT NULL)),
    CHECK ((state = 'failed') = (error_code IS NOT NULL)),
    CHECK ((state IN ('completed', 'failed')) = (finished_at IS NOT NULL))
);
-- At most one open job per attempt and stage.
CREATE UNIQUE INDEX jobs_one_open_idx ON jobs (attempt_id, stage)
    WHERE state IN ('pending', 'claimed');
CREATE INDEX jobs_queue_idx ON jobs (project_id, stage, created_at) WHERE state = 'pending';
CREATE INDEX jobs_lease_expiry_idx ON jobs (lease_expires_at) WHERE lease_expires_at IS NOT NULL;

CREATE FUNCTION jobs_freeze_spec() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'jobs are never deleted';
    END IF;
    IF (NEW.project_id, NEW.attempt_id, NEW.stage, NEW.run_number, NEW.science_revision,
        NEW.tester_id, NEW.spec, NEW.deadline_seconds, NEW.created_at)
       IS DISTINCT FROM
       (OLD.project_id, OLD.attempt_id, OLD.stage, OLD.run_number, OLD.science_revision,
        OLD.tester_id, OLD.spec, OLD.deadline_seconds, OLD.created_at) THEN
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
CREATE TRIGGER jobs_frozen BEFORE UPDATE OR DELETE ON jobs
    FOR EACH ROW EXECUTE FUNCTION jobs_freeze_spec();

-- Job outputs use the same one-time upload grants and verified artifacts as
-- agent uploads. For a job upload, lease_generation is the job's.
ALTER TABLE uploads ADD COLUMN job_id uuid REFERENCES jobs (id);
CREATE INDEX uploads_job_idx ON uploads (job_id) WHERE job_id IS NOT NULL;
ALTER TABLE artifacts ADD COLUMN job_id uuid REFERENCES jobs (id);
CREATE INDEX artifacts_job_idx ON artifacts (job_id) WHERE job_id IS NOT NULL;
