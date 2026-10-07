-- Research records: configuration revisions, tracks, hypotheses with their
-- draft revisions, relations and mentions, review cases and decisions, and
-- idempotency keys for repeatable writes.

-- Rows of append-only tables are never updated or deleted.
CREATE FUNCTION forbid_mutation() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION '% is append-only', TG_TABLE_NAME;
END;
$$;

-- Hypothesis numbers are sequential per project (#1, #2, ...), whatever the track.
ALTER TABLE projects ADD COLUMN next_hypothesis_number integer NOT NULL DEFAULT 1;

-- Immutable project configuration. Science and dashboard are versioned
-- independently; a dashboard revision records the science revision it was
-- validated against.
CREATE TABLE config_revisions (
    project_id       uuid NOT NULL REFERENCES projects (id),
    kind             text NOT NULL CHECK (kind IN ('science', 'dashboard')),
    revision         integer NOT NULL CHECK (revision > 0),
    content          jsonb NOT NULL,
    science_revision integer CHECK ((kind = 'dashboard') = (science_revision IS NOT NULL)),
    created_by       uuid NOT NULL REFERENCES users (id),
    created_at       timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, kind, revision)
);
CREATE TRIGGER config_revisions_immutable BEFORE UPDATE OR DELETE ON config_revisions
    FOR EACH ROW EXECUTE FUNCTION forbid_mutation();

CREATE TABLE tracks (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id  uuid NOT NULL REFERENCES projects (id),
    slug        text NOT NULL CHECK (slug ~ '^[a-z0-9][a-z0-9-]{0,62}$'),
    title       text NOT NULL CHECK (title ~ '\S'),
    description text NOT NULL DEFAULT '',
    producer    jsonb,
    gates       jsonb NOT NULL DEFAULT '[]',
    state       text NOT NULL DEFAULT 'active' CHECK (state IN ('active', 'paused', 'archived')),
    revision    integer NOT NULL DEFAULT 1,
    created_by  uuid NOT NULL REFERENCES users (id),
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now(),
    UNIQUE (project_id, slug)
);

CREATE TABLE hypotheses (
    id                 uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id         uuid NOT NULL REFERENCES projects (id),
    number             integer NOT NULL CHECK (number > 0),
    track_id           uuid NOT NULL REFERENCES tracks (id),
    state              text NOT NULL DEFAULT 'draft' CHECK (state IN (
                           'draft', 'queued', 'active', 'awaiting_human_review', 'promoted',
                           'rejected', 'inconclusive', 'declined', 'failed', 'cancelled')),
    revision           integer NOT NULL DEFAULT 1,
    approved_revision  integer,
    title              text NOT NULL,
    created_by_user    uuid REFERENCES users (id),
    created_by_service uuid REFERENCES service_accounts (id),
    created_at         timestamptz NOT NULL DEFAULT now(),
    updated_at         timestamptz NOT NULL DEFAULT now(),
    approved_at        timestamptz,
    UNIQUE (project_id, number),
    CHECK ((created_by_user IS NULL) <> (created_by_service IS NULL)),
    -- Only an approved exact revision leaves draft review for the work queue.
    CHECK (state IN ('draft', 'declined', 'cancelled')
           OR (approved_revision IS NOT NULL AND approved_at IS NOT NULL)),
    CHECK (approved_revision IS NULL OR approved_revision <= revision)
);
CREATE INDEX hypotheses_project_state_idx ON hypotheses (project_id, state, number);
CREATE INDEX hypotheses_track_state_idx ON hypotheses (track_id, state);

