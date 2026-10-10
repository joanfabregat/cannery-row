-- SPDX-License-Identifier: AGPL-3.0-only
-- Run documents: a completed run submits one Markdown document whose front
-- matter holds its claims, provenance, artifact roles and verified manifest,
-- and whose body holds optional run notes. Its claims are measured like the
-- measurements of a claimed result sheet. Run notes are not indexed for
-- search, and neither are new claimed reports; reports already indexed stay.

CREATE OR REPLACE FUNCTION search_index_evidence(e phase_outputs) RETURNS void
LANGUAGE plpgsql AS $$
DECLARE
    hypothesis uuid;
    body text;
BEGIN
    SELECT hypothesis_id INTO hypothesis FROM attempts WHERE id = e.attempt_id;
    IF e.stage = 'tester' THEN
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
    FROM jsonb_array_elements(
        CASE WHEN e.stage = 'agent' AND e.front_matter ? 'claims' THEN e.front_matter -> 'claims'
             ELSE coalesce(e.front_matter -> 'measurements', '[]') END) AS m
    WHERE e.stage = 'agent' OR (e.stage = 'tester' AND e.status = 'completed');
END;
$$;
