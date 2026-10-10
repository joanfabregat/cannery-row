-- SPDX-License-Identifier: AGPL-3.0-only
-- One verify job: a submitted run is verified by a single job that ends with
-- a verification report, Markdown with YAML front matter holding the verdict,
-- the gates, the verified measurements, the discrepancies, the comparisons
-- and the provenance. Its performer is the runner, under a registered
-- verifier service account, or an agent: an agent service account or a
-- researcher who did not run the attempt. The tester and evaluator stages,
-- their jobs, their service accounts and their attempt states go.
--
-- History is kept and rewritten into the new shape:
--   * tester and evaluator service accounts become verifiers;
--   * every science revision registers `verify`: the runner under the
--     tester's name and the evaluator's policy revision, or, for a revision
--     that registered no evaluator (built-in gates) or no complete one, an agent;
--   * each evaluator record and the tester records it assessed become one
--     verification record; a tester record nothing assessed becomes one
--     without a verdict;
--   * tester and evaluator jobs become verify jobs of the runner, numbered
--     in the order they were created;
--   * an attempt waiting on a test or an evaluation fails with code
--     `superseded` and a failure case; a researcher's retry queues a verify
--     job for it.

ALTER TABLE jobs DISABLE TRIGGER jobs_frozen;
ALTER TABLE phase_outputs DISABLE TRIGGER phase_outputs_immutable;
ALTER TABLE phase_outputs DISABLE TRIGGER phase_outputs_indexed;
ALTER TABLE measurements DISABLE TRIGGER measurements_immutable;
ALTER TABLE comparisons DISABLE TRIGGER comparisons_immutable;
ALTER TABLE decisions DISABLE TRIGGER decisions_immutable;
ALTER TABLE manifests DISABLE TRIGGER manifests_immutable;
ALTER TABLE attempt_failures DISABLE TRIGGER attempt_failures_immutable;
ALTER TABLE config_revisions DISABLE TRIGGER config_revisions_immutable;

-- Service accounts.
ALTER TABLE service_accounts DROP CONSTRAINT service_accounts_kind_check;
UPDATE service_accounts SET kind = 'verifier' WHERE kind IN ('tester', 'evaluator');
ALTER TABLE service_accounts ADD CONSTRAINT service_accounts_kind_check
    CHECK (kind IN ('agent', 'experimenter', 'verifier'));

