-- Imported research history: records loaded by `cannery import` from a
-- reviewed bundle keep their historical dates and carry an origin mark, so
-- they can never pass for live, tester-verified work. The importer only
-- inserts; every append-only trigger stays as it is. See docs/import.md.

-- Every record a bundle can create says where it came from: `live` (the
-- default, every API write) or `imported`, which requires a source
-- reference (a document location, an artifact URI or the bundle path). A
-- live row never has one.
DO $$
DECLARE
    tbl text;
BEGIN
    FOREACH tbl IN ARRAY ARRAY['hypotheses', 'hypothesis_revisions', 'attempts',
                                'evidence_records', 'decisions', 'review_cases', 'artifacts']
    LOOP
        EXECUTE format(
            'ALTER TABLE %I ADD COLUMN origin text NOT NULL DEFAULT ''live''
                 CHECK (origin IN (''live'', ''imported''))', tbl);
        EXECUTE format('ALTER TABLE %I ADD COLUMN source_ref text CHECK (source_ref ~ ''\S'')', tbl);
        EXECUTE format(
            'ALTER TABLE %I ADD CONSTRAINT %I CHECK ((origin = ''imported'') = (source_ref IS NOT NULL))',
            tbl, tbl || '_origin_source_check');
    END LOOP;
END;
$$;

-- An imported hypothesis keeps its id from the source history, unique per
-- project. Only imported hypotheses have one.
ALTER TABLE hypotheses ADD COLUMN external_id text
    CHECK (external_id ~ '^[A-Za-z0-9][A-Za-z0-9._:/+@-]{0,127}$');
ALTER TABLE hypotheses ADD CONSTRAINT hypotheses_external_id_imported
    CHECK (external_id IS NULL OR origin = 'imported');
CREATE UNIQUE INDEX hypotheses_external_id_idx ON hypotheses (project_id, external_id)
    WHERE external_id IS NOT NULL;

-- What the history states about an imported hypothesis or run beyond the
-- live columns, kept as the bundle gave it: a hypothesis's id, kind, claim,
-- control, sources and notes (its revision holds only a live document), a
-- run's label, configuration, notes and source revision (a failed run has no
-- evidence to hold them). Only imported rows have it, and they always do.
ALTER TABLE hypotheses ADD COLUMN imported jsonb CHECK (jsonb_typeof(imported) = 'object');
ALTER TABLE hypotheses ADD CONSTRAINT hypotheses_imported_origin_check
    CHECK ((origin = 'imported') = (imported IS NOT NULL));
ALTER TABLE attempts ADD COLUMN imported jsonb CHECK (jsonb_typeof(imported) = 'object');
ALTER TABLE attempts ADD CONSTRAINT attempts_imported_origin_check
    CHECK ((origin = 'imported') = (imported IS NOT NULL));

-- An imported attempt that finished without a decision of its own (another
-- run of the same hypothesis was decided, or it is still under review) is
-- `unreviewed`: terminal, and never a live state.
ALTER TABLE attempts DROP CONSTRAINT attempts_state_check;
ALTER TABLE attempts ADD CONSTRAINT attempts_state_check CHECK (state IN (
    'claimed', 'running', 'submitted', 'validating', 'testing', 'evaluating',
    'awaiting_human_review', 'promoted', 'rejected', 'inconclusive', 'failed', 'cancelled',
    'unreviewed'));
ALTER TABLE attempts ADD CONSTRAINT attempts_unreviewed_imported
    CHECK (state <> 'unreviewed' OR origin = 'imported');
DROP INDEX attempts_one_open_idx;
CREATE UNIQUE INDEX attempts_one_open_idx ON attempts (hypothesis_id)
    WHERE state NOT IN ('promoted', 'rejected', 'inconclusive', 'failed', 'cancelled',
                        'unreviewed');

-- Imported evidence is the history's measurements (a tester-stage record)
-- and its verdicts (an evaluator-stage record). No user or service account
-- published it, so it has no producer; it is never a claimed result sheet.
ALTER TABLE evidence_records DROP CONSTRAINT evidence_records_producer_check;
ALTER TABLE evidence_records ADD CONSTRAINT evidence_records_producer_check CHECK (
    (origin = 'live' AND (num_nonnulls(producer_user, producer_service) = 1
                          OR (stage = 'evaluator' AND producer_user IS NULL
                              AND producer_service IS NULL)))
    OR (origin = 'imported' AND stage IN ('tester', 'evaluator')
        AND producer_user IS NULL AND producer_service IS NULL)
);

