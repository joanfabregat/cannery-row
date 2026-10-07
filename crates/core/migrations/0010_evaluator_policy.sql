-- The evaluator owns thresholds and baselines: Cannery Row keeps no gates of
-- its own, and stores what each evaluator compared as queryable rows.

-- Track gates were built-in gates added per track, pinned at claim. The
-- registered evaluator now owns the whole policy, so they go, with their
-- stored values.
ALTER TABLE attempts DROP COLUMN track_gates;
ALTER TABLE tracks DROP COLUMN gates;

-- Comparisons of evaluator records as rows: per metric, split and slice, the
-- value the evaluator compared and the reference it compared it against, as
-- its own verdict reported them. Track and hypothesis are denormalised from
-- the attempt, like the attempt and project of a measurement row, so a
-- track's comparisons read as a time series without a join.
CREATE TABLE comparisons (
    id              bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    project_id      uuid NOT NULL REFERENCES projects (id),
    track_id        uuid NOT NULL REFERENCES tracks (id),
    hypothesis_id   uuid NOT NULL REFERENCES hypotheses (id),
    attempt_id      uuid NOT NULL REFERENCES attempts (id),
    evidence_id     uuid NOT NULL REFERENCES evidence_records (id),
    metric          text NOT NULL,
    split           text NOT NULL,
    dimensions      jsonb NOT NULL,
    dimension_keys  text[] NOT NULL,
    value           double precision NOT NULL,
    source          text NOT NULL CHECK (source IN ('tester', 'evaluator')),
    reference_value double precision NOT NULL,
    reference_label text NOT NULL,
    reference_kind  text NOT NULL CHECK (reference_kind IN (
                        'paper', 'benchmark', 'promoted_attempt', 'baseline', 'manual',
                        'other')),
    reference_ref   text,
    verdict         text NOT NULL CHECK (verdict IN ('pass', 'fail', 'inconclusive')),
    policy_revision text NOT NULL,
    recorded_at     timestamptz NOT NULL
);
-- /comparisons pages by id, newest first: the project's comparisons, and one
-- metric and split across tracks (a track filter joins tracks by slug and
-- scans the same order).
CREATE INDEX comparisons_project_idx ON comparisons (project_id, id);
CREATE INDEX comparisons_metric_idx ON comparisons (project_id, metric, split, id);
-- The comparisons of one evaluator record (the chart overlay of its attempt).
CREATE INDEX comparisons_evidence_idx ON comparisons (evidence_id);
CREATE TRIGGER comparisons_immutable BEFORE UPDATE OR DELETE ON comparisons
    FOR EACH ROW EXECUTE FUNCTION forbid_mutation();

CREATE FUNCTION comparisons_from_evidence(e evidence_records) RETURNS void
LANGUAGE sql AS $$
    INSERT INTO comparisons (project_id, track_id, hypothesis_id, attempt_id, evidence_id,
                             metric, split, dimensions, dimension_keys, value, source,
                             reference_value, reference_label, reference_kind, reference_ref,
                             verdict, policy_revision, recorded_at)
    SELECT e.project_id, a.track_id, a.hypothesis_id, e.attempt_id, e.id,
           c ->> 'metric', c ->> 'split', coalesce(c -> 'dimensions', '{}'),
           ARRAY(SELECT k FROM jsonb_object_keys(coalesce(c -> 'dimensions', '{}')) AS k
                 ORDER BY k COLLATE "C"),
           (c ->> 'value')::double precision, c ->> 'source',
           (c #>> '{reference,value}')::double precision, c #>> '{reference,label}',
           c #>> '{reference,kind}', c #>> '{reference,ref}',
           e.content #>> '{assessment,verdict}', e.content #>> '{assessment,policy_revision}',
           e.created_at
    FROM attempts a,
         jsonb_array_elements(coalesce(e.content #> '{assessment,comparisons}', '[]')) AS c
    WHERE a.id = e.attempt_id AND e.stage = 'evaluator' AND e.status = 'completed'
$$;

CREATE OR REPLACE FUNCTION index_on_insert() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    CASE TG_TABLE_NAME
        WHEN 'tracks' THEN PERFORM search_index_track(NEW);
        WHEN 'hypothesis_revisions' THEN PERFORM search_index_hypothesis(NEW);
        WHEN 'evidence_records' THEN
            PERFORM search_index_evidence(NEW);
            PERFORM measurements_from_evidence(NEW);
            PERFORM comparisons_from_evidence(NEW);
        WHEN 'decisions' THEN PERFORM search_index_decision(NEW);
        WHEN 'attempts' THEN PERFORM search_index_attempt(NEW);
        WHEN 'attempt_failures' THEN PERFORM search_index_failure(NEW);
        WHEN 'comments' THEN PERFORM search_index_comment(NEW);
    END CASE;
    RETURN NULL;
END;
$$;

-- No backfill: records published before this migration (by the built-in
-- evaluator) carry no comparisons, so points evaluated before it have no
-- reference overlay on charts or in the per-track history. Only records
-- published from now on fill the table.
