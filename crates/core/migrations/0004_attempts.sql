-- Attempts: claims and leases, uploads, verified artifacts and manifests,
-- evidence records and attempt failures.

-- Each claim of a hypothesis increments its lease generation.
ALTER TABLE hypotheses ADD COLUMN lease_generation integer NOT NULL DEFAULT 0;

CREATE TABLE attempts (
    id                  uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id          uuid NOT NULL REFERENCES projects (id),
    hypothesis_id       uuid NOT NULL REFERENCES hypotheses (id),
    sequence            integer NOT NULL CHECK (sequence > 0),
    state               text NOT NULL DEFAULT 'claimed' CHECK (state IN (
                            'claimed', 'running', 'submitted', 'validating', 'testing',
                            'evaluating', 'awaiting_human_review', 'promoted', 'rejected',
                            'inconclusive', 'failed', 'cancelled')),
    -- Rules pinned at claim time, so later edits cannot change them.
    hypothesis_revision integer NOT NULL,
    science_revision    integer NOT NULL,
    track_id            uuid NOT NULL REFERENCES tracks (id),
    producer            jsonb,
    claimed_by_user     uuid REFERENCES users (id),
    claimed_by_service  uuid REFERENCES service_accounts (id),
    via_channel         text NOT NULL,
    via_client          text,
    predecessor_id      uuid REFERENCES attempts (id),
    lease_generation    integer NOT NULL,
    lease_token_hash    bytea UNIQUE,
    lease_expires_at    timestamptz,
    claimed_at          timestamptz NOT NULL DEFAULT now(),
    started_at          timestamptz,
    submitted_at        timestamptz,
    finished_at         timestamptz,
    UNIQUE (hypothesis_id, sequence),
    CHECK ((claimed_by_user IS NULL) <> (claimed_by_service IS NULL)),
    -- Only a claimed or running attempt holds a lease.
    CHECK ((state IN ('claimed', 'running')) = (lease_token_hash IS NOT NULL)),
    CHECK ((lease_token_hash IS NULL) = (lease_expires_at IS NULL))
);
-- At most one attempt in progress per hypothesis in v1.
CREATE UNIQUE INDEX attempts_one_open_idx ON attempts (hypothesis_id)
    WHERE state NOT IN ('promoted', 'rejected', 'inconclusive', 'failed', 'cancelled');
CREATE INDEX attempts_project_state_idx ON attempts (project_id, state);
CREATE INDEX attempts_lease_expiry_idx ON attempts (lease_expires_at)
    WHERE lease_expires_at IS NOT NULL;

-- One-time upload grants. The grant's secret travels in a header; only its
-- hash is stored. An expired unused grant can be replaced for the same key.
CREATE TABLE uploads (
    id               uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    attempt_id       uuid NOT NULL REFERENCES attempts (id),
    lease_generation integer NOT NULL,
    token_hash       bytea NOT NULL UNIQUE,
    role             text NOT NULL,
    backend          text NOT NULL,
    bucket           text NOT NULL,
    key              text NOT NULL,
    declared_size    bigint NOT NULL CHECK (declared_size >= 0),
    declared_sha256  text NOT NULL CHECK (declared_sha256 ~ '^[0-9a-f]{64}$'),
    media_type       text NOT NULL,
    state            text NOT NULL DEFAULT 'pending'
                         CHECK (state IN ('pending', 'receiving', 'verified', 'failed')),
    expires_at       timestamptz NOT NULL,
    created_at       timestamptz NOT NULL DEFAULT now(),
    completed_at     timestamptz,
    UNIQUE (backend, bucket, key)
);
CREATE INDEX uploads_attempt_idx ON uploads (attempt_id);

-- Objects whose presence, size and SHA-256 the server verified.
CREATE TABLE artifacts (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id  uuid NOT NULL REFERENCES projects (id),
    attempt_id  uuid NOT NULL REFERENCES attempts (id),
    role        text NOT NULL,
    backend     text NOT NULL,
    bucket      text NOT NULL,
    key         text NOT NULL,
    generation  text,
    size_bytes  bigint NOT NULL CHECK (size_bytes >= 0),
    sha256      text NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    media_type  text NOT NULL,
    verified_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (backend, bucket, key)
);
CREATE INDEX artifacts_attempt_idx ON artifacts (attempt_id, role);
CREATE TRIGGER artifacts_immutable BEFORE UPDATE OR DELETE ON artifacts
    FOR EACH ROW EXECUTE FUNCTION forbid_mutation();

-- Verified artifact manifests. The digest is over the canonical JSON.
CREATE TABLE manifests (
    id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    attempt_id uuid NOT NULL REFERENCES attempts (id),
    stage      text NOT NULL CHECK (stage IN ('agent', 'tester')),
    content    jsonb NOT NULL,
    sha256     text NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX manifests_attempt_idx ON manifests (attempt_id);
CREATE TRIGGER manifests_immutable BEFORE UPDATE OR DELETE ON manifests
    FOR EACH ROW EXECUTE FUNCTION forbid_mutation();

-- Evidence envelopes (claimed result sheets now; tester and evaluator
-- records later). Frozen on acceptance.
CREATE TABLE evidence_records (
    id               uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id       uuid NOT NULL REFERENCES projects (id),
    attempt_id       uuid NOT NULL REFERENCES attempts (id),
    stage            text NOT NULL CHECK (stage IN ('agent', 'tester', 'evaluator')),
    status           text NOT NULL CHECK (status IN ('completed', 'failed')),
    revision         integer NOT NULL DEFAULT 1,
    content          jsonb NOT NULL,
    sha256           text NOT NULL,
    manifest_id      uuid REFERENCES manifests (id),
    producer_user    uuid REFERENCES users (id),
    producer_service uuid REFERENCES service_accounts (id),
    via_channel      text NOT NULL,
    via_client       text,
    created_at       timestamptz NOT NULL DEFAULT now(),
    UNIQUE (attempt_id, stage, revision)
);
CREATE TRIGGER evidence_records_immutable BEFORE UPDATE OR DELETE ON evidence_records
    FOR EACH ROW EXECUTE FUNCTION forbid_mutation();

CREATE TABLE attempt_failures (
    id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    attempt_id uuid NOT NULL REFERENCES attempts (id),
    stage      text NOT NULL CHECK (stage IN ('agent', 'tester', 'evaluator')),
    code       text NOT NULL,
    reason     text NOT NULL CHECK (reason ~ '\S'),
    details    jsonb NOT NULL DEFAULT '{}',
    log_refs   jsonb NOT NULL DEFAULT '[]',
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX attempt_failures_attempt_idx ON attempt_failures (attempt_id);
CREATE TRIGGER attempt_failures_immutable BEFORE UPDATE OR DELETE ON attempt_failures
    FOR EACH ROW EXECUTE FUNCTION forbid_mutation();

-- Result and failure review cases are about an attempt.
ALTER TABLE review_cases ADD COLUMN attempt_id uuid REFERENCES attempts (id);
ALTER TABLE review_cases ADD CONSTRAINT review_cases_attempt_check
    CHECK ((kind = 'draft') = (attempt_id IS NULL));