-- Every draft revision, as submitted. Approval pins one of them.
CREATE TABLE hypothesis_revisions (
    hypothesis_id    uuid NOT NULL REFERENCES hypotheses (id),
    revision         integer NOT NULL CHECK (revision > 0),
    content          jsonb NOT NULL,
    science_revision integer NOT NULL,
    author_user      uuid REFERENCES users (id),
    author_service   uuid REFERENCES service_accounts (id),
    via_channel      text NOT NULL,
    via_client       text,
    created_at       timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (hypothesis_id, revision),
    CHECK ((author_user IS NULL) <> (author_service IS NULL))
);
CREATE TRIGGER hypothesis_revisions_immutable BEFORE UPDATE OR DELETE ON hypothesis_revisions
    FOR EACH ROW EXECUTE FUNCTION forbid_mutation();

-- Typed relations of the current revision (a projection, rebuilt on edit).
CREATE TABLE hypothesis_relations (
    hypothesis_id uuid NOT NULL REFERENCES hypotheses (id),
    kind          text NOT NULL CHECK (kind IN ('derived_from', 'supersedes', 'related_to')),
    target_id     uuid NOT NULL REFERENCES hypotheses (id),
    PRIMARY KEY (hypothesis_id, kind, target_id),
    CHECK (hypothesis_id <> target_id)
);
CREATE INDEX hypothesis_relations_target_idx ON hypothesis_relations (target_id);

-- "#123" and "project#123" mentions in Markdown, indexed as backlinks.
CREATE TABLE mentions (
    source_type text NOT NULL CHECK (source_type IN ('hypothesis', 'attempt', 'comment', 'report')),
    source_id   uuid NOT NULL,
    target_id   uuid NOT NULL REFERENCES hypotheses (id),
    PRIMARY KEY (source_type, source_id, target_id)
);
CREATE INDEX mentions_target_idx ON mentions (target_id);

-- Human review. A case is opened for the exact revision under review and
-- resolved by a decision; decisions are immutable and corrected by a new
-- decision that supersedes the old one.
CREATE TABLE review_cases (
    id               uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id       uuid NOT NULL REFERENCES projects (id),
    hypothesis_id    uuid NOT NULL REFERENCES hypotheses (id),
    kind             text NOT NULL CHECK (kind IN ('draft', 'result', 'failure')),
    subject_revision integer NOT NULL,
    state            text NOT NULL DEFAULT 'pending' CHECK (state IN ('pending', 'resolved')),
    opened_at        timestamptz NOT NULL DEFAULT now(),
    resolved_at      timestamptz,
    CHECK ((state = 'resolved') = (resolved_at IS NOT NULL))
);
CREATE UNIQUE INDEX review_cases_one_pending_draft_idx ON review_cases (hypothesis_id)
    WHERE state = 'pending' AND kind = 'draft';
CREATE INDEX review_cases_queue_idx ON review_cases (project_id, state, kind, opened_at);

CREATE TABLE decisions (
    id               uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    review_case_id   uuid NOT NULL REFERENCES review_cases (id),
    action           text NOT NULL CHECK (action IN (
                         'approve', 'request_revision', 'decline', 'promote', 'reject',
                         'inconclusive', 'retry', 'close_failed')),
    subject_revision integer NOT NULL,
    reason           text NOT NULL CHECK (reason ~ '\S'),
    actor_user_id    uuid NOT NULL REFERENCES users (id),
    via_channel      text NOT NULL,
    via_client       text,
    decided_at       timestamptz NOT NULL DEFAULT now(),
    supersedes       uuid REFERENCES decisions (id)
);
CREATE INDEX decisions_case_idx ON decisions (review_case_id, decided_at);
CREATE TRIGGER decisions_immutable BEFORE UPDATE OR DELETE ON decisions
    FOR EACH ROW EXECUTE FUNCTION forbid_mutation();

-- Replay protection for writes that take an Idempotency-Key header. The key
-- is scoped to the operation and the authenticated actor; a replay with a
-- different request body is a conflict.
CREATE TABLE idempotency_keys (
    scope        text NOT NULL,
    actor        text NOT NULL,
    key          text NOT NULL CHECK (length(key) BETWEEN 1 AND 200),
    request_hash bytea NOT NULL,
    result_id    text NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (scope, actor, key)
);
