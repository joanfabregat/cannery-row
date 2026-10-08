-- SPDX-License-Identifier: AGPL-3.0-only
-- Phase outputs: every output of a phase is one Markdown document with YAML
-- front matter. The front matter is what Cannery Row needs for gating and
-- indexing, validated against the phase's JSON Schema; the body is free
-- prose. Evidence records become phase outputs: a stored evidence envelope
-- is the front matter of its record, with an empty body. An imported
-- attempt's retrospective report becomes an imported write-up: its
-- authorship as front matter, its Markdown as the body.

ALTER TABLE evidence_records RENAME TO phase_outputs;
ALTER TABLE phase_outputs RENAME COLUMN content TO front_matter;
ALTER TABLE phase_outputs ADD COLUMN body text NOT NULL DEFAULT '';
ALTER INDEX evidence_records_project_stage_idx RENAME TO phase_outputs_project_stage_idx;
ALTER TRIGGER evidence_records_immutable ON phase_outputs RENAME TO phase_outputs_immutable;
ALTER TRIGGER evidence_records_indexed ON phase_outputs RENAME TO phase_outputs_indexed;
DO $$
DECLARE
    found text;
BEGIN
    FOR found IN SELECT conname FROM pg_constraint
                 WHERE conrelid = 'phase_outputs'::regclass AND conname LIKE 'evidence\_records\_%'
    LOOP
        EXECUTE format('ALTER TABLE phase_outputs RENAME CONSTRAINT %I TO %I',
                       found, 'phase_outputs_' || substr(found, length('evidence_records_') + 1));
    END LOOP;
END;
$$;

-- A write-up is the `writeup` stage: completed, with a non-empty body. Only
-- imported write-ups exist for now, published by no user or service account.
ALTER TABLE phase_outputs DROP CONSTRAINT phase_outputs_stage_check;
ALTER TABLE phase_outputs ADD CONSTRAINT phase_outputs_stage_check
    CHECK (stage IN ('agent', 'tester', 'evaluator', 'writeup'));
ALTER TABLE phase_outputs ADD CONSTRAINT phase_outputs_writeup_check CHECK (
    stage <> 'writeup'
    OR (origin = 'imported' AND status = 'completed' AND jsonb_typeof(front_matter) = 'object'
        AND body ~ '\S' AND octet_length(body) <= 262144 AND sha256 ~ '^[0-9a-f]{64}$'));
ALTER TABLE phase_outputs DROP CONSTRAINT phase_outputs_producer_check;
ALTER TABLE phase_outputs ADD CONSTRAINT phase_outputs_producer_check CHECK (
    (origin = 'live' AND (num_nonnulls(producer_user, producer_service) = 1
                          OR (stage = 'evaluator' AND producer_user IS NULL
                              AND producer_service IS NULL)))
    OR (origin = 'imported' AND stage IN ('tester', 'evaluator', 'writeup')
        AND producer_user IS NULL AND producer_service IS NULL)
);

-- The read side reads the front matter. Only the evidence stages are indexed,
-- measured or compared; a write-up is none of them.
CREATE OR REPLACE FUNCTION search_index_evidence(e phase_outputs) RETURNS void
LANGUAGE plpgsql AS $$
DECLARE
    hypothesis uuid;
    body text;
