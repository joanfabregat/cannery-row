-- SPDX-License-Identifier: AGPL-3.0-only
-- Track plans. A track's plan is a chain of revisions. A researcher, or an
-- agent acting through a researcher's token, builds a revision as a draft
-- one piece at a time: the shared approach, then one entry per hypothesis
-- (unit) it lists, then an alignment entry for every hypothesis of the track
-- that is done or in flight. Submitting freezes the draft and opens a `plan`
-- review case; approving it creates and revises the hypotheses it lists in
-- one transaction.

-- A track starts in `planning`: nothing in it can be claimed until its first
-- plan is approved. Tracks created before plans keep working when they hold
-- hypotheses; the others wait for a plan. The column default stays `active`
-- for rows written directly; every track the API creates is set to
-- `planning` explicitly.
ALTER TABLE tracks DROP CONSTRAINT tracks_state_check;
ALTER TABLE tracks ADD CONSTRAINT tracks_state_check
    CHECK (state IN ('planning', 'active', 'paused', 'archived'));
UPDATE tracks t SET state = 'planning'
WHERE t.state = 'active'
  AND NOT EXISTS (SELECT 1 FROM hypotheses h WHERE h.track_id = t.id);

-- Size limits of what performers are handed, per project. A write that
-- exceeds one is refused where it is made; reads only truncate the two
-- derived lines (an index line, a context summary).
CREATE TABLE project_limits (
    project_id                uuid PRIMARY KEY REFERENCES projects (id),
    brief_max_bytes           integer NOT NULL DEFAULT 65536
                              CHECK (brief_max_bytes BETWEEN 1024 AND 262144),
    plan_approach_max_bytes   integer NOT NULL DEFAULT 65536
                              CHECK (plan_approach_max_bytes BETWEEN 1024 AND 262144),
    unit_brief_max_bytes      integer NOT NULL DEFAULT 32768
                              CHECK (unit_brief_max_bytes BETWEEN 1024 AND 131072),
    context_items_max         integer NOT NULL DEFAULT 64
                              CHECK (context_items_max BETWEEN 1 AND 256),
    index_line_max_bytes      integer NOT NULL DEFAULT 100
                              CHECK (index_line_max_bytes BETWEEN 40 AND 1000),
    context_summary_max_bytes integer NOT NULL DEFAULT 300
                              CHECK (context_summary_max_bytes BETWEEN 80 AND 2000),
    updated_by                uuid NOT NULL REFERENCES users (id),
    updated_at                timestamptz NOT NULL DEFAULT now()
);

-- A plan review case concerns a track's plan revision, not a hypothesis.
ALTER TABLE review_cases ALTER COLUMN hypothesis_id DROP NOT NULL;
ALTER TABLE review_cases DROP CONSTRAINT review_cases_kind_check;
ALTER TABLE review_cases ADD CONSTRAINT review_cases_kind_check
    CHECK (kind IN ('draft', 'plan', 'result', 'failure'));
ALTER TABLE review_cases DROP CONSTRAINT review_cases_attempt_check;
ALTER TABLE review_cases ADD CONSTRAINT review_cases_attempt_check
    CHECK ((kind IN ('draft', 'plan')) = (attempt_id IS NULL));
ALTER TABLE review_cases ADD CONSTRAINT review_cases_plan_check
    CHECK ((kind = 'plan') = (hypothesis_id IS NULL));
ALTER TABLE decisions DROP CONSTRAINT decisions_action_check;
ALTER TABLE decisions ADD CONSTRAINT decisions_action_check CHECK (action IN (
    'approve', 'request_revision', 'send_back', 'decline', 'promote', 'reject',
    'inconclusive', 'retry', 'close_failed'));

-- Plan decisions concern no hypothesis and are not search documents.
CREATE OR REPLACE FUNCTION search_index_decision(d decisions) RETURNS void LANGUAGE sql AS $$
    SELECT search_put(c.project_id, 'decision_reason', d.id, NULL, c.hypothesis_id,
                      c.attempt_id, d.actor_user_id, NULL, d.action, d.reason, d.decided_at)
    FROM review_cases c WHERE c.id = d.review_case_id AND c.hypothesis_id IS NOT NULL
$$;

