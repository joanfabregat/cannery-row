-- SPDX-License-Identifier: AGPL-3.0-only
-- Plan concerns. Any performer, a researcher or a service account working on
-- the project, can raise a concern about a track's plan: a wrong assumption,
-- a better idea, a blocker. A concern is a short Markdown document: front
-- matter with its kind and the hypothesis or attempt it comes from, the
-- argument as its body. While a concern is open no new hypothesis of its
-- track can be claimed; work already claimed continues. A plan revision
-- answers it (the revision lists it with how it answers it, and its approval
-- closes it), or a researcher dismisses it with a reason.

CREATE TABLE concerns (
    id                uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id        uuid NOT NULL REFERENCES projects (id),
    track_id          uuid NOT NULL REFERENCES tracks (id),
    kind              text NOT NULL CHECK (kind IN (
                          'wrong_assumption', 'better_idea', 'blocker', 'other')),
    -- The hypothesis, or the attempt of it, the concern comes from.
    hypothesis_id     uuid REFERENCES hypotheses (id),
    attempt_id        uuid REFERENCES attempts (id),
    front_matter      jsonb NOT NULL CHECK (jsonb_typeof(front_matter) = 'object'),
    body              text NOT NULL CHECK (body ~ '\S' AND octet_length(body) <= 16384),
    sha256            text NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    raised_by_user    uuid REFERENCES users (id),
    raised_by_service uuid REFERENCES service_accounts (id),
    via_channel       text NOT NULL CHECK (via_channel IN ('ui', 'api', 'mcp', 'cli')),
    via_client        text,
    raised_at         timestamptz NOT NULL DEFAULT now(),
    state             text NOT NULL DEFAULT 'open'
                          CHECK (state IN ('open', 'answered', 'dismissed')),
    -- The approved plan revision that answered it.
    answered_by       uuid REFERENCES plan_revisions (id),
    dismissed_by      uuid REFERENCES users (id),
    dismissal_reason  text,
    closed_at         timestamptz,
    CHECK (num_nonnulls(raised_by_user, raised_by_service) = 1),
    CHECK (attempt_id IS NULL OR hypothesis_id IS NOT NULL),
    CHECK ((state = 'open') = (closed_at IS NULL)),
    CHECK ((state = 'answered') = (answered_by IS NOT NULL)),
    CHECK ((state = 'dismissed') = (dismissed_by IS NOT NULL)),
    CHECK ((state = 'dismissed') = (dismissal_reason IS NOT NULL)),
    CHECK (dismissal_reason IS NULL OR dismissal_reason ~ '\S')
);
CREATE INDEX concerns_open_idx ON concerns (track_id) WHERE state = 'open';
CREATE INDEX concerns_project_idx ON concerns (project_id, raised_at, id);

-- A concern is written once; only an open one closes, once.
CREATE FUNCTION concerns_close_only() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'concerns are never deleted';
    END IF;
    IF OLD.state <> 'open' THEN
        RAISE EXCEPTION 'a closed concern is immutable';
    END IF;
    IF (NEW.id, NEW.project_id, NEW.track_id, NEW.kind, NEW.hypothesis_id, NEW.attempt_id,
        NEW.front_matter, NEW.body, NEW.sha256, NEW.raised_by_user, NEW.raised_by_service,
        NEW.via_channel, NEW.via_client, NEW.raised_at)
       IS DISTINCT FROM
       (OLD.id, OLD.project_id, OLD.track_id, OLD.kind, OLD.hypothesis_id, OLD.attempt_id,
        OLD.front_matter, OLD.body, OLD.sha256, OLD.raised_by_user, OLD.raised_by_service,
        OLD.via_channel, OLD.via_client, OLD.raised_at) THEN
        RAISE EXCEPTION 'a concern''s document is immutable';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER concerns_close_only BEFORE UPDATE OR DELETE ON concerns
    FOR EACH ROW EXECUTE FUNCTION concerns_close_only();

-- The concerns a plan revision answers, each with how. Entries change only
-- while their revision is a draft; approving the revision closes the
-- concerns it answers that are still open.
CREATE TABLE plan_answers (
    plan_revision_id uuid NOT NULL REFERENCES plan_revisions (id),
    concern_id       uuid NOT NULL REFERENCES concerns (id),
    how              text NOT NULL CHECK (how ~ '\S' AND octet_length(how) <= 16384),
    PRIMARY KEY (plan_revision_id, concern_id)
);
CREATE INDEX plan_answers_concern_idx ON plan_answers (concern_id);