BEGIN
    SELECT hypothesis_id INTO hypothesis FROM attempts WHERE id = e.attempt_id;
    IF e.stage = 'agent' THEN
        DELETE FROM search_documents
        WHERE kind = 'report' AND attempt_id = e.attempt_id AND source_id <> e.id;
    END IF;
    IF e.stage = 'agent' AND e.front_matter ? 'report' THEN
        PERFORM search_put(
            e.project_id, 'report', e.id, NULL, hypothesis, e.attempt_id, e.producer_user,
            e.producer_service, e.front_matter #>> '{report,what_was_tried}',
            concat_ws(E'\n',
                e.front_matter #>> '{report,configuration}',
                e.front_matter #>> '{report,observations}',
                e.front_matter #>> '{report,findings}', e.front_matter #>> '{report,limitations}',
                e.front_matter #>> '{report,next_question}', e.front_matter ->> 'observations',
                e.front_matter #>> '{report,body_markdown}'),
            e.created_at);
    ELSIF e.stage = 'tester' THEN
        body := concat_ws(E'\n', e.front_matter ->> 'observations', (
            SELECT string_agg(d ->> 'description', E'\n')
            FROM jsonb_array_elements(coalesce(e.front_matter -> 'discrepancies', '[]')) AS d));
        IF body ~ '\S' THEN
            PERFORM search_put(e.project_id, 'tester_observation', e.id, NULL, hypothesis,
                               e.attempt_id, e.producer_user, e.producer_service, '', body,
                               e.created_at);
        END IF;
    ELSIF e.stage = 'evaluator' AND e.front_matter ? 'assessment' THEN
        PERFORM search_put(e.project_id, 'evaluator_reason', e.id, NULL, hypothesis,
                           e.attempt_id, e.producer_user, e.producer_service,
                           e.front_matter #>> '{assessment,verdict}',
                           e.front_matter #>> '{assessment,reason}', e.created_at);
    END IF;
END;
$$;

CREATE OR REPLACE FUNCTION measurements_from_evidence(e phase_outputs) RETURNS void
LANGUAGE plpgsql AS $$
BEGIN
    IF e.origin = 'imported' AND EXISTS (
        SELECT 1 FROM jsonb_array_elements(coalesce(e.front_matter -> 'measurements', '[]')) AS m
        WHERE coalesce(m ->> 'authority', '')
              NOT IN ('imported_artifact', 'imported_transcribed')) THEN
        RAISE EXCEPTION 'an imported measurement needs an imported authority'
            USING ERRCODE = 'check_violation', CONSTRAINT = 'measurements_imported_authority';
    END IF;
    INSERT INTO measurements (project_id, attempt_id, evidence_id, authority, metric, split,
                              dimensions, dimension_keys, value, missing_reason, unit,
                              direction, sample_count, control_value, uncertainty_method,
                              uncertainty_lower, uncertainty_upper, recorded_at, source_ref)
    SELECT e.project_id, e.attempt_id, e.id,
           CASE WHEN e.origin = 'imported' THEN m ->> 'authority'
                WHEN e.stage = 'tester' THEN 'tester_verified'
                ELSE 'agent_claim' END,
           m ->> 'metric', m ->> 'split', coalesce(m -> 'dimensions', '{}'),
           ARRAY(SELECT k FROM jsonb_object_keys(coalesce(m -> 'dimensions', '{}')) AS k
                 ORDER BY k COLLATE "C"),
           (m ->> 'value')::double precision, m ->> 'missing_reason', m ->> 'unit',
           m ->> 'direction', (m ->> 'sample_count')::numeric,
           (m ->> 'control_value')::double precision, m #>> '{uncertainty,method}',
           (m #>> '{uncertainty,lower}')::double precision,
           (m #>> '{uncertainty,upper}')::double precision, e.created_at,
           CASE WHEN e.origin = 'imported' THEN m ->> 'source' END
    FROM jsonb_array_elements(coalesce(e.front_matter -> 'measurements', '[]')) AS m
    WHERE e.stage = 'agent' OR (e.stage = 'tester' AND e.status = 'completed');
END;
$$;

CREATE OR REPLACE FUNCTION comparisons_from_evidence(e phase_outputs) RETURNS void
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
           e.front_matter #>> '{assessment,verdict}',
           e.front_matter #>> '{assessment,policy_revision}',
           e.created_at
    FROM attempts a,
         jsonb_array_elements(coalesce(e.front_matter #> '{assessment,comparisons}', '[]')) AS c
    WHERE a.id = e.attempt_id AND e.stage = 'evaluator' AND e.status = 'completed'
$$;

CREATE OR REPLACE FUNCTION index_on_insert() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    CASE TG_TABLE_NAME
        WHEN 'tracks' THEN PERFORM search_index_track(NEW);
        WHEN 'hypothesis_revisions' THEN PERFORM search_index_hypothesis(NEW);
        WHEN 'phase_outputs' THEN
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

-- Imported reports become imported write-ups, keeping their dates, digest and
-- source. The front matter says how and when the report was written: a day
-- (`written_on`) or an instant (`written_at`), as the bundle gave it.
INSERT INTO phase_outputs (project_id, attempt_id, stage, status, revision, front_matter, body,
                           sha256, via_channel, via_client, created_at, origin, source_ref)
SELECT a.project_id, r.attempt_id, 'writeup', 'completed', 1,
       jsonb_strip_nulls(jsonb_build_object('kind', r.kind, 'author', r.author,
                                            'written_on', r.written_on,
                                            'written_at', r.written_at)),
       r.body_markdown, r.sha256, 'cli', 'cannery import', r.created_at, 'imported', r.source_ref
FROM imported_reports r JOIN attempts a ON a.id = r.attempt_id
ORDER BY r.created_at, r.attempt_id;
DROP TABLE imported_reports;
ALTER TABLE attempts DROP CONSTRAINT attempts_id_origin_key;