CREATE TABLE plan_revisions (
    id              uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id      uuid NOT NULL REFERENCES projects (id),
    track_id        uuid NOT NULL REFERENCES tracks (id),
    revision        integer NOT NULL CHECK (revision > 0),
    state           text NOT NULL DEFAULT 'draft' CHECK (state IN (
                        'draft', 'submitted', 'approved', 'sent_back', 'declined')),
    -- The revision whose content the draft started from, if any.
    based_on        integer,
    -- The track's shared approach and reasoning, Markdown.
    approach        text NOT NULL DEFAULT '' CHECK (octet_length(approach) <= 262144),
    created_by      uuid NOT NULL REFERENCES users (id),
    via_channel     text NOT NULL CHECK (via_channel IN ('ui', 'api', 'mcp', 'cli')),
    via_client      text,
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now(),
    submitted_by    uuid REFERENCES users (id),
    submitted_at    timestamptz,
    review_case_id  uuid UNIQUE REFERENCES review_cases (id),
    reviewed_by     uuid REFERENCES users (id),
    review_reason   text,
    reviewed_at     timestamptz,
    UNIQUE (track_id, revision),
    CHECK ((state = 'draft') = (submitted_at IS NULL)),
    CHECK ((state = 'draft') = (review_case_id IS NULL)),
    CHECK ((state IN ('approved', 'sent_back', 'declined')) = (reviewed_at IS NOT NULL)),
    CHECK (reviewed_at IS NULL OR (reviewed_by IS NOT NULL AND review_reason ~ '\S'))
);
-- At most one revision of a track is open (a draft or under review).
CREATE UNIQUE INDEX plan_revisions_one_open_idx ON plan_revisions (track_id)
    WHERE state IN ('draft', 'submitted');
CREATE INDEX plan_revisions_project_idx ON plan_revisions (project_id, state);

-- The units a revision lists. An entry with a hypothesis names an existing
-- unit (the plan keeps or revises it); one without is a new unit, created on
-- approval. Entries change only while their revision is a draft.
CREATE TABLE plan_units (
    plan_revision_id    uuid NOT NULL REFERENCES plan_revisions (id),
    key                 text NOT NULL CHECK (key ~ '^[a-z0-9][a-z0-9-]{0,62}$'),
    position            integer NOT NULL,
    hypothesis_id       uuid REFERENCES hypotheses (id),
    -- A unit added by a `redo` alignment entry, derived from that hypothesis.
    redo_of             uuid REFERENCES hypotheses (id),
    -- title, question, intervention, control, acceptance, parameters,
    -- relations and context, as validated when the entry was written.
    fields              jsonb NOT NULL CHECK (jsonb_typeof(fields) = 'object'),
    -- The unit's own text for whoever runs it, Markdown.
    brief               text NOT NULL DEFAULT '' CHECK (octet_length(brief) <= 131072),
    science_revision    integer NOT NULL,
    -- Set on approval: the hypothesis revision this entry wrote, if it changed.
    hypothesis_revision integer,
    PRIMARY KEY (plan_revision_id, key)
);
CREATE UNIQUE INDEX plan_units_hypothesis_idx ON plan_units (plan_revision_id, hypothesis_id)
    WHERE hypothesis_id IS NOT NULL;

-- One alignment entry per hypothesis of the track that is done or in flight.
CREATE TABLE plan_alignments (
    plan_revision_id uuid NOT NULL REFERENCES plan_revisions (id),
    hypothesis_id    uuid NOT NULL REFERENCES hypotheses (id),
    decision         text NOT NULL CHECK (decision IN ('keep', 'obsolete', 'redo')),
    reason           text NOT NULL CHECK (reason ~ '\S'),
    PRIMARY KEY (plan_revision_id, hypothesis_id)
);
CREATE INDEX plan_alignments_hypothesis_idx ON plan_alignments (hypothesis_id);

-- A hypothesis revision written by a plan carries the unit's brief.
ALTER TABLE hypothesis_revisions ADD COLUMN brief text;

-- An attempt pins the plan revision approved when it was claimed; NULL when
-- the track had no approved plan then.
ALTER TABLE attempts ADD COLUMN plan_revision integer;
ALTER TABLE attempts ADD CONSTRAINT attempts_plan_fkey
    FOREIGN KEY (track_id, plan_revision) REFERENCES plan_revisions (track_id, revision);