-- Science revisions.
UPDATE config_revisions SET content =
    (content - 'tester' - 'evaluator' - 'gates')
    || jsonb_build_object('verify',
        CASE WHEN NOT content ? 'gates'
                  AND jsonb_typeof(content #> '{evaluator,revision}') = 'string'
                  AND jsonb_typeof(coalesce(content #> '{tester,id}',
                                            content #> '{evaluator,id}')) = 'string'
             THEN jsonb_build_object(
                 'performer', 'runner',
                 'verifier', jsonb_build_object(
                     'id', coalesce(content #> '{tester,id}', content #> '{evaluator,id}'),
                     'revision', content #> '{evaluator,revision}'))
             ELSE '{"performer": "agent"}'::jsonb
        END)
    || CASE WHEN jsonb_typeof(content -> 'required_artifact_roles') = 'object'
            THEN jsonb_build_object('required_artifact_roles',
                ((content -> 'required_artifact_roles') - 'tester')
                || jsonb_build_object('verify',
                       coalesce(content #> '{required_artifact_roles,tester}', '[]'::jsonb)))
            ELSE '{}'::jsonb
       END
WHERE kind = 'science';

-- Attempts waiting on a test or an evaluation fail for review.
CREATE TEMPORARY TABLE superseded_attempts ON COMMIT DROP AS
SELECT a.id, a.project_id, a.hypothesis_id, a.state,
       ARRAY(SELECT j.id::text FROM jobs j
             WHERE j.attempt_id = a.id AND j.state IN ('pending', 'claimed')
             ORDER BY j.created_at, j.id) AS jobs
FROM attempts a
WHERE a.state IN ('submitted', 'testing', 'evaluating');

-- Verification records.
ALTER TABLE phase_outputs DROP CONSTRAINT phase_outputs_stage_check;
ALTER TABLE phase_outputs DROP CONSTRAINT phase_outputs_producer_check;

-- The tester records each evaluator record assessed, in the order it cites them.
CREATE TEMPORARY TABLE verification_cited ON COMMIT DROP AS
SELECT e.id AS evaluator_id, t.id AS tester_id, ref.ord
FROM phase_outputs e
CROSS JOIN LATERAL jsonb_array_elements(
    CASE WHEN jsonb_typeof(e.front_matter #> '{assessment,evidence}') = 'array'
         THEN e.front_matter #> '{assessment,evidence}' ELSE '[]'::jsonb END)
    WITH ORDINALITY AS ref(value, ord)
JOIN phase_outputs t ON t.attempt_id = e.attempt_id AND t.stage = 'tester'
                    AND t.id::text = ref.value ->> 'ref'
WHERE e.stage = 'evaluator';

CREATE TEMPORARY TABLE verification_merged (
    id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    evaluator_id uuid,
    tester_id    uuid,
    attempt_id   uuid NOT NULL,
    created_at   timestamptz NOT NULL,
    old_revision integer,
    revision     integer
) ON COMMIT DROP;
INSERT INTO verification_merged (evaluator_id, tester_id, attempt_id, created_at, old_revision)
SELECT e.id,
       (SELECT c.tester_id FROM verification_cited c WHERE c.evaluator_id = e.id
        ORDER BY c.ord LIMIT 1),
       e.attempt_id, e.created_at, e.revision
FROM phase_outputs e WHERE e.stage = 'evaluator'
UNION ALL
SELECT NULL, t.id, t.attempt_id, t.created_at, NULL
FROM phase_outputs t
WHERE t.stage = 'tester'
  AND NOT EXISTS (SELECT 1 FROM verification_cited c WHERE c.tester_id = t.id);
UPDATE verification_merged m SET revision = n.revision
FROM (SELECT id, row_number() OVER (PARTITION BY attempt_id ORDER BY created_at, id)::integer
             AS revision
      FROM verification_merged) n
WHERE n.id = m.id;

-- Every tester record goes to the latest verification record that holds it,
-- the one reads of the latest verification find.
CREATE TEMPORARY TABLE verification_targets ON COMMIT DROP AS
SELECT DISTINCT ON (s.tester_id) s.tester_id, m.id AS verification_id
FROM (SELECT tester_id, id AS merged_id FROM verification_merged WHERE tester_id IS NOT NULL
      UNION ALL
      SELECT c.tester_id, m.id FROM verification_cited c
      JOIN verification_merged m ON m.evaluator_id = c.evaluator_id) s
JOIN verification_merged m ON m.id = s.merged_id
ORDER BY s.tester_id, m.revision DESC;

-- Top-level keys whose value is null are left out.
CREATE FUNCTION pg_temp.present(value jsonb) RETURNS jsonb LANGUAGE sql AS $$
    SELECT coalesce(jsonb_object_agg(key, item), '{}'::jsonb)
    FROM jsonb_each(value) AS field(key, item)
    WHERE jsonb_typeof(item) <> 'null'
$$;

INSERT INTO phase_outputs (id, project_id, attempt_id, stage, status, revision, front_matter,
                           body, sha256, manifest_id, producer_user, producer_service,
                           via_channel, via_client, created_at, origin, source_ref)
SELECT m.id, coalesce(e.project_id, t.project_id), m.attempt_id, 'verification',
       CASE WHEN e.id IS NOT NULL THEN e.status WHEN t.origin = 'imported' THEN 'completed'
            ELSE 'failed' END,
       m.revision, f.front_matter, f.body,
       encode(sha256(convert_to(f.front_matter::text || E'\n' || f.body, 'UTF8')), 'hex'),
       coalesce(t.manifest_id, e.manifest_id),
       CASE WHEN t.producer_user IS NOT NULL OR t.producer_service IS NOT NULL
            THEN t.producer_user ELSE e.producer_user END,
       CASE WHEN t.producer_user IS NOT NULL OR t.producer_service IS NOT NULL
            THEN t.producer_service ELSE e.producer_service END,
       coalesce(e.via_channel, t.via_channel), coalesce(e.via_client, t.via_client),
       m.created_at, coalesce(e.origin, t.origin), coalesce(e.source_ref, t.source_ref)
FROM verification_merged m
LEFT JOIN phase_outputs e ON e.id = m.evaluator_id
LEFT JOIN phase_outputs t ON t.id = m.tester_id
CROSS JOIN LATERAL (
    SELECT pg_temp.present(jsonb_build_object(
               'verdict', e.front_matter #> '{assessment,verdict}',
               'reason', e.front_matter #> '{assessment,reason}',
               'policy_revision', e.front_matter #> '{assessment,policy_revision}',
               'gates', e.front_matter #> '{assessment,gates}',
               'measurements', coalesce((
                   SELECT jsonb_agg(item ORDER BY s.ord, i.ord)
                   FROM (SELECT m.tester_id AS tester_id, 0::bigint AS ord
                         WHERE m.evaluator_id IS NULL
                         UNION ALL
                         SELECT c.tester_id, c.ord FROM verification_cited c
                         WHERE c.evaluator_id = m.evaluator_id) s
                   JOIN phase_outputs r ON r.id = s.tester_id
                   CROSS JOIN LATERAL jsonb_array_elements(
                       CASE WHEN jsonb_typeof(r.front_matter -> 'measurements') = 'array'
                            THEN r.front_matter -> 'measurements' ELSE '[]'::jsonb END)
                       WITH ORDINALITY AS i(item, ord)), '[]'::jsonb),
               'discrepancies', (
                   SELECT jsonb_agg(item ORDER BY s.ord, i.ord)
                   FROM (SELECT m.tester_id AS tester_id, 0::bigint AS ord
                         WHERE m.evaluator_id IS NULL
                         UNION ALL
                         SELECT c.tester_id, c.ord FROM verification_cited c
                         WHERE c.evaluator_id = m.evaluator_id) s
                   JOIN phase_outputs r ON r.id = s.tester_id
                   CROSS JOIN LATERAL jsonb_array_elements(
                       CASE WHEN jsonb_typeof(r.front_matter -> 'discrepancies') = 'array'
                            THEN r.front_matter -> 'discrepancies' ELSE '[]'::jsonb END)
                       WITH ORDINALITY AS i(item, ord)),
               'comparisons', e.front_matter #> '{assessment,comparisons}',
               'provenance', coalesce(t.front_matter -> 'provenance',
                                      e.front_matter -> 'provenance') - 'tester_revision',
               'artifact_roles', t.front_matter -> 'artifact_roles',
               'extensions', coalesce(t.front_matter -> 'extensions',
                                      e.front_matter -> 'extensions')
           )) AS front_matter,
           coalesce(t.front_matter ->> 'observations', '') AS body
) f;

UPDATE measurements x SET evidence_id = v.verification_id
FROM verification_targets v WHERE x.evidence_id = v.tester_id;
UPDATE comparisons x SET evidence_id = m.id
FROM verification_merged m WHERE x.evidence_id = m.evaluator_id;
UPDATE jobs x SET evidence_id = m.id
FROM verification_merged m WHERE x.evidence_id = m.evaluator_id;
UPDATE jobs x SET evidence_id = v.verification_id
FROM verification_targets v WHERE x.evidence_id = v.tester_id;
UPDATE decisions d SET subject_revision = m.revision
FROM review_cases c JOIN verification_merged m ON m.evaluator_id = c.evidence_id
WHERE d.review_case_id = c.id AND d.subject_revision = m.old_revision;
UPDATE review_cases c SET evidence_id = m.id, subject_revision = m.revision
FROM verification_merged m WHERE c.evidence_id = m.evaluator_id;
DELETE FROM search_documents WHERE kind IN ('tester_observation', 'evaluator_reason');
DELETE FROM phase_outputs WHERE stage IN ('tester', 'evaluator');

ALTER TABLE phase_outputs ADD CONSTRAINT phase_outputs_stage_check
    CHECK (stage IN ('agent', 'verification', 'writeup'));
-- A verification record of the former built-in evaluator has no user or
-- service account behind it; every other live record has exactly one.
ALTER TABLE phase_outputs ADD CONSTRAINT phase_outputs_producer_check CHECK (
    (origin = 'live' AND (num_nonnulls(producer_user, producer_service) = 1
                          OR (stage = 'verification' AND producer_user IS NULL
                              AND producer_service IS NULL)))
    OR (origin = 'imported' AND stage IN ('verification', 'writeup')
        AND producer_user IS NULL AND producer_service IS NULL)
);

-- Manifests and failures name the verify phase.
ALTER TABLE manifests DROP CONSTRAINT manifests_stage_check;
UPDATE manifests SET stage = 'verify' WHERE stage IN ('tester', 'evaluator');
ALTER TABLE manifests ADD CONSTRAINT manifests_stage_check CHECK (stage IN ('agent', 'verify'));
ALTER TABLE attempt_failures DROP CONSTRAINT attempt_failures_stage_check;
UPDATE attempt_failures SET stage = 'verify' WHERE stage IN ('tester', 'evaluator');
ALTER TABLE attempt_failures ADD CONSTRAINT attempt_failures_stage_check
    CHECK (stage IN ('agent', 'verify'));

-- Verify jobs.
DROP INDEX jobs_one_open_idx;
DROP INDEX jobs_queue_idx;
ALTER TABLE jobs DROP CONSTRAINT jobs_stage_check;
ALTER TABLE jobs DROP CONSTRAINT jobs_manifest_check;
ALTER TABLE jobs DROP CONSTRAINT jobs_attempt_id_stage_run_number_key;
DO $$
DECLARE
    found text;
BEGIN
    SELECT conname INTO STRICT found FROM pg_constraint
    WHERE conrelid = 'jobs'::regclass AND contype = 'c'
      AND pg_get_constraintdef(oid) LIKE '%claimed_by_service IS NOT NULL%';
    EXECUTE format('ALTER TABLE jobs DROP CONSTRAINT %I', found);
END;
$$;
ALTER TABLE jobs RENAME COLUMN stage TO phase;
ALTER TABLE jobs RENAME COLUMN tester_id TO verifier_id;
ALTER TABLE jobs ALTER COLUMN verifier_id DROP NOT NULL;
ALTER TABLE jobs ADD COLUMN performer text NOT NULL DEFAULT 'runner';
ALTER TABLE jobs ALTER COLUMN performer DROP DEFAULT;
ALTER TABLE jobs ADD COLUMN claimed_by_user uuid REFERENCES users (id);
UPDATE jobs j SET run_number = n.run_number, phase = 'verify'
FROM (SELECT id, row_number() OVER (
                 PARTITION BY attempt_id
                 ORDER BY created_at, CASE phase WHEN 'tester' THEN 0 ELSE 1 END, run_number,
                          id)::integer AS run_number
      FROM jobs) n
WHERE n.id = j.id;
ALTER TABLE jobs ADD CONSTRAINT jobs_phase_check CHECK (phase = 'verify');
ALTER TABLE jobs ADD CONSTRAINT jobs_attempt_id_phase_run_number_key
    UNIQUE (attempt_id, phase, run_number);
-- The runner performs a job under the registered verifier; an agent under
-- no service name.
ALTER TABLE jobs ADD CONSTRAINT jobs_performer_check CHECK (
    performer IN ('runner', 'agent') AND (performer = 'runner') = (verifier_id IS NOT NULL));
-- A claimed or finished job has exactly one claimant, a service account or a
-- researcher, unless it was superseded before anyone claimed it.
ALTER TABLE jobs ADD CONSTRAINT jobs_claimant_check CHECK (
    num_nonnulls(claimed_by_service, claimed_by_user) <= 1
    AND (state = 'pending' OR error_code = 'superseded'
         OR (num_nonnulls(claimed_by_service, claimed_by_user) = 1 AND deadline IS NOT NULL)));
-- An agent may complete its job without uploading anything.
ALTER TABLE jobs ADD CONSTRAINT jobs_manifest_check
    CHECK (manifest_id IS NULL OR state = 'completed');
-- At most one open job per attempt and phase.
CREATE UNIQUE INDEX jobs_one_open_idx ON jobs (attempt_id, phase)
    WHERE state IN ('pending', 'claimed');
CREATE INDEX jobs_queue_idx ON jobs (project_id, phase, created_at) WHERE state = 'pending';

CREATE OR REPLACE FUNCTION jobs_freeze_spec() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'jobs are never deleted';
    END IF;
    IF (NEW.project_id, NEW.attempt_id, NEW.phase, NEW.run_number, NEW.science_revision,
        NEW.performer, NEW.verifier_id, NEW.spec, NEW.deadline_seconds, NEW.created_at,
        NEW.origin, NEW.previous_run_id)
       IS DISTINCT FROM
       (OLD.project_id, OLD.attempt_id, OLD.phase, OLD.run_number, OLD.science_revision,
        OLD.performer, OLD.verifier_id, OLD.spec, OLD.deadline_seconds, OLD.created_at,
        OLD.origin, OLD.previous_run_id) THEN
        RAISE EXCEPTION 'a job''s spec is immutable';
    END IF;
    IF OLD.state IN ('completed', 'failed') THEN
        RAISE EXCEPTION 'a finished job is immutable';
    END IF;
    IF NEW.lease_generation < OLD.lease_generation THEN
        RAISE EXCEPTION 'a job''s lease generation never decreases';
    END IF;
    -- A claimed job keeps its verifier until it finishes or returns to the queue.
    IF OLD.state = 'claimed' AND NEW.state <> 'pending'
       AND (NEW.claimed_by_service, NEW.claimed_by_user)
           IS DISTINCT FROM (OLD.claimed_by_service, OLD.claimed_by_user) THEN
        RAISE EXCEPTION 'a claimed job''s verifier cannot change';
    END IF;
    RETURN NEW;
END;
$$;

-- The superseded attempts fail with a failure case each, and their open
-- tester or evaluator jobs with them.
UPDATE jobs SET state = 'failed', error_code = 'superseded',
    error_reason = 'verification became a single verify job before this run finished',
    lease_token_hash = NULL, lease_expires_at = NULL, finished_at = now()
WHERE state IN ('pending', 'claimed');
INSERT INTO attempt_failures (attempt_id, stage, code, reason, details)
SELECT s.id, 'verify', 'superseded',
       'Verification became a single verify job while this attempt waited to be tested or '
       || 'evaluated; retry it to verify the run.',
       jsonb_build_object('state', s.state, 'job_ids', to_jsonb(s.jobs))
FROM superseded_attempts s ORDER BY s.id;
UPDATE attempts a SET state = 'failed', finished_at = coalesce(a.finished_at, now())
FROM superseded_attempts s WHERE a.id = s.id;
UPDATE hypotheses h SET state = 'awaiting_human_review', updated_at = now()
FROM superseded_attempts s WHERE h.id = s.hypothesis_id;
INSERT INTO review_cases (project_id, hypothesis_id, attempt_id, kind, subject_revision,
                          failure_id)
SELECT s.project_id, s.hypothesis_id, s.id, 'failure',
       (SELECT count(*) FROM attempt_failures f WHERE f.attempt_id = s.id),
       (SELECT f.id FROM attempt_failures f WHERE f.attempt_id = s.id AND f.code = 'superseded'
        ORDER BY f.created_at DESC, f.id LIMIT 1)
FROM superseded_attempts s ORDER BY s.id;
INSERT INTO audit_events (project_id, actor_kind, via_channel, action, subject_type, subject_id,
                          prior_state, new_state, reason)
SELECT s.project_id, 'system', 'system', 'attempt.failed', 'attempt', s.id::text,
       jsonb_build_object('state', s.state),
       jsonb_build_object('state', 'failed', 'stage', 'verify', 'code', 'superseded'),
       'Verification became a single verify job while this attempt waited to be tested or '
       || 'evaluated; retry it to verify the run.'
FROM superseded_attempts s ORDER BY s.project_id, s.id;

ALTER TABLE attempts DROP CONSTRAINT attempts_state_check;
ALTER TABLE attempts ADD CONSTRAINT attempts_state_check CHECK (state IN (
    'claimed', 'running', 'verifying', 'awaiting_human_review', 'promoted', 'rejected',
    'inconclusive', 'failed', 'cancelled', 'unreviewed'));

-- Search: a verification report is one document, its verdict as the title
-- and its reason, body and discrepancies as the text.
ALTER TABLE search_documents DROP CONSTRAINT search_documents_kind_check;
ALTER TABLE search_documents ADD CONSTRAINT search_documents_kind_check CHECK (kind IN (
    'track', 'hypothesis', 'attempt', 'report', 'verification', 'decision_reason', 'comment'));

CREATE OR REPLACE FUNCTION search_index_evidence(e phase_outputs) RETURNS void
LANGUAGE plpgsql AS $$
DECLARE
    hypothesis uuid;
    body text;
BEGIN
    IF e.stage <> 'verification' THEN
        RETURN;
    END IF;
    SELECT hypothesis_id INTO hypothesis FROM attempts WHERE id = e.attempt_id;
    body := concat_ws(E'\n', e.front_matter ->> 'reason', nullif(e.body, ''), (
        SELECT string_agg(d ->> 'description', E'\n')
        FROM jsonb_array_elements(
            CASE WHEN jsonb_typeof(e.front_matter -> 'discrepancies') = 'array'
                 THEN e.front_matter -> 'discrepancies' ELSE '[]'::jsonb END) AS d));
    IF body ~ '\S' OR e.front_matter ? 'verdict' THEN
        PERFORM search_put(e.project_id, 'verification', e.id, NULL, hypothesis, e.attempt_id,
                           e.producer_user, e.producer_service,
                           coalesce(e.front_matter ->> 'verdict', ''), body, e.created_at);
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
                WHEN e.stage = 'verification' THEN 'tester_verified'
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
    WHERE e.stage = 'agent' OR (e.stage = 'verification' AND e.status = 'completed');
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
           e.front_matter ->> 'verdict', e.front_matter ->> 'policy_revision', e.created_at
    FROM attempts a,
         jsonb_array_elements(coalesce(e.front_matter -> 'comparisons', '[]')) AS c
    WHERE a.id = e.attempt_id AND e.stage = 'verification' AND e.status = 'completed'
$$;

SELECT search_index_evidence(p) FROM phase_outputs p
WHERE p.stage = 'verification' ORDER BY p.created_at, p.id;

ALTER TABLE config_revisions ENABLE TRIGGER config_revisions_immutable;
ALTER TABLE attempt_failures ENABLE TRIGGER attempt_failures_immutable;
ALTER TABLE manifests ENABLE TRIGGER manifests_immutable;
ALTER TABLE decisions ENABLE TRIGGER decisions_immutable;
ALTER TABLE comparisons ENABLE TRIGGER comparisons_immutable;
ALTER TABLE measurements ENABLE TRIGGER measurements_immutable;
ALTER TABLE phase_outputs ENABLE TRIGGER phase_outputs_indexed;
ALTER TABLE phase_outputs ENABLE TRIGGER phase_outputs_immutable;
ALTER TABLE jobs ENABLE TRIGGER jobs_frozen;