-- Two authorities for imported measurements: read from a run artifact
-- (`imported_artifact`, sourced by the artifact's URI and a JSON Pointer) or
-- copied from a document (`imported_transcribed`, sourced by file, line and
-- commit). A live record's authority is still derived from the stage that
-- published it and never read from the document; an imported record's is
-- read from it, and the source it names is stored. Only an imported
-- authority has a source, so a live row can never carry one.
ALTER TABLE measurements DROP CONSTRAINT measurements_authority_check;
ALTER TABLE measurements ADD CONSTRAINT measurements_authority_check CHECK (authority IN (
    'agent_claim', 'tester_verified', 'imported_artifact', 'imported_transcribed'));
ALTER TABLE measurements ADD COLUMN source_ref text;
ALTER TABLE measurements ADD CONSTRAINT measurements_source_ref_check CHECK (
    (authority IN ('imported_artifact', 'imported_transcribed')) = (source_ref IS NOT NULL));

-- An imported record names one of the two imported authorities on each
-- measurement; anything else (a live authority, none) is refused rather than
-- stored under an authority the record does not have.
CREATE OR REPLACE FUNCTION measurements_from_evidence(e evidence_records) RETURNS void
LANGUAGE plpgsql AS $$
BEGIN
    IF e.origin = 'imported' AND EXISTS (
        SELECT 1 FROM jsonb_array_elements(coalesce(e.content -> 'measurements', '[]')) AS m
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
    FROM jsonb_array_elements(coalesce(e.content -> 'measurements', '[]')) AS m
    WHERE e.stage = 'agent' OR (e.stage = 'tester' AND e.status = 'completed');
END;
$$;

-- An imported artifact is a reference to an object outside the store (a
-- `gs://` or `s3://` URI) with its size and SHA-256: backend `external`,
-- never copied and never downloadable through the API. Several records may
-- reference one object, so only stored objects keep a unique key.
ALTER TABLE artifacts ADD COLUMN uri text CHECK (uri ~ '^[a-z][a-z0-9+.-]*://\S+$');
ALTER TABLE artifacts ADD CONSTRAINT artifacts_external_check CHECK (
    (backend = 'external') = (uri IS NOT NULL)
    AND (backend = 'external') = (origin = 'imported'));
ALTER TABLE artifacts DROP CONSTRAINT artifacts_backend_bucket_key_key;
CREATE UNIQUE INDEX artifacts_stored_key_idx ON artifacts (backend, bucket, key)
    WHERE backend <> 'external';

-- Historical evaluator policies: the rules imported verdicts were reached
-- under, recorded as documents and never run. Immutable.
CREATE TABLE historical_policies (
    project_id uuid NOT NULL REFERENCES projects (id),
    id         text NOT NULL CHECK (id ~ '^[a-z0-9][a-z0-9-]{0,62}$'),
    revision   text NOT NULL CHECK (revision ~ '^[A-Za-z0-9][A-Za-z0-9._:/+@-]{0,255}$'),
    content    jsonb NOT NULL,
    source_ref text NOT NULL CHECK (source_ref ~ '\S'),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, id, revision)
);
CREATE TRIGGER historical_policies_immutable BEFORE UPDATE OR DELETE ON historical_policies
    FOR EACH ROW EXECUTE FUNCTION forbid_mutation();

-- What each import loaded, entry by entry, as the bundle stated it: a
-- re-run of the same entry is a no-op, a changed entry is refused with the
-- difference. Updating an import is out of scope. Immutable.
CREATE TABLE import_entries (
    project_id    uuid NOT NULL REFERENCES projects (id),
    kind          text NOT NULL CHECK (kind IN ('project', 'track', 'policy', 'hypothesis')),
    key           text NOT NULL,
    content       jsonb NOT NULL,
    sha256        text NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    bundle_sha256 text NOT NULL CHECK (bundle_sha256 ~ '^[0-9a-f]{64}$'),
    imported_at   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, kind, key)
);
CREATE TRIGGER import_entries_immutable BEFORE UPDATE OR DELETE ON import_entries
    FOR EACH ROW EXECUTE FUNCTION forbid_mutation();
